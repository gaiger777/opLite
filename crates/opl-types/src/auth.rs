use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

/// 사용자. 리소스 이름이 로그인 이름이다. 외부 사용자는 `<사용자>@<영역>`이다.
pub type User = Resource<UserSpec, UserStatus>;
/// API 토큰. 리소스 이름은 사용자가 붙인 이름이다. 비밀 값은 해시만 저장한다.
pub type ApiToken = Resource<ApiTokenSpec, ApiTokenStatus>;
/// 로그인 세션(웹 UI의 쿠키). 리소스 이름은 쿠키 값의 SHA-256(16진수)이라 원문은 저장하지 않는다.
/// 모든 노드가 같은 세션을 본다(앞단 프록시가 어느 노드로 보내도 이어진다).
pub type Session = Resource<SessionSpec, SessionStatus>;

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
    /// argon2 PHC 문자열. 외부 사용자는 빈 문자열이다.
    pub password_hash: String,
    pub role: Role,
    /// 비밀번호가 바뀔 때마다 1 올린다. 세션은 만들 때의 값과 다르면 끊긴다.
    /// 0이면 쓰지 않는다(이 필드를 모르는 옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub session_epoch: u64,
    /// 외부 사용자의 영역 이름. 로컬 사용자는 None이고 쓰지 않는다(옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    /// OIDC 외부 사용자를 처음 만든 IdP의 사용자: `"<iss> <sub>"`(공백 하나로 이음). 같은 이름으로 다른
    /// 사람이 로그인하지 못하게 한다. 그 밖의 사용자는 None이고 쓰지 않는다(옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ResourceSpec for UserSpec {
    const KIND: Kind = Kind::User;
    type Status = UserStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserStatus {}

/// LDAP 영역의 외부 사용자 이름 규칙: `<사용자>@<영역>`. 마지막 `@`에서 나눈다(영역 이름에는 `@`가
/// 없다). 사용자 부분은 1~64자의 영문 소문자, 숫자, `.`, `_`, `-`이고 영역 부분은 리소스 이름 규칙이다.
pub fn validate_external_name(name: &str) -> Result<(), NameError> {
    validate_with(name, valid_external_user)
}

/// OIDC 영역의 외부 사용자 이름 규칙. 사용자 부분은 1~128자의 영문 소문자, 숫자, `.`, `_`, `+`, `-`와
/// 안쪽 `@` 하나까지다(이메일 모양 `alice@corp.com@okta` — 앞뒤가 비어 있지 않음).
pub fn validate_oidc_external_name(name: &str) -> Result<(), NameError> {
    validate_with(name, valid_oidc_user)
}

fn validate_with(name: &str, valid_user: fn(&str) -> bool) -> Result<(), NameError> {
    let ok = name
        .rsplit_once('@')
        .is_some_and(|(user, realm)| valid_user(user) && validate_name(realm).is_ok());
    if ok {
        Ok(())
    } else {
        Err(NameError::new(name))
    }
}

/// LDAP 영역 로그인 이름으로 외부 사용자 이름을 만든다. 사용자 부분은 소문자로 바꾼 뒤 검사한다.
pub fn external_name(user: &str, realm: &str) -> Result<String, NameError> {
    let name = format!("{}@{realm}", user.to_ascii_lowercase());
    validate_external_name(&name)?;
    Ok(name)
}

/// OIDC 이름 클레임으로 외부 사용자 이름을 만든다. 사용자 부분은 소문자로 바꾼 뒤 검사한다.
pub fn external_name_oidc(user: &str, realm: &str) -> Result<String, NameError> {
    let name = format!("{}@{realm}", user.to_ascii_lowercase());
    validate_oidc_external_name(&name)?;
    Ok(name)
}

fn valid_external_user(user: &str) -> bool {
    (1..=64).contains(&user.len())
        && user.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

fn valid_oidc_user(user: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.bytes().all(|c| {
                c.is_ascii_lowercase()
                    || c.is_ascii_digit()
                    || matches!(c, b'.' | b'_' | b'+' | b'-')
            })
    };
    (1..=128).contains(&user.len())
        && match user.split_once('@') {
            Some((local, domain)) => part(local) && part(domain),
            None => part(user),
        }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSpec {
    /// 세션 주인(User id)
    pub user: Uuid,
    /// 만들 때 사용자의 `session_epoch`. 다르면 세션이 끊긴다.
    pub epoch: u64,
    /// 끝나는 시각(유닉스 초). 지나면 쓸 수 없고 리더의 정리 작업이 지운다.
    pub expires_at: u64,
}

impl ResourceSpec for SessionSpec {
    const KIND: Kind = Kind::Session;
    type Status = SessionStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionStatus {
    /// 마지막으로 쓴 시각(유닉스 초). 노드가 가끔(최대 1분에 한 번) 적으므로 그만큼 늦을 수 있다.
    pub last_used: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_storage_format_is_stable() {
        let user = Uuid::from_u128(1);
        let spec = SessionSpec {
            user,
            epoch: 2,
            expires_at: 100,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "user": user.to_string(), "epoch": 2, "expires_at": 100 })
        );
        assert_eq!(serde_json::from_value::<SessionSpec>(json).unwrap(), spec);
        assert_eq!(SessionSpec::KIND, Kind::Session);
        assert_eq!(serde_json::to_value(Kind::Session).unwrap(), "session");
        let status: SessionStatus = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(status.last_used, 0);
    }

    #[test]
    fn user_session_epoch_defaults_to_zero() {
        // 옛 사용자에는 session_epoch가 없다.
        let old: UserSpec = serde_json::from_value(serde_json::json!({
            "password_hash": "h",
            "role": "admin"
        }))
        .unwrap();
        assert_eq!(old.session_epoch, 0);
        let user = UserSpec {
            session_epoch: 3,
            ..old
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(json["session_epoch"], 3);
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn zero_session_epoch_is_not_written() {
        // 옛 노드의 UserSpec은 모르는 필드를 거절하므로, 세대가 0이면 쓰지 않는다.
        let user = UserSpec {
            password_hash: "h".into(),
            role: Role::Admin,
            session_epoch: 0,
            realm: None,
            subject: None,
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "h", "role": "admin" })
        );
    }

    #[test]
    fn local_user_json_keeps_the_old_shape() {
        // 영역이 없는 사용자는 계획 34 모양 그대로다(옛 노드는 모르는 필드를 거절한다).
        let old = serde_json::json!({ "password_hash": "h", "role": "viewer", "session_epoch": 2 });
        let user: UserSpec = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(user.realm, None);
        assert_eq!(serde_json::to_value(&user).unwrap(), old);
    }

    #[test]
    fn external_user_round_trips_with_its_realm() {
        let user = UserSpec {
            password_hash: String::new(),
            role: Role::Operator,
            session_epoch: 0,
            realm: Some("corp".into()),
            subject: None,
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "", "role": "operator", "realm": "corp" })
        );
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn oidc_user_round_trips_with_its_subject() {
        let user = UserSpec {
            password_hash: String::new(),
            role: Role::Viewer,
            session_epoch: 0,
            realm: Some("okta".into()),
            subject: Some("https://idp.example.com u-1".into()),
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "", "role": "viewer", "realm": "okta",
                                "subject": "https://idp.example.com u-1" })
        );
        assert_eq!(serde_json::from_value::<UserSpec>(json).unwrap(), user);
    }

    #[test]
    fn validates_external_names() {
        let long = format!("{}@corp", "a".repeat(64));
        for ok in ["alice@corp", "a.b_c-d@ad-01", "0@a", long.as_str()] {
            assert_eq!(validate_external_name(ok), Ok(()), "{ok}");
        }
        let too_long = format!("{}@corp", "a".repeat(65));
        for bad in [
            "",
            "alice",
            "@corp",
            "alice@",
            "Alice@corp",
            "alice@Corp",
            "al ice@corp",
            "al*ce@corp",
            "a@b@corp",
            "alice@corp.com@okta",
            "alice@co.rp",
            "alice@-corp",
            too_long.as_str(),
        ] {
            assert!(validate_external_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_oidc_external_names() {
        let long = format!("{}@okta", "a".repeat(128));
        let long_email = format!("{}@corp.com@okta", "a".repeat(119));
        for ok in [
            "alice@okta",
            "alice@corp.com@okta",
            "a.b_c-d+e@sub.corp-1.com@okta",
            "0@a",
            long.as_str(),
            long_email.as_str(),
        ] {
            assert_eq!(validate_oidc_external_name(ok), Ok(()), "{ok}");
        }
        let too_long = format!("{}@okta", "a".repeat(129));
        let too_long_email = format!("{}@corp.com@okta", "a".repeat(120));
        for bad in [
            "",
            "alice",
            "@okta",
            "alice@",
            "Alice@okta",
            "alice@Okta",
            "@corp.com@okta",
            "alice@@okta",
            "a@b@c@okta",
            "al ice@okta",
            "a/b@okta",
            "a:b@okta",
            "a=b@okta",
            "a%40b@okta",
            "alice@corp%2ecom@okta",
            "alice@corp com@okta",
            "é@okta",
            too_long.as_str(),
            too_long_email.as_str(),
        ] {
            assert!(validate_oidc_external_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn external_names_split_at_the_last_at_sign() {
        // OIDC: 이메일 모양 이름의 사용자 부분은 `alice@corp.com`, 영역은 `okta`
        assert_eq!(
            external_name_oidc("Alice@Corp.com", "okta").unwrap(),
            "alice@corp.com@okta"
        );
        assert_eq!(
            "alice@corp.com@okta".rsplit_once('@'),
            Some(("alice@corp.com", "okta"))
        );
        assert_eq!(
            external_name_oidc("Bob+ops", "okta").unwrap(),
            "bob+ops@okta"
        );
        for bad in [
            "",
            "a b",
            "a@",
            "@b",
            "a@b@c",
            "a%40b",
            "é",
            &"a".repeat(129),
        ] {
            assert!(external_name_oidc(bad, "okta").is_err(), "{bad}");
        }
        assert!(external_name_oidc("alice", "Okta").is_err());
        // LDAP 영역에서는 `@`·`+`가 든 이름을 거절한다.
        for bad in ["alice@corp.com", "bob+ops"] {
            assert!(external_name(bad, "corp").is_err(), "{bad}");
        }
    }

    #[test]
    fn external_name_lowercases_the_user_part() {
        assert_eq!(external_name("Alice", "corp").unwrap(), "alice@corp");
        assert_eq!(external_name("j.doe_2", "ad").unwrap(), "j.doe_2@ad");
        // 영역 이름은 바꾸지 않는다.
        assert!(external_name("alice", "Corp").is_err());
        for bad in ["", "a@b", "a*", "a)", "a b", "é", &"a".repeat(65)] {
            assert!(external_name(bad, "corp").is_err(), "{bad}");
        }
    }
}
