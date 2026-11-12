//! VM NIC를 Linux 브리지에 잇는 tap 장치와 VXLAN 네트워크의 장치. iproute2(`ip`, `bridge`) 명령을
//! 부르므로 opld가 root(또는 CAP_NET_ADMIN)로 돌아야 한다. `bridge` 네트워크의 브리지는 관리자가
//! 미리 만들고, VXLAN 네트워크의 브리지(`oplv<vni>`)와 VXLAN 장치(`oplx<vni>`)는 opld가 만든다.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use async_trait::async_trait;
use opl_types::network::{VXLAN_PORT, is_managed_ifname, tap_prefix, vxlan_bridge, vxlan_device};
use opl_types::node::BridgeInfo;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::DriverError;

/// 드라이버가 만들어 브리지에 붙이는 tap 장치
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tap {
    pub ifname: String,
    pub bridge: String,
    pub vlan: Option<u16>,
}

/// 이 노드가 가져야 할 VXLAN 네트워크 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlay {
    pub vni: u32,
    pub mtu: u16,
    /// 이 노드의 underlay 주소(VXLAN 패킷을 보내는 주소)
    pub local: IpAddr,
    /// 다른 노드의 underlay 주소. BUM 패킷을 모두에게 보낸다.
    pub peers: Vec<IpAddr>,
}

/// `sync_overlays`가 바꾼 것
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayChanges {
    /// 장치를 만들거나 지우거나 다시 만들었다(브리지 목록이 바뀌었을 수 있다). FDB·MTU만 바꿨으면 false.
    pub devices_changed: bool,
}

/// 호스트 네트워크 조작. 테스트에서 바꿔 끼울 수 있게 트레이트로 둔다.
#[async_trait]
pub trait HostNet: Send + Sync + std::fmt::Debug {
    /// 이 호스트의 Linux 브리지
    async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError>;
    /// tap을 만들어 브리지에 붙이고 올린다. 같은 이름이 남아 있으면 지우고 다시 만든다.
    async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError>;
    /// VM의 tap을 모두 지운다. 없으면 아무것도 하지 않는다.
    async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError>;
    /// VXLAN 네트워크의 장치를 `desired`에 맞춘다. alias가 `opl:<tag>`인 관리 이름 장치만 내 것으로 보고,
    /// `desired`에 없는 내 장치는 지운다. 한 네트워크가 실패해도 나머지는 맞추고 오류를 모아 돌려준다.
    async fn sync_overlays(
        &self,
        tag: &str,
        desired: &[Overlay],
    ) -> Result<OverlayChanges, DriverError>;
}

/// iproute2로 tap과 VXLAN 장치를 다룬다.
#[derive(Debug, Default)]
pub struct IpRoute {
    /// 있으면 모든 `ip`·`bridge` 명령을 이 네트워크 이름공간에서 부른다(`-n NAME`, 시험용).
    netns: Option<String>,
    /// 이 노드의 클러스터 태그. 관리 이름 브리지(`oplv…`)는 alias가 `opl:<tag>`인 것만 브리지 목록에
    /// 넣고 tap을 붙인다. 없으면(위트니스, 클러스터 전) 관리 이름 브리지를 모두 뺀다.
    tag: Option<String>,
}

impl IpRoute {
    pub fn in_netns(name: impl Into<String>) -> Self {
        Self {
            netns: Some(name.into()),
            tag: None,
        }
    }

    /// 관리 이름 브리지 가운데 이 클러스터(`tag`)의 것만 쓴다.
    pub fn with_cluster_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    async fn run(
        &self,
        program: &'static str,
        args: &[impl AsRef<str>],
    ) -> Result<String, DriverError> {
        run(program, &netns_args(self.netns.as_deref(), args)).await
    }

    async fn delete_link(&self, ifname: &str) -> Result<(), DriverError> {
        match self.run("ip", &["link", "del", "dev", ifname]).await {
            Err(DriverError::ToolFailed { stderr, .. })
                if stderr.contains("Cannot find device") =>
            {
                Ok(())
            }
            other => other.map(|_| ()),
        }
    }

    async fn run_all(&self, cmds: Vec<Command>) -> Result<(), DriverError> {
        for (program, args) in cmds {
            self.run(program, &args).await?;
        }
        Ok(())
    }

    /// VXLAN 네트워크 하나를 맞춘다. 장치를 만들었으면 true.
    async fn sync_overlay(
        &self,
        alias: &str,
        overlay: &Overlay,
        links: &[OverlayLink],
    ) -> Result<bool, DriverError> {
        let plan = device_plan(alias, overlay, links)?;
        for (i, (program, args)) in plan.cmds.iter().enumerate() {
            if let Err(e) = self.run(program, args).await {
                // 반쯤 만든 장치(alias가 없으면 다음 차례에 내 것으로 보지 못한다)를 남기지 않는다.
                // 이번에 만든 장치만 지운다.
                for name in rollback_links(&plan.cmds[..i]) {
                    let _ = self.delete_link(&name).await;
                }
                return Err(e);
            }
        }
        let (peers, skipped) = usable_peers(overlay);
        if !skipped.is_empty() {
            tracing::warn!(
                vni = overlay.vni,
                local = %overlay.local,
                ?skipped,
                "ignoring VXLAN peers of a different address family than the local underlay"
            );
        }
        let dev = vxlan_device(overlay.vni);
        let out = self
            .run("bridge", &["-j", "fdb", "show", "dev", &dev])
            .await?;
        let current = parse_fdb_dsts(&out)?;
        self.run_all(fdb_commands(&dev, &current, &peers)).await?;
        Ok(!plan.created.is_empty())
    }
}

#[async_trait]
impl HostNet for IpRoute {
    async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
        let out = self
            .run("ip", &["-j", "-d", "link", "show", "type", "bridge"])
            .await?;
        let foreign = foreign_bridges(&out, self.tag.as_deref())?;
        Ok(parse_bridges(&out)?
            .into_iter()
            .map(|b| b.info)
            .filter(|b| !foreign.contains(&b.name))
            .collect())
    }

    async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError> {
        let out = self
            .run("ip", &["-j", "-d", "link", "show", "type", "bridge"])
            .await?;
        let bridges = parse_bridges(&out)?;
        let Some(bridge) = bridges.iter().find(|b| b.info.name == tap.bridge) else {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} not found on this node",
                tap.bridge
            )));
        };
        if foreign_bridges(&out, self.tag.as_deref())?.contains(&tap.bridge) {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} is not managed by this Orbivisor cluster",
                tap.bridge
            )));
        }
        if tap.vlan.is_some() && !bridge.info.vlan_filtering {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} is not VLAN-aware (vlan_filtering 1)",
                tap.bridge
            )));
        }
        // 이전 VM 인스턴스가 남긴 같은 이름의 tap
        self.delete_link(&tap.ifname).await?;
        for (program, args) in tap_commands(tap, bridge.default_pvid, bridge.mtu) {
            if let Err(e) = self.run(program, &args).await {
                let _ = self.delete_link(&tap.ifname).await;
                return Err(e);
            }
        }
        Ok(())
    }

    async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError> {
        let out = self.run("ip", &["-j", "link", "show"]).await?;
        let links: Vec<Link> = serde_json::from_str(&out).map_err(bad_output)?;
        let prefix = tap_prefix(vm);
        // 하나가 실패해도 나머지 tap은 지운다.
        let mut result = Ok(());
        for link in links.iter().filter(|l| l.ifname.starts_with(&prefix)) {
            if let Err(e) = self.delete_link(&link.ifname).await {
                result = result.and(Err(e));
            }
        }
        result
    }

    async fn sync_overlays(
        &self,
        tag: &str,
        desired: &[Overlay],
    ) -> Result<OverlayChanges, DriverError> {
        let alias = overlay_alias(tag);
        // 목록 명령에는 `-d`를 쓰지 않는다(parse_overlay_links).
        let all = self.run("ip", &["-j", "link", "show"]).await?;
        let bridges = self
            .run("ip", &["-j", "link", "show", "type", "bridge"])
            .await?;
        let mut links = parse_overlay_links(&all, &bridges)?;
        // VXLAN 세부는 장치마다 따로 읽는다. 읽지 못한 장치의 네트워크만 이번 차례에 건너뛴다.
        let mut unreadable = BTreeMap::new();
        for link in links.iter_mut().filter(|l| l.kind.is_none()) {
            let details = self
                .run("ip", &["-d", "-o", "link", "show", "dev", &link.ifname])
                .await;
            match details {
                Ok(text) => add_vxlan_details(link, &text),
                Err(e) => {
                    unreadable.insert(link.ifname.clone(), e.to_string());
                }
            }
        }
        let mut changes = OverlayChanges::default();
        let mut errors = Vec::new();
        for overlay in desired {
            if let Some(e) = unreadable_error(overlay, &unreadable) {
                errors.push(format!("VNI {}: {e}", overlay.vni));
                continue;
            }
            match self.sync_overlay(&alias, overlay, &links).await {
                Ok(created) => changes.devices_changed |= created,
                Err(e) => errors.push(format!("VNI {}: {e}", overlay.vni)),
            }
        }
        let (stale, busy) = stale_devices(&alias, &links, desired);
        errors.extend(busy);
        for name in stale {
            match self.delete_link(&name).await {
                Ok(()) => changes.devices_changed = true,
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        if errors.is_empty() {
            Ok(changes)
        } else {
            Err(DriverError::Overlay(errors.join("; ")))
        }
    }
}

/// 실행할 명령 하나(프로그램, 인자)
type Command = (&'static str, Vec<String>);

/// 이름공간이 있으면 인자 앞에 `-n NAME`을 붙인다(`ip`와 `bridge` 모두 받는다).
pub(crate) fn netns_args(netns: Option<&str>, a: &[impl AsRef<str>]) -> Vec<String> {
    let mut out = Vec::with_capacity(a.len() + 2);
    if let Some(ns) = netns {
        out.extend(["-n".to_string(), ns.to_string()]);
    }
    out.extend(a.iter().map(|s| s.as_ref().to_string()));
    out
}

/// 내 장치에 붙이는 alias(`opl:<클러스터 태그>`)
fn overlay_alias(tag: &str) -> String {
    format!("opl:{tag}")
}

/// 브리지 목록(`ip -j -d link show type bridge`의 출력) 가운데 관리 이름이면서 alias가 내 것
/// (`opl:<tag>`)이 아닌 브리지. 다른 클러스터의 것이나 만들다 멈춘 것이라 VM tap을 붙이지 않는다.
/// 태그가 없으면 관리 이름 브리지가 모두다.
fn foreign_bridges(json: &str, tag: Option<&str>) -> Result<BTreeSet<String>, DriverError> {
    let links: Vec<RawLink> = serde_json::from_str(json).map_err(bad_output)?;
    let mine = tag.map(overlay_alias);
    Ok(links
        .into_iter()
        .filter(|l| is_managed_ifname(&l.ifname) && (mine.is_none() || l.ifalias != mine))
        .map(|l| l.ifname)
        .collect())
}

/// `ip link show`에서 읽은 관리 이름 장치
#[derive(Debug, Clone, PartialEq, Eq)]
struct OverlayLink {
    ifname: String,
    kind: Option<String>,
    alias: Option<String>,
    mtu: u32,
    master: Option<String>,
    up: bool,
    /// VXLAN 장치의 (id, local, dstport)
    vxlan: Option<VxlanAttrs>,
    /// 이 장치를 master로 하는 장치(브리지의 포트)
    ports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VxlanAttrs {
    id: Option<u32>,
    local: Option<IpAddr>,
    port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct RawLink {
    ifname: String,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    mtu: u32,
    #[serde(default)]
    master: Option<String>,
    #[serde(default)]
    ifalias: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinkName {
    ifname: String,
}

/// 관리 이름(`oplv…`·`oplx…`) 장치를 읽는다. `links`는 `ip -j link show`, `bridges`는
/// `ip -j link show type bridge`의 출력이다. Ubuntu iproute2(fan 패치)는 VXLAN 장치의 `-j -d` JSON을
/// 깨뜨리고(`"id":7fan-map ,`), 호스트 이름공간에서는 `ip -d link show`(전체나 `type vxlan`) 목록이
/// 죽기도 한다(세그폴트). 그래서 목록에는 `-d`를 쓰지 않고, 브리지가 아닌 장치의 VXLAN 속성은 장치마다
/// `ip -d -o link show dev NAME`으로 읽어 `add_vxlan_details`로 더한다. 다른 장치는 모양을 따지지 않는다.
fn parse_overlay_links(links: &str, bridges: &str) -> Result<Vec<OverlayLink>, DriverError> {
    let all: Vec<serde_json::Value> = serde_json::from_str(links).map_err(bad_output)?;
    let mut ports: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for value in &all {
        if let (Some(master), Some(ifname)) = (value["master"].as_str(), value["ifname"].as_str()) {
            ports.entry(master).or_default().push(ifname.to_string());
        }
    }
    let bridges: BTreeSet<String> = if bridges.trim().is_empty() {
        BTreeSet::new()
    } else {
        let names: Vec<LinkName> = serde_json::from_str(bridges).map_err(bad_output)?;
        names.into_iter().map(|l| l.ifname).collect()
    };
    let mut out = Vec::new();
    for value in &all {
        if !value["ifname"].as_str().is_some_and(is_managed_ifname) {
            continue;
        }
        let raw = RawLink::deserialize(value).map_err(bad_output)?;
        out.push(OverlayLink {
            up: raw.flags.iter().any(|f| f == "UP"),
            ports: ports.remove(raw.ifname.as_str()).unwrap_or_default(),
            kind: bridges.contains(&raw.ifname).then(|| "bridge".to_string()),
            ifname: raw.ifname,
            alias: raw.ifalias,
            mtu: raw.mtu,
            master: raw.master,
            vxlan: None,
        });
    }
    Ok(out)
}

/// 장치 하나의 `ip -d -o link show dev NAME` 글이 VXLAN 장치의 것이면 종류와 속성을 더한다.
fn add_vxlan_details(link: &mut OverlayLink, text: &str) {
    if let Some(attrs) = parse_vxlan_details(text) {
        link.kind = Some("vxlan".to_string());
        link.vxlan = Some(attrs);
    }
}

/// 장치 하나의 `ip -d link show dev NAME` 글(`-o`로 한 줄이든 여러 줄이든)에서 `vxlan`으로 시작하는
/// 단락의 `id N`·`local ADDR`·`dstport P`를 읽는다. 그런 단락이 없으면 VXLAN 장치가 아니다(None).
/// 단락 안의 모르는 낱말(`fan-map` 따위)은 건너뛴다.
fn parse_vxlan_details(text: &str) -> Option<VxlanAttrs> {
    // `-o`는 줄 경계를 `\`로 남긴다.
    let section: Vec<&str> = text
        .lines()
        .flat_map(|line| line.split('\\'))
        .map(|part| part.split_whitespace().collect::<Vec<_>>())
        .find(|tokens| tokens.first() == Some(&"vxlan"))?;
    let value = |key: &str| section.windows(2).find(|w| w[0] == key).map(|w| w[1]);
    Some(VxlanAttrs {
        id: value("id").and_then(|v| v.parse().ok()),
        local: value("local").and_then(|v| v.parse().ok()),
        port: value("dstport").and_then(|v| v.parse().ok()),
    })
}

/// 이 네트워크의 장치 가운데 세부를 읽지 못한 것이 있으면 그 오류. 그 네트워크는 이번 차례에 건드리지
/// 않는다(종류를 몰라 맞는 장치를 지우고 다시 만들지 않게).
fn unreadable_error(overlay: &Overlay, unreadable: &BTreeMap<String, String>) -> Option<String> {
    [vxlan_bridge(overlay.vni), vxlan_device(overlay.vni)]
        .into_iter()
        .find_map(|name| {
            let e = unreadable.get(&name)?;
            Some(format!("cannot read details of {name}: {e}"))
        })
}

/// VXLAN 네트워크 하나의 장치를 맞추는 명령
#[derive(Debug, PartialEq, Eq)]
struct DevicePlan {
    cmds: Vec<Command>,
    /// 새로 만드는(다시 만드는 것 포함) 장치
    created: Vec<String>,
}

/// 브리지 `oplv<vni>`와 VXLAN 장치 `oplx<vni>`를 바라는 모양으로 만드는 명령. 이름이 같은 장치의
/// alias가 내 것이 아니면 건드리지 않고 오류다. 다만 alias도 포트도 없고 내려가 있는 장치는 만들다 멈춘
/// 내 장치로 보고 다시 만든다(alias와 up을 한 명령으로 주므로 만들다 멈춘 장치는 늘 내려가 있다).
/// VXLAN 장치의 id·local·dstport·주인이 다르면 지우고 다시 만들고, MTU만 다르면 바꾼다.
fn device_plan(
    alias: &str,
    overlay: &Overlay,
    links: &[OverlayLink],
) -> Result<DevicePlan, DriverError> {
    let br = vxlan_bridge(overlay.vni);
    let vx = vxlan_device(overlay.vni);
    let find = |name: &str| links.iter().find(|l| l.ifname == name);
    let (br_link, vx_link) = (find(&br), find(&vx));
    let mine = |l: &OverlayLink| l.alias.as_deref() == Some(alias);
    let half_made = |l: &OverlayLink| {
        !l.up && l.alias.as_deref().is_none_or(str::is_empty) && l.ports.is_empty()
    };
    for link in [br_link, vx_link].into_iter().flatten() {
        if !mine(link) && !half_made(link) {
            return Err(DriverError::InvalidSpec(format!(
                "device {:?} exists but is not managed by this Orbivisor cluster",
                link.ifname
            )));
        }
    }
    let br_ok = br_link.is_some_and(|l| mine(l) && l.kind.as_deref() == Some("bridge"));
    let vx_ok = br_ok
        && vx_link.is_some_and(|l| {
            mine(l)
                && l.kind.as_deref() == Some("vxlan")
                && l.master.as_deref() == Some(br.as_str())
                && l.vxlan
                    == Some(VxlanAttrs {
                        id: Some(overlay.vni),
                        local: Some(overlay.local),
                        port: Some(VXLAN_PORT),
                    })
        });
    let mtu = overlay.mtu.to_string();
    let mut plan = DevicePlan {
        cmds: Vec::new(),
        created: Vec::new(),
    };
    // VXLAN 장치를 먼저 지운다(브리지의 포트다).
    if !vx_ok && vx_link.is_some() {
        plan.cmds.push(("ip", args(&["link", "del", "dev", &vx])));
    }
    if !br_ok && br_link.is_some() {
        plan.cmds.push(("ip", args(&["link", "del", "dev", &br])));
    }
    match br_link {
        Some(link) if br_ok => adjust(&mut plan.cmds, link, overlay.mtu),
        _ => {
            plan.cmds
                .push(("ip", args(&["link", "add", "name", &br, "type", "bridge"])));
            plan.cmds.push((
                "ip",
                args(&["link", "set", "dev", &br, "mtu", &mtu, "alias", alias, "up"]),
            ));
            plan.created.push(br.clone());
        }
    }
    match vx_link {
        Some(link) if vx_ok => adjust(&mut plan.cmds, link, overlay.mtu),
        _ => {
            let (vni, local, port) = (
                overlay.vni.to_string(),
                overlay.local.to_string(),
                VXLAN_PORT.to_string(),
            );
            plan.cmds.push((
                "ip",
                args(&[
                    "link", "add", "name", &vx, "type", "vxlan", "id", &vni, "local", &local,
                    "dstport", &port,
                ]),
            ));
            plan.cmds.push((
                "ip",
                args(&[
                    "link", "set", "dev", &vx, "mtu", &mtu, "master", &br, "alias", alias, "up",
                ]),
            ));
            plan.created.push(vx);
        }
    }
    Ok(plan)
}

/// 실행한 명령 `done` 가운데 `ip link add`로 만든 장치. 실패했을 때 지울 것이므로 나중에 만든 것이
/// 먼저다.
fn rollback_links(done: &[Command]) -> Vec<String> {
    done.iter()
        .rev()
        .filter_map(|(program, a)| match a.as_slice() {
            [link, add, name, ifname, ..]
                if *program == "ip" && link == "link" && add == "add" && name == "name" =>
            {
                Some(ifname.clone())
            }
            _ => None,
        })
        .collect()
}

/// 이미 맞는 장치의 MTU와 up만 고친다.
fn adjust(cmds: &mut Vec<Command>, link: &OverlayLink, mtu: u16) {
    if link.mtu != u32::from(mtu) {
        let mtu = mtu.to_string();
        cmds.push((
            "ip",
            args(&["link", "set", "dev", &link.ifname, "mtu", &mtu]),
        ));
    }
    if !link.up {
        cmds.push(("ip", args(&["link", "set", "dev", &link.ifname, "up"])));
    }
}

/// FDB에 넣을 피어(나와 주소 체계가 같고 내가 아닌 것)와 주소 체계가 달라 뺀 피어
fn usable_peers(overlay: &Overlay) -> (BTreeSet<IpAddr>, Vec<IpAddr>) {
    let (same, other): (Vec<IpAddr>, Vec<IpAddr>) = overlay
        .peers
        .iter()
        .partition(|p| p.is_ipv4() == overlay.local.is_ipv4());
    let peers = same.into_iter().filter(|p| *p != overlay.local).collect();
    (peers, other)
}

const ZERO_MAC: &str = "00:00:00:00:00:00";

#[derive(Debug, Deserialize)]
struct FdbEntry {
    #[serde(default)]
    mac: String,
    #[serde(default)]
    dst: Option<String>,
}

/// `bridge -j fdb show dev oplx…`에서 전부 0 MAC 항목의 `dst`
fn parse_fdb_dsts(json: &str) -> Result<BTreeSet<IpAddr>, DriverError> {
    if json.trim().is_empty() {
        return Ok(BTreeSet::new());
    }
    let entries: Vec<FdbEntry> =
        serde_json::from_str(json).map_err(|e| DriverError::ToolFailed {
            tool: "bridge",
            stderr: format!("unexpected JSON output: {e}"),
        })?;
    Ok(entries
        .into_iter()
        .filter(|e| e.mac == ZERO_MAC)
        .filter_map(|e| e.dst?.parse().ok())
        .collect())
}

/// 전부 0 FDB 항목을 `desired`에 맞추는 명령: 없는 피어는 더하고 남는 것은 지운다.
fn fdb_commands(dev: &str, current: &BTreeSet<IpAddr>, desired: &BTreeSet<IpAddr>) -> Vec<Command> {
    let add = desired.difference(current).map(|p| ("append", p));
    let del = current.difference(desired).map(|p| ("del", p));
    add.chain(del)
        .map(|(op, p)| {
            let dst = p.to_string();
            (
                "bridge",
                args(&["fdb", op, ZERO_MAC, "dev", dev, "dst", &dst]),
            )
        })
        .collect()
}

/// 바라는 것에 없는 내 장치(VXLAN 장치가 먼저, 브리지가 다음)와 지우지 못하는 까닭. 자기 VXLAN 장치
/// 말고도 포트(VM tap 따위)가 남은 브리지는 그 VXLAN 장치와 함께 남기고 오류로 알린다.
fn stale_devices(
    alias: &str,
    links: &[OverlayLink],
    desired: &[Overlay],
) -> (Vec<String>, Vec<String>) {
    let keep: BTreeSet<String> = desired
        .iter()
        .flat_map(|o| [vxlan_bridge(o.vni), vxlan_device(o.vni)])
        .collect();
    let stale_links: Vec<&OverlayLink> = links
        .iter()
        .filter(|l| l.alias.as_deref() == Some(alias) && !keep.contains(&l.ifname))
        .collect();
    let mut stale: Vec<String> = stale_links.iter().map(|l| l.ifname.clone()).collect();
    let mut errors = Vec::new();
    for bridge in stale_links {
        let Some(vni) = bridge.ifname.strip_prefix("oplv") else {
            continue;
        };
        let own = format!("oplx{vni}");
        let ports: Vec<&str> = bridge
            .ports
            .iter()
            .filter(|p| **p != own)
            .map(String::as_str)
            .collect();
        if !ports.is_empty() {
            errors.push(format!(
                "bridge {} still has ports: {}",
                bridge.ifname,
                ports.join(", ")
            ));
            stale.retain(|n| *n != bridge.ifname && *n != own);
        }
    }
    // "oplx…"가 "oplv…"보다 앞에 오도록
    stale.sort_by(|a, b| {
        b.starts_with("oplx")
            .cmp(&a.starts_with("oplx"))
            .then(a.cmp(b))
    });
    (stale, errors)
}

/// tap을 만들고 브리지에 붙여 올리는 명령. 브리지 MTU를 알면 붙이기 전에 tap도 같게 맞춘다
/// (VXLAN 브리지의 MTU가 tap 기본값 1500으로 바뀌지 않게). VLAN이면 브리지 기본 VLAN 대신
/// 그 VLAN만 태그 없이 주고받게 한다.
fn tap_commands(tap: &Tap, default_pvid: u16, mtu: Option<u32>) -> Vec<Command> {
    let dev = tap.ifname.as_str();
    let mut cmds = vec![("ip", args(&["tuntap", "add", "dev", dev, "mode", "tap"]))];
    if let Some(mtu) = mtu {
        let mtu = mtu.to_string();
        cmds.push(("ip", args(&["link", "set", "dev", dev, "mtu", &mtu])));
    }
    cmds.push((
        "ip",
        args(&["link", "set", "dev", dev, "master", &tap.bridge]),
    ));
    if let Some(vlan) = tap.vlan {
        if default_pvid != 0 && default_pvid != vlan {
            let pvid = default_pvid.to_string();
            cmds.push(("bridge", args(&["vlan", "del", "dev", dev, "vid", &pvid])));
        }
        let vid = vlan.to_string();
        cmds.push((
            "bridge",
            args(&["vlan", "add", "dev", dev, "vid", &vid, "pvid", "untagged"]),
        ));
    }
    cmds.push(("ip", args(&["link", "set", "dev", dev, "up"])));
    cmds
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

pub(crate) async fn run(
    program: &'static str,
    args: &[impl AsRef<str>],
) -> Result<String, DriverError> {
    let output = tokio::process::Command::new(program)
        .args(args.iter().map(AsRef::as_ref))
        .output()
        .await?;
    if !output.status.success() {
        return Err(DriverError::ToolFailed {
            tool: program,
            stderr: failure_text(&output.stderr, output.status),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// 실패한 명령의 stderr. 비어 있으면(세그폴트 따위) 종료 상태를 쓴다.
pub(crate) fn failure_text(stderr: &[u8], status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    let text = String::from_utf8_lossy(stderr).trim().to_string();
    if !text.is_empty() {
        return text;
    }
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit status {code}"),
        (None, Some(signal)) => format!("killed by signal {signal}"),
        (None, None) => status.to_string(),
    }
}

#[derive(Debug, Deserialize)]
struct Link {
    ifname: String,
    #[serde(default)]
    mtu: Option<u32>,
    #[serde(default)]
    linkinfo: Option<LinkInfo>,
}

#[derive(Debug, Deserialize)]
struct LinkInfo {
    #[serde(default)]
    info_data: Option<BridgeData>,
}

#[derive(Debug, Deserialize)]
struct BridgeData {
    #[serde(default)]
    vlan_filtering: u8,
    /// 없으면 커널 기본값 1로 본다(0으로 보면 tap에 VLAN 1이 남는다).
    #[serde(default)]
    vlan_default_pvid: Option<u16>,
}

#[derive(Debug, PartialEq, Eq)]
struct Bridge {
    info: BridgeInfo,
    default_pvid: u16,
    mtu: Option<u32>,
}

/// `ip -j -d link show type bridge`의 출력
fn parse_bridges(json: &str) -> Result<Vec<Bridge>, DriverError> {
    let links: Vec<Link> = serde_json::from_str(json).map_err(bad_output)?;
    Ok(links
        .into_iter()
        .map(|l| {
            let data = l.linkinfo.and_then(|i| i.info_data);
            Bridge {
                info: BridgeInfo {
                    name: l.ifname,
                    vlan_filtering: data.as_ref().is_some_and(|d| d.vlan_filtering == 1),
                },
                default_pvid: data.and_then(|d| d.vlan_default_pvid).unwrap_or(1),
                mtu: l.mtu,
            }
        })
        .collect())
}

fn bad_output(e: serde_json::Error) -> DriverError {
    DriverError::ToolFailed {
        tool: "ip",
        stderr: format!("unexpected JSON output: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(vlan: Option<u16>) -> Tap {
        Tap {
            ifname: "op0123456789n0".into(),
            bridge: "vmbr0".into(),
            vlan,
        }
    }

    fn joined(cmds: Vec<(&'static str, Vec<String>)>) -> Vec<String> {
        cmds.into_iter()
            .map(|(p, a)| format!("{p} {}", a.join(" ")))
            .collect()
    }

    #[test]
    fn plain_tap_joins_the_bridge() {
        assert_eq!(
            joined(tap_commands(&tap(None), 1, None)),
            [
                "ip tuntap add dev op0123456789n0 mode tap",
                "ip link set dev op0123456789n0 master vmbr0",
                "ip link set dev op0123456789n0 up",
            ]
        );
    }

    #[test]
    fn vlan_tap_replaces_the_default_pvid() {
        assert_eq!(
            joined(tap_commands(&tap(Some(10)), 1, None)),
            [
                "ip tuntap add dev op0123456789n0 mode tap",
                "ip link set dev op0123456789n0 master vmbr0",
                "bridge vlan del dev op0123456789n0 vid 1",
                "bridge vlan add dev op0123456789n0 vid 10 pvid untagged",
                "ip link set dev op0123456789n0 up",
            ]
        );
        // 브리지에 기본 VLAN이 없거나(0) 같은 VLAN이면 지울 것이 없다.
        for pvid in [0, 10] {
            let cmds = joined(tap_commands(&tap(Some(10)), pvid, None));
            assert!(!cmds.iter().any(|c| c.contains("vlan del")), "{cmds:?}");
        }
    }

    #[test]
    fn tap_takes_the_bridge_mtu_before_joining() {
        assert_eq!(
            joined(tap_commands(&tap(None), 1, Some(1450))),
            [
                "ip tuntap add dev op0123456789n0 mode tap",
                "ip link set dev op0123456789n0 mtu 1450",
                "ip link set dev op0123456789n0 master vmbr0",
                "ip link set dev op0123456789n0 up",
            ]
        );
    }

    #[test]
    fn parses_bridges() {
        let json = r#"[
            {"ifindex":2,"ifname":"vmbr0","mtu":1500,"linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":1,"vlan_default_pvid":1}}},
            {"ifindex":3,"ifname":"br-plain","mtu":1450,"linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":0,"vlan_default_pvid":1}}},
            {"ifindex":4,"ifname":"br-nopvid","linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":1}}}
        ]"#;
        assert_eq!(
            parse_bridges(json).unwrap(),
            [
                Bridge {
                    info: BridgeInfo {
                        name: "vmbr0".into(),
                        vlan_filtering: true
                    },
                    default_pvid: 1,
                    mtu: Some(1500),
                },
                Bridge {
                    info: BridgeInfo {
                        name: "br-plain".into(),
                        vlan_filtering: false
                    },
                    default_pvid: 1,
                    mtu: Some(1450),
                },
                Bridge {
                    info: BridgeInfo {
                        name: "br-nopvid".into(),
                        vlan_filtering: true
                    },
                    default_pvid: 1,
                    mtu: None,
                },
            ]
        );
        assert!(parse_bridges("[]").unwrap().is_empty());
        assert!(parse_bridges("not json").is_err());
    }

    #[test]
    fn netns_prefixes_every_command() {
        assert_eq!(
            netns_args(Some("ns1"), &["-j", "link", "show"]),
            ["-n", "ns1", "-j", "link", "show"]
        );
        assert_eq!(netns_args(None, &["link", "show"]), ["link", "show"]);
    }

    const ALIAS: &str = "opl:0123456789ab";

    fn overlay(peers: &[&str]) -> Overlay {
        Overlay {
            vni: 100,
            mtu: 1450,
            local: "10.0.0.1".parse().unwrap(),
            peers: peers.iter().map(|p| p.parse().unwrap()).collect(),
        }
    }

    /// 시험용 장치 하나: `ip -j link show`의 항목(iproute2 6.x 모양), 브리지인지, VXLAN이면
    /// `ip -d -o link show dev NAME`의 줄
    struct Dev {
        json: String,
        bridge: bool,
        vxlan: Option<String>,
    }

    fn plain_dev(json: &str) -> Dev {
        Dev {
            json: json.into(),
            bridge: false,
            vxlan: None,
        }
    }

    fn bridge_dev(name: &str, alias: &str, mtu: u32) -> Dev {
        Dev {
            json: format!(
                r#"{{"ifindex":7,"ifname":"{name}","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":{mtu},"qdisc":"noqueue","operstate":"UP","linkmode":"DEFAULT","group":"default","txqlen":1000,"link_type":"ether","address":"5a:1e:2b:3c:4d:5e","broadcast":"ff:ff:ff:ff:ff:ff","ifalias":"{alias}"}}"#
            ),
            bridge: true,
            vxlan: None,
        }
    }

    /// Ubuntu iproute2 6.1(fan 패치)처럼 `fan-map`이 낀 줄
    fn vxlan_dev(name: &str, alias: &str, master: &str, id: u32, local: &str, mtu: u32) -> Dev {
        Dev {
            json: format!(
                r#"{{"ifindex":8,"ifname":"{name}","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":{mtu},"qdisc":"noqueue","master":"{master}","operstate":"UNKNOWN","linkmode":"DEFAULT","group":"default","txqlen":1000,"link_type":"ether","address":"8e:01:02:03:04:05","broadcast":"ff:ff:ff:ff:ff:ff","ifalias":"{alias}"}}"#
            ),
            bridge: false,
            vxlan: Some(format!(
                r"8: {name}: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu {mtu} qdisc noqueue master {master} state UNKNOWN mode DEFAULT group default qlen 1000\    link/ether 8e:01:02:03:04:05 brd ff:ff:ff:ff:ff:ff promiscuity 1  allmulti 1 minmtu 68 maxmtu 65535 \    vxlan id {id} fan-map local {local} srcport 0 0 dstport 4789 ttl auto ageing 300 udpcsum noudp6zerocsumtx noudp6zerocsumrx \    bridge_slave state forwarding priority 32 cost 100 \    alias {alias}"
            )),
        }
    }

    fn links(devs: &[Dev]) -> Vec<OverlayLink> {
        let json = |ds: Vec<&Dev>| {
            let items: Vec<&str> = ds.iter().map(|d| d.json.as_str()).collect();
            format!("[{}]", items.join(","))
        };
        let mut found = parse_overlay_links(
            &json(devs.iter().collect()),
            &json(devs.iter().filter(|d| d.bridge).collect()),
        )
        .unwrap();
        for (link, dev) in found.iter_mut().zip(devs) {
            if let Some(text) = &dev.vxlan {
                add_vxlan_details(link, text);
            }
        }
        found
    }

    // Ubuntu 24.04 CI의 실제 출력(`info: Using default fan map value (33)`은 stderr로 간다)
    const TXT_O: &str = r"3: oplx7: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1450 qdisc noqueue master oplv7 state UNKNOWN mode DEFAULT group default qlen 1000\    link/ether 12:4b:51:3a:e4:22 brd ff:ff:ff:ff:ff:ff promiscuity 1  allmulti 1 minmtu 68 maxmtu 65535 \    vxlan id 7 fan-map local 10.1.1.1 srcport 0 0 dstport 4789 ttl auto ageing 300 udpcsum noudp6zerocsumtx noudp6zerocsumrx \    bridge_slave state forwarding priority 32 cost 100 hairpin off guard off root_block off fastleave off learning on flood on port_id 0x8001 port_no 0x1 designated_port 32769 designated_cost 0 designated_bridge 8000.12:4b:51:3a:e4:22 designated_root 8000.12:4b:51:3a:e4:22 hold_timer    0.00 message_age_timer    0.00 forward_delay_timer   14.94 topology_change_ack 0 config_pending 0 proxy_arp off proxy_arp_wifi off mcast_router 1 mcast_fast_leave off mcast_flood on bcast_flood on mcast_to_unicast off neigh_suppress off group_fwd_mask 0 group_fwd_mask_str 0x0 vlan_tunnel off isolated off locked off addrgenmode eui64 numtxqueues 1 numrxqueues 1 gso_max_size 65536 gso_max_segs 65535 tso_max_size 65536 tso_max_segs 65535 gro_max_size 65536 \    alias opl:abc
";
    const TXT_V6: &str = "4: oplx8: <BROADCAST,MULTICAST> mtu 1500 qdisc noop state DOWN mode DEFAULT group default qlen 1000
    link/ether ae:d5:1f:26:19:47 brd ff:ff:ff:ff:ff:ff promiscuity 0  allmulti 0 minmtu 68 maxmtu 65535 
    vxlan id 8 fan-map local fd00::1 srcport 0 0 dstport 4789 ttl auto ageing 300 udpcsum noudp6zerocsumtx noudp6zerocsumrx addrgenmode eui64 numtxqueues 1 numrxqueues 1 gso_max_size 65536 gso_max_segs 65535 tso_max_size 65536 tso_max_segs 65535 gro_max_size 65536 
";
    const TXT_BRIDGE: &str = r"2: oplv7: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1450 qdisc noqueue state UP mode DEFAULT group default qlen 1000\    link/ether 12:4b:51:3a:e4:22 brd ff:ff:ff:ff:ff:ff promiscuity 0  allmulti 0 minmtu 68 maxmtu 65535 \    bridge forward_delay 1500 hello_time 200 max_age 2000 ageing_time 30000 stp_state 0 priority 32768 vlan_filtering 0 vlan_protocol 802.1Q bridge_id 8000.12:4b:51:3a:e4:22 addrgenmode eui64 numtxqueues 1 numrxqueues 1 \    alias opl:abc
";
    const TXT_TAP: &str = r"9: oplx9: <BROADCAST,MULTICAST> mtu 1500 qdisc noop state DOWN mode DEFAULT group default qlen 1000\    link/ether 6a:0b:1c:2d:3e:4f brd ff:ff:ff:ff:ff:ff promiscuity 0  allmulti 0 minmtu 68 maxmtu 65521 \    tun type tap pi off vnet_hdr off persist on addrgenmode eui64 numtxqueues 1 numrxqueues 1
";
    const JSON_V: &str = r#"[{"ifindex":3,"ifname":"oplx7","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":1450,"qdisc":"noqueue","master":"oplv7","operstate":"UNKNOWN","linkmode":"DEFAULT","group":"default","txqlen":1000,"link_type":"ether","address":"12:4b:51:3a:e4:22","broadcast":"ff:ff:ff:ff:ff:ff","ifalias":"opl:abc"}]"#;
    const JSON_B: &str = r#"[{"ifindex":2,"ifname":"oplv7","flags":["BROADCAST","MULTICAST","UP","LOWER_UP"],"mtu":1450,"qdisc":"noqueue","operstate":"UP","linkmode":"DEFAULT","group":"default","txqlen":1000,"link_type":"ether","address":"12:4b:51:3a:e4:22","broadcast":"ff:ff:ff:ff:ff:ff","ifalias":"opl:abc"}]"#;

    fn attrs(id: u32, local: &str, port: u16) -> VxlanAttrs {
        VxlanAttrs {
            id: Some(id),
            local: Some(local.parse().unwrap()),
            port: Some(port),
        }
    }

    #[test]
    fn parses_vxlan_details_with_and_without_fan_map() {
        // `-o` 한 줄, 여러 줄(IPv6)
        assert_eq!(parse_vxlan_details(TXT_O), Some(attrs(7, "10.1.1.1", 4789)));
        assert_eq!(parse_vxlan_details(TXT_V6), Some(attrs(8, "fd00::1", 4789)));
        // 호스트 이름공간에서는 `fan-map`이 없기도 하다. 이름 뒤에 "@dev"가 붙어도 된다.
        let plain = TXT_O
            .replace(" fan-map", "")
            .replace("3: oplx7:", "3: oplx7@eth0:");
        assert_eq!(
            parse_vxlan_details(&plain),
            Some(attrs(7, "10.1.1.1", 4789))
        );
        // stdout에 fan 안내가 섞여도 된다.
        let info = format!("info: Using default fan map value (33)\n{TXT_O}");
        assert_eq!(parse_vxlan_details(&info), Some(attrs(7, "10.1.1.1", 4789)));
        // local이 없으면 None
        let no_local = TXT_O.replace("local 10.1.1.1 ", "");
        assert_eq!(parse_vxlan_details(&no_local).unwrap().local, None);
        // `vxlan` 단락이 없으면 VXLAN 장치가 아니다(브리지, tap, 빈 출력).
        assert_eq!(parse_vxlan_details(TXT_BRIDGE), None);
        assert_eq!(parse_vxlan_details(TXT_TAP), None);
        assert_eq!(parse_vxlan_details(""), None);
    }

    #[test]
    fn parses_managed_links_only() {
        // CI에서 실제로 받은 출력. 목록만으로는 브리지만 종류를 안다.
        let mut found = parse_overlay_links(
            &format!(
                "[{},{}]",
                &JSON_B[1..JSON_B.len() - 1],
                &JSON_V[1..JSON_V.len() - 1]
            ),
            JSON_B,
        )
        .unwrap();
        let bridge = OverlayLink {
            ifname: "oplv7".into(),
            kind: Some("bridge".into()),
            alias: Some("opl:abc".into()),
            mtu: 1450,
            master: None,
            up: true,
            vxlan: None,
            ports: vec!["oplx7".into()],
        };
        let mut device = OverlayLink {
            ifname: "oplx7".into(),
            kind: None,
            alias: Some("opl:abc".into()),
            mtu: 1450,
            master: Some("oplv7".into()),
            up: true,
            vxlan: None,
            ports: vec![],
        };
        assert_eq!(found, [bridge.clone(), device.clone()]);
        // 장치 하나의 `ip -d -o link show dev oplx7`로 VXLAN인 것을 안다.
        add_vxlan_details(&mut found[1], TXT_O);
        device.kind = Some("vxlan".into());
        device.vxlan = Some(attrs(7, "10.1.1.1", 4789));
        assert_eq!(found, [bridge, device]);
        // VXLAN 단락이 없는 글이면 종류를 모르는 채로 둔다(tap 따위, 다시 만든다).
        let mut tap = found[1].clone();
        tap.kind = None;
        tap.vxlan = None;
        add_vxlan_details(&mut tap, TXT_TAP);
        assert_eq!((tap.kind, tap.vxlan), (None, None));
        // 관리 이름이 아닌 장치는 모양을 따지지 않는다.
        let all = format!(
            r#"[{{"ifindex":1,"ifname":"lo","flags":["LOOPBACK","UP","LOWER_UP"],"mtu":65536}},
                {{"ifindex":2,"ifname":"eth0","flags":"weird"}},
                {{"ifindex":3,"ifname":"oplv","mtu":1500}},
                {}]"#,
            vxlan_dev("oplx200", "opl:other", "oplv200", 200, "fd00::1", 9000).json
        );
        let found = parse_overlay_links(&all, "").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].kind, None);
        assert_eq!(found[0].mtu, 9000);
        // alias·flags가 없는 장치(내려가 있음)
        let bare = links(&[plain_dev(
            r#"{"ifname":"oplv5","mtu":1500,"flags":["BROADCAST"]}"#,
        )]);
        assert_eq!(bare[0].alias, None);
        assert_eq!(bare[0].kind, None);
        assert!(!bare[0].up);
        assert!(parse_overlay_links("not json", "[]").is_err());
        assert!(parse_overlay_links("[]", "not json").is_err());
    }

    #[test]
    fn unreadable_details_fail_only_their_network() {
        let unreadable = BTreeMap::from([(
            "oplx100".to_string(),
            "ip failed: killed by signal 11".to_string(),
        )]);
        assert_eq!(
            unreadable_error(&overlay(&[]), &unreadable).as_deref(),
            Some("cannot read details of oplx100: ip failed: killed by signal 11")
        );
        let other = Overlay {
            vni: 200,
            ..overlay(&[])
        };
        assert_eq!(unreadable_error(&other, &unreadable), None);
        assert_eq!(unreadable_error(&overlay(&[]), &BTreeMap::new()), None);
    }

    #[test]
    fn empty_stderr_falls_back_to_the_exit_status() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::ExitStatus;
        assert_eq!(
            failure_text(b"Cannot find device \"x\"\n", ExitStatus::from_raw(1 << 8)),
            "Cannot find device \"x\""
        );
        assert_eq!(
            failure_text(b"", ExitStatus::from_raw(1 << 8)),
            "exit status 1"
        );
        assert_eq!(
            failure_text(b" \n", ExitStatus::from_raw(2 << 8)),
            "exit status 2"
        );
        // 세그폴트(셸에서는 종료 139), core dump 비트가 있어도 같다.
        assert_eq!(
            failure_text(b"", ExitStatus::from_raw(11)),
            "killed by signal 11"
        );
        assert_eq!(
            failure_text(b"", ExitStatus::from_raw(11 | 0x80)),
            "killed by signal 11"
        );
    }

    #[test]
    fn creates_missing_devices() {
        let plan = device_plan(ALIAS, &overlay(&[]), &[]).unwrap();
        assert_eq!(
            joined(plan.cmds),
            [
                "ip link add name oplv100 type bridge",
                "ip link set dev oplv100 mtu 1450 alias opl:0123456789ab up",
                "ip link add name oplx100 type vxlan id 100 local 10.0.0.1 dstport 4789",
                "ip link set dev oplx100 mtu 1450 master oplv100 alias opl:0123456789ab up",
            ]
        );
        assert_eq!(plan.created, ["oplv100", "oplx100"]);
    }

    #[test]
    fn rollback_removes_only_the_links_added_so_far() {
        let cmds = device_plan(ALIAS, &overlay(&[]), &[]).unwrap().cmds;
        assert!(rollback_links(&cmds[..0]).is_empty());
        // 브리지를 만들고 alias를 붙이다 실패
        assert_eq!(rollback_links(&cmds[..1]), ["oplv100"]);
        // VXLAN 장치를 만들다 실패하면 그 장치는 없다.
        assert_eq!(rollback_links(&cmds[..2]), ["oplv100"]);
        assert_eq!(rollback_links(&cmds[..3]), ["oplx100", "oplv100"]);
        // 이미 있던 장치를 지우는 명령만 했으면 지울 것이 없다.
        let existing = links(&[
            bridge_dev("oplv100", ALIAS, 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.9", 1450),
        ]);
        let cmds = device_plan(ALIAS, &overlay(&[]), &existing).unwrap().cmds;
        assert_eq!(joined(cmds[..1].to_vec()), ["ip link del dev oplx100"]);
        assert!(rollback_links(&cmds[..1]).is_empty());
    }

    #[test]
    fn matching_devices_need_nothing() {
        let existing = links(&[
            bridge_dev("oplv100", ALIAS, 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.1", 1450),
        ]);
        let plan = device_plan(ALIAS, &overlay(&[]), &existing).unwrap();
        assert!(plan.cmds.is_empty(), "{:?}", plan.cmds);
        assert!(plan.created.is_empty());
    }

    #[test]
    fn mtu_and_down_devices_are_adjusted_in_place() {
        let mut existing = links(&[
            bridge_dev("oplv100", ALIAS, 1500),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.1", 1500),
        ]);
        existing[1].up = false;
        let plan = device_plan(ALIAS, &overlay(&[]), &existing).unwrap();
        assert_eq!(
            joined(plan.cmds),
            [
                "ip link set dev oplv100 mtu 1450",
                "ip link set dev oplx100 mtu 1450",
                "ip link set dev oplx100 up",
            ]
        );
        assert!(plan.created.is_empty());
    }

    #[test]
    fn mismatched_vxlan_is_recreated() {
        let recreate = [
            "ip link del dev oplx100",
            "ip link add name oplx100 type vxlan id 100 local 10.0.0.1 dstport 4789",
            "ip link set dev oplx100 mtu 1450 master oplv100 alias opl:0123456789ab up",
        ];
        for vxlan in [
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.9", 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 101, "10.0.0.1", 1450),
            vxlan_dev("oplx100", ALIAS, "vmbr0", 100, "10.0.0.1", 1450),
            {
                let mut d = vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.1", 1450);
                d.vxlan = d.vxlan.map(|t| t.replace("dstport 4789", "dstport 8472"));
                d
            },
        ] {
            let existing = links(&[bridge_dev("oplv100", ALIAS, 1450), vxlan]);
            let plan = device_plan(ALIAS, &overlay(&[]), &existing).unwrap();
            assert_eq!(joined(plan.cmds), recreate);
            assert_eq!(plan.created, ["oplx100"]);
        }
        // 브리지가 브리지가 아니면 둘 다 다시 만든다.
        let existing = links(&[
            vxlan_dev("oplv100", ALIAS, "x", 1, "10.0.0.1", 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.1", 1450),
        ]);
        let plan = device_plan(ALIAS, &overlay(&[]), &existing).unwrap();
        assert_eq!(
            joined(plan.cmds)[..2],
            ["ip link del dev oplx100", "ip link del dev oplv100"]
        );
        assert_eq!(plan.created, ["oplv100", "oplx100"]);
    }

    #[test]
    fn only_my_managed_bridges_take_taps() {
        let json = r#"[
            {"ifindex":2,"ifname":"vmbr0","mtu":1500},
            {"ifindex":3,"ifname":"oplv1","mtu":1450,"ifalias":"opl:0123456789ab"},
            {"ifindex":4,"ifname":"oplv2","mtu":1450,"ifalias":"opl:other"},
            {"ifindex":5,"ifname":"oplv3","mtu":1500},
            {"ifindex":6,"ifname":"oplvx","mtu":1500,"ifalias":"admin"}
        ]"#;
        let names = |a: &[&str]| a.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
        // 남의 것과 만들다 멈춘 것(alias 없음)은 뺀다. 관리 이름이 아니면 상관없다.
        assert_eq!(
            foreign_bridges(json, Some("0123456789ab")).unwrap(),
            names(&["oplv2", "oplv3"])
        );
        // 태그가 없으면(위트니스, 클러스터 전) 관리 이름 브리지는 모두 뺀다.
        assert_eq!(
            foreign_bridges(json, None).unwrap(),
            names(&["oplv1", "oplv2", "oplv3"])
        );
        assert!(foreign_bridges("not json", None).is_err());
    }

    #[test]
    fn foreign_devices_are_left_alone() {
        for existing in [
            links(&[bridge_dev("oplv100", "opl:other", 1450)]),
            links(&[vxlan_dev(
                "oplx100", "admin", "oplv100", 100, "10.0.0.1", 1450,
            )]),
            // alias가 없어도 포트가 있으면 누가 쓰는 장치다.
            links(&[
                plain_dev(r#"{"ifname":"oplv100","mtu":1450}"#),
                plain_dev(r#"{"ifname":"eth1","mtu":1500,"master":"oplv100"}"#),
            ]),
            // alias도 포트도 없어도 올라가 있으면 남의 것이다(opld는 alias와 up을 한 명령으로 준다).
            links(&[plain_dev(
                r#"{"ifname":"oplv100","mtu":1450,"flags":["BROADCAST","MULTICAST","UP"]}"#,
            )]),
        ] {
            let err = device_plan(ALIAS, &overlay(&[]), &existing).unwrap_err();
            assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
        }
    }

    #[test]
    fn half_made_devices_are_recreated() {
        // 만들고 alias를 붙이기 전에 멈춘 브리지(alias도 포트도 없음)
        let existing = links(&[plain_dev(
            r#"{"ifname":"oplv100","mtu":1500,"flags":["BROADCAST"]}"#,
        )]);
        let plan = device_plan(ALIAS, &overlay(&[]), &existing).unwrap();
        assert_eq!(
            joined(plan.cmds)[..2],
            [
                "ip link del dev oplv100",
                "ip link add name oplv100 type bridge"
            ]
        );
        assert_eq!(plan.created, ["oplv100", "oplx100"]);
        // 브리지는 내 것이고 VXLAN 장치만 alias 없이 내려간 채 남은 경우
        let mut existing = links(&[
            bridge_dev("oplv100", ALIAS, 1450),
            vxlan_dev("oplx100", "", "oplv100", 100, "10.0.0.1", 1450),
        ]);
        existing[1].up = false;
        let plan = device_plan(ALIAS, &overlay(&[]), &existing).unwrap();
        assert_eq!(
            joined(plan.cmds),
            [
                "ip link del dev oplx100",
                "ip link add name oplx100 type vxlan id 100 local 10.0.0.1 dstport 4789",
                "ip link set dev oplx100 mtu 1450 master oplv100 alias opl:0123456789ab up",
            ]
        );
        assert_eq!(plan.created, ["oplx100"]);
    }

    #[test]
    fn ipv6_local_is_compared() {
        let mut o = overlay(&[]);
        o.local = "fd00::1".parse().unwrap();
        let existing = links(&[
            bridge_dev("oplv100", ALIAS, 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "fd00:0::1", 1450),
        ]);
        assert!(device_plan(ALIAS, &o, &existing).unwrap().cmds.is_empty());
        assert_eq!(
            joined(device_plan(ALIAS, &o, &[]).unwrap().cmds)[2],
            "ip link add name oplx100 type vxlan id 100 local fd00::1 dstport 4789"
        );
    }

    #[test]
    fn peers_of_another_family_are_skipped() {
        let (peers, skipped) = usable_peers(&overlay(&[
            "10.0.0.3", "fd00::2", "10.0.0.1", "10.0.0.2", "10.0.0.3",
        ]));
        assert_eq!(
            peers.into_iter().collect::<Vec<_>>(),
            [
                "10.0.0.2".parse::<IpAddr>().unwrap(),
                "10.0.0.3".parse().unwrap()
            ]
        );
        assert_eq!(skipped, ["fd00::2".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn parses_fdb_default_destinations() {
        let json = r#"[
            {"mac":"00:00:00:00:00:00","dst":"10.0.0.2","flags":["self"],"state":"permanent"},
            {"mac":"00:00:00:00:00:00","dst":"fd00::2","flags":["self"],"state":"permanent"},
            {"mac":"52:54:00:12:34:56","dst":"10.0.0.3","flags":["self"],"state":""},
            {"mac":"8e:01:02:03:04:05","master":"oplv100","flags":[],"state":"permanent"},
            {"mac":"00:00:00:00:00:00","flags":["self"],"state":"permanent"}
        ]"#;
        assert_eq!(
            parse_fdb_dsts(json)
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>(),
            [
                "10.0.0.2".parse::<IpAddr>().unwrap(),
                "fd00::2".parse().unwrap()
            ]
        );
        assert!(parse_fdb_dsts("[]").unwrap().is_empty());
        assert!(parse_fdb_dsts("").unwrap().is_empty());
        assert!(parse_fdb_dsts("{").is_err());
    }

    #[test]
    fn fdb_is_brought_to_the_peers() {
        let set = |a: &[&str]| {
            a.iter()
                .map(|p| p.parse().unwrap())
                .collect::<BTreeSet<IpAddr>>()
        };
        assert_eq!(
            joined(fdb_commands(
                "oplx100",
                &set(&["10.0.0.2", "10.0.0.9"]),
                &set(&["10.0.0.2", "10.0.0.3"])
            )),
            [
                "bridge fdb append 00:00:00:00:00:00 dev oplx100 dst 10.0.0.3",
                "bridge fdb del 00:00:00:00:00:00 dev oplx100 dst 10.0.0.9",
            ]
        );
        assert!(fdb_commands("oplx100", &set(&["10.0.0.2"]), &set(&["10.0.0.2"])).is_empty());
    }

    #[test]
    fn only_my_unwanted_devices_are_stale() {
        let existing = links(&[
            bridge_dev("oplv100", ALIAS, 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.1", 1450),
            bridge_dev("oplv200", ALIAS, 1450),
            vxlan_dev("oplx200", ALIAS, "oplv200", 200, "10.0.0.1", 1450),
            bridge_dev("oplv300", "opl:other", 1450),
            vxlan_dev("oplx300", "opl:other", "oplv300", 300, "10.0.0.1", 1450),
        ]);
        assert_eq!(
            stale_devices(ALIAS, &existing, &[overlay(&[])]),
            (vec!["oplx200".to_string(), "oplv200".to_string()], vec![])
        );
        assert_eq!(
            stale_devices(ALIAS, &existing, &[]).0,
            ["oplx100", "oplx200", "oplv100", "oplv200"]
        );
    }

    #[test]
    fn bridges_that_still_have_ports_are_kept() {
        // oplv100에는 VM tap이 남아 있다(자기 oplx100은 포트로 치지 않는다).
        let existing = links(&[
            bridge_dev("oplv100", ALIAS, 1450),
            vxlan_dev("oplx100", ALIAS, "oplv100", 100, "10.0.0.1", 1450),
            plain_dev(r#"{"ifname":"op0123456789n0","mtu":1450,"master":"oplv100"}"#),
            bridge_dev("oplv200", ALIAS, 1450),
            vxlan_dev("oplx200", ALIAS, "oplv200", 200, "10.0.0.1", 1450),
        ]);
        assert_eq!(existing[0].ports, ["oplx100", "op0123456789n0"]);
        assert_eq!(
            stale_devices(ALIAS, &existing, &[]),
            (
                vec!["oplx200".to_string(), "oplv200".to_string()],
                vec!["bridge oplv100 still has ports: op0123456789n0".to_string()]
            )
        );
    }
}
