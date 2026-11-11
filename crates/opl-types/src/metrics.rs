//! 사용량 지표. 노드마다 5초마다 노드와 그 노드에서 도는 VM을 재고 메모리에 1시간 둔다(Raft에 쓰지 않는다).
//! 비율은 직전 측정과의 차이를 걸린 시간으로 나눈 값이고, 읽지 못한 값은 `null`이다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 노드 점 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeSample {
    /// 측정 시각(그 노드 벽시계의 유닉스 밀리초)
    pub t: i64,
    /// CPU 사용률 %(모든 코어를 합해 100)
    pub cpu: Option<f64>,
    pub mem_used_mib: Option<u64>,
    pub mem_total_mib: Option<u64>,
    /// 실제 디스크의 읽기 바이트/초(macOS는 `null`)
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    /// 실제 NIC의 받기 바이트/초
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
    /// 데이터 디렉터리가 있는 파일 시스템
    pub fs_used_mib: Option<u64>,
    pub fs_total_mib: Option<u64>,
}

/// VM 점 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmSample {
    /// 측정 시각(그 노드 벽시계의 유닉스 밀리초)
    pub t: i64,
    /// vCPU 대비 사용률 %(vCPU가 모두 바쁘면 100). QEMU 프로세스 전체라 100을 조금 넘을 수 있다.
    pub cpu: Option<f64>,
    /// QEMU 프로세스의 RSS
    pub mem_rss_mib: Option<u64>,
    /// 모든 디스크를 합한 값
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    pub disk_read_iops: Option<f64>,
    pub disk_write_iops: Option<f64>,
    /// 게스트 기준 받기 바이트/초(tap이 없으면 `null`)
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

/// 한 노드에 묻는 지표
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetricsQuery {
    pub scope: MetricsScope,
    /// 이 `t`보다 뒤의 점만(그 노드 시계 기준). 없으면 남은 것 모두.
    pub since: Option<i64>,
}

/// 무엇의 지표인가
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricsScope {
    /// 그 노드 자신
    Nodes,
    /// 그 노드에서 잰 VM 하나
    Vm { id: Uuid },
}

/// 한 노드의 점들(시각 순)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeSeries {
    pub node: Uuid,
    pub samples: Vec<NodeSample>,
}

/// 한 노드에서 잰 VM 하나의 점들(시각 순)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmSeries {
    pub vm: Uuid,
    /// 잰 노드
    pub node: Uuid,
    pub samples: Vec<VmSample>,
}

/// `GET /metrics/nodes`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeMetricsResponse {
    /// 읽은 노드마다 하나(노드 이름 순)
    pub nodes: Vec<NodeSeries>,
    /// 답하지 않은 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 지표를 모르는 옛 노드의 이름(정렬)
    pub unsupported: Vec<String>,
}

/// `GET /vms/{key}/metrics`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmMetricsResponse {
    pub vm: Uuid,
    /// 모든 노드에서 잰 점을 시각 순으로 합친 것(마이그레이션 앞뒤가 이어진다)
    pub samples: Vec<VmSample>,
    /// 답하지 않은 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 지표를 모르는 옛 노드의 이름(정렬)
    pub unsupported: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_tagged_by_kind() {
        let id = Uuid::now_v7();
        let q = MetricsQuery {
            scope: MetricsScope::Vm { id },
            since: Some(5),
        };
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"scope": {"kind": "vm", "id": id}, "since": 5})
        );
        assert_eq!(serde_json::from_value::<MetricsQuery>(json).unwrap(), q);
        let nodes = serde_json::json!({"scope": {"kind": "nodes"}, "since": null});
        assert_eq!(
            serde_json::from_value::<MetricsQuery>(nodes).unwrap(),
            MetricsQuery {
                scope: MetricsScope::Nodes,
                since: None
            }
        );
    }

    /// 읽지 못한 값은 빼지 않고 `null`로 보낸다(웹이 차트를 끊어 그린다).
    #[test]
    fn missing_values_are_null() {
        let series = VmSeries {
            vm: Uuid::now_v7(),
            node: Uuid::now_v7(),
            samples: vec![VmSample {
                t: 1_000,
                cpu: Some(12.5),
                mem_rss_mib: Some(512),
                disk_read_bps: None,
                disk_write_bps: None,
                disk_read_iops: None,
                disk_write_iops: None,
                net_rx_bps: Some(0.0),
                net_tx_bps: None,
            }],
        };
        let json = serde_json::to_value(&series).unwrap();
        assert_eq!(json["samples"][0]["cpu"], 12.5);
        assert_eq!(json["samples"][0]["disk_read_bps"], serde_json::Value::Null);
        assert_eq!(json["samples"][0]["net_rx_bps"], 0.0);
        assert_eq!(serde_json::from_value::<VmSeries>(json).unwrap(), series);
    }
}
