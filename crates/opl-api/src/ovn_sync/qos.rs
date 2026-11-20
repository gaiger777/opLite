//! OvnSync의 NIC QoS(계획 67 §8.1): SDN NIC의 `qos`마다 NB `QoS` 행 최대 둘(그 NIC 스위치의 `qos_rules` — 강한
//! 참조라 뿌리가 아님). 행은 포트와 같은 네트워크 묶음(한 트랜잭션)에서 내용으로 짝지어 쓴다: 같은 행은 두고, 다른 것만
//! 넣고 뺀다(뺀 행은 참조를 잃고 OVSDB가 지운다). 우리 표시가 없는 행(관리자가 넣은 것)은 둔다.

use std::collections::{BTreeMap, BTreeSet};

use opl_driver::ovn::ovsdb::{map_value, named_uuid, uuid_value};
use opl_types::qos::{NicQos, nic_rows};
use serde_json::{Value, json};

use super::nb::CLUSTER_KEY;

/// 행 주인 표시: 논리 포트 이름
pub const PORT_KEY: &str = "orbivisor:port";

/// 바라는 `QoS` 행 하나. `action`·`bandwidth`는 정수 맵이지만 견주기 쉽게 글로 든다(읽을 때 [`parse_map`]도 글로 준다).
///
/// [`parse_map`]: opl_driver::ovn::ovsdb::parse_map
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct QosNbRow {
    pub direction: String,
    pub priority: i64,
    pub r#match: String,
    pub action: BTreeMap<String, String>,
    pub bandwidth: BTreeMap<String, String>,
    pub external_ids: BTreeMap<String, String>,
}

/// NB에서 읽은 우리 `QoS` 행
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActualQos {
    pub uuid: String,
    pub row: QosNbRow,
}

/// SDN NIC(논리 포트 `port`)의 바라는 `QoS` 행(순수 [`nic_rows`]를 NB 칸으로)
pub fn rows(port: &str, qos: &NicQos, tag: &str) -> Vec<QosNbRow> {
    nic_rows(port, qos)
        .into_iter()
        .map(|r| {
            let mut bandwidth = BTreeMap::new();
            if let Some(rate) = r.rate_kbps {
                bandwidth.insert("rate".to_string(), rate.to_string());
            }
            if let Some(burst) = r.burst_kbit {
                bandwidth.insert("burst".to_string(), burst.to_string());
            }
            QosNbRow {
                direction: r.direction.as_str().into(),
                priority: i64::from(r.priority),
                r#match: r.r#match,
                action: r
                    .dscp
                    .map(|d| BTreeMap::from([("dscp".to_string(), d.to_string())]))
                    .unwrap_or_default(),
                bandwidth,
                external_ids: BTreeMap::from([
                    (CLUSTER_KEY.to_string(), tag.to_string()),
                    (PORT_KEY.to_string(), port.to_string()),
                ]),
            }
        })
        .collect()
}

/// 글로 든 정수 맵을 OVSDB 정수 맵 `["map", [[k, n], …]]`으로
fn int_map(m: &BTreeMap<String, String>) -> Value {
    json!([
        "map",
        m.iter()
            .map(|(k, v)| json!([k, v.parse::<i64>().unwrap_or_default()]))
            .collect::<Vec<_>>()
    ])
}

/// 행 넣기 연산
pub fn insert(name: &str, r: &QosNbRow) -> Value {
    json!({"op": "insert", "table": "QoS", "uuid-name": name,
        "row": {"priority": r.priority, "direction": r.direction, "match": r.r#match,
                "action": int_map(&r.action), "bandwidth": int_map(&r.bandwidth),
                "external_ids": map_value(&r.external_ids)}})
}

/// 스위치 하나의 `qos_rules`를 `want`에 맞추는 (넣는 연산, 넣을 참조, 뺄 참조). `current`는 스위치의 `qos_rules`
/// (읽은 uuid, 새 스위치면 비어 있음), `have`는 읽은 우리 행이다. `n`은 묶음 번호(named-uuid가 겹치지 않게).
pub fn plan(
    n: usize,
    want: &[QosNbRow],
    current: &[String],
    have: &[ActualQos],
) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let ours: Vec<&ActualQos> = have.iter().filter(|a| current.contains(&a.uuid)).collect();
    let (mut ops, mut add) = (Vec::new(), Vec::new());
    let mut kept = BTreeSet::new();
    for (i, w) in want.iter().enumerate() {
        match ours
            .iter()
            .find(|a| a.row == *w && !kept.contains(a.uuid.as_str()))
        {
            Some(a) => {
                kept.insert(a.uuid.as_str());
            }
            None => {
                let name = format!("q{n}_{i}");
                ops.push(insert(&name, w));
                add.push(named_uuid(&name));
            }
        }
    }
    let remove = ours
        .iter()
        .filter(|a| !kept.contains(a.uuid.as_str()))
        .map(|a| uuid_value(&a.uuid))
        .collect();
    (ops, add, remove)
}
