//! vSphere 클라이언트(계획 72 설계 §7): 지문 고정 HTTPS([`http`]), 손으로 쓰는 SOAP 봉투([`soap`]), 세션과 메서드
//! ([`client`]), 인벤토리([`inventory`]), CBT 바뀐 구역([`cbt`]), 디스크 읽기([`reader`] — 데이터스토어 `/folder`
//! 범위 읽기와 선택 nbdkit-vddk). 시험용 가짜 vSphere는 기능 `fake-vsphere`의 [`fake`]다.

pub mod cbt;
pub mod client;
#[cfg(feature = "fake-vsphere")]
pub mod fake;
pub mod http;
pub mod inventory;
pub mod reader;
pub mod soap;

use std::path::PathBuf;

use axum::http::StatusCode;

use crate::error::ApiError;
use soap::Fault;

/// 노드 설정 `[vmware]`(설계 §7.4): 선택 VDDK 길의 nbdkit과 VDDK 라이브러리 자리
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VmwareNodeConfig {
    /// nbdkit 실행 파일(없으면 VDDK 길을 쓸 수 없다)
    pub nbdkit: Option<PathBuf>,
    /// VDDK 라이브러리 디렉터리(원본의 `transport.vddk.libdir`가 없을 때)
    pub vddk_libdir: Option<PathBuf>,
}

impl VmwareNodeConfig {
    /// 원본의 libdir가 먼저, 없으면 노드 설정
    pub fn libdir_for(&self, source_libdir: Option<&str>) -> Option<PathBuf> {
        source_libdir
            .map(PathBuf::from)
            .or_else(|| self.vddk_libdir.clone())
    }
}

/// vSphere와 이야기하다 실패한 이유. 메시지는 사용자에게 그대로 보인다(비밀번호는 들지 않는다).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VsphereError {
    /// 사용자 이름·비밀번호가 틀렸거나(`InvalidLogin`) 세션이 끝났다(`NotAuthenticated` — 다시 로그인해도 안 될 때)
    #[error("vSphere rejected the login: {message}")]
    Auth { kind: String, message: String },
    /// 원본 쪽 권한이 모자라다(우리 권한과 헷갈리지 않게 400 `vmware_permission`)
    #[error("the vSphere user lacks a privilege{}: {message}", privilege.as_ref().map(|p| format!(" ({p})")).unwrap_or_default())]
    NoPermission {
        privilege: Option<String>,
        message: String,
    },
    /// 지금 상태에서 할 수 없다(`InvalidPowerState`·`InvalidState`·`TaskInProgress` …)
    #[error("vSphere refused in the current state ({kind}): {message}")]
    InvalidState { kind: String, message: String },
    /// 파일 결함(`FileFault` 갈래 — CBT `changeId`가 무효일 때도)
    #[error("vSphere file error ({kind}): {message}")]
    FileFault { kind: String, message: String },
    /// 무료 ESXi(API 읽기 전용)
    #[error("the vSphere host has a read-only API license (free ESXi): {0}")]
    Restricted(String),
    #[error("not found on vSphere ({kind}): {message}")]
    NotFound { kind: String, message: String },
    /// 서버가 메서드나 기능을 모른다(`MethodNotFound`·`NotSupported`)
    #[error("vSphere does not support this ({kind}): {message}")]
    NotSupported { kind: String, message: String },
    #[error("vSphere error {kind}: {message}")]
    Fault { kind: String, message: String },
    #[error("vSphere answered HTTP {0}")]
    Http(u16),
    #[error("cannot connect to vSphere: {0}")]
    Connect(String),
    /// 믿기로 한 지문이 없다(서버 인증서의 지문 둘)
    #[error("the vSphere certificate fingerprint {sha256} is not trusted yet")]
    Untrusted { sha256: String, sha1: String },
    #[error("the vSphere certificate fingerprint is {actual}, expected {expected}")]
    Fingerprint { expected: String, actual: String },
    /// 응답의 꼴이 틀렸다
    #[error("invalid vSphere response: {0}")]
    Invalid(String),
    /// vSphere Task가 한도 안에 끝나지 않았다
    #[error("timed out waiting for {0}")]
    Timeout(String),
}

/// 파일 결함 갈래(`FileFault`의 하위 형식 중 흔한 것)
const FILE_FAULTS: [&str; 12] = [
    "FileFault",
    "CannotAccessFile",
    "FileLocked",
    "FileAlreadyExists",
    "FileTooLarge",
    "FileBackedPortNotSupported",
    "NoDiskSpace",
    "NotADirectory",
    "NotAFile",
    "CannotCreateFile",
    "CannotDeleteFile",
    "GenericVmConfigFault",
];

/// 상태 결함 갈래
const STATE_FAULTS: [&str; 11] = [
    "InvalidState",
    "InvalidPowerState",
    "InvalidHostState",
    "InvalidVmState",
    "TaskInProgress",
    "SnapshotFault",
    "SnapshotDisabled",
    "SnapshotLocked",
    "ToolsUnavailable",
    "ConcurrentAccess",
    "HostNotConnected",
];

impl VsphereError {
    /// SOAP 결함을 나눈다(`detail`의 `xsi:type`으로).
    pub fn from_fault(f: Fault) -> Self {
        let message = if f.message.trim().is_empty() {
            f.kind.clone()
        } else {
            f.message.clone()
        };
        let kind = f.kind.clone();
        match kind.as_str() {
            "InvalidLogin" | "NotAuthenticated" | "InvalidLocale" => Self::Auth { kind, message },
            "NoPermission" => Self::NoPermission {
                privilege: f
                    .detail
                    .as_ref()
                    .and_then(|d| d.string_at(&["privilegeId"])),
                message,
            },
            "RestrictedVersion" => Self::Restricted(message),
            "ManagedObjectNotFound" | "FileNotFound" | "NotFound" => {
                Self::NotFound { kind, message }
            }
            "MethodNotFound" | "NotSupported" | "NotImplemented" => {
                Self::NotSupported { kind, message }
            }
            k if FILE_FAULTS.contains(&k) => Self::FileFault { kind, message },
            k if STATE_FAULTS.contains(&k) => Self::InvalidState { kind, message },
            _ => Self::Fault { kind, message },
        }
    }

    /// 결함의 형식(`InvalidPowerState` 등). 결함이 아니면 None.
    pub fn fault_kind(&self) -> Option<&str> {
        match self {
            Self::Auth { kind, .. }
            | Self::InvalidState { kind, .. }
            | Self::FileFault { kind, .. }
            | Self::NotFound { kind, .. }
            | Self::NotSupported { kind, .. }
            | Self::Fault { kind, .. } => Some(kind),
            Self::NoPermission { .. } => Some("NoPermission"),
            Self::Restricted(_) => Some("RestrictedVersion"),
            _ => None,
        }
    }

    /// 다시 해 볼 만한 연결 문제인가(끊김·시간 넘김·5xx)
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connect(_) | Self::Timeout(_))
            || matches!(self, Self::Http(s) if *s >= 500)
    }

    /// API 오류로(설계 §7.1): 인증 400 `vmware_auth`, 원본 권한 400 `vmware_permission`, 연결할 수 없음 502
    /// `vmware_unreachable`, 지문 없음 409 `untrusted_certificate`·다름 409 `fingerprint_mismatch`(둘 다
    /// `details.fingerprint`, 없음에는 `details.thumbprint_sha1`도), 무료 ESXi 400 `vmware_restricted`, 그 밖의
    /// 결함 400 `vmware_error`.
    pub fn api_error(self) -> ApiError {
        let message = self.to_string();
        match self {
            Self::Untrusted { sha256, sha1 } => {
                let mut e = ApiError::conflict(
                    "untrusted_certificate",
                    "verify the vSphere certificate fingerprint",
                );
                e.details =
                    Some(serde_json::json!({ "fingerprint": sha256, "thumbprint_sha1": sha1 }));
                e
            }
            Self::Fingerprint { actual, .. } => {
                let mut e = ApiError::conflict("fingerprint_mismatch", message);
                e.details = Some(serde_json::json!({ "fingerprint": actual }));
                e
            }
            Self::Auth { .. } => ApiError::new(StatusCode::BAD_REQUEST, "vmware_auth", message),
            Self::NoPermission { .. } => {
                ApiError::new(StatusCode::BAD_REQUEST, "vmware_permission", message)
            }
            Self::Restricted(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "vmware_restricted", message)
            }
            Self::Connect(_) | Self::Http(_) | Self::Timeout(_) | Self::Invalid(_) => {
                ApiError::new(StatusCode::BAD_GATEWAY, "vmware_unreachable", message)
            }
            Self::InvalidState { .. }
            | Self::FileFault { .. }
            | Self::NotFound { .. }
            | Self::NotSupported { .. }
            | Self::Fault { .. } => ApiError::new(StatusCode::BAD_REQUEST, "vmware_error", message),
        }
    }
}

impl From<http::HttpError> for VsphereError {
    fn from(e: http::HttpError) -> Self {
        match e {
            http::HttpError::Connect(m) => Self::Connect(m),
            http::HttpError::Untrusted { sha256, sha1 } => Self::Untrusted { sha256, sha1 },
            http::HttpError::Fingerprint { expected, actual } => {
                Self::Fingerprint { expected, actual }
            }
            http::HttpError::TooLarge(limit) => {
                Self::Invalid(format!("the response is larger than {limit} bytes"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::soap::{Node, parse_reply};
    use super::*;

    fn fault(xml_detail: &str, message: &str) -> VsphereError {
        let xml = format!(
            r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><soapenv:Body><soapenv:Fault><faultcode>ServerFaultCode</faultcode><faultstring>{message}</faultstring><detail>{xml_detail}</detail></soapenv:Fault></soapenv:Body></soapenv:Envelope>"#
        );
        match parse_reply(&xml).unwrap() {
            soap::Reply::Fault(f) => VsphereError::from_fault(f),
            soap::Reply::Ok(_) => panic!("not a fault"),
        }
    }

    #[test]
    fn maps_fault_types() {
        let e = fault(
            r#"<InvalidLoginFault xmlns="urn:vim25" xsi:type="InvalidLogin"/>"#,
            "Login failure",
        );
        assert!(matches!(&e, VsphereError::Auth { kind, .. } if kind == "InvalidLogin"));
        assert_eq!(e.api_error().code, "vmware_auth");

        let e = fault(
            r#"<NoPermissionFault xmlns="urn:vim25" xsi:type="NoPermission"><object type="VirtualMachine">vm-1</object><privilegeId>VirtualMachine.State.CreateSnapshot</privilegeId></NoPermissionFault>"#,
            "Permission to perform this operation was denied.",
        );
        assert_eq!(
            e,
            VsphereError::NoPermission {
                privilege: Some("VirtualMachine.State.CreateSnapshot".into()),
                message: "Permission to perform this operation was denied.".into()
            }
        );
        let api = e.api_error();
        assert_eq!((api.status.as_u16(), api.code), (400, "vmware_permission"));

        // vcsim의 NotAuthenticated는 faultstring이 비어 있다 — 형식 이름을 메시지로
        let e = fault(
            r#"<NotAuthenticatedFault xmlns="urn:vim25" xsi:type="NotAuthenticated"><object type="ViewManager">ViewManager</object><privilegeId>System.View</privilegeId></NotAuthenticatedFault>"#,
            "",
        );
        assert_eq!(
            e,
            VsphereError::Auth {
                kind: "NotAuthenticated".into(),
                message: "NotAuthenticated".into()
            }
        );
        assert!(matches!(
            fault(
                r#"<X xmlns="urn:vim25" xsi:type="InvalidPowerState"><requestedState>poweredOff</requestedState></X>"#,
                "x"
            ),
            VsphereError::InvalidState { .. }
        ));
        assert!(matches!(
            fault(
                r#"<X xmlns="urn:vim25" xsi:type="FileFault"><file>[ds] a</file></X>"#,
                "x"
            ),
            VsphereError::FileFault { .. }
        ));
        assert!(matches!(
            fault(
                r#"<X xmlns="urn:vim25" xsi:type="RestrictedVersion"/>"#,
                "x"
            ),
            VsphereError::Restricted(_)
        ));
        assert!(matches!(
            fault(
                r#"<MethodNotFoundFault xmlns="urn:vim25" xsi:type="MethodNotFound"><receiver type="VirtualMachine">vm-62</receiver><method>QueryChangedDiskAreas</method></MethodNotFoundFault>"#,
                "x"
            ),
            VsphereError::NotSupported { .. }
        ));
        assert!(matches!(
            fault(
                r#"<ManagedObjectNotFoundFault xmlns="urn:vim25" xsi:type="ManagedObjectNotFound"><obj type="VirtualMachine">vm-1</obj></ManagedObjectNotFoundFault>"#,
                "gone"
            ),
            VsphereError::NotFound { .. }
        ));
        let e = fault(r#"<X xmlns="urn:vim25" xsi:type="SystemError"/>"#, "boom");
        assert_eq!(e.fault_kind(), Some("SystemError"));
        assert_eq!(e.api_error().code, "vmware_error");
    }

    #[test]
    fn maps_connection_errors_to_api_codes() {
        let e: VsphereError = http::HttpError::Untrusted {
            sha256: "AA".into(),
            sha1: "BB".into(),
        }
        .into();
        let api = e.api_error();
        assert_eq!(
            (api.status.as_u16(), api.code),
            (409, "untrusted_certificate")
        );
        assert_eq!(
            api.details,
            Some(serde_json::json!({"fingerprint": "AA", "thumbprint_sha1": "BB"}))
        );
        let api = VsphereError::Fingerprint {
            expected: "A".into(),
            actual: "B".into(),
        }
        .api_error();
        assert_eq!(
            (api.status.as_u16(), api.code),
            (409, "fingerprint_mismatch")
        );
        let api = VsphereError::Connect("refused".into()).api_error();
        assert_eq!((api.status.as_u16(), api.code), (502, "vmware_unreachable"));
        assert!(VsphereError::Http(503).is_transient());
        assert!(!VsphereError::Http(404).is_transient());
        let _ = Node::default();
    }

    #[test]
    fn libdir_prefers_the_source() {
        let c = VmwareNodeConfig {
            nbdkit: None,
            vddk_libdir: Some("/opt/vddk".into()),
        };
        assert_eq!(c.libdir_for(Some("/x")), Some(PathBuf::from("/x")));
        assert_eq!(c.libdir_for(None), Some(PathBuf::from("/opt/vddk")));
    }
}
