//! 클러스터 설정. 클러스터에 하나만 있고 이름은 언제나 [`SETTINGS_NAME`]이다. 리소스가 없으면
//! 기본값으로 보고, 처음 바꿀 때 만든다.

use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

pub type Settings = Resource<SettingsSpec, SettingsStatus>;

/// 설정 리소스의 이름
pub const SETTINGS_NAME: &str = "cluster";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingsSpec {
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub updates: UpdateSettings,
    /// 구독 키 문자열. API는 내보내지 않고 `GET /subscription`이 상태만 보인다. 없으면 쓰지 않아
    /// 구독을 모르는 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(ignore))]
    pub subscription: Option<String>,
    /// 옵트인 텔레메트리. 기본값(꺼짐, 끝점 없음)이면 쓰지 않아 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "TelemetrySettings::is_default")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub telemetry: TelemetrySettings,
    /// 쿠키 요청을 받는 다른 출처(`https://portal.example.com`). 따로 띄운 웹 포털이 API를 부를 때 쓴다.
    /// 비어 있으면 같은 출처만 받고, 쓰지 않아 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub web_origins: Vec<String>,
    /// 앞단·VIP에서 API를 부르는 이름(DNS 이름·IP). 각 노드가 자기 API 인증서에 더한다(피어 인증서는 그대로).
    /// 비어 있으면 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub api_names: Vec<String>,
    /// 투표 멤버 상한: 3, 5, 7 가운데 하나. 없으면 5이고, 쓰지 않아 옛 노드도 설정을 계속 푼다. 위트니스도
    /// 센다. 노드가 이보다 적으면 모두 투표 멤버다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(nullable = false, default, minimum = 3, maximum = 7)
    )]
    pub voters: Option<u8>,
    /// 호스트가 꺼지는 중(재부팅·전원 끄기)일 때 노드가 HA VM을 다루는 방법. 없으면 `conditional`이고, 쓰지 않아
    /// 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub shutdown_policy: Option<ShutdownPolicy>,
    /// HA 복구를 위해 비워 두는 용량. 없으면 예약하지 않고(`none`), 쓰지 않아 옛 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ha_reserve: Option<HaReserve>,
    /// 리더가 HA 복구 때 대상 노드 하나에 한꺼번에 옮겨 켜는 VM 수(1~32). 없으면 4이고 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "openapi",
        schema(nullable = false, default, minimum = 1, maximum = 32)
    )]
    pub ha_start_concurrency: Option<u8>,
    /// 최적 배치(계획 53): 점수 정책과 균형 조정기. 없으면 기본값(`balance`, `recommend`, 15%)이고 쓰지 않아 옛
    /// 노드도 설정을 계속 푼다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub placement: Option<PlacementSettings>,
}

/// 최적 배치 설정. 모든 값이 기본이면 [`SettingsSpec::placement`]를 쓰지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PlacementSettings {
    /// 점수 정책
    #[serde(default, skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub policy: PlacementPolicy,
    /// 균형 조정기의 동작
    #[serde(default, skip_serializing_if = "is_default")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub mode: PlacementMode,
    /// 불균형 문턱(정수 백분율 1~50). 노드 CPU·메모리 사용률 표준편차의 평균이 이것을 넘으면 옮기기를 찾는다.
    #[serde(
        default = "default_placement_threshold",
        skip_serializing_if = "is_default_threshold"
    )]
    #[cfg_attr(feature = "openapi", schema(default = 15, minimum = 1, maximum = 50))]
    pub threshold: u8,
}

impl Default for PlacementSettings {
    fn default() -> Self {
        PlacementSettings {
            policy: PlacementPolicy::default(),
            mode: PlacementMode::default(),
            threshold: DEFAULT_PLACEMENT_THRESHOLD,
        }
    }
}

/// 불균형 문턱의 기본값과 최댓값(백분율)
pub const DEFAULT_PLACEMENT_THRESHOLD: u8 = 15;
pub const MAX_PLACEMENT_THRESHOLD: u8 = 50;

fn default_placement_threshold() -> u8 {
    DEFAULT_PLACEMENT_THRESHOLD
}

fn is_default_threshold(threshold: &u8) -> bool {
    *threshold == DEFAULT_PLACEMENT_THRESHOLD
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

impl PlacementSettings {
    /// 범위(문턱 1~50) 안인지
    pub fn valid(&self) -> bool {
        (1..=MAX_PLACEMENT_THRESHOLD).contains(&self.threshold)
    }
}

/// 배치 점수 정책
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum PlacementPolicy {
    /// CPU·메모리 여유가 많은 노드에 고르게 놓는다.
    #[default]
    Balance,
    /// 넘치지 않는 한 많이 쓴 노드에 모은다.
    Pack,
}

impl PlacementPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            PlacementPolicy::Balance => "balance",
            PlacementPolicy::Pack => "pack",
        }
    }
}

/// 균형 조정기의 동작
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum PlacementMode {
    /// 계산하지 않는다.
    Off,
    /// 옮기기를 추천만 한다.
    #[default]
    Recommend,
    /// 스스로 라이브 마이그레이션한다.
    Auto,
}

impl PlacementMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PlacementMode::Off => "off",
            PlacementMode::Recommend => "recommend",
            PlacementMode::Auto => "auto",
        }
    }
}

/// HA 용량 예약: HA 가능 노드의 메모리 가운데 복구 자리로 비워 두는 몫.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum HaReserve {
    /// 메모리가 가장 큰 노드 N대(1~8)만큼
    Nodes(u8),
    /// 전체 메모리의 P%(1~90)
    Percent(u8),
}

/// HA 용량 예약의 범위
pub const MAX_HA_RESERVE_NODES: u8 = 8;
pub const MAX_HA_RESERVE_PERCENT: u8 = 90;

impl HaReserve {
    /// 범위(노드 1~8, 백분율 1~90) 안인지
    pub fn valid(self) -> bool {
        match self {
            HaReserve::Nodes(n) => (1..=MAX_HA_RESERVE_NODES).contains(&n),
            HaReserve::Percent(p) => (1..=MAX_HA_RESERVE_PERCENT).contains(&p),
        }
    }
}

/// 대상 노드당 동시 시작 수의 기본값과 최댓값
pub const DEFAULT_HA_START_CONCURRENCY: u8 = 4;
pub const MAX_HA_START_CONCURRENCY: u8 = 32;

/// 노드 정상 종료 정책(설계 §8). `systemctl stop`·업그레이드 재시작이 아니라 호스트가 꺼지는 중일 때만 쓴다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum ShutdownPolicy {
    /// 재부팅이면 `freeze`, 전원 끄기면 `failover`
    #[default]
    Conditional,
    /// HA VM을 끄고 그 노드에 묶어 둔다(돌아오면 그 노드에서 다시 켠다).
    Freeze,
    /// HA VM을 끄고 리더가 다른 노드에서 켠다.
    Failover,
    /// 비우기로 VM을 미리 다른 노드로 옮긴다(남은 HA VM은 `failover`처럼).
    Migrate,
}

impl ShutdownPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ShutdownPolicy::Conditional => "conditional",
            ShutdownPolicy::Freeze => "freeze",
            ShutdownPolicy::Failover => "failover",
            ShutdownPolicy::Migrate => "migrate",
        }
    }
}

/// 투표 멤버 상한의 기본값
pub const DEFAULT_VOTERS: u8 = 5;
/// 고를 수 있는 투표 멤버 상한(홀수라 표가 갈리지 않는다)
pub const VOTER_LIMITS: [u8; 3] = [3, 5, 7];

impl SettingsSpec {
    /// 투표 멤버 상한. 없거나 고를 수 없는 값이면 기본값이다.
    pub fn voter_limit(&self) -> usize {
        let n = self.voters.filter(|n| VOTER_LIMITS.contains(n));
        usize::from(n.unwrap_or(DEFAULT_VOTERS))
    }

    /// 노드 정상 종료 정책. 없으면 `conditional`이다.
    pub fn shutdown_policy(&self) -> ShutdownPolicy {
        self.shutdown_policy.unwrap_or_default()
    }

    /// 대상 노드당 동시 시작 수. 없거나 범위 밖이면 기본값(4)이다.
    pub fn ha_start_concurrency(&self) -> usize {
        let n = self
            .ha_start_concurrency
            .filter(|n| (1..=MAX_HA_START_CONCURRENCY).contains(n));
        usize::from(n.unwrap_or(DEFAULT_HA_START_CONCURRENCY))
    }

    /// 최적 배치 설정. 없거나 범위 밖이면 기본값이다.
    pub fn placement(&self) -> PlacementSettings {
        self.placement.filter(|p| p.valid()).unwrap_or_default()
    }
}

impl ResourceSpec for SettingsSpec {
    const KIND: Kind = Kind::Settings;
    type Status = SettingsStatus;
}

/// 노드 업데이트 설정
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct UpdateSettings {
    /// 노드가 패키지를 받는 APT 저장소 채널
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub channel: Channel,
}

/// 업데이트 채널. APT 소스 파일의 `Components`가 된다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// 검증을 마친 릴리스
    #[default]
    Stable,
    /// 미리 써 보는 릴리스
    Testing,
    /// 구독한 클러스터용 릴리스. 고르려면 구독이 active여야 하고, 저장소는 구독 키로 인증한다.
    Enterprise,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Testing => "testing",
            Channel::Enterprise => "enterprise",
        }
    }
}

/// 텔레메트리 설정. 켜면 리더가 하루 한 번 클러스터 규모(이름·주소 없음)를 끝점에 보낸다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TelemetrySettings {
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub enabled: bool,
    /// 보낼 `https://` 주소. 없으면 빌드 값(`version::TELEMETRY_URL`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub endpoint: Option<String>,
}

impl TelemetrySettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingsStatus {
    /// 텔레메트리 상태. 처음 켤 때 생기고, 없으면 쓰지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub telemetry: Option<TelemetryStatus>,
}

/// 텔레메트리 상태
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TelemetryStatus {
    /// 보고에 붙는 무작위 id(UUID v4). 처음 켤 때 만들고, 꺼도 남는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub id: Option<Uuid>,
    /// 마지막으로 보낸 시각(유닉스 초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_sent_at: Option<u64>,
    /// 마지막 보내기가 실패한 이유
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_spec_means_the_stable_channel() {
        let spec: SettingsSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(spec, SettingsSpec::default());
        assert_eq!(spec.updates.channel, Channel::Stable);
        let spec: SettingsSpec = serde_json::from_str(r#"{"updates":{}}"#).unwrap();
        assert_eq!(spec.updates.channel, Channel::Stable);
        assert_eq!(
            serde_json::to_value(SettingsSpec::default()).unwrap(),
            serde_json::json!({ "updates": { "channel": "stable" } })
        );
    }

    #[test]
    fn channels_are_lowercase() {
        for channel in [Channel::Stable, Channel::Testing, Channel::Enterprise] {
            assert_eq!(serde_json::to_value(channel).unwrap(), channel.as_str());
        }
        let spec: SettingsSpec =
            serde_json::from_str(r#"{"updates":{"channel":"testing"}}"#).unwrap();
        assert_eq!(spec.updates.channel, Channel::Testing);
        let spec: SettingsSpec =
            serde_json::from_str(r#"{"updates":{"channel":"enterprise"}}"#).unwrap();
        assert_eq!(spec.updates.channel, Channel::Enterprise);
        assert_eq!(Channel::Enterprise.as_str(), "enterprise");
        assert!(serde_json::from_str::<Channel>(r#""Testing""#).is_err());
        assert!(serde_json::from_str::<Channel>(r#""nightly""#).is_err());
    }

    /// 구독이 없으면 계획 32 노드가 쓰던 모양 그대로 쓰고, 있으면 함께 쓰고 푼다.
    #[test]
    fn subscription_is_written_only_when_set() {
        let spec = SettingsSpec {
            updates: UpdateSettings {
                channel: Channel::Testing,
            },
            subscription: None,
            telemetry: TelemetrySettings::default(),
            web_origins: vec![],
            api_names: vec![],
            voters: None,
            shutdown_policy: None,
            ha_reserve: None,
            ha_start_concurrency: None,
            placement: None,
        };
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            serde_json::json!({ "updates": { "channel": "testing" } })
        );
        let spec = SettingsSpec {
            subscription: Some("opl1.a.b".into()),
            ..spec
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "updates": { "channel": "testing" }, "subscription": "opl1.a.b" })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
    }

    /// 허용 출처도 비어 있으면 쓰지 않는다.
    #[test]
    fn web_origins_are_written_only_when_set() {
        assert_eq!(
            serde_json::to_value(SettingsSpec::default()).unwrap(),
            serde_json::json!({ "updates": { "channel": "stable" } })
        );
        let spec = SettingsSpec {
            web_origins: vec!["https://portal.example.com".into()],
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["web_origins"],
            serde_json::json!(["https://portal.example.com"])
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
    }

    /// API 이름도 비어 있으면 쓰지 않는다.
    #[test]
    fn api_names_are_written_only_when_set() {
        let spec = SettingsSpec {
            api_names: vec!["orbivisor.example.com".into(), "192.168.0.200".into()],
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["api_names"],
            serde_json::json!(["orbivisor.example.com", "192.168.0.200"])
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
        assert!(
            serde_json::to_value(SettingsSpec::default())
                .unwrap()
                .get("api_names")
                .is_none()
        );
    }

    /// 투표 멤버 상한도 없으면 쓰지 않고, 없거나 고를 수 없는 값이면 5다.
    #[test]
    fn voters_are_written_only_when_set() {
        assert_eq!(SettingsSpec::default().voter_limit(), 5);
        let spec = SettingsSpec {
            voters: Some(3),
            ..SettingsSpec::default()
        };
        assert_eq!(spec.voter_limit(), 3);
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["voters"], 3);
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
        assert!(
            serde_json::to_value(SettingsSpec::default())
                .unwrap()
                .get("voters")
                .is_none()
        );
        let odd = SettingsSpec {
            voters: Some(4),
            ..SettingsSpec::default()
        };
        assert_eq!(odd.voter_limit(), 5);
    }

    /// 종료 정책도 없으면 쓰지 않고 `conditional`이다. 값은 소문자다.
    #[test]
    fn shutdown_policy_is_written_only_when_set() {
        assert_eq!(
            SettingsSpec::default().shutdown_policy(),
            ShutdownPolicy::Conditional
        );
        assert!(
            serde_json::to_value(SettingsSpec::default())
                .unwrap()
                .get("shutdown_policy")
                .is_none()
        );
        for policy in [
            ShutdownPolicy::Conditional,
            ShutdownPolicy::Freeze,
            ShutdownPolicy::Failover,
            ShutdownPolicy::Migrate,
        ] {
            let spec = SettingsSpec {
                shutdown_policy: Some(policy),
                ..SettingsSpec::default()
            };
            let json = serde_json::to_value(&spec).unwrap();
            assert_eq!(json["shutdown_policy"], policy.as_str());
            let back = serde_json::from_value::<SettingsSpec>(json).unwrap();
            assert_eq!(back.shutdown_policy(), policy);
        }
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"shutdown_policy":"Freeze"}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"shutdown_policy":"ignore"}"#).is_err());
    }

    /// HA 용량 예약·동시 시작도 없으면 쓰지 않는다. 예약은 `{"nodes": N}`이나 `{"percent": P}`다.
    #[test]
    fn ha_reserve_and_start_concurrency_are_written_only_when_set() {
        let none = serde_json::to_value(SettingsSpec::default()).unwrap();
        assert!(none.get("ha_reserve").is_none(), "{none}");
        assert!(none.get("ha_start_concurrency").is_none(), "{none}");
        assert_eq!(SettingsSpec::default().ha_start_concurrency(), 4);

        let spec = SettingsSpec {
            ha_reserve: Some(HaReserve::Nodes(1)),
            ha_start_concurrency: Some(2),
            ..SettingsSpec::default()
        };
        assert_eq!(spec.ha_start_concurrency(), 2);
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["ha_reserve"], serde_json::json!({ "nodes": 1 }));
        assert_eq!(json["ha_start_concurrency"], 2);
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);

        let spec: SettingsSpec = serde_json::from_str(r#"{"ha_reserve":{"percent":25}}"#).unwrap();
        assert_eq!(spec.ha_reserve, Some(HaReserve::Percent(25)));
        for bad in [
            r#"{"ha_reserve":"none"}"#,
            r#"{"ha_reserve":{"nodes":1,"percent":2}}"#,
            r#"{"ha_reserve":{"hosts":1}}"#,
        ] {
            assert!(serde_json::from_str::<SettingsSpec>(bad).is_err(), "{bad}");
        }

        assert!(HaReserve::Nodes(1).valid() && HaReserve::Nodes(8).valid());
        assert!(!HaReserve::Nodes(0).valid() && !HaReserve::Nodes(9).valid());
        assert!(HaReserve::Percent(1).valid() && HaReserve::Percent(90).valid());
        assert!(!HaReserve::Percent(0).valid() && !HaReserve::Percent(91).valid());
        let odd = SettingsSpec {
            ha_start_concurrency: Some(0),
            ..SettingsSpec::default()
        };
        assert_eq!(odd.ha_start_concurrency(), 4);
    }

    /// 최적 배치 설정도 없으면 쓰지 않는다. 기본값인 필드는 빼고 쓰고, 빠진 필드는 기본값이며, 모르는 키는 거절한다.
    #[test]
    fn placement_is_written_only_when_set() {
        let none = serde_json::to_value(SettingsSpec::default()).unwrap();
        assert!(none.get("placement").is_none(), "{none}");
        assert_eq!(
            SettingsSpec::default().placement(),
            PlacementSettings::default()
        );
        let d = PlacementSettings::default();
        assert_eq!(
            (d.policy, d.mode, d.threshold),
            (PlacementPolicy::Balance, PlacementMode::Recommend, 15)
        );
        assert_eq!(serde_json::to_value(d).unwrap(), serde_json::json!({}));

        let spec = SettingsSpec {
            placement: Some(PlacementSettings {
                policy: PlacementPolicy::Pack,
                mode: PlacementMode::Auto,
                threshold: 20,
            }),
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["placement"],
            serde_json::json!({ "policy": "pack", "mode": "auto", "threshold": 20 })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
        assert_eq!(spec.placement().threshold, 20);

        let spec: SettingsSpec = serde_json::from_str(r#"{"placement":{"mode":"off"}}"#).unwrap();
        assert_eq!(
            spec.placement(),
            PlacementSettings {
                mode: PlacementMode::Off,
                ..PlacementSettings::default()
            }
        );
        for bad in [
            r#"{"placement":{"interval":300}}"#,
            r#"{"placement":{"policy":"spread"}}"#,
            r#"{"placement":{"mode":"Auto"}}"#,
            r#"{"placement":{"threshold":0.15}}"#,
        ] {
            assert!(serde_json::from_str::<SettingsSpec>(bad).is_err(), "{bad}");
        }
        // 범위 밖 문턱은 기본값으로 본다.
        let odd = SettingsSpec {
            placement: Some(PlacementSettings {
                threshold: 51,
                ..PlacementSettings::default()
            }),
            ..SettingsSpec::default()
        };
        assert_eq!(odd.placement(), PlacementSettings::default());
        for policy in [PlacementPolicy::Balance, PlacementPolicy::Pack] {
            assert_eq!(serde_json::to_value(policy).unwrap(), policy.as_str());
        }
        for mode in [
            PlacementMode::Off,
            PlacementMode::Recommend,
            PlacementMode::Auto,
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), mode.as_str());
        }
    }

    /// 텔레메트리도 기본값(꺼짐, 끝점 없음, 상태 없음)이면 계획 32 모양 그대로 쓴다.
    #[test]
    fn telemetry_is_written_only_when_set() {
        let spec = SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: false,
                endpoint: None,
            },
            ..SettingsSpec::default()
        };
        assert_eq!(
            serde_json::to_value(&spec).unwrap(),
            serde_json::json!({ "updates": { "channel": "stable" } })
        );
        assert_eq!(
            serde_json::to_value(SettingsStatus::default()).unwrap(),
            serde_json::json!({})
        );

        let spec = SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: true,
                endpoint: None,
            },
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "updates": { "channel": "stable" }, "telemetry": { "enabled": true } })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);
        let spec = SettingsSpec {
            telemetry: TelemetrySettings {
                enabled: false,
                endpoint: Some("https://t.example/r".into()),
            },
            ..SettingsSpec::default()
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["telemetry"],
            serde_json::json!({ "enabled": false, "endpoint": "https://t.example/r" })
        );
        assert_eq!(serde_json::from_value::<SettingsSpec>(json).unwrap(), spec);

        let status = SettingsStatus {
            telemetry: Some(TelemetryStatus {
                id: Some(Uuid::nil()),
                last_sent_at: Some(5),
                last_error: None,
            }),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "telemetry": { "id": Uuid::nil(), "last_sent_at": 5 } })
        );
        assert_eq!(
            serde_json::from_value::<SettingsStatus>(json).unwrap(),
            status
        );
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"metrics":{}}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"telemetry":{"interval":1}}"#).is_err());
        assert!(serde_json::from_str::<SettingsStatus>(r#"{"telemetry":{"sent":1}}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"updates":{"mirror":"x"}}"#).is_err());
        assert!(serde_json::from_str::<SettingsStatus>(r#"{"x":1}"#).is_err());
    }

    #[test]
    fn settings_kind_is_settings() {
        assert_eq!(SettingsSpec::KIND, Kind::Settings);
        assert_eq!(serde_json::to_value(Kind::Settings).unwrap(), "settings");
    }
}
