//! 노드 physnet 연결과 게이트웨이 표시(계획 66 §7). 관리자가 만든 Linux 브리지(`vmbr1` 따위)에 veth 짝 하나로 Orbivisor
//! OVS 브리지를 잇고(`oplp<h8>b` → Linux 브리지, `oplp<h8>o` → OVS 브리지 `oplp<h8>x`), `ovn-bridge-mappings`에
//! `<physnet>:oplp<h8>x`를 적는다. **물리 NIC는 옮기지 않는다** — 관리자 브리지에는 veth 한쪽만 매단다.
//!
//! 주인 판단: OVS 브리지는 이름이 [`is_physnet_ifname`]이고 `x`로 끝나며 `Bridge.external_ids:orbivisor-cluster`가 이
//! 클러스터 태그인 것만 Orbivisor 것이다. 매핑 항목은 다리가 그런 브리지이거나 Orbivisor가 그 physnet에 쓸 이름 그대로인
//! 것만 더하고 뺀다. 남의 항목은 그대로 두고, 같은 physnet이 남의 다리에 매핑돼 있으면 덮지 않고 알린다("아무것도 몰래
//! 지우지 않는다"). `ovn-cms-options`는 `enable-chassis-as-gw` 낱말만 더하고 뺀다(다른 옵션은 둔다).
//!
//! 명령 목록은 순수 함수([`physnet_commands`], [`merge_mappings`], [`merge_cms_options`])로 만들고 [`OvnHost`]가 읽은
//! 상태에 맞춰 돌린다.

use std::collections::{BTreeMap, BTreeSet};

use opl_types::sdn::{NodeSdnConfig, PhysnetState, is_physnet_ifname, physnet_ifnames};
use serde::Deserialize;
use serde_json::Value;

use super::host::{OvnError, OvnHost, TAG_KEY, quote};
use super::status::{atom_map, parse_table};
use crate::ceph::runner::Cmd;

/// `Open_vSwitch.external_ids`의 physnet → OVS 브리지 매핑 칸(ovn-controller가 읽음)
pub const MAPPINGS_KEY: &str = "ovn-bridge-mappings";
/// `Open_vSwitch.external_ids`의 CMS 옵션 칸(OVN은 쓰지 않음 — 사람과 Neutron 관례용)
pub const CMS_OPTIONS_KEY: &str = "ovn-cms-options";
/// 게이트웨이 후보 표시(`ovn-cms-options`의 낱말 하나)
pub const GATEWAY_OPTION: &str = "enable-chassis-as-gw";
/// Orbivisor OVS 브리지의 physnet 이름 칸(`Bridge.external_ids`)
pub const PHYSNET_KEY: &str = "orbivisor-physnet";
/// VLAN 인식 Linux 브리지에서 veth에 여는 VLAN(트렁크 — pvid는 브리지 기본 그대로)
const TRUNK_VIDS: &str = "2-4094";

/// `ip -j link show`에서 읽은 장치 하나
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkFacts {
    pub mtu: Option<u32>,
    pub master: Option<String>,
    pub up: bool,
    /// Linux 브리지다(`ip -j -d link show type bridge`에 있음).
    pub bridge: bool,
    /// Linux 브리지의 `vlan_filtering 1`
    pub vlan_filtering: bool,
}

#[derive(Deserialize)]
struct RawLink {
    ifname: String,
    #[serde(default)]
    mtu: Option<u32>,
    #[serde(default)]
    master: Option<String>,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    linkinfo: Option<RawLinkInfo>,
}

#[derive(Deserialize)]
struct RawLinkInfo {
    #[serde(default)]
    info_data: Option<Value>,
}

/// 장치 목록: `links`는 `ip -j link show`, `bridges`는 `ip -j -d link show type bridge`의 출력이다(이름 → 장치). 목록에
/// `-d`를 쓰지 않는 까닭은 [`crate::net`]과 같다(Ubuntu iproute2가 VXLAN 장치의 `-d` JSON을 깨뜨림).
pub fn parse_links(links: &[u8], bridges: &[u8]) -> Result<BTreeMap<String, LinkFacts>, String> {
    let read = |json: &[u8]| -> Result<Vec<RawLink>, String> {
        if json.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        serde_json::from_slice(json).map_err(|e| format!("cannot read `ip -j link show`: {e}"))
    };
    let mut out: BTreeMap<String, LinkFacts> = read(links)?
        .into_iter()
        .map(|l| {
            let facts = LinkFacts {
                mtu: l.mtu,
                master: l.master,
                up: l.flags.iter().any(|f| f == "UP"),
                ..LinkFacts::default()
            };
            (l.ifname, facts)
        })
        .collect();
    for b in read(bridges)? {
        let vlan_filtering = b
            .linkinfo
            .and_then(|i| i.info_data)
            .and_then(|d| d["vlan_filtering"].as_u64())
            == Some(1);
        let facts = out.entry(b.ifname).or_default();
        facts.bridge = true;
        facts.vlan_filtering = vlan_filtering;
        if facts.mtu.is_none() {
            facts.mtu = b.mtu;
        }
    }
    Ok(out)
}

/// physnet 하나를 잇는 명령(이미 된 것은 건너뜀). `bridge`는 관리자 Linux 브리지(`name`, 읽은 상태), `veth`는 Linux 브리지
/// 쪽 veth(`oplp<h8>b`)의 지금 상태(없으면 None), `ovs_peer`는 OVS 쪽 veth(`oplp<h8>o`)의 지금 상태, `ovs_ready`는 OVS
/// 브리지와 그 포트가 이미 있는지다. veth 둘의 MTU는 브리지 MTU에 맞춘다. 브리지가 VLAN 인식이면 veth를 새로 매달 때
/// 트렁크(`vid 2-4094`)를 연다.
#[allow(clippy::too_many_arguments)]
pub fn physnet_commands(
    tag: &str,
    physnet: &str,
    bridge: &str,
    facts: &LinkFacts,
    veth: Option<&LinkFacts>,
    ovs_peer: Option<&LinkFacts>,
    ovs_ready: bool,
) -> Vec<Cmd> {
    let (o, b, x) = physnet_ifnames(tag, physnet);
    let mut cmds = Vec::new();
    let mtu = facts.mtu.map(|m| m.to_string());
    if veth.is_none() {
        cmds.push(Cmd::new(
            "ip",
            &["link", "add", &b, "type", "veth", "peer", "name", &o],
        ));
    }
    let attach = veth.is_none_or(|v| v.master.as_deref() != Some(bridge));
    if attach || veth.is_some_and(|v| v.mtu != facts.mtu || !v.up) {
        let mut args = vec!["link", "set", "dev", &b, "master", bridge];
        if let Some(m) = &mtu {
            args.extend(["mtu", m]);
        }
        args.push("up");
        cmds.push(Cmd::new("ip", &args));
    }
    if ovs_peer.is_none_or(|p| p.mtu != facts.mtu || !p.up) {
        let mut args = vec!["link", "set", "dev", &o];
        if let Some(m) = &mtu {
            args.extend(["mtu", m]);
        }
        args.push("up");
        cmds.push(Cmd::new("ip", &args));
    }
    if attach && facts.vlan_filtering {
        cmds.push(Cmd::new(
            "bridge",
            &["vlan", "add", "dev", &b, "vid", TRUNK_VIDS],
        ));
    }
    if !ovs_ready {
        let tag_id = format!("external_ids:{TAG_KEY}={tag}");
        cmds.push(Cmd::new(
            "ovs-vsctl",
            &[
                "--may-exist",
                "add-br",
                &x,
                "--",
                "set",
                "Bridge",
                &x,
                &tag_id,
                &format!("external_ids:{PHYSNET_KEY}={physnet}"),
                "--",
                "--may-exist",
                "add-port",
                &x,
                &o,
                "--",
                "set",
                "Interface",
                &o,
                &tag_id,
            ],
        ));
    }
    cmds
}

/// `ovn-bridge-mappings`(`physnet:bridge,…`)의 항목들(차례 그대로, 빈 항목 뺌)
pub fn parse_mappings(text: &str) -> Vec<(String, String)> {
    text.split(',')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| match e.split_once(':') {
            Some((p, b)) => (p.to_string(), b.to_string()),
            None => (e.to_string(), String::new()),
        })
        .collect()
}

/// [`merge_mappings`]의 결과
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedMappings {
    /// 새 칸 값(비면 칸을 지운다)
    pub text: String,
    /// 남의 다리에 매핑돼 있어 쓰지 않은 physnet → 그 다리
    pub conflicts: BTreeMap<String, String>,
}

/// 매핑 합치기: `ours`(이 클러스터의 OVS 브리지 이름)를 다리로 하거나 `want`에 든 그대로인 항목은 Orbivisor 것이라 빼고
/// `want`(physnet → OVS 브리지)를 physnet 순으로 뒤에 붙인다. 남의 항목은 차례 그대로 둔다. `want`의 physnet이 남의
/// 다리에 매핑돼 있으면 덮지 않고 `conflicts`에 둔다.
pub fn merge_mappings(
    current: &str,
    ours: &BTreeSet<String>,
    want: &BTreeMap<String, String>,
) -> MergedMappings {
    let mut kept = Vec::new();
    let mut conflicts = BTreeMap::new();
    for (physnet, bridge) in parse_mappings(current) {
        if ours.contains(&bridge) || want.get(&physnet) == Some(&bridge) {
            continue;
        }
        if want.contains_key(&physnet) {
            conflicts.insert(physnet.clone(), bridge.clone());
        }
        kept.push(if bridge.is_empty() {
            physnet
        } else {
            format!("{physnet}:{bridge}")
        });
    }
    for (physnet, bridge) in want {
        if !conflicts.contains_key(physnet) {
            kept.push(format!("{physnet}:{bridge}"));
        }
    }
    MergedMappings {
        text: kept.join(","),
        conflicts,
    }
}

/// `ovn-cms-options`(쉼표 목록)에서 [`GATEWAY_OPTION`]만 더하거나 뺀다. 다른 옵션은 차례 그대로 둔다.
pub fn merge_cms_options(current: &str, gateway: bool) -> String {
    let mut options: Vec<&str> = current
        .split(',')
        .map(str::trim)
        .filter(|o| !o.is_empty() && *o != GATEWAY_OPTION)
        .collect();
    if gateway {
        options.push(GATEWAY_OPTION);
    }
    options.join(",")
}

/// `ovn-cms-options`에 [`GATEWAY_OPTION`]이 있는지
pub fn has_gateway_option(options: &str) -> bool {
    options.split(',').any(|o| o.trim() == GATEWAY_OPTION)
}

/// [`OvnHost::reconcile_physnets`]의 결과
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhysnetReport {
    /// 원하는 physnet마다(이름 순) 상태
    pub physnets: Vec<PhysnetState>,
    /// `ovn-cms-options`에 [`GATEWAY_OPTION`]이 있다.
    pub gateway: bool,
    /// physnet 하나에 묶이지 않는 실패(빼기, 매핑·옵션 쓰기)
    pub errors: Vec<String>,
}

/// 이 클러스터의 OVS 브리지(Orbivisor physnet 브리지) 하나
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnedBridge {
    name: String,
    physnet: Option<String>,
}

impl OvnHost {
    /// physnet 연결과 게이트웨이 표시를 `config`(이 노드의 `NodeSpec.sdn`, 없으면 모두 뺌)에 맞춘다(§7). 이 클러스터의
    /// chassis일 때만 부른다. 차례: 원하는 physnet마다 veth·OVS 브리지 → 매핑 칸(남의 항목 보존) → 뺄 physnet의 veth →
    /// OVS 브리지 → `ovn-cms-options`. OVS를 읽지 못하면 실패하고, 그 밖의 실패는 physnet 상태나 `errors`에 담는다.
    pub async fn reconcile_physnets(
        &self,
        config: Option<&NodeSdnConfig>,
    ) -> Result<PhysnetReport, OvnError> {
        let tag = self.tag().to_string();
        let empty = BTreeMap::new();
        let wanted = config.map_or(&empty, |c| &c.physnets);
        let ids = self.external_ids().await?.ok_or_else(|| {
            OvnError::Command("Open vSwitch is not installed (ovs-vsctl is missing)".into())
        })?;
        let owned = self.owned_bridges().await?;
        let ours: BTreeSet<String> = owned.iter().map(|b| b.name.clone()).collect();
        let ports: BTreeSet<String> = self
            .tagged_interfaces()
            .await?
            .into_iter()
            .map(|i| i.name)
            .collect();
        let links = if wanted.is_empty() && owned.is_empty() {
            BTreeMap::new()
        } else {
            let all = self
                .run_ok(&Cmd::new("ip", &["-j", "link", "show"]))
                .await?;
            let bridges = self
                .run_ok(&Cmd::new(
                    "ip",
                    &["-j", "-d", "link", "show", "type", "bridge"],
                ))
                .await?;
            parse_links(&all.stdout, &bridges.stdout).map_err(OvnError::Command)?
        };
        let current = ids.get(MAPPINGS_KEY).cloned().unwrap_or_default();
        let names: BTreeMap<&String, (String, String, String)> = wanted
            .keys()
            .map(|p| (p, physnet_ifnames(&tag, p)))
            .collect();
        let all_want: BTreeMap<String, String> = names
            .iter()
            .map(|(p, (_, _, x))| ((*p).clone(), x.clone()))
            .collect();
        let conflicts = merge_mappings(&current, &ours, &all_want).conflicts;

        let mut report = PhysnetReport::default();
        let mut map_want = BTreeMap::new();
        for (physnet, bridge) in wanted {
            let (o, b, x) = &names[physnet];
            let state = |error: Option<String>| PhysnetState {
                name: physnet.clone(),
                bridge: bridge.clone(),
                ready: error.is_none(),
                error,
            };
            if let Some(other) = conflicts.get(physnet) {
                report.physnets.push(state(Some(format!(
                    "physnet {physnet} is mapped to {other} by someone else"
                ))));
                continue;
            }
            // 이미 있는 내 OVS 브리지의 매핑은 Linux 브리지가 잠시 없어도 둔다(흔들지 않음).
            if ours.contains(x) {
                map_want.insert(physnet.clone(), x.clone());
            }
            let facts = match links.get(bridge) {
                Some(f) if f.bridge => f,
                Some(_) => {
                    report
                        .physnets
                        .push(state(Some(format!("{bridge} is not a Linux bridge"))));
                    continue;
                }
                None => {
                    report
                        .physnets
                        .push(state(Some(format!("Linux bridge {bridge} does not exist"))));
                    continue;
                }
            };
            let ovs_ready = ours.contains(x) && ports.contains(o);
            let cmds = physnet_commands(
                &tag,
                physnet,
                bridge,
                facts,
                links.get(b),
                links.get(o),
                ovs_ready,
            );
            let mut failed = None;
            for cmd in &cmds {
                if let Err(e) = self.run_ok(cmd).await {
                    failed = Some(e.to_string());
                    break;
                }
            }
            if failed.is_none() {
                map_want.insert(physnet.clone(), x.clone());
            }
            report.physnets.push(state(failed));
        }

        // 매핑: 남의 항목은 그대로, 내 항목은 지금 원하는 것만
        let merged = merge_mappings(&current, &ours, &map_want);
        if merged.text != current {
            let cmd = if merged.text.is_empty() {
                Cmd::new(
                    "ovs-vsctl",
                    &["remove", "Open_vSwitch", ".", "external_ids", MAPPINGS_KEY],
                )
            } else {
                Cmd::new(
                    "ovs-vsctl",
                    &[
                        "set",
                        "Open_vSwitch",
                        ".",
                        &format!("external_ids:{MAPPINGS_KEY}={}", quote(&merged.text)),
                    ],
                )
            };
            if let Err(e) = self.run_ok(&cmd).await {
                let e = format!("cannot write {MAPPINGS_KEY}: {e}");
                for p in report.physnets.iter_mut().filter(|p| p.ready) {
                    p.ready = false;
                    p.error = Some(e.clone());
                }
                report.errors.push(e);
            }
        }

        // 빼기: 원하지 않는 내 OVS 브리지의 veth(한쪽을 지우면 짝도 사라진다), 그다음 브리지. 매핑에서 먼저 뺐다.
        // veth를 먼저 지우므로 도중에 멈춰도 내 표시가 있는 브리지가 남아 다음 차례가 다시 뺀다.
        let keep: BTreeSet<&String> = all_want.values().collect();
        for bridge in &owned {
            if keep.contains(&bridge.name) {
                continue;
            }
            let veth = format!("{}b", &bridge.name[..bridge.name.len() - 1]);
            let mut cmds = Vec::new();
            if links.contains_key(&veth) {
                cmds.push(Cmd::new("ip", &["link", "del", "dev", &veth]));
            }
            cmds.push(Cmd::new(
                "ovs-vsctl",
                &["--if-exists", "del-br", &bridge.name],
            ));
            for cmd in &cmds {
                if let Err(e) = self.run_ok(cmd).await {
                    report.errors.push(format!(
                        "cannot remove physnet {}: {e}",
                        bridge.physnet.as_deref().unwrap_or(&bridge.name)
                    ));
                    break;
                }
            }
        }

        // 게이트웨이 표시
        let gateway = config.is_some_and(|c| c.gateway.is_some());
        let options = ids.get(CMS_OPTIONS_KEY).cloned().unwrap_or_default();
        let want_options = merge_cms_options(&options, gateway);
        report.gateway = has_gateway_option(&options);
        if want_options != options {
            let cmd = if want_options.is_empty() {
                Cmd::new(
                    "ovs-vsctl",
                    &[
                        "remove",
                        "Open_vSwitch",
                        ".",
                        "external_ids",
                        CMS_OPTIONS_KEY,
                    ],
                )
            } else {
                Cmd::new(
                    "ovs-vsctl",
                    &[
                        "set",
                        "Open_vSwitch",
                        ".",
                        &format!("external_ids:{CMS_OPTIONS_KEY}={}", quote(&want_options)),
                    ],
                )
            };
            match self.run_ok(&cmd).await {
                Ok(_) => report.gateway = gateway,
                Err(e) => report
                    .errors
                    .push(format!("cannot write {CMS_OPTIONS_KEY}: {e}")),
            }
        }
        Ok(report)
    }

    /// 이 클러스터의 physnet OVS 브리지(이름이 `oplp<h8>x`이고 내 태그가 있는 `Bridge`). OVS가 없으면 빈 목록
    async fn owned_bridges(&self) -> Result<Vec<OwnedBridge>, OvnError> {
        let Some(out) = self
            .run_optional(&Cmd::new(
                "ovs-vsctl",
                &[
                    "--format=json",
                    "--columns=name,external_ids",
                    "find",
                    "Bridge",
                    &format!("external_ids:{TAG_KEY}={}", self.tag()),
                ],
            ))
            .await?
        else {
            return Ok(Vec::new());
        };
        let rows = parse_table(&out.stdout).map_err(OvnError::Command)?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?.to_string();
                let ids = row.get("external_ids").map(atom_map).unwrap_or_default();
                (is_physnet_ifname(&name)
                    && name.ends_with('x')
                    && ids.get(TAG_KEY).map(String::as_str) == Some(self.tag()))
                .then(|| OwnedBridge {
                    name,
                    physnet: ids.get(PHYSNET_KEY).cloned(),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
