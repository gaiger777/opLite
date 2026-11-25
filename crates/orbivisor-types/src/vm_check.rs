//! VM 만들기 미리 확인(계획 71 §9): `POST /vm-checks`의 응답. 만들기(`POST /vms`)와 같은 검사를 쓰지 않고 돌려 문제를
//! 자리(빌더의 칸 — 폼 오류 열쇠와 같은 꼴)마다 모은다. 저장하는 것이 없어 지금 리소스 타입은 그대로다.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::cpu::CpuModel;
use crate::vm::MachineType;

/// 문제의 무게
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum VmCheckLevel {
    /// 만들기가 거절한다.
    Error,
    /// 만들기는 받지만 알아 둘 것(예: 풀 남은 용량이 새 디스크보다 적음 — 씬 풀은 넘칠 수 있다)
    Warn,
}

/// 미리 확인이 찾은 문제 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmCheckProblem {
    /// 자리: `name`, `compute`, `disks`·`disks.N`, `cdroms.N`, `nics`·`nics.N`, `cloudInit`, `placement`, `protection`,
    /// `replication`, `quota`, `spec`(그 밖)
    pub at: String,
    /// 만들기가 낼 오류의 `code`(예: `forbidden`, `quota_exceeded`, `no_node`), 경고는 `pool_space`
    pub code: String,
    pub message: String,
    pub level: VmCheckLevel,
}

/// 미리 확인의 답. 문제가 있어도 200이다(문제 목록이 답).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmCheck {
    /// `problems`에 오류(`error`)가 없다
    pub ok: bool,
    pub problems: Vec<VmCheckProblem>,
    /// 켜진 채 노드 없이 만들면 스케줄러가 고를 노드(노드를 준 요청은 그 노드). 통과하지 못했거나 꺼진 채 노드 없이
    /// 만들거나 노드 이름을 볼 수 없으면(루트 `cluster.read` 없음) null
    pub node: Option<String>,
    /// 채운 머신(통과했을 때만)
    pub machine: Option<MachineType>,
    /// 별칭을 버전 모델로 고정한 CPU 모델(통과했을 때만)
    #[cfg_attr(feature = "openapi", schema(value_type = Option<String>))]
    pub cpu_model: Option<CpuModel>,
    /// NIC마다 받을 SDN 주소(예상 — 잡아 두지 않는다). 통과하지 못했으면 빈 목록
    #[cfg_attr(feature = "openapi", schema(value_type = Vec<Vec<String>>))]
    pub ips: Vec<Vec<IpAddr>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_round_trips_with_kebab_levels() {
        let check = VmCheck {
            ok: false,
            problems: vec![VmCheckProblem {
                at: "disks.0".into(),
                code: "pool_space".into(),
                message: "m".into(),
                level: VmCheckLevel::Warn,
            }],
            node: None,
            machine: Some(MachineType::Virt),
            cpu_model: Some(CpuModel::Host),
            ips: vec![vec!["10.0.0.5".parse().unwrap()]],
        };
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(json["problems"][0]["level"], "warn");
        assert_eq!(json["machine"], "virt");
        assert_eq!(json["cpu_model"], "host");
        assert_eq!(json["ips"], serde_json::json!([["10.0.0.5"]]));
        assert_eq!(serde_json::from_value::<VmCheck>(json).unwrap(), check);
    }
}
