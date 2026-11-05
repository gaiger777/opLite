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
