//! 인증 영역. 로컬 사용자 말고 바깥 디렉터리(LDAP)로 로그인하게 한다. 리소스 이름이 영역 이름이고,
//! 그 영역으로 로그인한 사용자는 `<사용자>@<영역>` 이름의 외부 사용자가 된다.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::Role;
use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

pub type Realm = Resource<RealmSpec, RealmStatus>;

/// 로컬 사용자의 영역 이름. 리소스로 만들 수 없다.
pub const LOCAL_REALM: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealmSpec {
    /// 로그인 화면에 보일 이름
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    pub backend: RealmBackend,
    /// 그룹에서 역할 정하기
    #[serde(default)]
    pub roles: RoleMapping,
}

impl ResourceSpec for RealmSpec {
    const KIND: Kind = Kind::Realm;
    type Status = RealmStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RealmStatus {}

/// 영역이 사용자를 확인하는 방법
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmBackend {
    Ldap(LdapRealm),
}

/// LDAP 디렉터리. `ldap://`는 StartTLS를 반드시 쓰고, `ldaps://`는 처음부터 TLS다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LdapRealm {
    /// `ldap://host[:port]` 또는 `ldaps://host[:port]`
    pub url: String,
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_dn: Option<String>,
    /// 서비스 계정 비밀번호. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_password: Option<String>,
    /// 사용자를 찾을 기준 DN(subtree)
    pub base_dn: String,
    /// 사용자 찾기 필터. `{username}`이 이스케이프된 로그인 이름으로 바뀐다.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    /// 그룹 DN을 담은 사용자 항목의 속성
    #[serde(default = "default_group_attr")]
    pub group_attr: String,
    /// 서버 인증서를 확인할 CA(PEM). 없으면 시스템 루트
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
    /// 연결과 작업마다의 시간 제한(초)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// 사용자 찾기 필터의 기본값
pub const DEFAULT_USER_FILTER: &str = "(&(objectClass=person)(uid={username}))";
/// 그룹 속성의 기본값
pub const DEFAULT_GROUP_ATTR: &str = "memberOf";
/// 시간 제한의 기본값(초)
pub const DEFAULT_TIMEOUT_SECS: u64 = 10;

fn default_user_filter() -> String {
    DEFAULT_USER_FILTER.into()
}

fn default_group_attr() -> String {
    DEFAULT_GROUP_ATTR.into()
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

/// 그룹에서 역할 정하기
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleMapping {
    #[serde(default)]
    pub groups: Vec<GroupRole>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 로그인을 거절한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Role>,
}

/// 이 그룹(보통 DN)에 든 사용자는 이 역할
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRole {
    pub group: String,
    pub role: Role,
}

impl RoleMapping {
    /// 사용자 그룹에 맞는 매핑 가운데 가장 높은 역할. 그룹은 앞뒤 공백을 떼고 ASCII 대소문자를
    /// 무시해 비교한다. 맞는 것이 없으면 `default`.
    pub fn role_for(&self, groups: &[String]) -> Option<Role> {
        self.groups
            .iter()
            .filter(|m| {
                groups
                    .iter()
                    .any(|g| g.trim().eq_ignore_ascii_case(m.group.trim()))
            })
            .map(|m| m.role)
            .max_by_key(|r| rank(*r))
            .or(self.default)
    }
}

/// 역할의 높이: admin > operator > viewer
fn rank(role: Role) -> u8 {
    match role {
        Role::Admin => 2,
        Role::Operator => 1,
        Role::Viewer => 0,
    }
}

impl RealmSpec {
    pub fn validate(&self) -> Result<(), RealmError> {
        match &self.backend {
            RealmBackend::Ldap(ldap) => ldap.validate()?,
        }
        if self.roles.groups.iter().any(|m| m.group.trim().is_empty()) {
            return Err(RealmError::EmptyGroup);
        }
        Ok(())
    }
}

impl LdapRealm {
    fn validate(&self) -> Result<(), RealmError> {
        if !ldap_url_has_host(&self.url) {
            return Err(RealmError::Url);
        }
        if self.base_dn.trim().is_empty() {
            return Err(RealmError::BaseDn);
        }
        if self.user_filter.matches("{username}").count() != 1 {
            return Err(RealmError::UserFilter);
        }
        if self.group_attr.trim().is_empty() {
            return Err(RealmError::GroupAttr);
        }
        if self.bind_password.is_some() && self.bind_dn.is_none() {
            return Err(RealmError::BindPasswordWithoutDn);
        }
        if self.bind_dn.is_some() && self.bind_password.as_deref().unwrap_or("").is_empty() {
            return Err(RealmError::BindDnWithoutPassword);
        }
        if !(1..=30).contains(&self.timeout_secs) {
            return Err(RealmError::Timeout);
        }
        Ok(())
    }
}

/// `ldap://` 또는 `ldaps://`로 시작하고 호스트가 있는지
fn ldap_url_has_host(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("ldaps://")
        .or_else(|| url.strip_prefix("ldap://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // [IPv6]:port
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    !host.is_empty()
}

/// 영역 이름 규칙: 리소스 이름 규칙이고 `local`이 아니며 UUID로 풀리지 않는다(API가 id로 읽는다).
pub fn validate_realm_name(name: &str) -> Result<(), RealmError> {
    validate_name(name)?;
    if name == LOCAL_REALM {
        return Err(RealmError::ReservedName);
    }
    if uuid::Uuid::try_parse(name).is_ok() {
        return Err(RealmError::LooksLikeId(name.to_string()));
    }
    Ok(())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RealmError {
    #[error(transparent)]
    Name(#[from] NameError),
    #[error("the realm name \"local\" is reserved for local users")]
    ReservedName,
    #[error("invalid name {0:?}: names must not look like an id")]
    LooksLikeId(String),
    #[error("url must be ldap://host[:port] or ldaps://host[:port]")]
    Url,
    #[error("base_dn must not be empty")]
    BaseDn,
    #[error("user_filter must contain {{username}} exactly once")]
    UserFilter,
    #[error("group_attr must not be empty")]
    GroupAttr,
    #[error("bind_password needs bind_dn")]
    BindPasswordWithoutDn,
    #[error("bind_dn needs bind_password; remove bind_dn for an anonymous search")]
    BindDnWithoutPassword,
    #[error("timeout_secs must be between 1 and 30")]
    Timeout,
    #[error("roles.groups[].group must not be empty")]
    EmptyGroup,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn ldap() -> LdapRealm {
        serde_json::from_value(json!({
            "url": "ldaps://ldap.example.com",
            "base_dn": "dc=example,dc=com"
        }))
        .unwrap()
    }

    fn spec(ldap: LdapRealm) -> RealmSpec {
        RealmSpec {
            display: None,
            backend: RealmBackend::Ldap(ldap),
            roles: RoleMapping::default(),
        }
    }

    #[test]
    fn kind_is_realm() {
        assert_eq!(RealmSpec::KIND, Kind::Realm);
        assert_eq!(serde_json::to_value(Kind::Realm).unwrap(), "realm");
    }

    #[test]
    fn ldap_fields_have_defaults() {
        let l = ldap();
        assert_eq!(l.user_filter, "(&(objectClass=person)(uid={username}))");
        assert_eq!(l.group_attr, "memberOf");
        assert_eq!(l.timeout_secs, 10);
        assert_eq!(l.bind_dn, None);
        assert_eq!(l.bind_password, None);
        assert_eq!(l.ca_pem, None);
    }

    #[test]
    fn realm_spec_round_trips_as_tagged_json() {
        let full = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap",
                "url": "ldap://ldap.example.com:389",
                "bind_dn": "cn=svc,dc=example,dc=com",
                "bind_password": "s3cret",
                "base_dn": "dc=example,dc=com",
                "user_filter": "(sAMAccountName={username})",
                "group_attr": "memberOf",
                "ca_pem": "-----BEGIN CERTIFICATE-----",
                "timeout_secs": 5
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        let s: RealmSpec = serde_json::from_value(full.clone()).unwrap();
        let RealmBackend::Ldap(l) = &s.backend;
        assert_eq!(l.bind_password.as_deref(), Some("s3cret"));
        assert_eq!(l.timeout_secs, 5);
        assert_eq!(s.roles.default, Some(Role::Viewer));
        assert_eq!(serde_json::to_value(&s).unwrap(), full);

        // 없는 선택 필드는 쓰지 않는다.
        let small = spec(ldap());
        let json = serde_json::to_value(&small).unwrap();
        assert_eq!(
            json,
            json!({
                "backend": {
                    "kind": "ldap",
                    "url": "ldaps://ldap.example.com",
                    "base_dn": "dc=example,dc=com",
                    "user_filter": "(&(objectClass=person)(uid={username}))",
                    "group_attr": "memberOf",
                    "timeout_secs": 10
                },
                "roles": { "groups": [] }
            })
        );
        assert_eq!(serde_json::from_value::<RealmSpec>(json).unwrap(), small);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let backend = json!({ "kind": "ldap", "url": "ldaps://h", "base_dn": "dc=x" });
        let ok = json!({ "backend": backend });
        assert!(serde_json::from_value::<RealmSpec>(ok).is_ok());

        let mut extra_spec = json!({ "backend": backend });
        extra_spec["bogus"] = json!(1);
        let mut extra_ldap = backend.clone();
        extra_ldap["bogus"] = json!(1);
        let mut extra_kind = backend.clone();
        extra_kind["kind"] = json!("oidc");
        for bad in [
            extra_spec,
            json!({ "backend": extra_ldap }),
            json!({ "backend": extra_kind }),
            json!({ "backend": backend, "roles": { "bogus": [] } }),
            json!({ "backend": backend, "roles": { "groups": [{ "group": "g", "role": "admin", "x": 1 }] } }),
        ] {
            assert!(
                serde_json::from_value::<RealmSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn validates_ldap_realms() {
        assert_eq!(spec(ldap()).validate(), Ok(()));
        for url in [
            "ldap://h",
            "ldaps://h:636",
            "ldap://10.0.0.1:389/",
            "ldaps://[::1]:636",
        ] {
            let l = LdapRealm {
                url: url.into(),
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Ok(()), "{url}");
        }
        for url in [
            "",
            "h",
            "http://h",
            "ldap://",
            "ldaps://:636",
            "ldaps:///dc=x",
            "ldaps://[]:636",
        ] {
            let l = LdapRealm {
                url: url.into(),
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Err(RealmError::Url), "{url}");
        }

        let cases = [
            (
                LdapRealm {
                    base_dn: " ".into(),
                    ..ldap()
                },
                RealmError::BaseDn,
            ),
            (
                LdapRealm {
                    user_filter: "(uid=x)".into(),
                    ..ldap()
                },
                RealmError::UserFilter,
            ),
            (
                LdapRealm {
                    user_filter: "(|(uid={username})(mail={username}))".into(),
                    ..ldap()
                },
                RealmError::UserFilter,
            ),
            (
                LdapRealm {
                    group_attr: "".into(),
                    ..ldap()
                },
                RealmError::GroupAttr,
            ),
            (
                LdapRealm {
                    bind_password: Some("p".into()),
                    ..ldap()
                },
                RealmError::BindPasswordWithoutDn,
            ),
            (
                LdapRealm {
                    timeout_secs: 0,
                    ..ldap()
                },
                RealmError::Timeout,
            ),
            (
                LdapRealm {
                    timeout_secs: 31,
                    ..ldap()
                },
                RealmError::Timeout,
            ),
        ];
        for (l, err) in cases {
            assert_eq!(spec(l.clone()).validate(), Err(err), "{l:?}");
        }

        // 경계값과 서비스 계정
        for timeout_secs in [1, 30] {
            let l = LdapRealm {
                timeout_secs,
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Ok(()));
        }
        // 전체 마감(timeout_secs × 4 + 1초)이 121초를 넘지 않는다.
        assert_eq!(
            RealmError::Timeout.to_string(),
            "timeout_secs must be between 1 and 30"
        );
        let l = LdapRealm {
            bind_dn: Some("cn=svc".into()),
            bind_password: Some("p".into()),
            ..ldap()
        };
        assert_eq!(spec(l).validate(), Ok(()));
        // bind_dn이 있으면 비밀번호도 있어야 한다. 익명 검색은 둘 다 없을 때만이다.
        for bind_password in [None, Some(String::new())] {
            let l = LdapRealm {
                bind_dn: Some("cn=svc".into()),
                bind_password,
                ..ldap()
            };
            assert_eq!(spec(l).validate(), Err(RealmError::BindDnWithoutPassword));
        }
        assert_eq!(
            RealmError::BindDnWithoutPassword.to_string(),
            "bind_dn needs bind_password; remove bind_dn for an anonymous search"
        );

        let mut s = spec(ldap());
        s.roles.groups.push(GroupRole {
            group: "  ".into(),
            role: Role::Admin,
        });
        assert_eq!(s.validate(), Err(RealmError::EmptyGroup));
    }

    fn mapping() -> RoleMapping {
        RoleMapping {
            groups: vec![
                GroupRole {
                    group: "cn=viewers,dc=x".into(),
                    role: Role::Viewer,
                },
                GroupRole {
                    group: "cn=Admins,dc=x".into(),
                    role: Role::Admin,
                },
                GroupRole {
                    group: " cn=ops,dc=x ".into(),
                    role: Role::Operator,
                },
            ],
            default: None,
        }
    }

    fn groups(gs: &[&str]) -> Vec<String> {
        gs.iter().map(|g| g.to_string()).collect()
    }

    #[test]
    fn role_for_picks_the_highest_matching_role() {
        let m = mapping();
        assert_eq!(
            m.role_for(&groups(&["cn=viewers,dc=x", "cn=ops,dc=x"])),
            Some(Role::Operator)
        );
        assert_eq!(
            m.role_for(&groups(&[
                "cn=ops,dc=x",
                "cn=admins,dc=x",
                "cn=viewers,dc=x"
            ])),
            Some(Role::Admin)
        );
        assert_eq!(
            m.role_for(&groups(&["cn=viewers,dc=x"])),
            Some(Role::Viewer)
        );
    }

    #[test]
    fn role_for_ignores_ascii_case_and_surrounding_spaces() {
        let m = mapping();
        assert_eq!(m.role_for(&groups(&["CN=ADMINS,DC=X"])), Some(Role::Admin));
        assert_eq!(
            m.role_for(&groups(&["  cn=OPS,dc=x"])),
            Some(Role::Operator)
        );
        // 부분 문자열은 맞지 않는다.
        assert_eq!(m.role_for(&groups(&["cn=admins,dc=x,dc=y"])), None);
    }

    #[test]
    fn role_for_falls_back_to_default_or_none() {
        let mut m = mapping();
        assert_eq!(m.role_for(&groups(&["cn=other,dc=x"])), None);
        assert_eq!(m.role_for(&[]), None);
        m.default = Some(Role::Viewer);
        assert_eq!(m.role_for(&groups(&["cn=other,dc=x"])), Some(Role::Viewer));
        // 맞는 그룹이 있으면 default보다 그것이다(낮아도).
        m.default = Some(Role::Admin);
        assert_eq!(
            m.role_for(&groups(&["cn=viewers,dc=x"])),
            Some(Role::Viewer)
        );
    }

    #[test]
    fn validates_realm_names() {
        for ok in ["corp", "ad-01", "a"] {
            assert_eq!(validate_realm_name(ok), Ok(()), "{ok}");
        }
        assert_eq!(validate_realm_name("local"), Err(RealmError::ReservedName));
        // id로 읽히는 이름은 이름으로 부를 수 없다.
        for id in [
            "0192f0c1-5d2e-7000-8000-000000000000",
            "0192f0c15d2e70008000000000000000",
        ] {
            assert_eq!(
                validate_realm_name(id),
                Err(RealmError::LooksLikeId(id.into())),
                "{id}"
            );
        }
        assert_eq!(
            RealmError::LooksLikeId("0192f0c15d2e70008000000000000000".into()).to_string(),
            "invalid name \"0192f0c15d2e70008000000000000000\": names must not look like an id"
        );
        for bad in ["", "Corp", "a@b", "-a", &"a".repeat(64)] {
            assert!(
                matches!(validate_realm_name(bad), Err(RealmError::Name(_))),
                "{bad}"
            );
        }
    }
}
