//! 게스트 에이전트(QGA)가 알려 준 게스트 정보(계획 59). VM이 도는 노드의 메모리에만 두고 API가 읽는다. 게스트가
//! 보낸 값은 믿지 않으므로 [`GuestInfo::truncate`]로 수와 길이를 자른다.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 게스트 정보 하나에 담는 NIC 수의 상한
pub const MAX_GUEST_NICS: usize = 64;
/// NIC 하나에 담는 주소 수의 상한
pub const MAX_GUEST_ADDRESSES: usize = 32;
/// 담는 파일 시스템 수의 상한
pub const MAX_GUEST_FILESYSTEMS: usize = 64;
/// 게스트가 보낸 문자열 하나의 상한(바이트). 넘으면 글자 경계에서 자른다.
pub const MAX_GUEST_STRING: usize = 256;

/// 게스트 비밀번호 설정의 사용자 이름 길이 상한(바이트)
pub const MAX_GUEST_USER: usize = 64;
/// 게스트 비밀번호 길이 상한(바이트)
pub const MAX_GUEST_PASSWORD: usize = 256;

/// 게스트 에이전트에서 모은 게스트 정보. 실패한 명령(옛 에이전트, freeze 중)의 칸은 비어 있다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestInfo {
    /// 모은 시각(유닉스 초)
    pub collected_at: u64,
    /// 에이전트 버전(`guest-info`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub agent_version: Option<String>,
    /// 게스트 OS(`guest-get-osinfo`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub os: Option<GuestOs>,
    /// 호스트 이름(`guest-get-host-name`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub hostname: Option<String>,
    /// NIC와 주소(`guest-network-get-interfaces`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub interfaces: Vec<GuestNic>,
    /// 파일 시스템(`guest-get-fsinfo`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub filesystems: Vec<GuestFs>,
}

/// 게스트 OS(`guest-get-osinfo`의 `id`·`pretty-name`·`version`·`kernel-release`)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestOs {
    /// 예: `ubuntu`, `mswindows`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub id: Option<String>,
    /// 예: `Ubuntu 24.04.1 LTS`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub kernel: Option<String>,
}

/// 게스트의 NIC 하나
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestNic {
    pub name: String,
    /// 하드웨어 주소(게스트가 보낸 문자열 그대로)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub mac: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub addresses: Vec<GuestAddr>,
}

/// NIC의 IP 주소 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestAddr {
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = "ip"))]
    pub ip: IpAddr,
    pub prefix: u8,
}

/// 게스트의 파일 시스템 하나. 크기는 에이전트가 알려 줄 때만 있다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestFs {
    pub mountpoint: String,
    pub fstype: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub total_bytes: Option<u64>,
}

impl GuestInfo {
    /// 게스트가 보낸 값을 상한으로 자른다: NIC [`MAX_GUEST_NICS`], NIC마다 주소 [`MAX_GUEST_ADDRESSES`], 파일 시스템
    /// [`MAX_GUEST_FILESYSTEMS`], 문자열 [`MAX_GUEST_STRING`]바이트.
    pub fn truncate(&mut self) {
        for s in [&mut self.agent_version, &mut self.hostname]
            .into_iter()
            .flatten()
        {
            truncate_str(s);
        }
        if let Some(os) = &mut self.os {
            for s in [&mut os.id, &mut os.name, &mut os.version, &mut os.kernel]
                .into_iter()
                .flatten()
            {
                truncate_str(s);
            }
        }
        self.interfaces.truncate(MAX_GUEST_NICS);
        for nic in &mut self.interfaces {
            truncate_str(&mut nic.name);
            if let Some(mac) = &mut nic.mac {
                truncate_str(mac);
            }
            nic.addresses.truncate(MAX_GUEST_ADDRESSES);
        }
        self.filesystems.truncate(MAX_GUEST_FILESYSTEMS);
        for fs in &mut self.filesystems {
            truncate_str(&mut fs.mountpoint);
            truncate_str(&mut fs.fstype);
        }
    }
}

/// 문자열을 [`MAX_GUEST_STRING`]바이트 이하로 글자 경계에서 자른다.
fn truncate_str(s: &mut String) {
    if s.len() > MAX_GUEST_STRING {
        let mut end = MAX_GUEST_STRING;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

/// 게스트 정보 읽기의 결과(`GET /vms/{key}/guest`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuestInfoState {
    /// 에이전트가 답했다(`info`가 있다).
    Answered,
    /// VM에 에이전트 채널이 없거나 에이전트가 아직 답한 적이 없다.
    NoAgent,
    /// VM이 돌고 있지 않다.
    NotRunning,
    /// VM이 도는 노드에 닿지 못했다.
    Unreachable,
    /// VM이 도는 노드가 게스트 에이전트 기능을 모른다(옛 노드).
    Unsupported,
}

/// 게스트 정보 읽기의 응답
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestInfoResponse {
    pub state: GuestInfoState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub info: Option<GuestInfo>,
}

/// 게스트 비밀번호 설정 요청의 잘못(문구에 비밀번호를 넣지 않는다)
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GuestPasswordError {
    #[error("user must be 1 to {MAX_GUEST_USER} bytes without control characters")]
    User,
    #[error("password must be 1 to {MAX_GUEST_PASSWORD} bytes")]
    Password,
}

/// 게스트 비밀번호 설정(`guest-set-user-password`)의 사용자 이름과 비밀번호를 검사한다. 사용자 이름은 1~
/// [`MAX_GUEST_USER`]바이트이고 제어 문자가 없다. 비밀번호는 1~[`MAX_GUEST_PASSWORD`]바이트다.
pub fn validate_guest_password(user: &str, password: &str) -> Result<(), GuestPasswordError> {
    if user.is_empty() || user.len() > MAX_GUEST_USER || user.chars().any(char::is_control) {
        return Err(GuestPasswordError::User);
    }
    if password.is_empty() || password.len() > MAX_GUEST_PASSWORD {
        return Err(GuestPasswordError::Password);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_round_trips_and_leaves_out_empty_fields() {
        let empty = GuestInfo {
            collected_at: 5,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            serde_json::json!({ "collected_at": 5 })
        );
        let info = GuestInfo {
            collected_at: 1_790_000_000,
            agent_version: Some("8.2.2".into()),
            os: Some(GuestOs {
                id: Some("ubuntu".into()),
                name: Some("Ubuntu 24.04.1 LTS".into()),
                version: Some("24.04".into()),
                kernel: None,
            }),
            hostname: Some("web".into()),
            interfaces: vec![GuestNic {
                name: "eth0".into(),
                mac: Some("52:54:00:12:34:56".into()),
                addresses: vec![
                    GuestAddr {
                        ip: "10.0.2.15".parse().unwrap(),
                        prefix: 24,
                    },
                    GuestAddr {
                        ip: "fe80::1".parse().unwrap(),
                        prefix: 64,
                    },
                ],
            }],
            filesystems: vec![GuestFs {
                mountpoint: "/".into(),
                fstype: "ext4".into(),
                used_bytes: Some(1 << 30),
                total_bytes: Some(10 << 30),
            }],
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["interfaces"][0]["addresses"][1]["ip"], "fe80::1");
        assert!(json["os"].get("kernel").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<GuestInfo>(json).unwrap(), info);

        let response = GuestInfoResponse {
            state: GuestInfoState::NoAgent,
            info: None,
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({ "state": "no_agent" })
        );
    }

    #[test]
    fn truncate_caps_counts_and_string_lengths() {
        let long = "가".repeat(200); // 600바이트, 3바이트 글자
        let addr = GuestAddr {
            ip: "10.0.0.1".parse().unwrap(),
            prefix: 8,
        };
        let mut info = GuestInfo {
            collected_at: 1,
            agent_version: Some("x".repeat(300)),
            os: Some(GuestOs {
                name: Some(long.clone()),
                ..Default::default()
            }),
            hostname: Some(long.clone()),
            interfaces: (0..100)
                .map(|i| GuestNic {
                    name: format!("eth{i}"),
                    mac: Some(long.clone()),
                    addresses: vec![addr; 50],
                })
                .collect(),
            filesystems: (0..100)
                .map(|_| GuestFs {
                    mountpoint: long.clone(),
                    fstype: "ext4".into(),
                    ..Default::default()
                })
                .collect(),
        };
        info.truncate();
        assert_eq!(info.agent_version.as_ref().unwrap().len(), MAX_GUEST_STRING);
        // 글자 경계(3바이트)에서 자른다.
        assert_eq!(info.hostname.as_ref().unwrap().len(), 255);
        assert_eq!(info.os.as_ref().unwrap().name.as_ref().unwrap().len(), 255);
        assert_eq!(info.interfaces.len(), MAX_GUEST_NICS);
        assert_eq!(info.interfaces[63].name, "eth63");
        assert!(
            info.interfaces
                .iter()
                .all(|n| n.addresses.len() == MAX_GUEST_ADDRESSES
                    && n.mac.as_ref().unwrap().len() <= MAX_GUEST_STRING)
        );
        assert_eq!(info.filesystems.len(), MAX_GUEST_FILESYSTEMS);
        assert!(
            info.filesystems
                .iter()
                .all(|f| f.mountpoint.len() <= MAX_GUEST_STRING)
        );
    }

    #[test]
    fn password_request_is_checked_without_echoing_the_password() {
        assert_eq!(validate_guest_password("Administrator", "s3cret"), Ok(()));
        assert_eq!(
            validate_guest_password("", "x"),
            Err(GuestPasswordError::User)
        );
        assert_eq!(
            validate_guest_password(&"u".repeat(65), "x"),
            Err(GuestPasswordError::User)
        );
        assert_eq!(
            validate_guest_password("root\n", "x"),
            Err(GuestPasswordError::User)
        );
        assert_eq!(
            validate_guest_password("root", ""),
            Err(GuestPasswordError::Password)
        );
        let long = "p".repeat(257);
        let err = validate_guest_password("root", &long).unwrap_err();
        assert!(!err.to_string().contains(&long));
        assert_eq!(validate_guest_password("root", &"p".repeat(256)), Ok(()));
    }
}
