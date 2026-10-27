mod common;

use std::collections::BTreeMap;

use common::{TestCluster, WAIT, node, volume};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchError, WatchEvent};
use opl_types::resource::Kind;
use opl_types::storage::{VolumeSpec, VolumeStatus};

#[tokio::test]
async fn create_get_update_delete_round_trip() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));

    let mut v = volume("disk-a");
    let rev = c.create(&v).await.unwrap();
    v.revision = rev;
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap(), Some(v.clone()));
    assert_eq!(
        c.get_by_name::<VolumeSpec>("disk-a").unwrap(),
        Some(v.clone())
    );

    let mut spec = v.spec.clone();
    spec.size_bytes *= 2;
    let labels = BTreeMap::from([("tier".to_string(), "gold".to_string())]);
    let rev2 = c
        .update_spec(v.id, Some(rev), labels.clone(), &spec)
        .await
        .unwrap();
    assert!(rev2 > rev);

    let status = VolumeStatus {
        allocated_bytes: Some(4096),
        ..Default::default()
    };
    let rev3 = c.update_status::<VolumeSpec>(v.id, &status).await.unwrap();
    let got = c.get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev3);
    assert_eq!(got.spec, spec);
    assert_eq!(got.labels, labels);
    assert_eq!(got.status, status);

    let (list_rev, items) = c.list::<VolumeSpec>().unwrap();
    assert_eq!(list_rev, rev3);
    assert_eq!(items, vec![got]);

    c.delete::<VolumeSpec>(v.id, Some(rev3)).await.unwrap();
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap(), None);
    assert_eq!(c.get_by_name::<VolumeSpec>("disk-a").unwrap(), None);
    // 삭제한 이름은 다시 쓸 수 있다.
    c.create(&volume("disk-a")).await.unwrap();
}

#[tokio::test]
async fn conflicting_writes_are_rejected() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let v = volume("disk-a");
    let rev = c.create(&v).await.unwrap();

    let err = c.create(&v).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::AlreadyExists { kind: Kind::Volume, id }) if id == v.id
    ));

    let err = c.create(&volume("disk-a")).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::NameTaken { .. })
    ));

    let err = c
        .update_spec(v.id, Some(rev + 100), BTreeMap::new(), &v.spec)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::RevisionMismatch { expected, actual })
            if expected == rev + 100 && actual == rev
    ));

    let err = c
        .delete::<VolumeSpec>(v.id, Some(rev + 100))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::RevisionMismatch { .. })
    ));

    let missing = volume("ghost").id;
    let err = c
        .update_status::<VolumeSpec>(missing, &VolumeStatus::default())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::NotFound { .. })
    ));

    // 거부된 명령은 아무것도 바꾸지 않는다.
    assert_eq!(c.get::<VolumeSpec>(v.id).unwrap().unwrap().revision, rev);
}

#[tokio::test]
async fn data_survives_restart() {
    let mut tc = TestCluster::start(1).await;
    let v = volume("disk-a");
    let rev = tc.get(node(1)).create(&v).await.unwrap();

    tc.stop(node(1)).await;
    tc.open(node(1)).await;
    tc.leader().await;

    let c = tc.get(node(1));
    let got = c.get::<VolumeSpec>(v.id).unwrap().unwrap();
    assert_eq!(got.revision, rev);
    let rev2 = c.create(&volume("disk-b")).await.unwrap();
    assert!(rev2 > rev, "revisions keep increasing after restart");
}

/// 다음 이벤트를 기다린다. 이벤트가 오지 않으면 멈추지 않고 실패한다.
async fn next(w: &mut Watch) -> WatchEvent {
    tokio::time::timeout(WAIT, w.next())
        .await
        .expect("no watch event")
        .unwrap()
}

#[tokio::test]
async fn watch_follows_list_without_gaps_or_duplicates() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let a = volume("disk-a");
    c.create(&a).await.unwrap();

    let (rev, items) = c.list::<VolumeSpec>().unwrap();
    assert_eq!(items.len(), 1);
    let mut w = c.watch(rev).unwrap();

    let b = volume("disk-b");
    let rev_b = c.create(&b).await.unwrap();
    let rev_del = c.delete::<VolumeSpec>(a.id, None).await.unwrap();

    match next(&mut w).await {
        WatchEvent::Put { kind, resource } => {
            assert_eq!(kind, Kind::Volume);
            assert_eq!(resource.id, b.id);
            assert_eq!(resource.revision, rev_b);
        }
        other => panic!("unexpected {other:?}"),
    }
    match next(&mut w).await {
        WatchEvent::Deleted {
            revision, resource, ..
        } => {
            assert_eq!(revision, rev_del);
            assert_eq!(resource.id, a.id);
        }
        other => panic!("unexpected {other:?}"),
    }

    // 지난 revision부터 다시 구독하면 보관한 변경을 다시 받는다.
    let mut again = c.watch(rev).unwrap();
    assert_eq!(next(&mut again).await.revision(), rev_b);
}

#[tokio::test]
async fn watch_history_does_not_survive_restart() {
    let mut tc = TestCluster::start(1).await;
    let rev = tc.get(node(1)).create(&volume("disk-a")).await.unwrap();
    tc.stop(node(1)).await;
    tc.open(node(1)).await;
    tc.leader().await;

    let c = tc.get(node(1));
    assert!(matches!(c.watch(rev - 1), Err(WatchError::Expired { .. })));
    let (list_rev, _) = c.list::<VolumeSpec>().unwrap();
    assert!(c.watch(list_rev).is_ok());
}

fn vm(name: &str, volumes: &[&str], macs: &[&str]) -> opl_types::vm::Vm {
    let spec = serde_json::json!({
        "cpu": {"sockets": 1, "cores": 1},
        "memory_mib": 256,
        "machine": "virt",
        "disks": volumes.iter().map(|v| serde_json::json!({"volume": v, "bus": "virtio-blk"})).collect::<Vec<_>>(),
        "nics": macs.iter().map(|m| serde_json::json!({"network": "user", "model": "virtio", "mac": m})).collect::<Vec<_>>(),
    });
    opl_types::resource::Resource::new(name, serde_json::from_value(spec).unwrap())
}

#[tokio::test]
async fn a_volume_or_mac_belongs_to_one_vm() {
    use opl_types::vm::VmSpec;
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let mac = "52:54:00:00:00:01";
    let a = vm("a", &["root-a"], &[mac]);
    c.create(&a).await.unwrap();

    let in_use = |e: ClusterError, field: &str| match e {
        ClusterError::Rejected(Rejection::InUse {
            kind: Kind::Vm,
            field: f,
            holder,
            ..
        }) => f == field && holder == a.id,
        _ => false,
    };
    let err = c.create(&vm("b", &["root-a"], &[])).await.unwrap_err();
    assert!(in_use(err, "volume"));
    let err = c.create(&vm("b", &["root-b"], &[mac])).await.unwrap_err();
    assert!(in_use(err, "mac"));

    // 다른 VM을 바꿔 겹치게 해도 거부한다. 자기 spec을 다시 쓰는 것은 된다.
    let b = vm("b", &["root-b"], &[]);
    let rev = c.create(&b).await.unwrap();
    let err = c
        .update_spec(
            b.id,
            Some(rev),
            BTreeMap::new(),
            &vm("b", &["root-a"], &[]).spec,
        )
        .await
        .unwrap_err();
    assert!(in_use(err, "volume"));
    c.update_spec::<VmSpec>(a.id, None, BTreeMap::new(), &a.spec)
        .await
        .unwrap();

    // 한 VM 안에서 같은 볼륨을 두 번 붙일 수도 없다.
    let c2 = vm("c", &["root-c", "root-c"], &[]);
    let err = c.create(&c2).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::InUse { holder, .. }) if holder == c2.id
    ));
}

fn snapshot(vm_id: uuid::Uuid, name: &str) -> opl_types::snapshot::Snapshot {
    let spec = serde_json::json!({
        "vm": vm_id,
        "name": name,
        "memory": false,
        "vm_spec": vm("src", &["root-a"], &[]).spec,
    });
    let mut s: opl_types::snapshot::Snapshot =
        opl_types::resource::Resource::new("", serde_json::from_value(spec).unwrap());
    s.name = s.id.to_string();
    s
}

#[tokio::test]
async fn a_snapshot_name_is_unique_within_its_vm() {
    let tc = TestCluster::start(1).await;
    let c = tc.get(node(1));
    let (vm_a, vm_b) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let first = snapshot(vm_a, "before");
    c.create(&first).await.unwrap();

    let err = c.create(&snapshot(vm_a, "before")).await.unwrap_err();
    assert!(matches!(
        err,
        ClusterError::Rejected(Rejection::InUse { kind: Kind::Snapshot, holder, .. }) if holder == first.id
    ));
    // 다른 VM은 같은 이름을 쓸 수 있고, 같은 VM도 다른 이름은 된다.
    c.create(&snapshot(vm_b, "before")).await.unwrap();
    c.create(&snapshot(vm_a, "after")).await.unwrap();
}
