//! 클러스터 자체의 정보와 노드 합류 토큰. 클러스터를 만들고 노드를 더할 때만 쓴다.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::resource::{Kind, Resource, ResourceSpec};

/// 클러스터에 하나만 있다. 리소스 이름이 클러스터 이름이다.
pub type ClusterInfo = Resource<ClusterSpec, ClusterStatus>;
/// 노드 합류 토큰. 한 번 쓰면 지운다. 리소스 이름은 id 문자열이다.
pub type JoinToken = Resource<JoinTokenSpec, JoinTokenStatus>;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterSpec {
    /// 클러스터 CA 인증서(PEM)
    pub ca_cert_pem: String,
    /// CA 개인키(PEM). 어느 노드든 합류 요청에 서명할 수 있게 복제한다. API로 내보내지 않는다.
    pub ca_key_pem: String,
}

/// 로그에 개인키가 찍히지 않게 한다.
impl fmt::Debug for ClusterSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClusterSpec")
            .field("ca_cert_pem", &self.ca_cert_pem)
            .field("ca_key_pem", &"<redacted>")
            .finish()
    }
}

impl ResourceSpec for ClusterSpec {
    const KIND: Kind = Kind::Cluster;
    type Status = ClusterStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ClusterStatus {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenSpec {
    /// 비밀 값의 SHA-256 (16진수)
    pub secret_hash: String,
    /// 만료 시각(유닉스 초)
    pub expires_at: u64,
}

impl ResourceSpec for JoinTokenSpec {
    const KIND: Kind = Kind::JoinToken;
    type Status = JoinTokenStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct JoinTokenStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_ca_key() {
        let spec = ClusterSpec {
            ca_cert_pem: "CERT".into(),
            ca_key_pem: "SECRET-KEY".into(),
        };
        let shown = format!("{spec:?}");
        assert!(shown.contains("CERT"));
        assert!(!shown.contains("SECRET-KEY"));
    }
}
