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
