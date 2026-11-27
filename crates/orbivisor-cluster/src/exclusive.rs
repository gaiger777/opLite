//! 클러스터 안에서 리소스 하나만 쓸 수 있는 값. 상태 머신이 커밋 순서대로 확인하므로, 여러 노드의
//! API가 동시에 같은 값을 써도 하나만 성공한다.

use orbivisor_types::folder::FolderSpec;
use orbivisor_types::resource::Kind;
use serde::Deserialize;
use serde_json::Value;

/// 종류가 달라도 한 키 공간을 쓰는 필드(계획 66): 주소 `ip`는 VM NIC·라우터 외부 주소·유동 IP·부하 분산기 주소(계획
/// 67)가 함께 쓴다.
pub(crate) const SHARED_IP_KINDS: [Kind; 4] =
    [Kind::Vm, Kind::Router, Kind::FloatingIp, Kind::LoadBalancer];

/// spec에서 다른 리소스와 겹치면 안 되는 값을 `(필드, 값)`으로 꺼낸다. 지금은 VM이 붙인
/// 볼륨 이름과 NIC의 MAC 주소, SDN NIC 주소의 `{네트워크 이름}/{주소}`(계획 65 — 템플릿은 보지 않는다), 스냅샷의
/// `{vm id}/{이름}`, 폴더의 형제 이름 키(`FolderSpec::sibling_key`)다. 계획 66: 라우터 외부 주소와 유동 IP도 같은
/// `ip` 키(종류 사이에서 겹치지 않음 — [`SHARED_IP_KINDS`]), 라우터 인터페이스 네트워크 `router-net`(한 네트워크는
/// 라우터 하나에만), 유동 IP가 붙은 NIC `fip-nic`(`{vm id}/{MAC}`), 공급자 네트워크의 `physnet`(`{physnet}/{vlan|flat}`).
/// 계획 67: 부하 분산기의 VIP·바깥 VIP·건강 검사 주소도 `ip`, 리스너 `lb-listener`(`{VIP 네트워크}/{VIP}/{프로토콜}/{포트}`),
/// 끝나지 않은 네트워크 옮기기의 원본 `migration-source`(한 네트워크에 하나)와 VM `migration-vm`(한 VM은 한 옮기기에만 —
/// VM 목록은 스펙 `vms`와 상태 `vms[].vm`이라 `status`도 본다. `Finished`·`RolledBack`이면 값이 없다), 다리를 놓을 수 있는
/// (`bridge`가 `false`가 아닌) 옮기기의 대상 `migration-target`(한 대상 네트워크에 다리 하나 — 리뷰 1 I1).
/// 계획 68: 백업 저장소의 자리 `backup-store-target`(`pool:<풀 이름>` 또는 `s3:<host>/<버킷>/<접두사>` — 한 곳에 저장소
/// 하나).
/// 계획 72: VMware 이전 항목의 원본 VM `vmware-vm`(`instance_uuid` — 한 원본 VM은 끝나지 않은 항목 하나에만. 끝난 항목
/// `finalized`·`reverted`·`cancelled`는 잡지 않아 같은 VM을 새 계획에 넣을 수 있다).
/// 계획 76: 도는(`running`) DR 실행의 계획 `dr-run`(계획 id — 한 복구 계획에 실행 하나).
pub(crate) fn exclusive_values(
    kind: Kind,
    spec: &Value,
    status: &Value,
) -> Vec<(&'static str, String)> {
    if kind == Kind::Folder {
        // 풀 수 없는 폴더 스펙은 API가 쓰지 않는다. 겹침 검사에서는 없는 것으로 본다.
        return FolderSpec::deserialize(spec)
            .map(|f| vec![("name", f.sibling_key())])
            .unwrap_or_default();
    }
    if kind == Kind::Snapshot {
        return match (spec["vm"].as_str(), spec["name"].as_str()) {
            (Some(vm), Some(name)) => vec![("name", format!("{vm}/{name}"))],
            _ => Vec::new(),
        };
    }
    let ip = |network: &Value, ip: &Value| {
        let network = network.as_str()?;
        let ip = ip.as_str()?.parse::<std::net::IpAddr>().ok()?;
        Some(("ip", format!("{network}/{}", ip.to_canonical())))
    };
    match kind {
        Kind::Router => {
            let mut values: Vec<_> = spec["interfaces"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| i["network"].as_str())
                .map(|n| ("router-net", n.to_string()))
                .collect();
            let external = &spec["external"];
            for a in external["ips"].as_array().into_iter().flatten() {
                values.extend(ip(&external["network"], a));
            }
            return values;
        }
        Kind::FloatingIp => {
            let mut values: Vec<_> = ip(&spec["network"], &spec["address"]).into_iter().collect();
            let target = &spec["target"];
            if let (Some(vm), Some(mac)) = (target["vm"].as_str(), target["mac"].as_str()) {
                values.push(("fip-nic", format!("{vm}/{}", mac.to_ascii_lowercase())));
            }
            return values;
        }
        Kind::LoadBalancer => {
            let mut values: Vec<_> = ip(&spec["network"], &spec["vip"]).into_iter().collect();
            let external = &spec["external"];
            values.extend(ip(&external["network"], &external["ip"]));
            for m in spec["monitors"].as_array().into_iter().flatten() {
                values.extend(ip(&m["network"], &m["ip"]));
            }
            if let (Some(network), Some(vip)) = (spec["network"].as_str(), spec["vip"].as_str()) {
                for l in spec["listeners"].as_array().into_iter().flatten() {
                    if let (Some(proto), Some(port)) = (l["protocol"].as_str(), l["port"].as_u64())
                    {
                        values.push(("lb-listener", format!("{network}/{vip}/{proto}/{port}")));
                    }
                }
            }
            return values;
        }
        Kind::NetworkMigration => {
            if matches!(status["phase"].as_str(), Some("finished" | "rolled-back")) {
                return Vec::new();
            }
            let mut values: Vec<_> = spec["source"]
                .as_str()
                .map(|s| ("migration-source", s.to_string()))
                .into_iter()
                .collect();
            // 한 VM이 NIC 여럿을 옮기면 상태에 여러 번 나오므로 한 번만 낸다.
            let vms: std::collections::BTreeSet<&str> = spec["vms"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .chain(
                    status["vms"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|m| m["vm"].as_str()),
                )
                .collect();
            values.extend(vms.into_iter().map(|v| ("migration-vm", v.to_string())));
            // 다리를 놓을 수 있는 옮기기는 대상 하나에 하나(리뷰 1 I1 — 한 논리 스위치에 l2gateway 둘이면 물리 L2와 고리가
            // 생기거나 두 LAN이 하나로 이어진다). 다리 없이(`bridge: false`) 옮기는 것은 여럿이어도 된다.
            if spec["bridge"].as_bool() != Some(false)
                && let Some(t) = spec["target"].as_str()
            {
                values.push(("migration-target", t.to_string()));
            }
            return values;
        }
        Kind::Network => {
            let provider = &spec["sdn"]["provider"];
            return match (provider["physnet"].as_str(), provider["vlan"].as_u64()) {
                (Some(p), Some(vlan)) => vec![("physnet", format!("{p}/{vlan}"))],
                (Some(p), None) => vec![("physnet", format!("{p}/flat"))],
                _ => Vec::new(),
            };
        }
        Kind::BackupStore => {
            return orbivisor_types::backup_store::store_target_key_value(spec)
                .map(|k| ("backup-store-target", k))
                .into_iter()
                .collect();
        }
        Kind::VmwareVm => {
            if matches!(
                status["state"].as_str(),
                Some("finalized" | "reverted" | "cancelled")
            ) {
                return Vec::new();
            }
            return spec["instance_uuid"]
                .as_str()
                .map(|u| ("vmware-vm", u.to_string()))
                .into_iter()
                .collect();
        }
        Kind::DrRun => {
            // 계획 76: 계획 하나에 도는 실행 하나(시험·장애 조치·뒷정리 모두). 끝난 실행은 잡지 않는다. 상태가 없으면
            // 막 만든 실행(`running`)이다.
            if status["state"].as_str().is_some_and(|s| s != "running") {
                return Vec::new();
            }
            return spec["plan"]
                .as_str()
                .map(|p| ("dr-run", p.to_string()))
                .into_iter()
                .collect();
        }
        Kind::Vm => {}
        _ => return Vec::new(),
    }
    let field = |list: &str, key: &str, name: &'static str| {
        spec[list]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(move |item| item[key].as_str().map(|v| (name, v.to_string())))
            .collect::<Vec<_>>()
    };
    let mut values = field("disks", "volume", "volume");
    values.extend(field("nics", "mac", "mac"));
    // NIC 주소는 네트워크마다 하나다(폴더가 다른 네트워크는 같은 CIDR을 쓸 수 있다). 템플릿은 주소를 갖지 않는다.
    if spec["template"].as_bool() != Some(true) {
        for nic in spec["nics"].as_array().into_iter().flatten() {
            for a in nic["ips"].as_array().into_iter().flatten() {
                values.extend(ip(&nic["network"], a));
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(kind: Kind, spec: &Value) -> Vec<(&'static str, String)> {
        exclusive_values(kind, spec, &Value::Null)
    }

    #[test]
    fn extracts_vm_volumes_and_macs() {
        let spec = json!({
            "disks": [{"volume": "root"}, {"volume": "data"}],
            "nics": [{"network": "user", "mac": "52:54:00:00:00:01"}, {"network": "user", "mac": null}]
        });
        assert_eq!(
            values(Kind::Vm, &spec),
            vec![
                ("volume", "root".to_string()),
                ("volume", "data".to_string()),
                ("mac", "52:54:00:00:00:01".to_string()),
            ]
        );
        assert!(values(Kind::Volume, &spec).is_empty());
    }

    /// 계획 72: 한 원본 VM은 끝나지 않은 VMware 이전 항목 하나에만 든다. 끝난 항목은 잡지 않는다.
    #[test]
    fn extracts_vmware_source_vms() {
        let item = json!({ "migration": "m", "source_ref": "vm-1", "instance_uuid": "5003-a", "target_name": "web" });
        assert_eq!(
            values(Kind::VmwareVm, &item),
            vec![("vmware-vm", "5003-a".to_string())]
        );
        for state in ["draft", "syncing", "done", "failed", "rolled-back"] {
            assert_eq!(
                exclusive_values(Kind::VmwareVm, &item, &json!({ "state": state })).len(),
                1,
                "{state}"
            );
        }
        for state in ["finalized", "reverted", "cancelled"] {
            assert!(
                exclusive_values(Kind::VmwareVm, &item, &json!({ "state": state })).is_empty(),
                "{state}"
            );
        }
        for kind in [Kind::VmwareSource, Kind::VmwareMigration, Kind::VmExport] {
            assert!(values(kind, &item).is_empty(), "{kind:?}");
        }
    }

    /// 계획 76: 한 복구 계획에는 도는 실행(`running`) 하나만. 끝난 실행은 잡지 않는다.
    #[test]
    fn extracts_the_plan_of_running_dr_runs() {
        let run = json!({ "plan": "0190a0b0-0000-7000-8000-000000000001", "plan_name": "web", "kind": "test" });
        let want = vec![("dr-run", "0190a0b0-0000-7000-8000-000000000001".to_string())];
        assert_eq!(values(Kind::DrRun, &run), want);
        assert_eq!(
            exclusive_values(Kind::DrRun, &run, &json!({ "state": "running" })),
            want
        );
        for state in ["succeeded", "partial", "failed", "cancelled"] {
            assert!(
                exclusive_values(Kind::DrRun, &run, &json!({ "state": state })).is_empty(),
                "{state}"
            );
        }
        assert!(values(Kind::RecoveryPlan, &run).is_empty());
    }

    /// 계획 68: 백업 저장소 하나는 한 곳(풀 또는 S3 버킷·접두사)에만 둔다.
    #[test]
    fn extracts_backup_store_targets() {
        assert_eq!(
            values(Kind::BackupStore, &json!({ "backend": { "pool": "bk" } })),
            vec![("backup-store-target", "pool:bk".to_string())]
        );
        let s3 = json!({ "backend": { "s3": {
            "endpoint": "https://S3.example.com:443", "bucket": "orbi-bk", "prefix": "a/",
            "access_key_id": "k", "secret_access_key": "s"
        } } });
        assert_eq!(
            values(Kind::BackupStore, &s3),
            vec![(
                "backup-store-target",
                "s3:s3.example.com/orbi-bk/a".to_string()
            )]
        );
        assert!(values(Kind::BackupStore, &json!({})).is_empty());
    }

    #[test]
    fn extracts_nic_addresses_per_network_except_templates() {
        let spec = json!({
            "nics": [
                {"network": "tenant-a", "mac": "52:54:00:00:00:01", "ips": ["10.0.0.5", "2001:db8::5"]},
                {"network": "tenant-b", "mac": "52:54:00:00:00:02", "ips": ["::ffff:10.0.0.5"]},
                {"network": "lan", "mac": "52:54:00:00:00:03"}
            ]
        });
        assert_eq!(
            values(Kind::Vm, &spec),
            vec![
                ("mac", "52:54:00:00:00:01".to_string()),
                ("mac", "52:54:00:00:00:02".to_string()),
                ("mac", "52:54:00:00:00:03".to_string()),
                ("ip", "tenant-a/10.0.0.5".to_string()),
                ("ip", "tenant-a/2001:db8::5".to_string()),
                // IPv4 대응 IPv6는 IPv4로 센다.
                ("ip", "tenant-b/10.0.0.5".to_string()),
            ]
        );
        let mut template = spec.clone();
        template["template"] = json!(true);
        assert!(values(Kind::Vm, &template).iter().all(|(f, _)| *f == "mac"));
    }

    #[test]
    fn extracts_snapshot_name_within_its_vm() {
        let vm = "0195f7a0-0000-7000-8000-000000000001";
        let spec = json!({
            "vm": vm,
            "name": "before-upgrade",
            "vm_spec": {"disks": [{"volume": "root"}]}
        });
        // 같은 VM 안에서만 이름이 겹치면 안 된다. vm_spec의 볼륨은 배타 값이 아니다.
        assert_eq!(
            values(Kind::Snapshot, &spec),
            vec![("name", format!("{vm}/before-upgrade"))]
        );
    }

    #[test]
    fn extracts_folder_sibling_key() {
        let parent = "0195f7a0-0000-7000-8000-000000000001";
        assert_eq!(
            values(
                Kind::Folder,
                &json!({"name": "Prod", "kind": "vm", "parent": parent})
            ),
            vec![("name", format!("{parent}/prod"))]
        );
        // 루트 바로 아래는 종류마다 따로 센다.
        assert_eq!(
            values(Kind::Folder, &json!({"name": "A", "kind": "network"})),
            vec![("name", "root-network/a".to_string())]
        );
        assert!(values(Kind::Folder, &json!({"name": 1})).is_empty());
    }

    /// 계획 66: 라우터·유동 IP·공급자 네트워크의 독점 값
    #[test]
    fn extracts_routing_values() {
        let router = json!({
            "interfaces": [{"network": "t1"}, {"network": "t2"}],
            "external": {"network": "ext", "ips": ["203.0.113.10", "2001:db8::10"]}
        });
        assert_eq!(
            values(Kind::Router, &router),
            vec![
                ("router-net", "t1".to_string()),
                ("router-net", "t2".to_string()),
                ("ip", "ext/203.0.113.10".to_string()),
                ("ip", "ext/2001:db8::10".to_string()),
            ]
        );
        assert!(values(Kind::Router, &json!({})).is_empty());
        let vm = "0195f7a0-0000-7000-8000-000000000001";
        let fip = json!({
            "network": "ext",
            "address": "203.0.113.120",
            "target": {"vm": vm, "mac": "52:54:00:AA:00:01"}
        });
        assert_eq!(
            values(Kind::FloatingIp, &fip),
            vec![
                ("ip", "ext/203.0.113.120".to_string()),
                ("fip-nic", format!("{vm}/52:54:00:aa:00:01")),
            ]
        );
        // 주소를 아직 고르지 않은 유동 IP는 값이 없다.
        assert!(values(Kind::FloatingIp, &json!({"network": "ext"})).is_empty());
        assert_eq!(
            values(
                Kind::Network,
                &json!({"sdn": {"provider": {"physnet": "ext", "vlan": 100}}})
            ),
            vec![("physnet", "ext/100".to_string())]
        );
        assert_eq!(
            values(
                Kind::Network,
                &json!({"sdn": {"provider": {"physnet": "ext"}}})
            ),
            vec![("physnet", "ext/flat".to_string())]
        );
        // 오버레이·브리지 네트워크는 값이 없다(지금 그대로).
        assert!(values(Kind::Network, &json!({"sdn": {}})).is_empty());
        assert!(values(Kind::Network, &json!({"bridge": "vmbr0"})).is_empty());
    }

    /// 계획 67: 부하 분산기의 주소·리스너, 끝나지 않은 네트워크 옮기기의 원본·VM
    #[test]
    fn extracts_sdn_service_values() {
        let lb = json!({
            "network": "t1",
            "vip": "10.10.0.50",
            "external": {"network": "ext", "ip": "203.0.113.130"},
            "listeners": [{"protocol": "tcp", "port": 80}, {"protocol": "udp", "port": 53}],
            "monitors": [{"network": "t1", "ip": "10.10.0.60"}, {"network": "t2", "ip": "10.20.0.60"}]
        });
        assert_eq!(
            values(Kind::LoadBalancer, &lb),
            vec![
                ("ip", "t1/10.10.0.50".to_string()),
                ("ip", "ext/203.0.113.130".to_string()),
                ("ip", "t1/10.10.0.60".to_string()),
                ("ip", "t2/10.20.0.60".to_string()),
                ("lb-listener", "t1/10.10.0.50/tcp/80".to_string()),
                ("lb-listener", "t1/10.10.0.50/udp/53".to_string()),
            ]
        );
        // VIP를 아직 고르지 않았으면 값이 없다.
        assert!(
            values(
                Kind::LoadBalancer,
                &json!({"network": "t1", "listeners": [{"protocol": "tcp", "port": 80}]})
            )
            .is_empty()
        );
        let (a, b) = (
            "0195f7a0-0000-7000-8000-000000000001",
            "0195f7a0-0000-7000-8000-000000000002",
        );
        let mig = json!({"source": "lan", "target": "t-lan", "vms": [a]});
        // 상태의 VM(NIC마다 한 줄)은 한 번만
        let status = json!({"phase": "running", "vms": [{"vm": b, "nic": 0}, {"vm": b, "nic": 1}, {"vm": a, "nic": 0}]});
        assert_eq!(
            exclusive_values(Kind::NetworkMigration, &mig, &status),
            vec![
                ("migration-source", "lan".to_string()),
                ("migration-vm", a.to_string()),
                ("migration-vm", b.to_string()),
                ("migration-target", "t-lan".to_string()),
            ]
        );
        // 다리 없이 옮기면 대상을 잡지 않는다.
        let no_bridge = json!({"source": "lan", "target": "t-lan", "bridge": false});
        assert_eq!(
            values(Kind::NetworkMigration, &no_bridge),
            vec![("migration-source", "lan".to_string())]
        );
        for done in ["finished", "rolled-back"] {
            assert!(
                exclusive_values(Kind::NetworkMigration, &mig, &json!({"phase": done})).is_empty()
            );
        }
        assert_eq!(values(Kind::NetworkMigration, &mig).len(), 3);
    }
}
