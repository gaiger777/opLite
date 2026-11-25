//! 설정 이력(계획 73 §7): 출처를 단 쓰기가 모든 노드에 같은 이력을 남기고, 기능이 모든 노드에 있을 때만 싼다.

mod common;

use std::time::Duration;

use common::{TestCluster, volume};
use orbivisor_cluster::ChangeContext;
use orbivisor_types::history::{Actor, ActorAuth, ChangeMeta, ChangeSource, HistoryOp};
use orbivisor_types::node::{CONFIG_HISTORY, FEATURES, Node, NodeSpec, NodeStatus};
use orbivisor_types::resource::{Kind, Resource};
use orbivisor_types::storage::VolumeSpec;
use uuid::Uuid;

/// 노드 리소스를 기능 `features`로 만든다(실제 노드는 시작할 때 자기 status에 적는다).
async fn advertise(tc: &TestCluster, leader: orbivisor_cluster::types::NodeId, features: &[&str]) {
    for id in tc.nodes.keys() {
        let mut node: Node = Resource::new(
            format!("n{}", id.0.as_u128()),
            serde_json::from_value::<NodeSpec>(serde_json::json!({ "address": "10.0.0.1:8007" }))
                .unwrap(),
        );
        node.id = id.0;
        node.status = NodeStatus {
            features: features.iter().map(|f| f.to_string()).collect(),
            ..NodeStatus::default()
        };
        tc.get(leader).create(&node).await.unwrap();
    }
}

fn web_change() -> ChangeContext {
    ChangeContext::new(ChangeMeta {
        actor: Actor {
            user: Some("admin".into()),
            user_id: Some(Uuid::from_u128(7)),
            auth: ActorAuth::Session,
            task: None,
        },
        source: ChangeSource::Web,
        reason: Some("resize for the sale".into()),
        // 따르미가 보낸 시각은 리더가 덮는다.
        at_ms: 1,
        ..ChangeMeta::default()
    })
}

#[tokio::test]
async fn followers_forward_attributed_writes_and_every_node_keeps_the_same_history() {
    let tc = TestCluster::start_forwarding(3).await;
    let leader = tc.leader().await;
    advertise(&tc, leader, FEATURES).await;
    let follower = *tc.nodes.keys().find(|id| **id != leader).unwrap();
    let mut disk = volume("disk-a");
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let rev = web_change()
        .scope(async {
            let c = tc.get(follower);
            c.create(&disk).await.unwrap();
            disk.spec.size_bytes *= 2;
            c.update_spec(disk.id, None, Default::default(), &disk.spec)
                .await
                .unwrap();
            // status 쓰기는 이력이 없다.
            c.update_status::<VolumeSpec>(disk.id, &Default::default())
                .await
                .unwrap()
        })
        .await;
    // 출처 없이(리더 루프처럼) 지운다 — `System`으로 남는다.
    let rev = rev.max(
        tc.get(leader)
            .delete::<VolumeSpec>(disk.id, None)
            .await
            .unwrap(),
    );
    let mut seen = Vec::new();
    for id in tc.nodes.keys() {
        tc.wait_applied(*id, rev).await;
        let rows = tc.get(*id).history_of(Kind::Volume, disk.id).unwrap();
        seen.push(serde_json::to_string(&rows).unwrap());
        let ops: Vec<_> = rows.iter().map(|r| r.op).collect();
        assert_eq!(
            ops,
            vec![HistoryOp::Create, HistoryOp::Update, HistoryOp::Delete],
            "{id}"
        );
        assert!(
            rows[0].at_ms >= before,
            "the leader stamps the time: {rows:?}"
        );
        assert_eq!(rows[0].actor.user.as_deref(), Some("admin"));
        assert_eq!(rows[1].reason.as_deref(), Some("resize for the sale"));
        assert_eq!(rows[1].spec["size_bytes"], 2u64 << 30);
        assert_eq!(rows[2].source, ChangeSource::System);
        assert_eq!(rows[2].actor.auth, ActorAuth::System);
    }
    assert!(seen.windows(2).all(|w| w[0] == w[1]));
    let totals = tc.get(follower).history_totals().unwrap();
    assert_eq!(totals.count, 3);
}

/// 기능 `config-history`를 모르는 노드가 하나라도 있으면(또는 노드 리소스가 없으면) 옛 명령 그대로 쓴다.
#[tokio::test]
async fn writes_are_not_attributed_until_every_node_has_the_feature() {
    let tc = TestCluster::start(1).await;
    let leader = tc.leader().await;
    let disk = volume("disk-a");
    web_change()
        .scope(tc.get(leader).create(&disk))
        .await
        .unwrap();
    assert!(
        tc.get(leader)
            .history_of(Kind::Volume, disk.id)
            .unwrap()
            .is_empty()
    );

    let old: Vec<&str> = FEATURES
        .iter()
        .copied()
        .filter(|f| *f != CONFIG_HISTORY)
        .collect();
    advertise(&tc, leader, &old).await;
    let disk2 = volume("disk-b");
    web_change()
        .scope(tc.get(leader).create(&disk2))
        .await
        .unwrap();
    assert!(
        tc.get(leader)
            .history_of(Kind::Volume, disk2.id)
            .unwrap()
            .is_empty()
    );

    // 업그레이드를 마치면 그때부터 남는다.
    let node = tc.get(leader).list::<NodeSpec>().unwrap().1.remove(0);
    let mut status = node.status;
    status.features.push(CONFIG_HISTORY.into());
    tc.get(leader)
        .update_status::<NodeSpec>(node.id, &status)
        .await
        .unwrap();
    let disk3 = volume("disk-c");
    web_change()
        .scope(tc.get(leader).create(&disk3))
        .await
        .unwrap();
    assert_eq!(
        tc.get(leader)
            .history_of(Kind::Volume, disk3.id)
            .unwrap()
            .len(),
        1
    );
}

/// 쓰기 길 전체(Raft 로그 fsync·적용 포함)에서 이력의 비용. 기본으로 돌지 않는다:
/// `cargo test --release -p orbivisor-cluster --test history -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn write_path_overhead() {
    const N: usize = 300;
    async fn run(features: &[&str]) -> Duration {
        let tc = TestCluster::start(1).await;
        let leader = tc.leader().await;
        advertise(&tc, leader, features).await;
        let c = tc.get(leader);
        let mut disk = volume("disk-a");
        c.create(&disk).await.unwrap();
        let t = std::time::Instant::now();
        for i in 0..N {
            disk.spec.size_bytes = (i as u64 + 2) << 30;
            web_change()
                .scope(c.update_spec(disk.id, None, Default::default(), &disk.spec))
                .await
                .unwrap();
        }
        let d = t.elapsed();
        let rows = c.history_totals().unwrap().count;
        assert_eq!(rows > 0, features.contains(&CONFIG_HISTORY));
        d
    }
    let old: Vec<&str> = FEATURES
        .iter()
        .copied()
        .filter(|f| *f != CONFIG_HISTORY)
        .collect();
    let (mut off, mut on) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        off.push(run(&old).await);
        on.push(run(FEATURES).await);
    }
    off.sort();
    on.sort();
    let (a, b) = (off[2], on[2]);
    println!(
        "cluster write path: {N} spec updates without history {a:?}, with history {b:?}, overhead {:+.1}%",
        (b.as_secs_f64() / a.as_secs_f64() - 1.0) * 100.0
    );
}
