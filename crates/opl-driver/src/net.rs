//! VM NIC를 Linux 브리지에 잇는 tap 장치. iproute2(`ip`, `bridge`) 명령을 부르므로 opld가
//! root(또는 CAP_NET_ADMIN)로 돌아야 한다. 브리지 자체는 관리자가 미리 만든다.

use async_trait::async_trait;
use opl_types::network::tap_prefix;
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

/// 호스트 네트워크 조작. 테스트에서 바꿔 끼울 수 있게 트레이트로 둔다.
#[async_trait]
pub trait HostNet: Send + Sync + std::fmt::Debug {
    /// 이 호스트의 Linux 브리지
    async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError>;
    /// tap을 만들어 브리지에 붙이고 올린다. 같은 이름이 남아 있으면 지우고 다시 만든다.
    async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError>;
    /// VM의 tap을 모두 지운다. 없으면 아무것도 하지 않는다.
    async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError>;
}

/// iproute2로 tap을 다룬다.
#[derive(Debug, Default)]
pub struct IpRoute;

#[async_trait]
impl HostNet for IpRoute {
    async fn bridges(&self) -> Result<Vec<BridgeInfo>, DriverError> {
        let out = run("ip", &["-j", "-d", "link", "show", "type", "bridge"]).await?;
        Ok(parse_bridges(&out)?.into_iter().map(|b| b.info).collect())
    }

    async fn create_tap(&self, tap: &Tap) -> Result<(), DriverError> {
        let out = run("ip", &["-j", "-d", "link", "show", "type", "bridge"]).await?;
        let bridges = parse_bridges(&out)?;
        let Some(bridge) = bridges.iter().find(|b| b.info.name == tap.bridge) else {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} not found on this node",
                tap.bridge
            )));
        };
        if tap.vlan.is_some() && !bridge.info.vlan_filtering {
            return Err(DriverError::InvalidSpec(format!(
                "bridge {:?} is not VLAN-aware (vlan_filtering 1)",
                tap.bridge
            )));
        }
        // 이전 VM 인스턴스가 남긴 같은 이름의 tap
        delete_link(&tap.ifname).await?;
        for (program, args) in tap_commands(tap, bridge.default_pvid) {
            if let Err(e) = run(program, &args).await {
                let _ = delete_link(&tap.ifname).await;
                return Err(e);
            }
        }
        Ok(())
    }

    async fn delete_vm_taps(&self, vm: Uuid) -> Result<(), DriverError> {
        let out = run("ip", &["-j", "link", "show"]).await?;
        let links: Vec<Link> = serde_json::from_str(&out).map_err(bad_output)?;
        let prefix = tap_prefix(vm);
        // 하나가 실패해도 나머지 tap은 지운다.
        let mut result = Ok(());
        for link in links.iter().filter(|l| l.ifname.starts_with(&prefix)) {
            if let Err(e) = delete_link(&link.ifname).await {
                result = result.and(Err(e));
            }
        }
        result
    }
}

/// tap을 만들고 브리지에 붙여 올리는 명령. VLAN이면 브리지 기본 VLAN 대신 그 VLAN만
/// 태그 없이 주고받게 한다.
fn tap_commands(tap: &Tap, default_pvid: u16) -> Vec<(&'static str, Vec<String>)> {
    let dev = tap.ifname.as_str();
    let mut cmds = vec![
        ("ip", args(&["tuntap", "add", "dev", dev, "mode", "tap"])),
        (
            "ip",
            args(&["link", "set", "dev", dev, "master", &tap.bridge]),
        ),
    ];
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

async fn delete_link(ifname: &str) -> Result<(), DriverError> {
    match run("ip", &["link", "del", "dev", ifname]).await {
        Err(DriverError::ToolFailed { stderr, .. }) if stderr.contains("Cannot find device") => {
            Ok(())
        }
        other => other.map(|_| ()),
    }
}

async fn run(program: &'static str, args: &[impl AsRef<str>]) -> Result<String, DriverError> {
    let output = tokio::process::Command::new(program)
        .args(args.iter().map(AsRef::as_ref))
        .output()
        .await?;
    if !output.status.success() {
        return Err(DriverError::ToolFailed {
            tool: program,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Debug, Deserialize)]
struct Link {
    ifname: String,
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
            joined(tap_commands(&tap(None), 1)),
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
            joined(tap_commands(&tap(Some(10)), 1)),
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
            let cmds = joined(tap_commands(&tap(Some(10)), pvid));
            assert!(!cmds.iter().any(|c| c.contains("vlan del")), "{cmds:?}");
        }
    }

    #[test]
    fn parses_bridges() {
        let json = r#"[
            {"ifindex":2,"ifname":"vmbr0","linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":1,"vlan_default_pvid":1}}},
            {"ifindex":3,"ifname":"br-plain","linkinfo":{"info_kind":"bridge","info_data":{"vlan_filtering":0,"vlan_default_pvid":1}}},
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
                    default_pvid: 1
                },
                Bridge {
                    info: BridgeInfo {
                        name: "br-plain".into(),
                        vlan_filtering: false
                    },
                    default_pvid: 1
                },
                Bridge {
                    info: BridgeInfo {
                        name: "br-nopvid".into(),
                        vlan_filtering: true
                    },
                    default_pvid: 1
                },
            ]
        );
        assert!(parse_bridges("[]").unwrap().is_empty());
        assert!(parse_bridges("not json").is_err());
    }
}
