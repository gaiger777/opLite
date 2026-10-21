use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};

/// 로컬 사용자. 리소스 이름이 로그인 이름이다.
pub type User = Resource<UserSpec, UserStatus>;
/// API 토큰. 리소스 이름은 사용자가 붙인 이름이다. 비밀 값은 해시만 저장한다.
pub type ApiToken = Resource<ApiTokenSpec, ApiTokenStatus>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// 모든 작업
    Admin,
    /// VM과 볼륨을 만들고 다룬다.
    Operator,
    /// 읽기만 한다.
    Viewer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserSpec {
    /// argon2 PHC 문자열
    pub password_hash: String,
    pub role: Role,
}

impl ResourceSpec for UserSpec {
    const KIND: Kind = Kind::User;
    type Status = UserStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserStatus {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiTokenSpec {
    /// 토큰 주인(User id). 권한은 주인의 역할을 따른다.
    pub user: Uuid,
    /// 비밀 값의 SHA-256 (16진수)
    pub secret_hash: String,
}

impl ResourceSpec for ApiTokenSpec {
    const KIND: Kind = Kind::ApiToken;
    type Status = ApiTokenStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ApiTokenStatus {}
