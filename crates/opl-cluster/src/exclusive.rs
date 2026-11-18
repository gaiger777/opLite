//! 클러스터 안에서 리소스 하나만 쓸 수 있는 값. 상태 머신이 커밋 순서대로 확인하므로, 여러 노드의
//! API가 동시에 같은 값을 써도 하나만 성공한다.

use opl_types::folder::FolderSpec;
use opl_types::resource::Kind;
use serde::Deserialize;
use serde_json::Value;

/// spec에서 다른 리소스와 겹치면 안 되는 값을 `(필드, 값)`으로 꺼낸다. 지금은 VM이 붙인
/// 볼륨 이름과 NIC의 MAC 주소, 스냅샷의 `{vm id}/{이름}`, 폴더의 형제 이름 키(`FolderSpec::sibling_key`)다.
pub(crate) fn exclusive_values(kind: Kind, spec: &Value) -> Vec<(&'static str, String)> {
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
    if kind != Kind::Vm {
        return Vec::new();
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
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_vm_volumes_and_macs() {
        let spec = json!({
            "disks": [{"volume": "root"}, {"volume": "data"}],
            "nics": [{"network": "user", "mac": "52:54:00:00:00:01"}, {"network": "user", "mac": null}]
        });
        assert_eq!(
            exclusive_values(Kind::Vm, &spec),
            vec![
                ("volume", "root".to_string()),
                ("volume", "data".to_string()),
                ("mac", "52:54:00:00:00:01".to_string()),
            ]
        );
        assert!(exclusive_values(Kind::Volume, &spec).is_empty());
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
            exclusive_values(Kind::Snapshot, &spec),
            vec![("name", format!("{vm}/before-upgrade"))]
        );
    }

    #[test]
    fn extracts_folder_sibling_key() {
        let parent = "0195f7a0-0000-7000-8000-000000000001";
        assert_eq!(
            exclusive_values(
                Kind::Folder,
                &json!({"name": "Prod", "kind": "vm", "parent": parent})
            ),
            vec![("name", format!("{parent}/prod"))]
        );
        // 루트 바로 아래는 종류마다 따로 센다.
        assert_eq!(
            exclusive_values(Kind::Folder, &json!({"name": "A", "kind": "network"})),
            vec![("name", "root-network/a".to_string())]
        );
        assert!(exclusive_values(Kind::Folder, &json!({"name": 1})).is_empty());
    }
}
