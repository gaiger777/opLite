//! 클러스터 설정. 클러스터에 하나만 있고 이름은 언제나 [`SETTINGS_NAME`]이다. 리소스가 없으면
//! 기본값으로 보고, 처음 바꿀 때 만든다.

use serde::{Deserialize, Serialize};

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
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Testing => "testing",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SettingsStatus {}

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
        for channel in [Channel::Stable, Channel::Testing] {
            assert_eq!(serde_json::to_value(channel).unwrap(), channel.as_str());
        }
        let spec: SettingsSpec =
            serde_json::from_str(r#"{"updates":{"channel":"testing"}}"#).unwrap();
        assert_eq!(spec.updates.channel, Channel::Testing);
        assert!(serde_json::from_str::<Channel>(r#""Testing""#).is_err());
        assert!(serde_json::from_str::<Channel>(r#""enterprise""#).is_err());
    }

    /// 구독이 없으면 계획 32 노드가 쓰던 모양 그대로 쓰고, 있으면 함께 쓰고 푼다.
    #[test]
    fn subscription_is_written_only_when_set() {
        let spec = SettingsSpec {
            updates: UpdateSettings {
                channel: Channel::Testing,
            },
            subscription: None,
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

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"telemetry":{}}"#).is_err());
        assert!(serde_json::from_str::<SettingsSpec>(r#"{"updates":{"mirror":"x"}}"#).is_err());
        assert!(serde_json::from_str::<SettingsStatus>(r#"{"x":1}"#).is_err());
    }

    #[test]
    fn settings_kind_is_settings() {
        assert_eq!(SettingsSpec::KIND, Kind::Settings);
        assert_eq!(serde_json::to_value(Kind::Settings).unwrap(), "settings");
    }
}
