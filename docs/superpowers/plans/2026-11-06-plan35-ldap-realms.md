# opLite 계획 35: 인증 영역과 LDAP 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 회사 디렉터리(OpenLDAP, Active Directory)의 계정으로 opLite에 로그인하게 한다. 관리자가 인증 영역(LDAP 서버, 사용자 찾는 법, 그룹 → 역할 매핑)을 만들면, 사용자는 로그인 화면에서 영역을 고르고 디렉터리 비밀번호로 들어온다. 처음 로그인하면 `alice@corp` 같은 opLite 사용자가 생기고 역할은 로그인할 때마다 그룹으로 다시 정한다. API·oplctl·웹, 실제 OpenLDAP로 확인하는 CI 작업. 하위 프로젝트 10의 두 번째 계획이다.

**Architecture:**
- **영역:** 새 리소스 `Realm`(계정 종류처럼 admin만 읽고 씀). 종류는 지금 `ldap` 하나이고 계획 36이 `oidc`를 더한다. 서비스 계정 비밀번호는 API·watch로 나가지 않는다.
- **외부 사용자:** 첫 로그인에 보통 `User` 리소스를 만든다(`realm` 필드, 빈 비밀번호 해시). 그래서 세션, API 토큰, 세션 세대, 권한 확인, 감사 로그가 로컬 사용자와 같은 경로를 탄다.
- **LDAP:** `ldap3`(rustls). StartTLS나 LDAPS만, 서비스 바인드 → 이스케이프한 필터로 검색 → 사용자 DN으로 바인드 → 그룹 속성.
- **옛 노드 호환:** 새 Kind와 `UserSpec.realm`을 쓰기 전에 `require_current`(모든 노드가 이 빌드 이상).

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, `ldap3` 0.12.1(새 의존, rustls-ring)·`lber`(시험), tokio, clap. 웹은 React·TypeScript·react-query·vitest·msw. CI는 GitHub Actions(ubuntu-latest의 slapd).

**설계 문서:** `docs/superpowers/specs/2026-11-06-enterprise-design.md` (§3과 §3.6 "구현(계획 35)")
**선행 계획:** 계획 1~34 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `Realm` 리소스·API, 로그인 화면용 영역 목록 | OIDC SSO (계획 36) |
| LDAP 로그인(StartTLS·LDAPS, 그룹 → 역할), 외부 사용자 | 디렉터리 동기화, 중첩 그룹 풀기, SAML·Kerberos |
| 영역 시험 동작, oplctl `realm`·`login --realm`, 웹 `/realms` | 세분화된 RBAC(역할은 그대로 셋) |
| CI 작업 `ldap`(실제 slapd) | 외부 사용자의 2단계 인증 |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust·웹 변경과 시험. LDAP 시험은 127.0.0.1의 가짜 LDAP 서버(시험 CA의 TLS)로 한다.
- **Linux CI에서만 하는 것:** 실제 OpenLDAP 시험(`--features ldap-it`, 작업 `ldap`)과 `ldap-setup.sh` 실행. 이 Mac에서는 컴파일과 shellcheck까지.
- **새 크레이트:** Task 3의 `cargo check`가 crates.io에서 `ldap3`·`lber`·`rustls-native-certs`를 받는다.

## 설계 결정

- **비밀번호는 쓰기 전용:** 응답에는 `has_bind_password`만. 바꾸기에서 비밀번호를 생략하면 URL·bind DN·CA가 그대로일 때만 저장된 값을 남긴다 — 관리자가 URL을 바꿔 서비스 계정 비밀번호를 다른 서버로 보내지 못하게.
- **익명 검색은 bind DN과 비밀번호가 둘 다 없을 때만:** bind DN에 빈 비밀번호는 RFC 4513의 인증 없는 바인드라 거절한다.
- **실패는 모두 같은 401:** 없는 사용자, 틀린 비밀번호, 빈 비밀번호, 여러 항목. 실패 응답은 최소 300ms로 맞춘다. 디렉터리에 닿지 못하면 503 `realm_unavailable`, 역할이 매핑되지 않으면 403 `no_role`.
- **영역을 지우면** 그 영역 사용자의 세션·토큰이 다음 요청에서 401이 된다(사용자·토큰 리소스는 남음).
- **웹은 영역을 이름으로 부른다**(감사 대상이 `realms/corp`로 남게).

## 파일 구조

```
crates/opl-types/src/{realm.rs, auth.rs, resource.rs, lib.rs}
crates/opl-api/src/{realms.rs, ldap.rs, users.rs, auth.rs, lib.rs}, crates/opl-api/tests/{realms.rs, ldap.rs, ldap_login.rs, ldap_it.rs, common/ldap.rs, openapi.rs, …}
crates/oplctl/src/cli.rs, crates/opld/src/bootstrap.rs
.github/scripts/ldap-setup.sh, .github/workflows/ci.yml
web/src/realms/{RealmsPage.tsx, RealmsPage.test.tsx, realms.ts}, web/src/{pages/LoginPage.tsx, auth/AuthProvider.tsx, auth/permissions.ts, accounts/UsersPage.tsx, layout/AppLayout.tsx, i18n/*.json, …}
README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`는 싣지 않는다. API가 바뀌는 Task의 "생성 파일" 단계에서 `pnpm -C web gen:api`로 만든다. `Cargo.lock`도 싣지 않는다. Task 3의 "생성 파일" 단계에서 `cargo check`가 새 패키지를 더한다. 셸 스크립트의 들여쓰기 탭은 그대로 옮긴다.

---

### Task 1: 인증 영역 타입과 외부 사용자 이름

`opl_types::realm`: 새 리소스 `Realm`(`Kind::Realm`)과 `RealmSpec{display, backend: RealmBackend::Ldap(LdapRealm), roles: RoleMapping}`. `LdapRealm`의 기본값(사용자 필터 `(&(objectClass=person)(uid={username}))`, 그룹 속성 `memberOf`, 시간 제한 10초), 검증(`ldap://`·`ldaps://`, `{username}` 한 번, bind DN에는 비밀번호, 시간 제한 1~30, 이름 `local`·UUID 모양 거절), 그룹 → 역할 매핑 `role_for`(가장 높은 역할, 대소문자 무시, 없으면 기본 역할), Debug에서 비밀 가리기. `UserSpec.realm`(없으면 직렬화하지 않아 계획 34 노드가 계속 풂)과 외부 사용자 이름 `<사용자>@<영역>`.

**Files:**
- Create: `crates/opl-types/src/realm.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-types/src/auth.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`
- Modify: `crates/opld/src/bootstrap.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/users.rs`에서

```rust
            session_epoch: 0,
        },
```

을 다음으로 바꾼다:

```rust
            session_epoch: 0,
            realm: None,
        },
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
                session_epoch: 0,
            },
```

을 다음으로 바꾼다:

```rust
                session_epoch: 0,
                realm: None,
            },
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            session_epoch: 0,
        },
```

을 다음으로 바꾼다:

```rust
            session_epoch: 0,
            realm: None,
        },
```

`crates/opl-types/src/auth.rs`에서

```rust

use crate::resource::{Kind, Resource, ResourceSpec};

/// 로컬 사용자. 리소스 이름이 로그인 이름이다.
pub type User = Resource<UserSpec, UserStatus>;
```

을 다음으로 바꾼다:

```rust

use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

/// 사용자. 리소스 이름이 로그인 이름이다. 외부 사용자는 `<사용자>@<영역>`이다.
pub type User = Resource<UserSpec, UserStatus>;
```

`crates/opl-types/src/auth.rs`에서

```rust
pub struct UserSpec {
    /// argon2 PHC 문자열
    pub password_hash: String,
```

을 다음으로 바꾼다:

```rust
pub struct UserSpec {
    /// argon2 PHC 문자열. 외부 사용자는 빈 문자열이다.
    pub password_hash: String,
```

`crates/opl-types/src/auth.rs`에서

```rust
    pub session_epoch: u64,
}
```

을 다음으로 바꾼다:

```rust
    pub session_epoch: u64,
    /// 외부 사용자의 영역 이름. 로컬 사용자는 None이고 쓰지 않는다(옛 노드가 읽을 수 있게).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
}
```

`crates/opl-types/src/auth.rs`에서

```rust
pub struct UserStatus {}
```

을 다음으로 바꾼다:

```rust
pub struct UserStatus {}

/// 외부 사용자 이름 규칙: `<사용자>@<영역>`. 사용자 부분은 1~64자의 영문 소문자, 숫자, `.`, `_`,
/// `-`이고 영역 부분은 리소스 이름 규칙이다.
pub fn validate_external_name(name: &str) -> Result<(), NameError> {
    let ok = name
        .split_once('@')
        .is_some_and(|(user, realm)| valid_external_user(user) && validate_name(realm).is_ok());
    if ok {
        Ok(())
    } else {
        Err(NameError::new(name))
    }
}

/// 영역 로그인 이름으로 외부 사용자 이름을 만든다. 사용자 부분은 소문자로 바꾼 뒤 검사한다.
pub fn external_name(user: &str, realm: &str) -> Result<String, NameError> {
    let name = format!("{}@{realm}", user.to_ascii_lowercase());
    validate_external_name(&name)?;
    Ok(name)
}

fn valid_external_user(user: &str) -> bool {
    (1..=64).contains(&user.len())
        && user.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}
```

`crates/opl-types/src/auth.rs`에서

```rust
            session_epoch: 0,
        };
```

을 다음으로 바꾼다:

```rust
            session_epoch: 0,
            realm: None,
        };
```

`crates/opl-types/src/auth.rs`에서

```rust
        );
    }
}
```

을 다음으로 바꾼다:

```rust
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
        };
        let json = serde_json::to_value(&user).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "password_hash": "", "role": "operator", "realm": "corp" })
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
            "alice@co.rp",
            "alice@-corp",
            too_long.as_str(),
        ] {
            assert!(validate_external_name(bad).is_err(), "{bad}");
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
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod node;
pub mod resource;
```

을 다음으로 바꾼다:

```rust
pub mod node;
pub mod realm;
pub mod resource;
```

`crates/opl-types/src/realm.rs`를 만든다:

```rust
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
```

`crates/opl-types/src/resource.rs`에서

```rust
    Settings,
}
```

을 다음으로 바꾼다:

```rust
    Settings,
    Realm,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Settings => "settings",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::Settings => "settings",
            Kind::Realm => "realm",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
pub struct NameError(String);
```

을 다음으로 바꾼다:

```rust
pub struct NameError(String);

impl NameError {
    pub(crate) fn new(name: &str) -> Self {
        Self(name.to_string())
    }
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Settings,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::Settings,
            Kind::Realm,
        ] {
```

`crates/opld/src/bootstrap.rs`에서

```rust
            session_epoch: 0,
        },
```

을 다음으로 바꾼다:

```rust
            session_epoch: 0,
            realm: None,
        },
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1303개, 실패 0

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(types): 인증 영역 타입과 외부 사용자 이름 — opl-types realm.rs(Realm·RealmSpec{display, backend, roles}·RealmBackend::Ldap(kind 태그)·LdapRealm(user_filter·group_attr·timeout_secs 기본값, 모르는 필드 거절)·RoleMapping::role_for(공백·ASCII 대소문자 무시, 가장 높은 역할, 없으면 default)·GroupRole·RealmSpec::validate·validate_realm_name(local 예약)·RealmError), Kind::Realm, UserSpec.realm(None이면 쓰지 않아 계획 34 모양 그대로), validate_external_name·external_name(<사용자>@<영역>, 사용자 부분은 소문자 1~64자 [a-z0-9._-])"
```

---

### Task 2: 인증 영역 API

`/api/v1/realms` 목록·만들기·읽기·바꾸기(`If-Match`)·지우기(admin만 — 영역은 계정 종류), 로그인 화면용 `GET /api/v1/auth/realms`(인증 없음, local 먼저). 응답은 bind 비밀번호 대신 `has_bind_password`, 바꾸기에서 비밀번호를 생략하면 URL·bind DN·CA가 그대로일 때만 저장된 값을 남김(아니면 400), `""`는 지움. URL·bind DN은 앞뒤 공백을 떼고 CA는 줄 끝을 맞춰 저장한다. 새 Kind라 쓰기 전에 `require_current`. watch에는 넣지 않는다.

**Files:**
- Create: `crates/opl-api/src/realms.rs`
- Create: `crates/opl-api/tests/realms.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-types/src/realm.rs`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/auth.rs`에서

```rust
    fn check(&self, principal: &Principal, action: Action, kind: Kind) -> bool {
        let account = matches!(kind, Kind::User | Kind::ApiToken | Kind::JoinToken);
        match principal.role {
```

을 다음으로 바꾼다:

```rust
    fn check(&self, principal: &Principal, action: Action, kind: Kind) -> bool {
        let account = matches!(
            kind,
            Kind::User | Kind::ApiToken | Kind::JoinToken | Kind::Realm
        );
        match principal.role {
```

`crates/opl-api/src/auth.rs`에서

```rust
        assert!(!a.check(&viewer, Action::Read, Kind::User));
```

을 다음으로 바꾼다:

```rust
        assert!(!a.check(&viewer, Action::Read, Kind::User));
        assert!(!a.check(&viewer, Action::Read, Kind::Realm));
```

`crates/opl-api/src/auth.rs`에서

```rust
        assert!(!a.check(&op, Action::Read, Kind::ApiToken));

        let admin = principal(Role::Admin);
        assert!(a.check(&admin, Action::Write, Kind::User));
    }
```

을 다음으로 바꾼다:

```rust
        assert!(!a.check(&op, Action::Read, Kind::ApiToken));
        assert!(!a.check(&op, Action::Read, Kind::Realm));
        assert!(!a.check(&op, Action::Write, Kind::Realm));

        let admin = principal(Role::Admin);
        assert!(a.check(&admin, Action::Write, Kind::User));
        assert!(a.check(&admin, Action::Read, Kind::Realm));
        assert!(a.check(&admin, Action::Write, Kind::Realm));
    }
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod limit;
pub mod resources;
```

을 다음으로 바꾼다:

```rust
pub mod limit;
pub mod realms;
pub mod resources;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(users::whoami))
        .routes(routes!(users::list_users, users::create_user))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(users::whoami))
        .routes(routes!(realms::list_login_realms))
        .routes(routes!(users::list_users, users::create_user))
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(users::delete_token))
        .routes(routes!(r::list_nodes))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(users::delete_token))
        .routes(routes!(realms::list_realms, realms::create_realm))
        .routes(routes!(
            realms::get_realm,
            realms::update_realm,
            realms::delete_realm
        ))
        .routes(routes!(r::list_nodes))
```

`crates/opl-api/src/realms.rs`를 만든다:

```rust
//! 인증 영역 `/api/v1/realms`와 로그인 화면이 읽는 `/api/v1/auth/realms`. 영역은 계정 종류처럼
//! admin만 읽고 바꾼다. 서비스 계정 비밀번호는 응답에 넣지 않고 `has_bind_password`만 보인다.
//! 영역은 watch로도 내보내지 않는다(비밀번호를 담고 있다).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::realm::{
    LdapRealm, Realm, RealmBackend, RealmSpec, RoleMapping, validate_realm_name,
};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, if_match, lookup};
use crate::state::AppState;

/// 영역 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되, `backend.bind_password`를 빼면 저장된 값을
/// 그대로 두고(`url`·`bind_dn`·`ca_pem`이 모두 그대로일 때만 — 하나라도 바뀌면 400), 빈 글이면
/// 지운다. `bind_dn`에는 비밀번호가 있어야 한다(익명 검색은 둘 다 없을 때). `url`·`bind_dn`은 앞뒤
/// 공백을 떼고 `ca_pem`은 CRLF를 LF로 바꿔 앞뒤 공백을 뗀 뒤 비교·저장한다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmRequest {
    /// 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    /// 로그인 화면에 보일 이름
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub display: Option<String>,
    pub backend: RealmBackend,
    #[serde(default)]
    pub roles: RoleMapping,
}

/// 응답의 영역. 서비스 계정 비밀번호 대신 있는지만 보인다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub display: Option<String>,
    pub backend: RealmBackendView,
    pub roles: RoleMapping,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmBackendView {
    Ldap(LdapRealmView),
}

/// [`LdapRealm`]에서 `bind_password`를 뺀 것
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LdapRealmView {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub bind_dn: Option<String>,
    pub base_dn: String,
    pub user_filter: String,
    pub group_attr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub ca_pem: Option<String>,
    pub timeout_secs: u64,
    /// 서비스 계정 비밀번호가 저장되어 있는지
    pub has_bind_password: bool,
}

impl From<Realm> for RealmView {
    fn from(r: Realm) -> Self {
        let RealmBackend::Ldap(l) = r.spec.backend;
        Self {
            id: r.id,
            name: r.name,
            revision: r.revision,
            display: r.spec.display,
            backend: RealmBackendView::Ldap(LdapRealmView {
                url: l.url,
                bind_dn: l.bind_dn,
                base_dn: l.base_dn,
                user_filter: l.user_filter,
                group_attr: l.group_attr,
                ca_pem: l.ca_pem,
                timeout_secs: l.timeout_secs,
                has_bind_password: l.bind_password.is_some(),
            }),
            roles: r.spec.roles,
        }
    }
}

/// 요청을 저장할 spec으로. `stored`는 바꿀 때의 지금 값이다(빠진 비밀번호를 채운다).
fn spec_of(req: &RealmRequest, stored: Option<&RealmSpec>) -> Result<RealmSpec, ApiError> {
    let RealmBackend::Ldap(mut ldap) = req.backend.clone();
    // 저장·비교 전에 다듬는다: 끝 줄바꿈이 있는 CA 파일과 웹이 다듬어 보낸 같은 CA를 같은 값으로 본다.
    ldap.url = ldap.url.trim().to_string();
    ldap.bind_dn = ldap.bind_dn.map(|dn| dn.trim().to_string());
    ldap.ca_pem = ldap
        .ca_pem
        .map(|pem| pem.replace("\r\n", "\n").trim().to_string());
    ldap.bind_password = match ldap.bind_password.take() {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(p),
        None => match stored.map(stored_ldap) {
            Some(old) if old.bind_password.is_some() => {
                // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다.
                if old.url != ldap.url || old.bind_dn != ldap.bind_dn || old.ca_pem != ldap.ca_pem {
                    return Err(ApiError::bad_request(
                        "bind_password must be given again when url, bind_dn or ca_pem changes",
                    ));
                }
                old.bind_password.clone()
            }
            _ => None,
        },
    };
    let spec = RealmSpec {
        display: req.display.clone().filter(|d| !d.trim().is_empty()),
        backend: RealmBackend::Ldap(ldap),
        roles: req.roles.clone(),
    };
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(spec)
}

fn stored_ldap(spec: &RealmSpec) -> &LdapRealm {
    let RealmBackend::Ldap(ldap) = &spec.backend;
    ldap
}

#[utoipa::path(get, path = "/realms", tag = "auth",
    responses((status = 200, body = List<RealmView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_realms(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<RealmView>>, ApiError> {
    state.authorize(&principal, Action::Read, RealmSpec::KIND)?;
    let (revision, realms) = state.cluster.list::<RealmSpec>()?;
    Ok(Json(List {
        revision,
        items: realms.into_iter().map(RealmView::from).collect(),
    }))
}

/// 이름은 리소스 이름 규칙이고 `local`은 쓸 수 없다. 나쁜 값은 400 `invalid_request`.
#[utoipa::path(post, path = "/realms", tag = "auth",
    request_body = RealmRequest,
    responses((status = 201, body = RealmView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<RealmRequest>,
) -> Result<(StatusCode, Json<RealmView>), ApiError> {
    state.authorize(&principal, Action::Write, RealmSpec::KIND)?;
    let name = req
        .name
        .clone()
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    check_name(&name)?;
    validate_realm_name(&name).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let spec = spec_of(&req, None)?;
    // 옛 노드는 새 종류를 풀지 못한다.
    crate::cluster::require_current(&state.cluster)?;
    let mut realm: Realm = Resource::new(name, spec);
    realm.revision = state.cluster.create(&realm).await?;
    Ok((StatusCode::CREATED, Json(realm.into())))
}

#[utoipa::path(get, path = "/realms/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RealmView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<RealmView>, ApiError> {
    state.authorize(&principal, Action::Read, RealmSpec::KIND)?;
    Ok(Json(lookup::<RealmSpec>(&state.cluster, &key)?.into()))
}

/// 영역을 통째로 바꾼다(`bind_password` 규칙은 [`RealmRequest`]). `If-Match`가 없으면 읽은
/// revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(put, path = "/realms/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = RealmRequest,
    responses((status = 200, body = RealmView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RealmRequest>,
) -> Result<Json<RealmView>, ApiError> {
    state.authorize(&principal, Action::Write, RealmSpec::KIND)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
        if let Some(name) = &req.name
            && *name != realm.name
        {
            return Err(ApiError::bad_request(
                "the name of a realm cannot be changed",
            ));
        }
        if let Some(expected) = expected
            && expected != realm.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: realm.revision,
            })
            .into());
        }
        let spec = spec_of(&req, Some(&realm.spec))?;
        if spec == realm.spec {
            return Ok(Json(realm.into()));
        }
        crate::cluster::require_current(&state.cluster)?;
        match state
            .cluster
            .update_spec(realm.id, Some(realm.revision), realm.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(
            lookup::<RealmSpec>(&state.cluster, &realm.id.to_string())?.into(),
        ));
    }
    Err(ApiError::conflict(
        "busy",
        "the realm kept changing while being updated; try again",
    ))
}

#[utoipa::path(delete, path = "/realms/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, RealmSpec::KIND)?;
    let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
    crate::cluster::require_current(&state.cluster)?;
    state
        .cluster
        .delete::<RealmSpec>(realm.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 로그인 화면에 보일 영역의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealmKind {
    /// 로컬 사용자
    Local,
    Ldap,
}

/// 로그인 화면에 보일 영역
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmSummary {
    pub name: String,
    /// 없으면 null
    #[schema(required = true)]
    pub display: Option<String>,
    pub kind: RealmKind,
}

/// 로그인할 수 있는 영역. 로그인 전에 읽는다. `local`이 먼저, 나머지는 이름 순이다.
#[utoipa::path(get, path = "/auth/realms", tag = "auth", security(()),
    responses((status = 200, body = Vec<RealmSummary>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_login_realms(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<RealmSummary>>, ApiError> {
    let (_, mut realms) = state.cluster.list::<RealmSpec>()?;
    realms.sort_by(|a, b| a.name.cmp(&b.name));
    let local = RealmSummary {
        name: opl_types::realm::LOCAL_REALM.into(),
        display: None,
        kind: RealmKind::Local,
    };
    let others = realms.into_iter().map(|r| RealmSummary {
        name: r.name,
        display: r.spec.display,
        kind: match r.spec.backend {
            RealmBackend::Ldap(_) => RealmKind::Ldap,
        },
    });
    Ok(Json(std::iter::once(local).chain(others).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_debug_hides_the_bind_password() {
        let req: RealmRequest = serde_json::from_value(serde_json::json!({
            "name": "corp",
            "backend": {
                "kind": "ldap",
                "url": "ldaps://h",
                "bind_dn": "cn=svc",
                "bind_password": "s3cret",
                "base_dn": "dc=x"
            }
        }))
        .unwrap();
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("***"), "{shown}");
    }
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/auth/login",
        "/version",
```

을 다음으로 바꾼다:

```rust
        "/auth/login",
        "/auth/realms",
        "/realms",
        "/realms/{key}",
        "/version",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "#/components/schemas/SettingsPatch"
    );
```

을 다음으로 바꾼다:

```rust
        "#/components/schemas/SettingsPatch"
    );
    // 영역 응답에는 서비스 계정 비밀번호가 없고 있는지만 보인다. 영역 목록은 로그인 전에 읽는다.
    let ldap_view = &doc["components"]["schemas"]["LdapRealmView"]["properties"];
    assert!(ldap_view["bind_password"].is_null(), "{ldap_view}");
    assert!(ldap_view["has_bind_password"].is_object(), "{ldap_view}");
    assert_eq!(
        doc["paths"]["/auth/realms"]["get"]["security"],
        serde_json::json!([{}])
    );
```

`crates/opl-api/tests/realms.rs`를 만든다:

```rust
//! 인증 영역 API: 만들기·읽기·바꾸기·지우기, admin만(읽기도), `local` 거절, 검증, 서비스 계정
//! 비밀번호를 내보내지 않음과 PUT의 bind_password 규칙, 옛 노드 409, 인증 없는 `/auth/realms`, 감사 로그.

mod common;

use std::time::Instant;

use common::{Env, WAIT};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::realm::{RealmBackend, RealmSpec};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 서비스 계정을 쓰는 LDAP 영역 만들기 본문
fn corp() -> Value {
    json!({
        "name": "corp",
        "display": "Corp directory",
        "backend": {
            "kind": "ldap",
            "url": "ldaps://ldap.example.com",
            "bind_dn": "cn=svc,dc=example,dc=com",
            "bind_password": "svc-secret",
            "base_dn": "dc=example,dc=com"
        },
        "roles": {
            "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
            "default": "viewer"
        }
    })
}

/// 저장된 서비스 계정 비밀번호
fn stored_password(env: &Env, name: &str) -> Option<String> {
    let realm = env.cluster.get_by_name::<RealmSpec>(name).unwrap().unwrap();
    let RealmBackend::Ldap(ldap) = realm.spec.backend;
    ldap.bind_password
}

#[tokio::test]
async fn realms_are_created_read_updated_and_deleted() {
    let env = Env::start().await;
    let admin = env.admin().await;

    let (status, created) = admin.post("/realms", corp()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["name"], "corp");
    assert_eq!(created["display"], "Corp directory");
    // 비밀번호는 응답에 없고, 있는지만 보인다. 빠진 값은 기본값으로 채운다.
    assert_eq!(
        created["backend"],
        json!({
            "kind": "ldap",
            "url": "ldaps://ldap.example.com",
            "bind_dn": "cn=svc,dc=example,dc=com",
            "base_dn": "dc=example,dc=com",
            "user_filter": "(&(objectClass=person)(uid={username}))",
            "group_attr": "memberOf",
            "timeout_secs": 10,
            "has_bind_password": true
        })
    );
    assert_eq!(created["roles"], corp()["roles"]);
    assert!(!created.to_string().contains("svc-secret"));
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    let (status, got) = admin.get("/realms/corp").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got, created);
    let id = created["id"].as_str().unwrap();
    assert_eq!(admin.get(&format!("/realms/{id}")).await.1, created);

    let (status, list) = admin.get("/realms").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"], json!([created]));
    assert!(list["revision"].as_u64().unwrap() >= created["revision"].as_u64().unwrap());

    // PUT은 통째로 바꾼다(비밀번호는 빼면 그대로).
    let mut body = corp();
    body.as_object_mut().unwrap().remove("name");
    body["display"] = json!("Corp");
    body["backend"]["timeout_secs"] = json!(5);
    body["backend"]
        .as_object_mut()
        .unwrap()
        .remove("bind_password");
    body["roles"] = json!({ "groups": [] });
    let revision = created["revision"].as_u64();
    let (status, updated) = admin.put("/realms/corp", body.clone(), revision).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["display"], "Corp");
    assert_eq!(updated["backend"]["timeout_secs"], 5);
    assert_eq!(updated["backend"]["has_bind_password"], true);
    assert_eq!(updated["roles"], json!({ "groups": [] }));
    assert!(updated["revision"].as_u64() > revision);
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    // 옛 revision으로는 바꾸지 않는다.
    assert_error(
        &admin.put("/realms/corp", body.clone(), revision).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );

    // 같은 이름을 주는 것은 괜찮고, 다른 이름은 400이다.
    body["name"] = json!("corp");
    assert_eq!(
        admin.put("/realms/corp", body.clone(), None).await.0,
        StatusCode::OK
    );
    body["name"] = json!("other");
    assert_error(
        &admin.put("/realms/corp", body, None).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );

    // 이미 있는 이름
    let got = admin.post("/realms", corp()).await;
    assert_eq!(got.0, StatusCode::CONFLICT, "{}", got.1);

    let (status, body) = admin.delete("/realms/corp").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_error(
        &admin.get("/realms/corp").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));
    assert_error(
        &admin.delete("/realms/corp").await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// PUT에서 bind_password를 빼면 url·bind_dn·ca_pem이 모두 그대로일 때만 저장된 값을 두고(하나라도
/// 바뀌면 400 — 비밀번호를 다른 서버로 보내지 않게), 빈 글이면 지우고, 값이면 바꾼다. 서비스 계정
/// DN에는 비밀번호가 있어야 하고, 익명 검색은 둘 다 없을 때만이다.
#[tokio::test]
async fn the_bind_password_is_kept_replaced_or_cleared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut body = corp();
    body.as_object_mut().unwrap().remove("name");
    let without_password = |f: &dyn Fn(&mut Value)| {
        let mut b = body.clone();
        b["backend"]
            .as_object_mut()
            .unwrap()
            .remove("bind_password");
        f(&mut b);
        b
    };

    // 연결 대상이 같으면 생략은 그대로 둔다.
    let (status, got) = admin
        .put(
            "/realms/corp",
            without_password(&|b| b["display"] = json!("Corp")),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], true);
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    // url·bind_dn·ca_pem 가운데 하나라도 바꾸면서 생략하면 400이고 아무것도 바뀌지 않는다.
    for changed in [
        without_password(&|b| b["backend"]["url"] = json!("ldaps://evil.example.com")),
        without_password(&|b| b["backend"]["bind_dn"] = json!("cn=other,dc=example,dc=com")),
        without_password(&|b| b["backend"]["ca_pem"] = json!("-----BEGIN CERTIFICATE-----")),
        without_password(&|b| {
            b["backend"].as_object_mut().unwrap().remove("bind_dn");
        }),
    ] {
        let got = admin.put("/realms/corp", changed.clone(), None).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert_eq!(
            got.1["message"],
            "bind_password must be given again when url, bind_dn or ca_pem changes",
            "{changed}"
        );
    }
    let (_, got) = admin.get("/realms/corp").await;
    assert_eq!(got["backend"]["url"], "ldaps://ldap.example.com");
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));

    // 바꾸면서 새 비밀번호를 주면 된다.
    let mut moved = body.clone();
    moved["backend"]["url"] = json!("ldaps://ldap2.example.com");
    moved["backend"]["bind_password"] = json!("new-secret");
    let (status, got) = admin.put("/realms/corp", moved.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["url"], "ldaps://ldap2.example.com");
    assert_eq!(got["backend"]["has_bind_password"], true);
    assert!(!got.to_string().contains("new-secret"));
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("new-secret"));

    // 빈 글은 지우는 것이라 bind_dn이 남아 있으면 400이다.
    moved["backend"]["bind_password"] = json!("");
    let got = admin.put("/realms/corp", moved.clone(), None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_eq!(
        got.1["message"],
        "bind_dn needs bind_password; remove bind_dn for an anonymous search"
    );
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("new-secret"));

    // bind_dn도 빼면 익명 검색이 된다.
    moved["backend"].as_object_mut().unwrap().remove("bind_dn");
    let (status, got) = admin.put("/realms/corp", moved.clone(), None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], false);
    assert!(got["backend"].get("bind_dn").is_none(), "{got}");
    assert_eq!(stored_password(&env, "corp"), None);

    // 저장된 비밀번호가 없으면 생략은 그대로 익명이다(url을 바꿔도).
    moved["backend"]
        .as_object_mut()
        .unwrap()
        .remove("bind_password");
    moved["backend"]["url"] = json!("ldaps://ldap3.example.com");
    let (status, got) = admin.put("/realms/corp", moved, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], false);

    // 만들 때도 bind_dn에는 비밀번호가 있어야 하고, 둘 다 없으면 익명이다.
    let mut anon = corp();
    anon["name"] = json!("anon");
    anon["backend"]["bind_password"] = json!("");
    assert_error(
        &admin.post("/realms", anon.clone()).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    anon["backend"].as_object_mut().unwrap().remove("bind_dn");
    let (status, got) = admin.post("/realms", anon).await;
    assert_eq!(status, StatusCode::CREATED, "{got}");
    assert_eq!(got["backend"]["has_bind_password"], false);
    assert_eq!(stored_password(&env, "anon"), None);
}

/// 서버가 url·bind_dn의 앞뒤 공백을 떼고 CA의 CRLF를 LF로 바꿔 앞뒤 공백을 뗀 값을 저장·비교한다.
/// 그래서 끝 줄바꿈이 있는 CA(`oplctl --ca-file`)로 만든 영역을 다듬은 같은 CA(웹)로 바꿔도 비밀번호가 남는다.
#[tokio::test]
async fn urls_dns_and_cas_are_trimmed_before_they_are_kept_or_compared() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ca = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
    for (name, sent_ca) in [
        ("lf", format!("{ca}\n")),
        ("crlf", format!("{}\r\n", ca.replace('\n', "\r\n"))),
    ] {
        let mut body = corp();
        body["name"] = json!(name);
        body["backend"]["url"] = json!(" ldaps://ldap.example.com\n");
        body["backend"]["bind_dn"] = json!(" cn=svc,dc=example,dc=com ");
        body["backend"]["ca_pem"] = json!(sent_ca);
        let (status, got) = admin.post("/realms", body).await;
        assert_eq!(status, StatusCode::CREATED, "{got}");
        assert_eq!(got["backend"]["url"], "ldaps://ldap.example.com");
        assert_eq!(got["backend"]["bind_dn"], "cn=svc,dc=example,dc=com");
        assert_eq!(got["backend"]["ca_pem"], ca, "{name}");

        let mut body = corp();
        body.as_object_mut().unwrap().remove("name");
        body["backend"]
            .as_object_mut()
            .unwrap()
            .remove("bind_password");
        body["backend"]["ca_pem"] = json!(ca);
        body["display"] = json!("Trimmed");
        let (status, got) = admin.put(&format!("/realms/{name}"), body, None).await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["backend"]["has_bind_password"], true);
        assert_eq!(stored_password(&env, name).as_deref(), Some("svc-secret"));
    }
}

/// 계정 종류처럼 admin만 읽고 바꾼다.
#[tokio::test]
async fn only_admins_read_or_change_realms() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut body = corp();
    body.as_object_mut().unwrap().remove("name");
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        let mut other = corp();
        other["name"] = json!(format!("by-{role}"));
        for got in [
            client.get("/realms").await,
            client.get("/realms/corp").await,
            client.post("/realms", other).await,
            client.put("/realms/corp", body.clone(), None).await,
            client.delete("/realms/corp").await,
        ] {
            assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
        }
    }
    assert_error(
        &env.anonymous().get("/realms").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    let (_, list) = admin.get("/realms").await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(stored_password(&env, "corp").as_deref(), Some("svc-secret"));
}

#[tokio::test]
async fn bad_realms_are_rejected() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let with = |f: &dyn Fn(&mut Value)| {
        let mut body = corp();
        f(&mut body);
        body
    };
    for (body, message) in [
        (
            with(&|b| b["name"] = json!("local")),
            "the realm name \"local\" is reserved for local users",
        ),
        (with(&|b| b["name"] = json!("Corp")), ""),
        (
            with(&|b| b["name"] = json!("0192f0c1-5d2e-7000-8000-000000000000")),
            "",
        ),
        (
            with(&|b| {
                b.as_object_mut().unwrap().remove("name");
            }),
            "name is required",
        ),
        (
            with(&|b| b["backend"]["url"] = json!("https://ldap.example.com")),
            "url must be ldap://host[:port] or ldaps://host[:port]",
        ),
        (
            with(&|b| b["backend"]["base_dn"] = json!(" ")),
            "base_dn must not be empty",
        ),
        (
            with(&|b| b["backend"]["user_filter"] = json!("(uid=alice)")),
            "user_filter must contain {username} exactly once",
        ),
        (
            with(&|b| b["backend"]["timeout_secs"] = json!(0)),
            "timeout_secs must be between 1 and 30",
        ),
        (
            with(&|b| {
                b["backend"].as_object_mut().unwrap().remove("bind_dn");
            }),
            "bind_password needs bind_dn",
        ),
        (
            with(&|b| b["roles"]["groups"][0]["group"] = json!("")),
            "roles.groups[].group must not be empty",
        ),
        // 모르는 필드와 종류
        (with(&|b| b["bogus"] = json!(1)), ""),
        (with(&|b| b["backend"]["kind"] = json!("oidc")), ""),
        (with(&|b| b["backend"]["bogus"] = json!(1)), ""),
    ] {
        let got = admin.post("/realms", body.clone()).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        if !message.is_empty() {
            assert_eq!(got.1["message"], message, "{body}");
        }
    }
    assert_eq!(admin.get("/realms").await.1["items"], json!([]));

    // 바꿀 때도 검증한다.
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut body = corp();
    body["backend"]["url"] = json!("ldap://");
    let got = admin.put("/realms/corp", body, None).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert_error(
        &admin.put("/realms/nope", corp(), None).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

/// 이 빌드보다 낮거나 버전을 적지 않은 노드가 있으면 영역을 쓰지 않는다(옛 노드는 새 종류를 풀지 못한다).
#[tokio::test]
async fn realms_wait_until_every_node_runs_this_build() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();

    let mut other = corp();
    other["name"] = json!("other");
    let mut body = corp();
    body["display"] = json!("changed");
    for got in [
        admin.post("/realms", other).await,
        admin.put("/realms/corp", body, None).await,
        admin.delete("/realms/corp").await,
    ] {
        assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    }
    // 읽기는 된다.
    assert_eq!(
        admin.get("/realms/corp").await.1["display"],
        "Corp directory"
    );
    assert_eq!(
        admin.get("/realms").await.1["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// 로그인 화면이 읽는 목록: 인증 없이, `local`이 먼저, 나머지는 이름 순, 비밀은 없음.
#[tokio::test]
async fn the_login_screen_lists_realms_without_login() {
    let env = Env::start().await;
    let anonymous = env.anonymous();
    let (status, got) = anonymous.get("/auth/realms").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!([{ "name": "local", "display": null, "kind": "local" }])
    );

    let admin = env.admin().await;
    for (name, display) in [("zeta", Some("Zeta")), ("corp", None), ("ad", Some("AD"))] {
        let mut body = corp();
        body["name"] = json!(name);
        match display {
            Some(d) => body["display"] = json!(d),
            None => {
                body.as_object_mut().unwrap().remove("display");
            }
        }
        let (status, got) = admin.post("/realms", body).await;
        assert_eq!(status, StatusCode::CREATED, "{got}");
    }
    let (status, got) = anonymous.get("/auth/realms").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(
        got,
        json!([
            { "name": "local", "display": null, "kind": "local" },
            { "name": "ad", "display": "AD", "kind": "ldap" },
            { "name": "corp", "display": null, "kind": "ldap" },
            { "name": "zeta", "display": "Zeta", "kind": "ldap" },
        ])
    );
}

/// 영역을 바꾸는 요청은 감사 미들웨어가 적는다(본문의 비밀번호는 적지 않는다).
#[tokio::test]
async fn realm_changes_are_audited() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_eq!(admin.post("/realms", corp()).await.0, StatusCode::CREATED);
    assert_eq!(admin.delete("/realms/corp").await.0, StatusCode::NO_CONTENT);
    let start = Instant::now();
    let entries = loop {
        let entries: Vec<AuditEntry> = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        if entries.iter().any(|e| e.action == "DELETE /realms/{key}") {
            break entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let created = entries
        .iter()
        .find(|e| e.action == "POST /realms" && e.status == 201)
        .expect("POST /realms is recorded");
    assert_eq!(created.user.as_deref(), Some("admin"));
    let deleted = entries
        .iter()
        .find(|e| e.action == "DELETE /realms/{key}")
        .unwrap();
    assert_eq!(deleted.status, 204);
    assert_eq!(deleted.target.as_ref().unwrap().key, "corp");
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains("svc-secret"), "{text}");
}
```

`crates/opl-types/src/realm.rs` 전체:

```rust
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RealmBackend {
    Ldap(LdapRealm),
}

/// LDAP 디렉터리. `ldap://`는 StartTLS를 반드시 쓰고, `ldaps://`는 처음부터 TLS다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct LdapRealm {
    /// `ldap://host[:port]` 또는 `ldaps://host[:port]`
    pub url: String,
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub bind_dn: Option<String>,
    /// 서비스 계정 비밀번호. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
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
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
    /// 연결과 작업마다의 시간 제한(초)
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

// 서비스 계정 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for LdapRealm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LdapRealm")
            .field("url", &self.url)
            .field("bind_dn", &self.bind_dn)
            .field("bind_password", &self.bind_password.as_ref().map(|_| "***"))
            .field("base_dn", &self.base_dn)
            .field("user_filter", &self.user_filter)
            .field("group_attr", &self.group_attr)
            .field("ca_pem", &self.ca_pem)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RoleMapping {
    #[serde(default)]
    pub groups: Vec<GroupRole>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 로그인을 거절한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub default: Option<Role>,
}

/// 이 그룹(보통 DN)에 든 사용자는 이 역할
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    fn debug_hides_the_bind_password() {
        let l = LdapRealm {
            bind_dn: Some("cn=svc".into()),
            bind_password: Some("s3cret".into()),
            ..ldap()
        };
        let shown = format!("{l:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains(r#"bind_password: Some("***")"#), "{shown}");
        assert!(shown.contains(r#"bind_dn: Some("cn=svc")"#), "{shown}");
        assert!(shown.contains("timeout_secs: 10"), "{shown}");
        let shown = format!("{:?}", spec(l));
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(format!("{:?}", ldap()).contains("bind_password: None"));
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
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("admin", "write", "join-token")).toBe(true);
  for (const role of ["operator", "viewer"] as const) {
```

을 다음으로 바꾼다:

```ts
  expect(can("admin", "write", "join-token")).toBe(true);
  expect(can("admin", "write", "realm")).toBe(true);
  for (const role of ["operator", "viewer"] as const) {
```

`web/src/auth/permissions.test.ts`에서

```ts
    expect(can(role, "write", "join-token")).toBe(false);
  }
```

을 다음으로 바꾼다:

```ts
    expect(can(role, "write", "join-token")).toBe(false);
    // 인증 영역도 계정 종류다(서비스 계정 비밀번호를 다룬다).
    expect(can(role, "read", "realm")).toBe(false);
    expect(can(role, "write", "realm")).toBe(false);
  }
```

`web/src/auth/permissions.ts`에서

```ts
/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind = WatchKind | "settings" | "user" | "api-token" | "join-token";

/** 계정 종류. admin이 아니면 읽지도 못한다(자기 토큰과 비밀번호는 예외로 서버가 따로 허락한다). */
const ACCOUNT: ReadonlySet<Kind> = new Set(["user", "api-token", "join-token"]);
```

을 다음으로 바꾼다:

```ts
/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind = WatchKind | "settings" | "user" | "api-token" | "join-token" | "realm";

/**
 * 계정 종류와 인증 영역. admin이 아니면 읽지도 못한다(자기 토큰과 비밀번호는 예외로 서버가 따로 허락한다).
 */
const ACCOUNT: ReadonlySet<Kind> = new Set(["user", "api-token", "join-token", "realm"]);
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1313개, 실패 0

Run: `pnpm -C web typecheck && pnpm -C web test`
Expected: 모두 통과

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts web/src/auth
git commit -m "feat(api): 인증 영역 API — opl-api realms.rs: GET/POST /api/v1/realms, GET/PUT/DELETE /api/v1/realms/{key}(RealmRequest{name(만들 때), display, backend, roles}; PUT은 통째로 바꾸고 If-Match·재시도는 보안 그룹과 같음; backend.bind_password를 빼면 저장된 값을 두고(bind_dn이 있을 때) \"\"이면 지움), 응답 RealmView는 bind_password 대신 has_bind_password, 쓰기는 모두 require_current 뒤(409 cluster_upgrading), 이름 local·검증 실패는 400 invalid_request. 인증 없는 GET /api/v1/auth/realms([{name, display, kind}], local 먼저, 나머지 이름 순). RoleAuthorizer와 웹 permissions.ts가 realm을 계정 종류로(읽기·쓰기 admin만), watch에는 넣지 않음. opl-types realm 타입에 openapi ToSchema, schema.d.ts 다시 만듦"
```

---

### Task 3: LDAP 클라이언트와 영역 시험 동작

새 의존 `ldap3` 0.12.1(rustls-ring, MIT/Apache)과 시험용 `lber`. `opl_api::ldap::authenticate`: `ldap://`는 StartTLS 필수, CA PEM이 있으면 그것만 믿음, 서비스 바인드 → `{username}`을 이스케이프한 필터로 검색(결과 2개에서 끊음) → 찾은 DN으로 사용자 바인드 → 그룹. 빈 비밀번호는 연결하지 않고 거절, 실패 응답은 최소 300ms, 전체 마감. `POST /api/v1/realms/{key}/actions/test`가 단계별 결과를 보인다. 시험은 `lber`로 만든 가짜 LDAP 서버(LDAPS와 StartTLS)로 한다.

**Files:**
- Create: `crates/opl-api/src/ldap.rs`
- Create: `crates/opl-api/tests/common/ldap.rs`
- Create: `crates/opl-api/tests/ldap.rs`
- Modify: `Cargo.toml`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/realms.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`Cargo.toml`에서

```toml
libc = "0.2.191"
```

을 다음으로 바꾼다:

```toml
libc = "0.2.191"
ldap3 = { version = "0.12.1", default-features = false, features = ["tls-rustls-ring"] }
lber = "0.5.1"
```

`crates/opl-api/Cargo.toml`에서

```toml
hyper-util.workspace = true
libc.workspace = true
```

을 다음으로 바꾼다:

```toml
hyper-util.workspace = true
ldap3.workspace = true
libc.workspace = true
```

`crates/opl-api/Cargo.toml`에서

```toml
[dev-dependencies]
openraft.workspace = true
```

을 다음으로 바꾼다:

```toml
[dev-dependencies]
# 시험용 가짜 LDAP 서버의 BER
bytes.workspace = true
lber.workspace = true
openraft.workspace = true
```

`crates/opl-api/src/ldap.rs`를 만든다:

```rust
//! LDAP 영역의 사용자 확인. 서비스 계정(없으면 익명)으로 사용자 항목을 찾고, 그 DN과 비밀번호로
//! 다시 바인드해 확인한 뒤 그룹 속성 값을 읽는다. `ldap://`는 반드시 StartTLS를 쓰고
//! `ldaps://`는 처음부터 TLS다. 평문으로는 바인드하지 않는다.

use std::sync::Arc;
use std::time::Duration;

use ldap3::adapters::EntriesOnly;
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry, SearchOptions};
use opl_types::realm::{LdapRealm, RealmSpec};
use rustls::RootCertStore;
use tokio::time::Instant;

use crate::realms::{RealmTestResult, RealmTestStep};

/// 잘못된 비밀번호의 LDAP 결과 코드
const INVALID_CREDENTIALS: u32 = 49;
/// 검색 결과가 크기 제한을 넘었다는 LDAP 결과 코드
const SIZE_LIMIT_EXCEEDED: u32 = 4;
/// 사용자 확인이 실패할 때 걸리는 가장 짧은 시간. 없는 사용자와 틀린 비밀번호의 응답 시간 차로
/// 사용자가 있는지 알아내지 못하게 한다.
const MIN_FAILURE: Duration = Duration::from_millis(300);

/// 확인된 사용자
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdapUser {
    pub dn: String,
    /// 사용자 항목의 그룹 속성 값들
    pub groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LdapError {
    /// 사용자가 없거나 여럿이거나 비밀번호가 틀렸다.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// 디렉터리에 닿지 못했거나 서비스 계정 바인드·검색이 실패했다.
    #[error("{0}")]
    Unavailable(String),
}

/// 시험 동작의 단계 이름
pub const STEP_CONNECT: &str = "connect";
pub const STEP_SERVICE_BIND: &str = "service_bind";
pub const STEP_SEARCH: &str = "search";
pub const STEP_USER_BIND: &str = "user_bind";
pub const STEP_GROUPS: &str = "groups";

/// `username`과 `password`를 영역의 디렉터리로 확인한다. 빈 비밀번호는 연결하지 않고 거절한다
/// (빈 비밀번호의 단순 바인드는 많은 서버가 익명으로 받아 준다). 전체가 마감(`deadline`) 안에 끝나고,
/// `InvalidCredentials`는 시작부터 적어도 `MIN_FAILURE`(300ms) 뒤에 돌려준다.
pub async fn authenticate(
    realm: &LdapRealm,
    username: &str,
    password: &str,
) -> Result<LdapUser, LdapError> {
    let started = Instant::now();
    let result = if password.is_empty() {
        Err(LdapError::InvalidCredentials)
    } else {
        tokio::time::timeout(
            deadline(realm),
            run(realm, username, password, &mut Trace::default()),
        )
        .await
        .unwrap_or_else(|_| Err(timed_out()))
    };
    if result == Err(LdapError::InvalidCredentials) {
        tokio::time::sleep_until(started + MIN_FAILURE).await;
    }
    result
}

/// [`authenticate`]와 같은 흐름을 단계마다 기록한다. 실패한 단계에서 멈춘다. 사용자도 세션도
/// 만들지 않고, 비밀번호는 결과에 넣지 않는다.
pub async fn test(spec: &RealmSpec, username: &str, password: &str) -> RealmTestResult {
    let opl_types::realm::RealmBackend::Ldap(realm) = &spec.backend;
    let mut trace = Trace::default();
    let result =
        tokio::time::timeout(deadline(realm), run(realm, username, password, &mut trace)).await;
    let result = result.unwrap_or_else(|_| {
        // 마감에 걸린 단계를 실패로 적는다.
        let step = [
            STEP_CONNECT,
            STEP_SERVICE_BIND,
            STEP_SEARCH,
            STEP_USER_BIND,
            STEP_GROUPS,
        ]
        .get(trace.steps.len())
        .copied()
        .unwrap_or(STEP_GROUPS);
        Err(trace.fail(step, timed_out()))
    });
    let (groups, role) = match result {
        Ok(user) => {
            let role = spec.roles.role_for(&user.groups);
            (user.groups, role)
        }
        Err(_) => (Vec::new(), None),
    };
    RealmTestResult {
        steps: trace.steps,
        dn: trace.dn,
        groups,
        role,
    }
}

/// 단계 결과와 찾은 DN
#[derive(Default)]
struct Trace {
    steps: Vec<RealmTestStep>,
    dn: Option<String>,
}

impl Trace {
    fn ok(&mut self, name: &str, message: impl Into<String>) {
        self.steps.push(RealmTestStep {
            name: name.into(),
            ok: true,
            message: message.into(),
        });
    }

    /// 실패한 단계를 기록하고 그 오류를 돌려준다.
    fn fail(&mut self, name: &str, error: LdapError) -> LdapError {
        self.steps.push(RealmTestStep {
            name: name.into(),
            ok: false,
            message: error.to_string(),
        });
        error
    }
}

async fn run(
    realm: &LdapRealm,
    username: &str,
    password: &str,
    trace: &mut Trace,
) -> Result<LdapUser, LdapError> {
    let mut ldap = match connect(realm).await {
        Ok(ldap) => ldap,
        Err(e) => return Err(trace.fail(STEP_CONNECT, e)),
    };
    trace.ok(
        STEP_CONNECT,
        if realm.url.starts_with("ldaps://") {
            format!("connected to {} over TLS", realm.url)
        } else {
            format!("connected to {} with StartTLS", realm.url)
        },
    );
    let result = steps(&mut ldap, realm, username, password, trace).await;
    let _ = ldap.with_timeout(timeout(realm)).unbind().await;
    result
}

/// 연결한 뒤의 단계: 서비스 바인드, 검색, 사용자 바인드, 그룹
async fn steps(
    ldap: &mut Ldap,
    realm: &LdapRealm,
    username: &str,
    password: &str,
    trace: &mut Trace,
) -> Result<LdapUser, LdapError> {
    let limit = timeout(realm);
    match &realm.bind_dn {
        Some(dn) => {
            let pw = realm.bind_password.as_deref().unwrap_or("");
            match ldap.with_timeout(limit).simple_bind(dn, pw).await {
                Ok(r) if r.rc == 0 => trace.ok(STEP_SERVICE_BIND, format!("bound as {dn}")),
                Ok(r) => {
                    let e = unavailable(format!(
                        "the service account bind failed: {} ({})",
                        r.text, r.rc
                    ));
                    return Err(trace.fail(STEP_SERVICE_BIND, e));
                }
                Err(e) => return Err(trace.fail(STEP_SERVICE_BIND, unavailable(e))),
            }
        }
        None => trace.ok(
            STEP_SERVICE_BIND,
            "no service account; searching anonymously",
        ),
    }

    let filter = realm
        .user_filter
        .replace("{username}", &ldap3::ldap_escape(username));
    // 둘이면 이미 하나가 아니다. 서버가 크기 제한을 지키지 않아도 둘에서 멈춘다.
    let search = ldap
        .with_search_options(
            SearchOptions::new()
                .sizelimit(2)
                .timelimit(i32::try_from(realm.timeout_secs).unwrap_or(i32::MAX)),
        )
        .with_timeout(limit)
        .streaming_search_with(
            // 참조(referral)와 중간 응답은 항목이 아니다.
            EntriesOnly::new(),
            &realm.base_dn,
            Scope::Subtree,
            &filter,
            vec![realm.group_attr.as_str()],
        )
        .await;
    let mut stream = match search {
        Ok(stream) => stream,
        Err(e) => {
            let e = unavailable(format!("the search failed: {e}"));
            return Err(trace.fail(STEP_SEARCH, e));
        }
    };
    let mut entries: Vec<SearchEntry> = Vec::new();
    while entries.len() < 2 {
        match stream.next().await {
            Ok(Some(entry)) => entries.push(SearchEntry::construct(entry)),
            Ok(None) => break,
            Err(e) => {
                let e = unavailable(format!("the search failed: {e}"));
                return Err(trace.fail(STEP_SEARCH, e));
            }
        }
    }
    let id = stream.ldap_handle().last_id();
    let done = stream.finish().await;
    let more = entries.len() >= 2 || done.rc == SIZE_LIMIT_EXCEEDED;
    if entries.len() >= 2 {
        // 남은 결과는 받지 않는다.
        let _ = ldap.abandon(id).await;
    } else if done.rc != 0 && !more {
        let e = unavailable(format!("the search failed: {} ({})", done.text, done.rc));
        return Err(trace.fail(STEP_SEARCH, e));
    }
    if more || entries.is_empty() {
        trace.steps.push(RealmTestStep {
            name: STEP_SEARCH.into(),
            ok: false,
            message: if more {
                "more than one entry matches the user".into()
            } else {
                "no entry matches the user".into()
            },
        });
        return Err(LdapError::InvalidCredentials);
    }
    let entry = entries.remove(0);
    trace.ok(STEP_SEARCH, format!("found {}", entry.dn));
    trace.dn = Some(entry.dn.clone());

    if password.is_empty() {
        return Err(trace.fail(STEP_USER_BIND, LdapError::InvalidCredentials));
    }
    match ldap
        .with_timeout(limit)
        .simple_bind(&entry.dn, password)
        .await
    {
        Ok(r) if r.rc == 0 => trace.ok(STEP_USER_BIND, format!("bound as {}", entry.dn)),
        Ok(r) if r.rc == INVALID_CREDENTIALS => {
            return Err(trace.fail(STEP_USER_BIND, LdapError::InvalidCredentials));
        }
        Ok(r) => {
            let e = unavailable(format!("the user bind failed: {} ({})", r.text, r.rc));
            return Err(trace.fail(STEP_USER_BIND, e));
        }
        Err(e) => return Err(trace.fail(STEP_USER_BIND, unavailable(e))),
    }

    // 서버가 속성 이름의 대소문자를 바꿔 돌려줄 수 있다.
    let groups: Vec<String> = entry
        .attrs
        .into_iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(&realm.group_attr))
        .flat_map(|(_, values)| values)
        .collect();
    trace.ok(
        STEP_GROUPS,
        format!("{} value(s) of {}", groups.len(), realm.group_attr),
    );
    Ok(LdapUser {
        dn: entry.dn,
        groups,
    })
}

fn timeout(realm: &LdapRealm) -> Duration {
    Duration::from_secs(realm.timeout_secs)
}

/// 확인 전체의 마감: 연결·바인드·검색·바인드 넷의 시간 제한과 1초
fn deadline(realm: &LdapRealm) -> Duration {
    timeout(realm) * 4 + Duration::from_secs(1)
}

fn timed_out() -> LdapError {
    LdapError::Unavailable("timed out".into())
}

fn unavailable(e: impl ToString) -> LdapError {
    LdapError::Unavailable(e.to_string())
}

/// TLS로 연결한다(`ldap://`는 StartTLS). 서버 인증서는 `ca_pem`(없으면 시스템 루트)으로 확인하고
/// 호스트 이름도 맞춘다.
async fn connect(realm: &LdapRealm) -> Result<Ldap, LdapError> {
    let mut settings = LdapConnSettings::new()
        .set_conn_timeout(timeout(realm))
        .set_starttls(true);
    if let Some(pem) = &realm.ca_pem {
        settings = settings.set_config(Arc::new(client_config(pem)?));
    }
    let (conn, ldap) = LdapConnAsync::with_settings(settings, &realm.url)
        .await
        .map_err(|e| unavailable(format!("cannot connect to {}: {e}", realm.url)))?;
    ldap3::drive!(conn);
    Ok(ldap)
}

/// `ca_pem`의 인증서만 루트로 믿는 TLS 설정
fn client_config(pem: &str) -> Result<rustls::ClientConfig, LdapError> {
    let mut roots = RootCertStore::empty();
    let certs = opl_peer::pki::certs(pem).map_err(|e| unavailable(format!("the CA: {e}")))?;
    for cert in certs {
        roots
            .add(cert)
            .map_err(|e| unavailable(format!("the CA: {e}")))?;
    }
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(unavailable)?
    .with_root_certificates(roots)
    .with_no_client_auth())
}
```

`crates/opl-api/src/lib.rs`에서

```rust
mod janitor;
pub mod limit;
```

을 다음으로 바꾼다:

```rust
mod janitor;
pub mod ldap;
pub mod limit;
```

`crates/opl-api/src/lib.rs`에서

```rust
        ))
        .routes(routes!(r::list_nodes))
```

을 다음으로 바꾼다:

```rust
        ))
        .routes(routes!(realms::test_realm))
        .routes(routes!(r::list_nodes))
```

`crates/opl-api/src/realms.rs`에서

```rust
use opl_cluster::types::Rejection;
use opl_types::realm::{
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::Rejection;
use opl_types::auth::Role;
use opl_types::realm::{
```

`crates/opl-api/src/realms.rs`에서

```rust

/// 로그인 화면에 보일 영역의 종류
```

을 다음으로 바꾼다:

```rust

/// 영역 시험 본문
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RealmTestRequest {
    pub username: String,
    pub password: String,
}

// 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for RealmTestRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealmTestRequest")
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

/// 영역 시험의 결과. 단계는 `connect`, `service_bind`, `search`, `user_bind`, `groups` 순이고
/// 실패한 단계에서 멈춘다.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestResult {
    pub steps: Vec<RealmTestStep>,
    /// 찾은 사용자 항목. 찾기 전에 멈췄으면 null
    #[schema(required = true)]
    pub dn: Option<String>,
    /// 모든 단계가 성공했을 때의 그룹 속성 값들
    pub groups: Vec<String>,
    /// 그룹에서 정해질 역할. 실패했거나 맞는 매핑이 없으면 null
    #[schema(required = true)]
    pub role: Option<Role>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RealmTestStep {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// 사용자 이름과 비밀번호로 영역의 디렉터리를 시험한다. 사용자도 세션도 만들지 않는다.
#[utoipa::path(post, path = "/realms/{key}/actions/test", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = RealmTestRequest,
    responses((status = 200, body = RealmTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn test_realm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<RealmTestRequest>,
) -> Result<Json<RealmTestResult>, ApiError> {
    state.authorize(&principal, Action::Operate, RealmSpec::KIND)?;
    let realm = lookup::<RealmSpec>(&state.cluster, &key)?;
    Ok(Json(
        crate::ldap::test(&realm.spec, &req.username, &req.password).await,
    ))
}

/// 로그인 화면에 보일 영역의 종류
```

`crates/opl-api/src/realms.rs`에서

```rust
        assert!(shown.contains("***"), "{shown}");
    }
}
```

을 다음으로 바꾼다:

```rust
        assert!(shown.contains("***"), "{shown}");
    }

    #[test]
    fn test_request_debug_hides_the_password() {
        let req = RealmTestRequest {
            username: "alice".into(),
            password: "s3cret".into(),
        };
        let shown = format!("{req:?}");
        assert!(!shown.contains("s3cret"), "{shown}");
        assert!(shown.contains("alice"), "{shown}");
    }
}
```

`crates/opl-api/tests/common/ldap.rs`를 만든다:

```rust
//! 시험용 가짜 LDAP 서버. `lber`로 BER을 읽고 쓴다. 단순 바인드, 검색(필터는 and·등호·있음만
//! 해석하고 나머지는 맞는 것이 없음), 언바인드, StartTLS 확장 동작만 안다. 시험 CA가 서명한
//! `127.0.0.1` 인증서로 처음부터 TLS(LDAPS)이거나, 평문으로 받아 StartTLS 뒤 TLS로 바꾼다.
//! StartTLS 모드에서 TLS 전에 온 바인드·검색은 13(confidentialityRequired)으로 거절한다.
//! 검색의 크기 제한은 기본으로 지키지 않고(모든 항목을 보냄) 받은 값만 기록한다.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::BytesMut;
use lber::common::TagClass;
use lber::structure::{PL, StructureTag};
use lber::structures::{ASNTag, Enumerated, Integer, OctetString, Sequence, Set, Tag};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

const STARTTLS_OID: &str = "1.3.6.1.4.1.1466.20037";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 처음부터 TLS(`ldaps://`)
    Ldaps,
    /// 평문으로 받아 StartTLS 뒤 TLS(`ldap://`)
    StartTls,
    /// 평문으로 받고 StartTLS를 2(protocolError)로 거절한다(`ldap://`).
    StartTlsRefused,
}

/// 디렉터리의 사용자 항목(`objectClass: person`)
#[derive(Debug, Clone)]
pub struct Person {
    pub uid: String,
    pub dn: String,
    pub password: String,
    /// `memberOf` 값들
    pub groups: Vec<String>,
}

impl Person {
    /// `uid=<uid>,ou=people,dc=example,dc=com`
    pub fn new(uid: &str, password: &str, groups: &[&str]) -> Self {
        Person {
            uid: uid.into(),
            dn: format!("uid={uid},ou=people,{BASE_DN}"),
            password: password.into(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }
}

pub const BASE_DN: &str = "dc=example,dc=com";
pub const SERVICE_DN: &str = "cn=svc,dc=example,dc=com";
pub const SERVICE_PASSWORD: &str = "svc-secret";

#[derive(Debug, Clone)]
pub struct Directory {
    /// 서비스 계정 (DN, 비밀번호)
    pub service: Option<(String, String)>,
    pub people: Vec<Person>,
}

impl Directory {
    /// [`SERVICE_DN`] 서비스 계정과 사람들
    pub fn with(people: Vec<Person>) -> Self {
        Directory {
            service: Some((SERVICE_DN.into(), SERVICE_PASSWORD.into())),
            people,
        }
    }
}

#[derive(Default)]
struct Stats {
    connections: AtomicUsize,
    binds: AtomicUsize,
    searches: AtomicUsize,
    /// 검색 필터의 등호 조건(`attr=value`, 받은 그대로)
    assertions: Mutex<Vec<String>>,
    /// 검색 요청의 크기 제한
    size_limits: Mutex<Vec<i64>>,
    abandons: AtomicUsize,
}

/// 시험이 고르는 서버의 이상한 동작
#[derive(Default)]
struct Quirks {
    /// 검색 결과를 이만큼만 보내고 4(sizeLimitExceeded)로 끝낸다(서버 쪽 제한).
    truncate_at: Mutex<Option<usize>>,
    /// 검색에 항목 대신 참조를 끝없이 천천히 보낸다.
    endless_references: std::sync::atomic::AtomicBool,
}

struct Site {
    mode: Mode,
    directory: Directory,
    acceptor: TlsAcceptor,
    stats: Stats,
    quirks: Quirks,
}

pub struct FakeLdap {
    /// `ldaps://127.0.0.1:<port>` 또는 `ldap://127.0.0.1:<port>`
    pub url: String,
    /// 서버 인증서에 서명한 CA(PEM)
    pub ca_pem: String,
    site: Arc<Site>,
    server: JoinHandle<()>,
}

impl FakeLdap {
    pub async fn start(mode: Mode, directory: Directory) -> FakeLdap {
        let ca = ClusterCa::generate("ldap").unwrap();
        let (key_pem, csr) = node_key_and_csr().unwrap();
        let sans = ["127.0.0.1".to_string()];
        let tls = NodeTls {
            ca_pem: ca.cert_pem.clone(),
            cert_pem: ca.sign_node(&csr, uuid::Uuid::new_v4(), &sans).unwrap(),
            key_pem,
        };
        let config = opl_peer::tls::api_server_config(&tls).unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let site = Arc::new(Site {
            mode,
            directory,
            acceptor: TlsAcceptor::from(Arc::new(config)),
            stats: Stats::default(),
            quirks: Quirks::default(),
        });
        let s = site.clone();
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = tcp.accept().await {
                tokio::spawn(connection(stream, s.clone()));
            }
        });
        let scheme = match mode {
            Mode::Ldaps => "ldaps",
            Mode::StartTls | Mode::StartTlsRefused => "ldap",
        };
        FakeLdap {
            url: format!("{scheme}://{addr}"),
            ca_pem: ca.cert_pem,
            site,
            server,
        }
    }

    pub fn connections(&self) -> usize {
        self.site.stats.connections.load(Ordering::SeqCst)
    }

    pub fn binds(&self) -> usize {
        self.site.stats.binds.load(Ordering::SeqCst)
    }

    pub fn searches(&self) -> usize {
        self.site.stats.searches.load(Ordering::SeqCst)
    }

    /// 지금까지 받은 검색 요청의 크기 제한(0은 제한 없음)
    pub fn size_limits(&self) -> Vec<i64> {
        self.site.stats.size_limits.lock().unwrap().clone()
    }

    pub fn abandons(&self) -> usize {
        self.site.stats.abandons.load(Ordering::SeqCst)
    }

    /// 검색 결과를 `n`개만 보내고 4(sizeLimitExceeded)로 끝낸다.
    pub fn truncate_at(&self, n: usize) {
        *self.site.quirks.truncate_at.lock().unwrap() = Some(n);
    }

    /// 검색에 참조를 끝없이 보낸다(하나하나는 시간 제한 안에 온다).
    pub fn send_endless_references(&self) {
        self.site
            .quirks
            .endless_references
            .store(true, Ordering::SeqCst);
    }

    /// 지금까지 받은 검색 필터의 등호 조건(`attr=value`)
    pub fn assertions(&self) -> Vec<String> {
        self.site.stats.assertions.lock().unwrap().clone()
    }
}

impl Drop for FakeLdap {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn connection(tcp: TcpStream, site: Arc<Site>) {
    site.stats.connections.fetch_add(1, Ordering::SeqCst);
    match site.mode {
        Mode::Ldaps => {
            if let Ok(tls) = site.acceptor.accept(tcp).await {
                serve(tls, &site, true).await;
            }
        }
        Mode::StartTls | Mode::StartTlsRefused => {
            if let Some(tcp) = serve(tcp, &site, false).await
                && let Ok(tls) = site.acceptor.accept(tcp).await
            {
                serve(tls, &site, true).await;
            }
        }
    }
}

/// 요청을 처리한다. TLS가 아닐 때 StartTLS를 받으면 성공을 답하고 스트림을 돌려준다.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    site: &Site,
    tls: bool,
) -> Option<S> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let message = loop {
            match lber::parse::parse_tag(&buf) {
                Ok((rest, tag)) => {
                    let used = buf.len() - rest.len();
                    buf.drain(..used);
                    break tag;
                }
                Err(_) => {
                    let mut chunk = [0u8; 4096];
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return None,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
            }
        };
        let mut parts = message.expect_constructed()?.into_iter();
        let id = uint(&parts.next()?.expect_primitive()?);
        let op = parts.next()?;
        let replies = match (op.class, op.id) {
            (TagClass::Application, 0) => vec![bind(site, tls, op)],
            (TagClass::Application, 2) => return None,
            (TagClass::Application, 3) => {
                let replies = search(site, tls, op);
                if tls && site.quirks.endless_references.load(Ordering::SeqCst) {
                    // 연결이 끊길 때까지
                    loop {
                        write(&mut stream, id, reference()).await?;
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                }
                replies
            }
            (TagClass::Application, 16) => {
                site.stats.abandons.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            (TagClass::Application, 23) => {
                let oid = string(op.expect_constructed()?.into_iter().next()?);
                if oid == STARTTLS_OID && !tls && site.mode != Mode::StartTlsRefused {
                    write(&mut stream, id, extended(0, "")).await?;
                    return Some(stream);
                }
                vec![extended(2, "unsupported extended operation")]
            }
            _ => return None,
        };
        for reply in replies {
            write(&mut stream, id, reply).await?;
        }
    }
}

async fn write<S: AsyncWrite + Unpin>(stream: &mut S, id: u64, op: Tag) -> Option<()> {
    let message = Tag::Sequence(Sequence {
        inner: vec![
            Tag::Integer(Integer {
                inner: id as i64,
                ..Default::default()
            }),
            op,
        ],
        ..Default::default()
    });
    let mut out = BytesMut::new();
    lber::write::encode_into(&mut out, message.into_structure()).ok()?;
    stream.write_all(&out).await.ok()?;
    stream.flush().await.ok()
}

fn bind(site: &Site, tls: bool, op: StructureTag) -> Tag {
    site.stats.binds.fetch_add(1, Ordering::SeqCst);
    if !tls && site.mode != Mode::Ldaps {
        return response(1, 13, "TLS confidentiality required");
    }
    let mut parts = op.expect_constructed().unwrap().into_iter();
    let _version = parts.next();
    let name = string(parts.next().unwrap());
    let password = string(parts.next().unwrap());
    let directory = &site.directory;
    let ok = (name.is_empty() && password.is_empty())
        || directory
            .service
            .as_ref()
            .is_some_and(|(dn, pw)| dn.eq_ignore_ascii_case(&name) && *pw == password)
        || directory
            .people
            .iter()
            .any(|p| p.dn.eq_ignore_ascii_case(&name) && p.password == password);
    if ok {
        response(1, 0, "")
    } else {
        response(1, 49, "invalid credentials")
    }
}

fn search(site: &Site, tls: bool, op: StructureTag) -> Vec<Tag> {
    site.stats.searches.fetch_add(1, Ordering::SeqCst);
    if !tls && site.mode != Mode::Ldaps {
        return vec![response(5, 13, "TLS confidentiality required")];
    }
    let parts: Vec<StructureTag> = op.expect_constructed().unwrap();
    let base = string(parts[0].clone()).to_ascii_lowercase();
    let size_limit = uint(&parts[3].clone().expect_primitive().unwrap()) as i64;
    site.stats.size_limits.lock().unwrap().push(size_limit);
    let filter = parts[6].clone();
    let wanted: Vec<String> = parts[7]
        .clone()
        .expect_constructed()
        .unwrap()
        .into_iter()
        .map(string)
        .collect();
    record(&site.stats, &filter);
    let mut replies: Vec<Tag> = site
        .directory
        .people
        .iter()
        .filter(|p| p.dn.to_ascii_lowercase().ends_with(&base))
        .filter(|p| matches(&filter, p))
        .map(|p| entry(p, &wanted))
        .collect();
    match *site.quirks.truncate_at.lock().unwrap() {
        Some(n) if replies.len() > n => {
            replies.truncate(n);
            replies.push(response(5, 4, "size limit exceeded"));
        }
        _ => replies.push(response(5, 0, "")),
    }
    replies
}

/// 검색 결과 참조(SearchResultReference)
fn reference() -> Tag {
    Tag::Sequence(Sequence {
        class: TagClass::Application,
        id: 19,
        inner: vec![octets("ldaps://elsewhere.example.com/dc=example,dc=com")],
    })
}

/// 사람 항목의 속성 값들
fn values(p: &Person, attr: &str) -> Vec<String> {
    match attr.to_ascii_lowercase().as_str() {
        "objectclass" => vec!["person".into()],
        "uid" => vec![p.uid.clone()],
        "memberof" => p.groups.clone(),
        _ => Vec::new(),
    }
}

fn matches(filter: &StructureTag, p: &Person) -> bool {
    if filter.class != TagClass::Context {
        return false;
    }
    match (filter.id, &filter.payload) {
        // and
        (0, PL::C(inner)) => inner.iter().all(|f| matches(f, p)),
        // 등호
        (3, PL::C(inner)) => {
            let attr = string(inner[0].clone());
            let value = string(inner[1].clone());
            values(p, &attr)
                .iter()
                .any(|v| v.eq_ignore_ascii_case(&value))
        }
        // 있음
        (7, PL::P(attr)) => !values(p, &String::from_utf8_lossy(attr)).is_empty(),
        _ => false,
    }
}

fn record(stats: &Stats, filter: &StructureTag) {
    match (filter.class, filter.id, &filter.payload) {
        (TagClass::Context, 0, PL::C(inner)) => inner.iter().for_each(|f| record(stats, f)),
        (TagClass::Context, 3, PL::C(inner)) => {
            let attr = string(inner[0].clone());
            let value = string(inner[1].clone());
            stats
                .assertions
                .lock()
                .unwrap()
                .push(format!("{attr}={value}"));
        }
        _ => {}
    }
}

fn entry(p: &Person, wanted: &[String]) -> Tag {
    let attrs = ["objectClass", "uid", "memberOf"]
        .into_iter()
        .filter(|a| wanted.is_empty() || wanted.iter().any(|w| w.eq_ignore_ascii_case(a)))
        .map(|a| {
            Tag::Sequence(Sequence {
                inner: vec![
                    octets(a),
                    Tag::Set(Set {
                        inner: values(p, a).iter().map(|v| octets(v)).collect(),
                        ..Default::default()
                    }),
                ],
                ..Default::default()
            })
        })
        .collect();
    Tag::Sequence(Sequence {
        class: TagClass::Application,
        id: 4,
        inner: vec![
            octets(&p.dn),
            Tag::Sequence(Sequence {
                inner: attrs,
                ..Default::default()
            }),
        ],
    })
}

/// 결과 코드만 있는 응답(`LDAPResult`)
fn response(op: u64, rc: i64, text: &str) -> Tag {
    Tag::Sequence(Sequence {
        class: TagClass::Application,
        id: op,
        inner: vec![
            Tag::Enumerated(Enumerated {
                inner: rc,
                ..Default::default()
            }),
            octets(""),
            octets(text),
        ],
    })
}

fn extended(rc: i64, text: &str) -> Tag {
    let Tag::Sequence(mut seq) = response(24, rc, text) else {
        unreachable!()
    };
    if rc == 0 {
        seq.inner.push(Tag::OctetString(OctetString {
            class: TagClass::Context,
            id: 10,
            inner: STARTTLS_OID.as_bytes().to_vec(),
        }));
    }
    Tag::Sequence(seq)
}

fn octets(s: &str) -> Tag {
    Tag::OctetString(OctetString {
        inner: s.as_bytes().to_vec(),
        ..Default::default()
    })
}

fn string(tag: StructureTag) -> String {
    String::from_utf8(tag.expect_primitive().unwrap_or_default()).unwrap()
}

fn uint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |n, b| (n << 8) | u64::from(*b))
}
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
pub mod fake_apt;
pub mod ustar;
```

을 다음으로 바꾼다:

```rust
pub mod fake_apt;
pub mod ldap;
pub mod ustar;
```

`crates/opl-api/tests/ldap.rs`를 만든다:

```rust
//! LDAP 클라이언트(`opl_api::ldap`)와 영역 시험 동작: 가짜 LDAP 서버(LDAPS와 ldap://+StartTLS)로
//! 성공, 잘못된 자격 증명, 디렉터리에 닿지 못함, 필터 이스케이프, 시험 동작의 단계 결과와 권한.

mod common;

use std::time::{Duration, Instant};

use common::Env;
use common::ldap::{BASE_DN, Directory, FakeLdap, Mode, Person, SERVICE_DN, SERVICE_PASSWORD};
use opl_api::ldap::{LdapError, LdapUser, authenticate};
use opl_peer::pki::ClusterCa;
use opl_types::realm::{DEFAULT_GROUP_ATTR, DEFAULT_USER_FILTER, LdapRealm};
use reqwest::StatusCode;
use serde_json::{Value, json};

const ADMINS: &str = "cn=admins,ou=groups,dc=example,dc=com";
const STAFF: &str = "cn=staff,ou=groups,dc=example,dc=com";

fn alice() -> Person {
    Person::new("alice", "alice-pw", &[ADMINS, STAFF])
}

fn bob() -> Person {
    Person::new("bob", "bob-pw", &[STAFF])
}

/// 서비스 계정으로 찾는 영역
fn realm(ldap: &FakeLdap) -> LdapRealm {
    LdapRealm {
        url: ldap.url.clone(),
        bind_dn: Some(SERVICE_DN.into()),
        bind_password: Some(SERVICE_PASSWORD.into()),
        base_dn: BASE_DN.into(),
        user_filter: DEFAULT_USER_FILTER.into(),
        group_attr: DEFAULT_GROUP_ATTR.into(),
        ca_pem: Some(ldap.ca_pem.clone()),
        timeout_secs: 5,
    }
}

fn alice_user() -> LdapUser {
    LdapUser {
        dn: format!("uid=alice,ou=people,{BASE_DN}"),
        groups: vec![ADMINS.into(), STAFF.into()],
    }
}

fn unavailable(got: Result<LdapUser, LdapError>) -> String {
    match got {
        Err(LdapError::Unavailable(message)) => message,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[tokio::test]
async fn a_user_is_found_and_bound_over_ldaps_and_starttls() {
    for mode in [Mode::Ldaps, Mode::StartTls] {
        let ldap = FakeLdap::start(mode, Directory::with(vec![alice(), bob()])).await;
        let got = authenticate(&realm(&ldap), "alice", "alice-pw").await;
        assert_eq!(got, Ok(alice_user()), "{mode:?}");
        // 서비스 계정 바인드, 검색, 사용자 바인드
        assert_eq!((ldap.binds(), ldap.searches()), (2, 1), "{mode:?}");
        assert_eq!(
            ldap.assertions(),
            ["objectClass=person", "uid=alice"],
            "{mode:?}"
        );
    }
}

#[tokio::test]
async fn without_a_service_account_the_search_is_anonymous() {
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![bob()])).await;
    let mut realm = realm(&ldap);
    realm.bind_dn = None;
    realm.bind_password = None;
    let got = authenticate(&realm, "bob", "bob-pw").await.unwrap();
    assert_eq!(got.groups, [STAFF]);
    // 사용자 바인드만
    assert_eq!(ldap.binds(), 1);
}

#[tokio::test]
async fn wrong_credentials_are_invalid() {
    let mut directory = Directory::with(vec![alice(), bob()]);
    // 같은 uid의 항목 둘
    directory.people.push(Person {
        dn: format!("uid=carol,ou=other,{BASE_DN}"),
        ..Person::new("carol", "carol-pw", &[])
    });
    directory.people.push(Person {
        dn: format!("uid=carol,ou=more,{BASE_DN}"),
        ..Person::new("carol", "carol-pw", &[])
    });
    let ldap = FakeLdap::start(Mode::Ldaps, directory).await;
    let realm = realm(&ldap);

    for (user, password) in [
        ("alice", "wrong"),
        ("nobody", "alice-pw"),
        ("carol", "carol-pw"),
    ] {
        assert_eq!(
            authenticate(&realm, user, password).await,
            Err(LdapError::InvalidCredentials),
            "{user}"
        );
    }

    // 빈 비밀번호는 서버에 닿지 않는다.
    let connections = ldap.connections();
    let binds = ldap.binds();
    assert_eq!(
        authenticate(&realm, "alice", "").await,
        Err(LdapError::InvalidCredentials)
    );
    assert_eq!((ldap.connections(), ldap.binds()), (connections, binds));
}

#[tokio::test]
async fn special_characters_in_the_user_name_are_escaped() {
    // 사람이 alice 하나뿐이라, 이스케이프하지 않으면 `*`가 alice와 맞는다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let realm = realm(&ldap);
    for user in ["*", "alice)(uid=*", "al*"] {
        assert_eq!(
            authenticate(&realm, user, "alice-pw").await,
            Err(LdapError::InvalidCredentials),
            "{user}"
        );
    }
    // 서버는 이름을 글자 그대로 받았다.
    let assertions = ldap.assertions();
    assert!(assertions.contains(&"uid=*".to_string()), "{assertions:?}");
    assert!(
        assertions.contains(&"uid=alice)(uid=*".to_string()),
        "{assertions:?}"
    );
    // 사용자 바인드는 없었다(서비스 계정만).
    assert_eq!(ldap.binds(), 3);
}

#[tokio::test]
async fn an_unreachable_directory_is_unavailable() {
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;

    // 서비스 계정 비밀번호가 틀림
    let mut bad = realm(&ldap);
    bad.bind_password = Some("wrong".into());
    let message = unavailable(authenticate(&bad, "alice", "alice-pw").await);
    assert!(message.contains("service account"), "{message}");

    // 믿지 않는 CA
    let mut other = realm(&ldap);
    other.ca_pem = Some(ClusterCa::generate("other").unwrap().cert_pem);
    unavailable(authenticate(&other, "alice", "alice-pw").await);

    // 닫힌 포트
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut closed = realm(&ldap);
    closed.url = format!("ldaps://127.0.0.1:{port}");
    unavailable(authenticate(&closed, "alice", "alice-pw").await);

    // 받기만 하고 답하지 않는 서버는 시간 제한으로 끝난다.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut slow = realm(&ldap);
    slow.url = format!("ldaps://{}", silent.local_addr().unwrap());
    slow.timeout_secs = 1;
    let started = Instant::now();
    unavailable(authenticate(&slow, "alice", "alice-pw").await);
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(silent);
}

/// 같은 uid의 항목 `n`개
fn many_carols(n: usize) -> Vec<Person> {
    (0..n)
        .map(|i| Person {
            dn: format!("uid=carol,ou=p{i},{BASE_DN}"),
            ..Person::new("carol", "carol-pw", &[])
        })
        .collect()
}

#[tokio::test]
async fn the_search_stops_at_two_entries() {
    // 서버가 크기 제한을 지키지 않고 많이 보내도 둘에서 멈추고 검색을 버린다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(many_carols(50))).await;
    let mut realm = realm(&ldap);
    assert_eq!(
        authenticate(&realm, "carol", "carol-pw").await,
        Err(LdapError::InvalidCredentials)
    );
    assert_eq!(ldap.size_limits(), [2]);
    // 서비스 계정만 바인드했고 검색은 버렸다.
    assert_eq!(ldap.binds(), 1);
    let started = Instant::now();
    while ldap.abandons() == 0 {
        assert!(started.elapsed() < Duration::from_secs(5), "no abandon");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 서버 쪽 제한으로 4(sizeLimitExceeded)가 오면 여럿과 같다.
    ldap.truncate_at(1);
    assert_eq!(
        authenticate(&realm, "carol", "carol-pw").await,
        Err(LdapError::InvalidCredentials)
    );
    assert_eq!(ldap.binds(), 2);

    // 한 명이면 그대로 된다(크기 제한이 결과를 바꾸지 않음).
    realm.user_filter = "(uid={username})".into();
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    realm.url = ldap.url.clone();
    realm.ca_pem = Some(ldap.ca_pem.clone());
    ldap.truncate_at(1);
    assert_eq!(
        authenticate(&realm, "alice", "alice-pw").await,
        Ok(alice_user())
    );
}

#[tokio::test]
async fn the_whole_check_has_a_deadline() {
    // 참조를 끝없이 보내는 서버: 하나하나는 작업 시간 제한 안에 오지만 전체 마감에 걸린다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    ldap.send_endless_references();
    let mut realm = realm(&ldap);
    realm.timeout_secs = 1;
    let started = Instant::now();
    let message = unavailable(authenticate(&realm, "alice", "alice-pw").await);
    assert_eq!(message, "timed out");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(4) && elapsed < Duration::from_secs(8),
        "{elapsed:?}"
    );

    let spec = opl_types::realm::RealmSpec {
        display: None,
        backend: opl_types::realm::RealmBackend::Ldap(realm),
        roles: Default::default(),
    };
    let result = opl_api::ldap::test(&spec, "alice", "alice-pw").await;
    let last = result.steps.last().unwrap();
    assert_eq!((last.name.as_str(), last.ok), ("search", false));
    assert_eq!(last.message, "timed out");
}

#[tokio::test]
async fn the_host_name_and_starttls_are_required() {
    // 인증서는 127.0.0.1이라 localhost로는 믿지 않는다.
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let mut by_name = realm(&ldap);
    let port = ldap.url.rsplit(':').next().unwrap();
    by_name.url = format!("ldaps://localhost:{port}");
    let message = unavailable(authenticate(&by_name, "alice", "alice-pw").await);
    assert!(ldap.connections() >= 1, "{message}");
    assert_eq!(ldap.binds(), 0, "{message}");

    // StartTLS를 거절하는 서버에는 바인드하지 않는다.
    let ldap = FakeLdap::start(Mode::StartTlsRefused, Directory::with(vec![alice()])).await;
    unavailable(authenticate(&realm(&ldap), "alice", "alice-pw").await);
    assert_eq!(ldap.connections(), 1);
    assert_eq!(ldap.binds(), 0);
}

#[tokio::test]
async fn invalid_credentials_take_at_least_300ms() {
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let realm = realm(&ldap);
    for (user, password) in [("nobody", "x"), ("alice", "wrong"), ("alice", "")] {
        let started = Instant::now();
        assert_eq!(
            authenticate(&realm, user, password).await,
            Err(LdapError::InvalidCredentials),
            "{user}"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(300),
            "{user}: {:?}",
            started.elapsed()
        );
    }
}

/// 가짜 서버를 쓰는 영역 만들기 본문
fn corp(ldap: &FakeLdap) -> Value {
    json!({
        "name": "corp",
        "backend": {
            "kind": "ldap",
            "url": ldap.url,
            "bind_dn": SERVICE_DN,
            "bind_password": SERVICE_PASSWORD,
            "base_dn": BASE_DN,
            "ca_pem": ldap.ca_pem,
        },
        "roles": {
            "groups": [
                { "group": STAFF, "role": "viewer" },
                { "group": ADMINS, "role": "admin" }
            ]
        }
    })
}

fn step_names(result: &Value) -> Vec<(String, bool)> {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["ok"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_test_action_reports_each_step() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ldap = FakeLdap::start(Mode::StartTls, Directory::with(vec![alice(), bob()])).await;
    let (status, body) = admin.post("/realms", corp(&ldap)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (_, users) = admin.get("/users").await;

    let (status, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "alice", "password": "alice-pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let all = ["connect", "service_bind", "search", "user_bind", "groups"];
    assert_eq!(
        step_names(&result),
        all.map(|s| (s.to_string(), true)).to_vec()
    );
    assert_eq!(result["dn"], alice_user().dn);
    assert_eq!(result["groups"], json!([ADMINS, STAFF]));
    assert_eq!(result["role"], "admin");
    assert!(!result.to_string().contains("alice-pw"));
    assert!(!result.to_string().contains(SERVICE_PASSWORD));

    // 틀린 비밀번호는 user_bind에서 멈춘다.
    let (status, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "bob", "password": "secret-guess" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        step_names(&result),
        [
            ("connect".to_string(), true),
            ("service_bind".to_string(), true),
            ("search".to_string(), true),
            ("user_bind".to_string(), false)
        ]
    );
    assert_eq!(result["dn"], format!("uid=bob,ou=people,{BASE_DN}"));
    assert_eq!(result["groups"], json!([]));
    assert_eq!(result["role"], Value::Null);
    assert!(!result.to_string().contains("secret-guess"));

    // 없는 사용자는 search에서 멈춘다.
    let (_, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "nobody", "password": "x" }),
        )
        .await;
    assert_eq!(
        step_names(&result).last().unwrap(),
        &("search".to_string(), false)
    );
    assert_eq!(result["dn"], Value::Null);

    // 사용자를 만들지 않는다.
    assert_eq!(admin.get("/users").await.1, users);

    // 닿지 않는 디렉터리는 connect에서 멈춘다.
    drop(ldap);
    let (_, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": "alice", "password": "alice-pw" }),
        )
        .await;
    assert_eq!(step_names(&result), [("connect".to_string(), false)]);

    // 없는 영역
    let (status, _) = admin
        .post(
            "/realms/other/actions/test",
            json!({ "username": "alice", "password": "alice-pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn only_admins_can_test_a_realm() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let ldap = FakeLdap::start(Mode::Ldaps, Directory::with(vec![alice()])).await;
    let (status, body) = admin.post("/realms", corp(&ldap)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let body = json!({ "username": "alice", "password": "alice-pw" });
    for role in ["operator", "viewer"] {
        let client = env.user(&format!("u-{role}"), role).await;
        let (status, got) = client.post("/realms/corp/actions/test", body.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {got}");
    }
    let (status, _) = env
        .anonymous()
        .post("/realms/corp/actions/test", body)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // 거절된 요청은 디렉터리에 닿지 않았다.
    assert_eq!(ldap.connections(), 0);
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/realms/{key}",
        "/version",
```

을 다음으로 바꾼다:

```rust
        "/realms/{key}",
        "/realms/{key}/actions/test",
        "/version",
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check --workspace`
Expected: `Cargo.lock`에 `ldap3`·`lber`·`rustls-native-certs`가 더해진다(crates.io에서 받음, 이 Task의 커밋에 넣는다)

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1325개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add Cargo.toml Cargo.lock crates web/src/api/schema.d.ts
git commit -m "feat(api): LDAP 클라이언트와 영역 시험 동작 — 워크스페이스 의존 ldap3 0.12.1(default-features 끔, tls-rustls-ring). opl-api ldap.rs: authenticate(realm, username, password) -> Result<LdapUser{dn, groups}, LdapError{InvalidCredentials, Unavailable}>(빈 비밀번호는 연결하지 않고 거절, ldap://는 StartTLS 필수·ldaps://는 처음부터 TLS, ca_pem이 있으면 그 CA만 루트·없으면 ldap3 기본(시스템 루트), 호스트 이름 확인, 서비스 계정(없으면 익명) 바인드 실패는 Unavailable, user_filter의 {username}은 ldap_escape, base_dn subtree 검색 결과가 정확히 1개가 아니면 InvalidCredentials, 사용자 바인드 49는 InvalidCredentials, 그룹은 group_attr 값(속성 이름 대소문자 무시), 연결과 작업마다 timeout_secs, 끝나면 unbind)와 같은 흐름을 단계로 기록하는 test(spec, …) -> RealmTestResult. POST /api/v1/realms/{key}/actions/test {username, password}(admin만): steps(connect·service_bind·search·user_bind·groups, 실패한 단계에서 멈춤), dn, groups, role — 사용자·세션을 만들지 않고 비밀번호를 내보내지 않음. 시험용 가짜 LDAP 서버 tests/common/ldap.rs(lber·bytes dev 의존; LDAPS와 ldap://+StartTLS, 바인드·검색·연결 횟수와 필터 등호 조건 기록). schema.d.ts 다시 만듦"
```

---

### Task 4: LDAP 영역 로그인과 외부 사용자

`POST /auth/login`의 `realm`: LDAP 영역이면 시도 제한(키 `<사용자>@<영역>`) → 디렉터리 확인 → 그룹으로 역할(없으면 403 `no_role`, 디렉터리에 닿지 못하면 503 `realm_unavailable`) → 처음이면 `User` `alice@corp`를 만들고 역할이 바뀌었으면 고치며 세션 세대를 올림 → 세션. 외부 사용자의 비밀번호 바꾸기는 400 `external_user`, 영역이 지워지면 그 사용자의 세션·토큰은 401. 감사 detail에 `realm=`.

**Files:**
- Create: `crates/opl-api/tests/ldap_login.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/users.rs`
- Modify: `crates/opl-api/tests/common/ldap.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/auth.rs`에서

```rust
use opl_types::auth::{ApiTokenSpec, Role, User, UserSpec};
use opl_types::resource::Kind;
```

을 다음으로 바꾼다:

```rust
use opl_types::auth::{ApiTokenSpec, Role, User, UserSpec};
use opl_types::realm::RealmSpec;
use opl_types::resource::Kind;
```

`crates/opl-api/src/auth.rs`에서

```rust
    pub(crate) fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        Ok(self.cluster.get::<UserSpec>(user)?.map(principal_of))
    }
```

을 다음으로 바꾼다:

```rust
    pub(crate) fn principal_for(&self, user: Uuid) -> Result<Option<Principal>, ApiError> {
        match self.cluster.get::<UserSpec>(user)? {
            Some(u) if self.realm_exists(&u)? => Ok(Some(principal_of(u))),
            _ => Ok(None),
        }
    }

    /// 외부 사용자의 영역이 있다(로컬 사용자는 늘 참). 영역을 지우면 그 사용자의 세션과 토큰은
    /// 멈추고, 같은 이름으로 다시 만들면 다시 쓸 수 있다.
    fn realm_exists(&self, user: &User) -> Result<bool, ApiError> {
        match &user.spec.realm {
            Some(realm) => Ok(self.cluster.get_by_name::<RealmSpec>(realm)?.is_some()),
            None => Ok(true),
        }
    }
```

`crates/opl-api/src/auth.rs`에서

```rust
            self.sessions.remove(session);
            return Err(ApiError::unauthorized());
```

을 다음으로 바꾼다:

```rust
            self.sessions.remove(session);
            return Err(ApiError::unauthorized());
        }
        if !self.realm_exists(&user)? {
            return Err(ApiError::unauthorized());
```

`crates/opl-api/src/users.rs` 전체:

```rust
//! 로그인, 사용자, API 토큰. 비밀번호 해시와 토큰 해시는 응답에 넣지 않는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::audit::AuditAuth;
use opl_types::auth::{ApiToken, ApiTokenSpec, Role, User, UserSpec, external_name};
use opl_types::realm::{LOCAL_REALM, RealmBackend, RealmSpec};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{
    Action, Principal, SESSION_COOKIE, SESSION_TTL, dummy_hash, hash_password, new_token,
    session_cookie, verify_password,
};
use crate::error::{ApiError, Json};
use crate::limit::ClientIp;
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;

pub const MIN_PASSWORD_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct UserView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub role: Role,
    /// 외부 사용자의 영역. 로컬 사용자는 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub realm: Option<String>,
}

impl From<User> for UserView {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            name: u.name,
            revision: u.revision,
            role: u.spec.role,
            realm: u.spec.realm,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct TokenView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub user: Uuid,
}

impl From<ApiToken> for TokenView {
    fn from(t: ApiToken) -> Self {
        Self {
            id: t.id,
            name: t.name,
            revision: t.revision,
            user: t.spec.user,
        }
    }
}

fn check_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    /// 영역 로그인이면 영역 안의 이름이다(`alice`로 로그인하면 사용자 `alice@<영역>`).
    pub username: String,
    pub password: String,
    /// 없거나 `local`이면 로컬 사용자
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub realm: Option<String>,
}

/// 세션 쿠키를 발급한다. 웹 UI가 쓴다. CLI는 로그인한 뒤 API 토큰을 만들어 쓴다.
/// 실패가 이어지면 잠시 막는다(429 `too_many_attempts`, `Retry-After`).
/// LDAP 영역 로그인은 디렉터리로 확인하고 외부 사용자 `<이름>@<영역>`을 만들거나 역할을 고친다.
/// 모르는 영역은 400, 맞는 역할이 없으면 403 `no_role`, 디렉터리에 닿지 못하면 503 `realm_unavailable`.
#[utoipa::path(post, path = "/auth/login", tag = "auth", security(()),
    request_body = LoginRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn login(
    State(state): State<Arc<AppState>>,
    ClientIp(ip): ClientIp,
    audit: AuditNote,
    Json(req): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let user = match req.realm.as_deref() {
        Some(realm) if realm != LOCAL_REALM => login_realm(&state, ip, &audit, &req, realm).await?,
        _ => login_local(&state, ip, &audit, req).await?,
    };
    audit.principal(
        Principal {
            user: user.id,
            name: user.name.clone(),
            role: user.spec.role,
        },
        AuditAuth::Session,
    );
    let session = state.sessions.create(user.id, user.spec.session_epoch);
    let mut cookie = format!(
        "{SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    let mut response = Json(UserView::from(user)).into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok(response)
}

/// 로컬 사용자의 비밀번호를 확인한다.
async fn login_local(
    state: &AppState,
    ip: std::net::IpAddr,
    audit: &AuditNote,
    req: LoginRequest,
) -> Result<User, ApiError> {
    // 이름 형식이 아닌 것은 감사 로그에 넣지 않는다(비밀번호를 잘못 넣었을 수 있다).
    let name_ok = validate_name(&req.username).is_ok();
    audit.detail(if name_ok {
        format!("username={}", req.username)
    } else {
        "username=<invalid>".into()
    });
    // 이름 형식이 아니면 그런 사용자는 있을 수 없다. 해시를 계산하지 않고 IP만 실패로 센다(긴 이름을
    // 기록에 넣지 않는다). 있을 수 없는 이름이라 응답 시간으로 드러나는 것도 없다.
    if !name_ok {
        state.attempts.begin_ip(ip)?;
        return Err(wrong_credentials());
    }
    // 막혀 있으면 해시를 계산하지 않는다. 이 시도는 미리 실패로 센다.
    let attempt = state.attempts.begin(&req.username, ip)?;
    let user = state.cluster.get_by_name::<UserSpec>(&req.username)?;
    // 사용자가 없어도 해시를 비교해 응답 시간으로 사용자 존재 여부를 알 수 없게 한다.
    let hash = match &user {
        Some(u) => u.spec.password_hash.clone(),
        None => dummy_hash().await,
    };
    let ok = verify_password(req.password, hash).await;
    // 없는 사용자도 실패다(begin이 이미 셌다).
    let Some(user) = user.filter(|_| ok) else {
        return Err(wrong_credentials());
    };
    state.attempts.success(&req.username, ip, attempt);
    Ok(user)
}

/// 영역의 디렉터리로 확인하고 외부 사용자를 만들거나 역할을 맞춘다. 시도 제한의 키는 외부 사용자 이름이다.
async fn login_realm(
    state: &AppState,
    ip: std::net::IpAddr,
    audit: &AuditNote,
    req: &LoginRequest,
    realm: &str,
) -> Result<User, ApiError> {
    // 영역 부분이 맞는 아무 이름으로 사용자 부분만 본다.
    let user_ok = external_name(&req.username, LOCAL_REALM).is_ok();
    audit.detail(format!(
        "username={} realm={}",
        if user_ok {
            req.username.as_str()
        } else {
            "<invalid>"
        },
        if validate_name(realm).is_ok() {
            realm
        } else {
            "<invalid>"
        },
    ));
    let Ok(name) = external_name(&req.username, realm) else {
        state.attempts.begin_ip(ip)?;
        return Err(wrong_credentials());
    };
    let stored = state
        .cluster
        .get_by_name::<RealmSpec>(realm)?
        .ok_or_else(|| ApiError::bad_request(format!("unknown realm {realm:?}")))?;
    let attempt = state.attempts.begin(&name, ip)?;
    let RealmBackend::Ldap(ldap) = &stored.spec.backend;
    // 디렉터리에는 소문자로 바꾼 이름(외부 사용자 이름의 앞부분)을 묻는다.
    let username = &name[..name.len() - realm.len() - 1];
    let found = match crate::ldap::authenticate(ldap, username, &req.password).await {
        Ok(found) => found,
        Err(crate::ldap::LdapError::InvalidCredentials) => return Err(wrong_credentials()),
        Err(crate::ldap::LdapError::Unavailable(reason)) => {
            tracing::warn!(realm, %reason, "the directory of the realm is unavailable");
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "realm_unavailable",
                format!("the directory of realm {realm} is unavailable"),
            ));
        }
    };
    let role = stored.spec.roles.role_for(&found.groups).ok_or_else(|| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "no_role",
            "no role is mapped for this user",
        )
    })?;
    // 외부 사용자는 옛 노드가 모르는 필드(`realm`)를 쓴다.
    crate::cluster::require_current(&state.cluster)?;
    let user = upsert_external(state, &name, realm, role).await?;
    state.attempts.success(&name, ip, attempt);
    Ok(user)
}

/// 외부 사용자를 만들거나 역할을 고친다(역할이 바뀌면 옛 세션을 끊는다). 같으면 쓰지 않는다.
/// 같은 사용자의 로그인이 겹쳐 진 쪽은 다시 읽어 한 번 더 한다.
async fn upsert_external(
    state: &AppState,
    name: &str,
    realm: &str,
    role: Role,
) -> Result<User, ApiError> {
    let mut retried = false;
    loop {
        let written = match state.cluster.get_by_name::<UserSpec>(name)? {
            Some(user) if user.spec.role == role => return Ok(user),
            Some(mut user) => {
                user.spec.role = role;
                user.spec.session_epoch += 1;
                state
                    .cluster
                    .update_spec(
                        user.id,
                        Some(user.revision),
                        user.labels.clone(),
                        &user.spec,
                    )
                    .await
                    .map(|revision| User { revision, ..user })
            }
            None => {
                let user: User = Resource::new(
                    name,
                    UserSpec {
                        password_hash: String::new(),
                        role,
                        session_epoch: 0,
                        realm: Some(realm.into()),
                    },
                );
                state
                    .cluster
                    .create(&user)
                    .await
                    .map(|revision| User { revision, ..user })
            }
        };
        match written {
            Ok(user) => return Ok(user),
            Err(ClusterError::Rejected(
                Rejection::AlreadyExists { .. }
                | Rejection::NameTaken { .. }
                | Rejection::RevisionMismatch { .. },
            )) if !retried => retried = true,
            Err(e) => return Err(e.into()),
        }
    }
}

fn wrong_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "wrong username or password",
    )
}

/// 세션을 끝내고 브라우저의 세션 쿠키를 지운다.
#[utoipa::path(post, path = "/auth/logout", tag = "auth", security(()),
    responses((status = 204, description = "Logged out; the session cookie is cleared",
        headers(("set-cookie" = String)))))]
pub async fn logout(
    State(state): State<Arc<AppState>>,
    audit: AuditNote,
    headers: HeaderMap,
) -> Response {
    if let Some(id) = session_cookie(&headers) {
        // 지우기 전에 세션의 사용자를 감사 로그에 남긴다.
        if let Some((user, _)) = state.sessions.get(id)
            && let Ok(Some(principal)) = state.principal_for(user)
        {
            audit.principal(principal, AuditAuth::Session);
        }
        state.sessions.remove(id);
    }
    let mut cookie = format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    if state.secure_cookies {
        cookie.push_str("; Secure");
    }
    (
        StatusCode::NO_CONTENT,
        [(
            SET_COOKIE,
            HeaderValue::from_str(&cookie).expect("ASCII cookie"),
        )],
    )
        .into_response()
}

#[utoipa::path(get, path = "/auth/whoami", tag = "auth",
    responses((status = 200, body = Principal), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn whoami(principal: Principal) -> Json<Principal> {
    Json(principal)
}

#[utoipa::path(get, path = "/users", tag = "auth",
    responses((status = 200, body = List<UserView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_users(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<UserView>>, ApiError> {
    state.authorize(&principal, Action::Read, UserSpec::KIND)?;
    let (revision, users) = state.cluster.list::<UserSpec>()?;
    Ok(Json(List {
        revision,
        items: users.into_iter().map(UserView::from).collect(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateUserRequest {
    pub name: String,
    pub password: String,
    pub role: Role,
}

#[utoipa::path(post, path = "/users", tag = "auth",
    request_body = CreateUserRequest,
    responses((status = 201, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    check_name(&req.name)?;
    check_password(&req.password)?;
    let mut user: User = Resource::new(
        req.name,
        UserSpec {
            password_hash: hash_password(req.password).await?,
            role: req.role,
            session_epoch: 0,
            realm: None,
        },
    );
    user.revision = state.cluster.create(&user).await?;
    Ok((StatusCode::CREATED, Json(user.into())))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateUserRequest {
    #[serde(default)]
    pub password: Option<String>,
    /// 자기 비밀번호를 바꿀 때 필요하다.
    #[serde(default)]
    pub current_password: Option<String>,
    #[serde(default)]
    pub role: Option<Role>,
}

/// 관리자는 누구의 비밀번호와 역할이든 바꾼다. 다른 사용자는 자기 비밀번호만 바꾼다.
/// 자기 비밀번호를 바꿀 때는 현재 비밀번호가 필요하고(훔친 세션으로 계정을 빼앗지 못하게),
/// 바꾸면 모든 노드에서 그 사용자의 세션이 끝난다(API 토큰은 남는다). 마지막 관리자는 역할을 내려놓을 수 없다.
/// 현재 비밀번호 확인은 로그인과 같은 시도 제한을 받는다.
#[utoipa::path(put, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    request_body = UpdateUserRequest,
    responses((status = 200, body = UserView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    ClientIp(ip): ClientIp,
    Path(key): Path<String>,
    Json(req): Json<UpdateUserRequest>,
) -> Result<Json<UserView>, ApiError> {
    // 관리 권한이 없으면 다른 사용자가 있는지조차 알려 주지 않는다.
    let may_write = state
        .authorizer
        .check(&principal, Action::Write, UserSpec::KIND);
    if !may_write && key != principal.name && key != principal.user.to_string() {
        return Err(ApiError::forbidden());
    }
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    let is_self = user.id == principal.user;
    if !is_self || req.role.is_some() {
        state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    }
    if req.password.is_some() && user.spec.realm.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "external_user",
            "the password of an external user is managed by its realm",
        ));
    }
    let mut spec = user.spec.clone();
    let password_changed = req.password.is_some();
    if let Some(password) = req.password {
        check_password(&password)?;
        if is_self {
            let attempt = state.attempts.begin(&principal.name, ip)?;
            let current = req.current_password.unwrap_or_default();
            if !verify_password(current, user.spec.password_hash.clone()).await {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "invalid_credentials",
                    "the current password is wrong",
                ));
            }
            state.attempts.success(&principal.name, ip, attempt);
        }
        spec.password_hash = hash_password(password).await?;
        // 모든 노드의 옛 세션을 끊는다(쿠키 인증이 세대를 비교한다).
        spec.session_epoch += 1;
    }
    if let Some(role) = req.role {
        if user.spec.role == Role::Admin && role != Role::Admin {
            let (_, users) = state.cluster.list::<UserSpec>()?;
            let admins = users.iter().filter(|u| u.spec.role == Role::Admin).count();
            if admins <= 1 {
                return Err(ApiError::conflict(
                    "last_admin",
                    "the last admin cannot give up the admin role",
                ));
            }
        }
        spec.role = role;
    }
    state
        .cluster
        .update_spec(user.id, Some(user.revision), user.labels, &spec)
        .await?;
    if password_changed {
        state.sessions.remove_user(user.id);
    }
    Ok(Json(
        lookup::<UserSpec>(&state.cluster, &user.id.to_string())?.into(),
    ))
}

/// 사용자와 그 사용자의 토큰을 지운다. 자기 자신은 지울 수 없다(마지막 관리자를 잃지 않게).
#[utoipa::path(delete, path = "/users/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, UserSpec::KIND)?;
    let user = lookup::<UserSpec>(&state.cluster, &key)?;
    if user.id == principal.user {
        return Err(ApiError::conflict(
            "cannot_delete_self",
            "you cannot delete your own account",
        ));
    }
    // 토큰은 요청마다 주인을 다시 읽으므로, 사용자를 먼저 지우면 남은 토큰도 바로 쓸 수 없다.
    state.cluster.delete::<UserSpec>(user.id, None).await?;
    let (_, tokens) = state.cluster.list::<ApiTokenSpec>()?;
    for token in tokens.into_iter().filter(|t| t.spec.user == user.id) {
        state.cluster.delete::<ApiTokenSpec>(token.id, None).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTokenRequest {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedToken {
    pub token: TokenView,
    /// `Authorization: Bearer <secret>`로 쓴다. 다시 볼 수 없다.
    pub secret: String,
}

fn may_manage(state: &AppState, principal: &Principal, token: &ApiToken) -> bool {
    token.spec.user == principal.user
        || state
            .authorizer
            .check(principal, Action::Write, ApiTokenSpec::KIND)
}

/// 요청한 사용자의 토큰을 만든다. 토큰의 권한은 주인의 역할을 따른다.
#[utoipa::path(post, path = "/tokens", tag = "auth",
    request_body = CreateTokenRequest,
    responses((status = 201, body = CreatedToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    check_name(&req.name)?;
    let mut token: ApiToken = Resource::new(
        req.name,
        ApiTokenSpec {
            user: principal.user,
            secret_hash: String::new(),
        },
    );
    let (hash, secret) = new_token(token.id);
    token.spec.secret_hash = hash;
    token.revision = state.cluster.create(&token).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token: token.into(),
            secret,
        }),
    ))
}

/// 자기 토큰. 관리자는 모든 토큰을 본다.
#[utoipa::path(get, path = "/tokens", tag = "auth",
    responses((status = 200, body = List<TokenView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_tokens(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<TokenView>>, ApiError> {
    let (revision, tokens) = state.cluster.list::<ApiTokenSpec>()?;
    let items = tokens
        .into_iter()
        .filter(|t| may_manage(&state, &principal, t))
        .map(TokenView::from)
        .collect();
    Ok(Json(List { revision, items }))
}

#[utoipa::path(delete, path = "/tokens/{key}", tag = "auth",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let token = lookup::<ApiTokenSpec>(&state.cluster, &key)?;
    // 남의 토큰은 있는지조차 알리지 않는다.
    if !may_manage(&state, &principal, &token) {
        return Err(ApiError::not_found(format!("api-token {key:?} not found")));
    }
    state.cluster.delete::<ApiTokenSpec>(token.id, None).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

`crates/opl-api/tests/common/ldap.rs`에서

```rust
    mode: Mode,
    directory: Directory,
    acceptor: TlsAcceptor,
```

을 다음으로 바꾼다:

```rust
    mode: Mode,
    /// 시험이 도중에 바꿀 수 있다([`FakeLdap::set_groups`]).
    directory: Mutex<Directory>,
    acceptor: TlsAcceptor,
```

`crates/opl-api/tests/common/ldap.rs`에서

```rust
            mode,
            directory,
            acceptor: TlsAcceptor::from(Arc::new(config)),
```

을 다음으로 바꾼다:

```rust
            mode,
            directory: Mutex::new(directory),
            acceptor: TlsAcceptor::from(Arc::new(config)),
```

`crates/opl-api/tests/common/ldap.rs`에서

```rust
        self.site.stats.assertions.lock().unwrap().clone()
    }
```

을 다음으로 바꾼다:

```rust
        self.site.stats.assertions.lock().unwrap().clone()
    }

    /// `uid` 사람의 그룹을 바꾼다.
    pub fn set_groups(&self, uid: &str, groups: &[&str]) {
        let mut directory = self.site.directory.lock().unwrap();
        let person = directory
            .people
            .iter_mut()
            .find(|p| p.uid == uid)
            .expect("person");
        person.groups = groups.iter().map(|g| g.to_string()).collect();
    }
```

`crates/opl-api/tests/common/ldap.rs`에서

```rust
    let password = string(parts.next().unwrap());
    let directory = &site.directory;
    let ok = (name.is_empty() && password.is_empty())
```

을 다음으로 바꾼다:

```rust
    let password = string(parts.next().unwrap());
    let directory = site.directory.lock().unwrap();
    let ok = (name.is_empty() && password.is_empty())
```

`crates/opl-api/tests/common/ldap.rs`에서

```rust
        .directory
        .people
```

을 다음으로 바꾼다:

```rust
        .directory
        .lock()
        .unwrap()
        .people
```

`crates/opl-api/tests/ldap_login.rs`를 만든다:

```rust
//! LDAP 영역 로그인과 외부 사용자(가짜 LDAP 서버): 첫 로그인에 사용자가 생기고 다음 로그인은
//! 쓰지 않음, 그룹이 바뀌면 역할과 옛 세션, 매핑 없음·디렉터리 없음·모르는 영역, 시도 제한,
//! 외부 사용자의 비밀번호, 영역을 지우면 세션·토큰이 멈춤, 감사 detail, 외부 사용자의 API 토큰.

mod common;

use std::time::{Duration, Instant};

use common::ldap::{BASE_DN, Directory, FakeLdap, Mode, Person, SERVICE_DN, SERVICE_PASSWORD};
use common::{ADMIN_PASSWORD, Client, Env, WAIT};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use reqwest::StatusCode;
use serde_json::{Value, json};

const ADMINS: &str = "cn=admins,ou=groups,dc=example,dc=com";
const STAFF: &str = "cn=staff,ou=groups,dc=example,dc=com";

/// alice는 admin, bob은 viewer, carol은 매핑되는 그룹이 없다.
async fn directory() -> FakeLdap {
    FakeLdap::start(
        Mode::StartTls,
        Directory::with(vec![
            Person::new("alice", "alice-pw", &[ADMINS, STAFF]),
            Person::new("bob", "bob-pw", &[STAFF]),
            Person::new("carol", "carol-pw", &["cn=other,dc=example,dc=com"]),
        ]),
    )
    .await
}

fn corp(ldap: &FakeLdap) -> Value {
    json!({
        "name": "corp",
        "backend": {
            "kind": "ldap",
            "url": ldap.url,
            "bind_dn": SERVICE_DN,
            "bind_password": SERVICE_PASSWORD,
            "base_dn": BASE_DN,
            "ca_pem": ldap.ca_pem,
        },
        "roles": {
            "groups": [
                { "group": STAFF, "role": "viewer" },
                { "group": ADMINS, "role": "admin" }
            ]
        }
    })
}

/// 가짜 디렉터리와 그것을 쓰는 영역 `corp`
async fn setup() -> (Env, FakeLdap) {
    let env = Env::start().await;
    let ldap = directory().await;
    let (status, body) = env.admin().await.post("/realms", corp(&ldap)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (env, ldap)
}

struct Login {
    status: StatusCode,
    body: Value,
    /// `opl_session=…`
    cookie: Option<String>,
}

async fn login(env: &Env, user: &str, password: &str, realm: Option<&str>) -> Login {
    let mut body = json!({ "username": user, "password": password });
    if let Some(realm) = realm {
        body["realm"] = json!(realm);
    }
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let cookie = resp
        .headers()
        .get("set-cookie")
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    Login {
        status,
        body: resp.json().await.unwrap(),
        cookie,
    }
}

/// 영역 `corp`로 로그인하고 세션 쿠키를 돌려준다.
async fn corp_login(env: &Env, user: &str, password: &str) -> (Value, String) {
    let got = login(env, user, password, Some("corp")).await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    (got.body, got.cookie.unwrap())
}

async fn whoami(env: &Env, cookie: &str) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v1/auth/whoami", env.base))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

fn assert_error(got: &Login, status: StatusCode, code: &str) {
    assert_eq!(got.status, status, "{}", got.body);
    assert_eq!(got.body["code"], code, "{}", got.body);
    assert!(got.cookie.is_none());
}

fn stored(env: &Env, name: &str) -> Option<Resource<UserSpec, opl_types::auth::UserStatus>> {
    env.cluster.get_by_name::<UserSpec>(name).unwrap()
}

#[tokio::test]
async fn the_first_login_creates_the_external_user_and_later_logins_do_not_write() {
    let (env, _ldap) = setup().await;
    let (body, cookie) = corp_login(&env, "alice", "alice-pw").await;
    assert_eq!(body["name"], "alice@corp");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["realm"], "corp");
    let (status, me) = whoami(&env, &cookie).await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(
        (me["name"].as_str(), me["role"].as_str()),
        (Some("alice@corp"), Some("admin"))
    );

    let user = stored(&env, "alice@corp").unwrap();
    assert_eq!(user.spec.realm.as_deref(), Some("corp"));
    assert_eq!(user.spec.password_hash, "");
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 0));

    // 다시 로그인해도(대문자도 같은 사용자) 쓰지 않는다.
    let (body, _) = corp_login(&env, "Alice", "alice-pw").await;
    assert_eq!(body["name"], "alice@corp");
    assert_eq!(stored(&env, "alice@corp").unwrap().revision, user.revision);

    // 사용자 목록에 영역이 보인다(로컬 사용자에는 없다).
    let (_, users) = env.admin().await.get("/users").await;
    let items = users["items"].as_array().unwrap();
    let alice = items.iter().find(|u| u["name"] == "alice@corp").unwrap();
    assert_eq!(alice["realm"], "corp");
    let admin = items.iter().find(|u| u["name"] == "admin").unwrap();
    assert!(admin.get("realm").is_none(), "{admin}");
}

#[tokio::test]
async fn a_changed_group_changes_the_role_and_ends_old_sessions() {
    let (env, ldap) = setup().await;
    let (body, old) = corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(body["role"], "viewer");
    assert_eq!(whoami(&env, &old).await.0, StatusCode::OK);

    ldap.set_groups("bob", &[ADMINS]);
    let (body, new) = corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(body["role"], "admin");
    let user = stored(&env, "bob@corp").unwrap();
    assert_eq!((user.spec.role, user.spec.session_epoch), (Role::Admin, 1));
    assert_eq!(whoami(&env, &new).await.1["role"], "admin");
    // 옛 역할의 세션은 끝났다.
    assert_eq!(whoami(&env, &old).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn failed_realm_logins_give_distinct_errors() {
    let (env, ldap) = setup().await;

    // 매핑되는 그룹이 없다(사용자를 만들지 않는다).
    let got = login(&env, "carol", "carol-pw", Some("corp")).await;
    assert_error(&got, StatusCode::FORBIDDEN, "no_role");
    assert!(stored(&env, "carol@corp").is_none());

    let got = login(&env, "alice", "wrong", Some("corp")).await;
    assert_error(&got, StatusCode::UNAUTHORIZED, "invalid_credentials");
    assert_eq!(got.body["message"], "wrong username or password");
    // 외부 사용자 이름이 될 수 없다.
    for user in ["no one", "a@b", ""] {
        let got = login(&env, user, "alice-pw", Some("corp")).await;
        assert_error(&got, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    let got = login(&env, "alice", "alice-pw", Some("nowhere")).await;
    assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
    assert!(stored(&env, "alice@corp").is_none());

    // `local`은 로컬 사용자다.
    assert_eq!(
        login(&env, "admin", ADMIN_PASSWORD, Some("local"))
            .await
            .status,
        StatusCode::OK
    );

    // 외부 사용자는 로컬 로그인 경로로 들어오지 못한다.
    corp_login(&env, "alice", "alice-pw").await;
    for realm in [None, Some("local")] {
        let got = login(&env, "alice@corp", "alice-pw", realm).await;
        assert_error(&got, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }

    // 디렉터리에 닿지 못한다.
    drop(ldap);
    let got = login(&env, "alice", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::SERVICE_UNAVAILABLE, "realm_unavailable");
    assert_eq!(
        got.body["message"],
        "the directory of realm corp is unavailable"
    );
}

#[tokio::test]
async fn realm_logins_are_throttled_by_the_external_name() {
    let (env, _ldap) = setup().await;
    for i in 0..5 {
        let got = login(&env, "alice", "wrong", Some("corp")).await;
        assert_eq!(got.status, StatusCode::UNAUTHORIZED, "{i}");
    }
    let got = login(&env, "alice", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    // 대문자로 바꿔도 같은 키다.
    let got = login(&env, "ALICE", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    // 다른 사용자와 로컬 사용자는 막히지 않는다.
    corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(
        login(&env, "admin", ADMIN_PASSWORD, None).await.status,
        StatusCode::OK
    );
}

/// 매핑되는 역할이 없는 로그인도 실패로 센다.
#[tokio::test]
async fn logins_without_a_role_are_throttled() {
    let (env, _ldap) = setup().await;
    for _ in 0..5 {
        let got = login(&env, "carol", "carol-pw", Some("corp")).await;
        assert_error(&got, StatusCode::FORBIDDEN, "no_role");
    }
    let got = login(&env, "carol", "carol-pw", Some("corp")).await;
    assert_error(&got, StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
    assert!(stored(&env, "carol@corp").is_none());
}

/// 역할이 바뀌면 다른 세션은 끝나고, API 토큰은 남아 새 역할로 동작한다.
#[tokio::test]
async fn a_changed_role_ends_other_sessions_and_tokens_follow_it() {
    let (env, ldap) = setup().await;
    let (_, first) = corp_login(&env, "bob", "bob-pw").await;
    let (_, second) = corp_login(&env, "bob", "bob-pw").await;
    let bob = token_client(&env, &first).await;
    assert_eq!(bob.get("/auth/whoami").await.1["role"], "viewer");
    assert_eq!(bob.get("/realms").await.0, StatusCode::FORBIDDEN);

    ldap.set_groups("bob", &[ADMINS]);
    let (_, third) = corp_login(&env, "bob", "bob-pw").await;
    for old in [&first, &second] {
        assert_eq!(whoami(&env, old).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(whoami(&env, &third).await.1["role"], "admin");
    let (status, me) = bob.get("/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["role"], "admin");
    assert_eq!(bob.get("/realms").await.0, StatusCode::OK);
}

#[tokio::test]
async fn the_password_of_an_external_user_cannot_be_changed() {
    let (env, _ldap) = setup().await;
    let (_, cookie) = corp_login(&env, "bob", "bob-pw").await;
    let admin = env.admin().await;
    let (status, body) = admin
        .put(
            "/users/bob@corp",
            json!({ "password": "new-password" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "external_user");

    // 본인도 못 바꾼다.
    let bob = token_client(&env, &cookie).await;
    let (status, body) = bob
        .put(
            "/users/bob@corp",
            json!({ "password": "new-password", "current_password": "bob-pw" }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "external_user");

    // 역할은 바꿀 수 있다(다음 로그인에 매핑으로 다시 정해진다).
    let (status, body) = admin
        .put("/users/bob@corp", json!({ "role": "operator" }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["role"].as_str(), body["realm"].as_str()),
        (Some("operator"), Some("corp"))
    );
    let (body, _) = corp_login(&env, "bob", "bob-pw").await;
    assert_eq!(body["role"], "viewer");
}

/// 세션 쿠키로 API 토큰을 만들고 그 토큰을 쓰는 클라이언트
async fn token_client(env: &Env, cookie: &str) -> Client {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/tokens", env.base))
        .header("cookie", cookie)
        .json(&json!({ "name": "cli" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    Client {
        token: body["secret"].as_str().map(str::to_string),
        ..env.anonymous()
    }
}

#[tokio::test]
async fn external_users_use_api_tokens_until_their_realm_is_deleted() {
    let (env, ldap) = setup().await;
    let (_, cookie) = corp_login(&env, "alice", "alice-pw").await;
    let alice = token_client(&env, &cookie).await;
    let (status, me) = alice.get("/auth/whoami").await;
    assert_eq!(status, StatusCode::OK, "{me}");
    assert_eq!(me["name"], "alice@corp");
    // 외부 admin도 admin이다.
    assert_eq!(alice.get("/realms").await.0, StatusCode::OK);

    let admin = env.admin().await;
    assert_eq!(admin.delete("/realms/corp").await.0, StatusCode::NO_CONTENT);
    assert_eq!(whoami(&env, &cookie).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(alice.get("/auth/whoami").await.0, StatusCode::UNAUTHORIZED);
    // 사용자는 남는다.
    assert!(stored(&env, "alice@corp").is_some());

    // 같은 이름으로 다시 만들면 이어 쓴다.
    assert_eq!(
        admin.post("/realms", corp(&ldap)).await.0,
        StatusCode::CREATED
    );
    assert_eq!(whoami(&env, &cookie).await.0, StatusCode::OK);
    assert_eq!(alice.get("/auth/whoami").await.0, StatusCode::OK);
}

#[tokio::test]
async fn concurrent_first_logins_make_one_user() {
    let (env, _ldap) = setup().await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let url = format!("{}/api/v1/auth/login", env.base);
        set.spawn(async move {
            reqwest::Client::new()
                .post(url)
                .json(&json!({ "username": "bob", "password": "bob-pw", "realm": "corp" }))
                .send()
                .await
                .unwrap()
                .status()
        });
    }
    for status in set.join_all().await {
        assert_eq!(status, StatusCode::OK);
    }
    let (_, users) = env.cluster.list::<UserSpec>().unwrap();
    assert_eq!(users.iter().filter(|u| u.name == "bob@corp").count(), 1);
}

/// 외부 사용자를 쓰는 로그인은 옛 노드가 모르는 필드(`realm`)를 쓰므로 기다린다.
#[tokio::test]
async fn realm_logins_wait_until_every_node_runs_this_build() {
    let (env, _ldap) = setup().await;
    let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    node.status.version = Some("0.0.1~old".into());
    env.cluster.create(&node).await.unwrap();
    let got = login(&env, "alice", "alice-pw", Some("corp")).await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert!(stored(&env, "alice@corp").is_none());
}

#[tokio::test]
async fn realm_logins_are_audited_with_the_realm() {
    let (env, _ldap) = setup().await;
    corp_login(&env, "alice", "alice-pw").await;
    login(&env, "bob", "wrong", Some("corp")).await;
    login(&env, "bob", "wrong", Some("Bad Realm")).await;
    let start = Instant::now();
    let entries = loop {
        let entries: Vec<AuditEntry> = env
            .state
            .audit
            .read(AuditQuery {
                since: 0,
                until: u64::MAX,
                user: None,
                action: None,
                limit: 1000,
            })
            .await
            .unwrap();
        let logins = entries
            .iter()
            .filter(|e| e.action == "POST /auth/login")
            .count();
        // admin 로그인(setup)과 셋
        if logins >= 4 {
            break entries;
        }
        assert!(start.elapsed() < WAIT, "audit entries: {entries:#?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let detail = |status: u16, detail: &str| {
        entries
            .iter()
            .find(|e| e.status == status && e.detail.as_deref() == Some(detail))
            .unwrap_or_else(|| panic!("{status} {detail}: {entries:#?}"))
    };
    let ok = detail(200, "username=alice realm=corp");
    assert_eq!(ok.user.as_deref(), Some("alice@corp"));
    detail(401, "username=bob realm=corp");
    detail(401, "username=bob realm=<invalid>");
    let text = serde_json::to_string(&entries).unwrap();
    assert!(!text.contains("alice-pw"), "{text}");
}
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1336개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat(api): LDAP 영역 로그인과 외부 사용자 — POST /auth/login에 realm(없거나 local이면 그대로). 영역 로그인은 external_name(<사용자>@<영역>, 사용자 부분 소문자)이 안 되면 IP만 실패로 세고 401, 모르는 영역은 400 invalid_request, 시도 제한 키는 외부 사용자 이름, 디렉터리 확인(ldap::authenticate)의 InvalidCredentials는 로컬과 같은 401 invalid_credentials, Unavailable은 503 realm_unavailable(원인은 warn 로그), 매핑되는 역할이 없으면 403 no_role. 그 뒤 require_current를 보고 외부 사용자(realm, 빈 password_hash)를 만들거나 역할이 바뀌었으면 역할과 session_epoch를 고침(같으면 쓰지 않음, 겹친 로그인의 already_exists·name_taken·revision 충돌은 다시 읽어 한 번 더). 감사 detail에 realm=<영역>(이름 형식이 아니면 <invalid>). 외부 사용자의 비밀번호 PUT은 400 external_user(역할은 바꿀 수 있고 다음 로그인에 매핑으로 다시 정해짐). 세션·토큰 인증과 principal_for는 외부 사용자의 영역이 없으면 401(다시 만들면 이어 씀). UserView.realm. 가짜 LDAP 서버에 set_groups. schema.d.ts 다시 만듦"
```

---

### Task 5: oplctl realm과 실제 OpenLDAP 시험

`oplctl realm list|show|add-ldap|set-ldap|remove|test`(`set-ldap`은 준 것만 바꾸고 `--clear-*`로 지움), `oplctl login --realm`. opl-api 기능 `ldap-it`의 `tests/ldap_it.rs`와 CI 작업 `ldap`: Ubuntu 러너에 slapd를 설치해 `.github/scripts/ldap-setup.sh`(slapd를 지우고 새로 설정하므로 `CI=true`나 `OPL_LDAP_SETUP_WIPE=1`에서만 돈다)로 LDAPS·StartTLS·memberOf를 켜고 실제 디렉터리로 로그인을 시험한다. 이 Mac에서는 `ldap-it` 컴파일과 shellcheck까지.

**Files:**
- Create: `.github/scripts/ldap-setup.sh`
- Create: `crates/opl-api/tests/ldap_it.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `crates/opl-api/Cargo.toml`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/scripts/ldap-setup.sh`를 만든다:

```sh
#!/bin/sh
# CI `ldap` 작업의 시험 디렉터리: Ubuntu의 slapd(OpenLDAP)를 debconf로 설정하고, 시험 CA가 서명한
# 서버 인증서로 LDAPS(636)와 StartTLS(389)를 켜고, memberOf 오버레이 아래 사용자 둘과 그룹 하나를
# 넣는다. opl-api의 `ldap-it` 시험(tests/ldap_it.rs)이 이 디렉터리를 쓴다.
# 값은 시험과 같은 OPL_TEST_LDAP_* 환경 변수에서 읽는다(없으면 아래 기본값).
# 경고: 이 기계의 slapd 설정과 디렉터리(데이터베이스)를 지우고 새로 만든다. 그래서 CI(CI=true)가
# 아니면 OPL_LDAP_SETUP_WIPE=1을 주어야 돈다.
# 쓰는 법(root): sudo -E sh .github/scripts/ldap-setup.sh  (CI 밖: sudo -E OPL_LDAP_SETUP_WIPE=1 sh …)
# 다시 돌려도 된다: 늘 debconf 값으로 slapd를 다시 설정해 새 데이터베이스에서 시작한다.
set -eu

if [ "${CI:-}" != true ] && [ "${OPL_LDAP_SETUP_WIPE:-}" != 1 ]; then
	cat >&2 <<'MSG'
ldap-setup: this wipes the slapd configuration and directory on this machine and makes a new test
directory. Run it only on a throwaway machine: set OPL_LDAP_SETUP_WIPE=1 (CI=true also allows it).
MSG
	exit 1
fi

BASE_DN=${OPL_TEST_LDAP_BASE_DN:-dc=example,dc=com}
BIND_DN=${OPL_TEST_LDAP_BIND_DN:-cn=admin,$BASE_DN}
BIND_PASSWORD=${OPL_TEST_LDAP_BIND_PASSWORD:-opl-admin-pw}
CA_OUT=${OPL_TEST_LDAP_CA:-/tmp/opl-test-ldap-ca.pem}
USER_NAME=${OPL_TEST_LDAP_USER:-alice}
USER_PASSWORD=${OPL_TEST_LDAP_USER_PASSWORD:-alice-pw}
OTHER_NAME=${OPL_TEST_LDAP_OTHER_USER:-bob}
OTHER_PASSWORD=${OPL_TEST_LDAP_OTHER_PASSWORD:-bob-pw}
GROUP_DN=${OPL_TEST_LDAP_GROUP:-cn=admins,ou=groups,$BASE_DN}

# slapd가 읽을 TLS 파일. Ubuntu의 slapd AppArmor 프로필이 /etc/ldap 아래를 읽게 한다.
TLS_CA=/etc/ldap/opl-test-ca.pem
TLS_CERT=/etc/ldap/opl-test-cert.pem
TLS_KEY=/etc/ldap/opl-test-key.pem

die() {
	echo "ldap-setup: $*" >&2
	exit 1
}

[ "$(id -u)" -eq 0 ] || die "run as root"

# dc=example,dc=com → example.com (debconf가 이 도메인으로 기준 DN을 만든다)
DOMAIN=$(printf '%s\n' "$BASE_DN" | sed -e 's/^dc=//' -e 's/,dc=/./g')
[ "$(printf '%s\n' "$DOMAIN" | sed -e 's/^/dc=/' -e 's/\./,dc=/g')" = "$BASE_DN" ] ||
	die "OPL_TEST_LDAP_BASE_DN must be dc=…,dc=… (got $BASE_DN)"
# debconf는 관리자를 cn=admin,<기준 DN>(rootDN)으로 만든다.
[ "$BIND_DN" = "cn=admin,$BASE_DN" ] || die "OPL_TEST_LDAP_BIND_DN must be cn=admin,$BASE_DN"
GROUP_CN=${GROUP_DN%%,*}
GROUP_CN=${GROUP_CN#cn=}
[ "$GROUP_DN" = "cn=$GROUP_CN,ou=groups,$BASE_DN" ] ||
	die "OPL_TEST_LDAP_GROUP must be cn=<name>,ou=groups,$BASE_DN"

export DEBIAN_FRONTEND=noninteractive

# slapd의 설정 스크립트는 쓴 비밀번호 답을 지우므로, 설치와 다시 설정 앞에서 매번 넣는다.
preseed() {
	debconf-set-selections <<SEL
slapd slapd/no_configuration boolean false
slapd slapd/domain string $DOMAIN
slapd shared/organization string opLite test
slapd slapd/password1 password $BIND_PASSWORD
slapd slapd/password2 password $BIND_PASSWORD
slapd slapd/purge_database boolean true
slapd slapd/move_old_database boolean true
slapd slapd/dump_database select when needed
SEL
}

echo "== slapd: domain $DOMAIN"
preseed
apt-get update -q
apt-get install -y -q slapd ldap-utils openssl

# cn=config를 고치는 ldapmodify·찾기(로컬 소켓, root의 EXTERNAL)
config_modify() {
	ldapmodify -Q -Y EXTERNAL -H ldapi:///
}
config_search() {
	ldapsearch -Q -LLL -o ldif-wrap=no -Y EXTERNAL -H ldapi:/// -b cn=config "$@"
}

# slapd가 소켓으로 답할 때까지(최대 30초)
wait_slapd() {
	i=0
	until config_search -s base dn >/dev/null 2>&1; do
		i=$((i + 1))
		[ "$i" -lt 30 ] || die "slapd did not answer on ldapi:///"
		sleep 1
	done
}

# 미리 설치된 slapd이거나 다시 돌릴 때도 같은 상태가 되게 늘 다시 설정한다(옛 데이터베이스는
# debconf의 move_old_database로 /var/backups에 옮겨진다).
echo "== slapd: reconfigure with a new database"
preseed
dpkg-reconfigure -f noninteractive slapd
wait_slapd
slapd -VV 2>&1 | head -n 1 || true

DB_DN=$(config_search "(olcSuffix=$BASE_DN)" dn | sed -n 's/^dn: //p')
[ -n "$DB_DN" ] || die "no database for $BASE_DN"
echo "== database $DB_DN"

echo "== TLS: test CA and a server certificate for 127.0.0.1 and localhost"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cat >"$WORK/ca.cnf" <<CNF
[req]
distinguished_name = dn
prompt = no
x509_extensions = v3_ca
[dn]
CN = opLite test LDAP CA
[v3_ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
CNF
cat >"$WORK/server.cnf" <<CNF
[req]
distinguished_name = dn
prompt = no
[dn]
CN = localhost
CNF
openssl req -x509 -newkey rsa:2048 -nodes -days 2 -sha256 -config "$WORK/ca.cnf" \
	-keyout "$WORK/ca.key" -out "$WORK/ca.pem" 2>/dev/null
openssl req -new -newkey rsa:2048 -nodes -sha256 -config "$WORK/server.cnf" \
	-keyout "$WORK/server.key" -out "$WORK/server.csr" 2>/dev/null
cat >"$WORK/server.ext" <<EXT
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1
EXT
openssl x509 -req -in "$WORK/server.csr" -CA "$WORK/ca.pem" -CAkey "$WORK/ca.key" \
	-CAcreateserial -days 2 -sha256 -extfile "$WORK/server.ext" -out "$WORK/server.pem" 2>/dev/null
openssl verify -CAfile "$WORK/ca.pem" "$WORK/server.pem"

install -m 0644 "$WORK/ca.pem" "$TLS_CA"
install -m 0644 "$WORK/server.pem" "$TLS_CERT"
install -m 0640 -g openldap "$WORK/server.key" "$TLS_KEY"
install -m 0644 "$WORK/ca.pem" "$CA_OUT"

config_modify <<LDIF
dn: cn=config
changetype: modify
replace: olcTLSCACertificateFile
olcTLSCACertificateFile: $TLS_CA
-
replace: olcTLSCertificateFile
olcTLSCertificateFile: $TLS_CERT
-
replace: olcTLSCertificateKeyFile
olcTLSCertificateKeyFile: $TLS_KEY
LDIF

echo "== memberOf overlay"
MODULE_DN=$(config_search "(objectClass=olcModuleList)" dn | sed -n 's/^dn: //p' | head -n 1)
if [ -z "$MODULE_DN" ]; then
	config_modify <<LDIF
dn: cn=module,cn=config
changetype: add
objectClass: olcModuleList
cn: module
olcModulePath: /usr/lib/ldap
olcModuleLoad: memberof
LDIF
elif ! config_search "(objectClass=olcModuleList)" olcModuleLoad | grep -qi memberof; then
	config_modify <<LDIF
dn: $MODULE_DN
changetype: modify
add: olcModuleLoad
olcModuleLoad: memberof
LDIF
fi
if ! config_search "(&(objectClass=olcMemberOf))" dn | grep -q '^dn:'; then
	config_modify <<LDIF
dn: olcOverlay=memberof,$DB_DN
changetype: add
objectClass: olcOverlayConfig
objectClass: olcMemberOf
olcOverlay: memberof
olcMemberOfDangling: ignore
olcMemberOfGroupOC: groupOfNames
olcMemberOfMemberAD: member
olcMemberOfMemberOfAD: memberOf
LDIF
fi

echo "== LDAPS and StartTLS listeners"
if grep -q '^[#[:space:]]*SLAPD_SERVICES=' /etc/default/slapd; then
	sed -i 's|^[#[:space:]]*SLAPD_SERVICES=.*|SLAPD_SERVICES="ldap:/// ldaps:/// ldapi:///"|' /etc/default/slapd
else
	echo 'SLAPD_SERVICES="ldap:/// ldaps:/// ldapi:///"' >>/etc/default/slapd
fi
grep '^SLAPD_SERVICES=' /etc/default/slapd
systemctl restart slapd
wait_slapd

echo "== people and a group"
USER_HASH=$(slappasswd -s "$USER_PASSWORD")
OTHER_HASH=$(slappasswd -s "$OTHER_PASSWORD")
ldapadd -x -H ldapi:/// -D "$BIND_DN" -w "$BIND_PASSWORD" <<LDIF
dn: ou=people,$BASE_DN
objectClass: organizationalUnit
ou: people

dn: ou=groups,$BASE_DN
objectClass: organizationalUnit
ou: groups

dn: uid=$USER_NAME,ou=people,$BASE_DN
objectClass: person
objectClass: organizationalPerson
objectClass: inetOrgPerson
uid: $USER_NAME
cn: $USER_NAME
sn: $USER_NAME
userPassword: $USER_HASH

dn: uid=$OTHER_NAME,ou=people,$BASE_DN
objectClass: person
objectClass: organizationalPerson
objectClass: inetOrgPerson
uid: $OTHER_NAME
cn: $OTHER_NAME
sn: $OTHER_NAME
userPassword: $OTHER_HASH

dn: $GROUP_DN
objectClass: groupOfNames
cn: $GROUP_CN
member: uid=$USER_NAME,ou=people,$BASE_DN
LDIF

echo "== check: LDAPS search sees memberOf, StartTLS bind as the user"
LDAPTLS_CACERT=$CA_OUT
LDAPTLS_REQCERT=demand
export LDAPTLS_CACERT LDAPTLS_REQCERT
ldapsearch -x -LLL -o ldif-wrap=no -H ldaps://127.0.0.1:636 -D "$BIND_DN" -w "$BIND_PASSWORD" \
	-b "$BASE_DN" "(uid=$USER_NAME)" memberOf >"$WORK/member"
cat "$WORK/member"
grep -qi "^memberOf: $GROUP_DN\$" "$WORK/member" || die "uid=$USER_NAME has no memberOf $GROUP_DN"
ldapwhoami -x -ZZ -H ldap://127.0.0.1:389 -D "uid=$USER_NAME,ou=people,$BASE_DN" -w "$USER_PASSWORD"
ldapwhoami -x -ZZ -H ldap://127.0.0.1:389 -D "uid=$OTHER_NAME,ou=people,$BASE_DN" -w "$OTHER_PASSWORD"
echo "ldap-setup: ready (CA $CA_OUT)"
```

`.github/workflows/ci.yml`에서

```yaml
          shopt -s globstar
          shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm
      - run: bash packaging/tests/build-deb-test.sh
```

을 다음으로 바꾼다:

```yaml
          shopt -s globstar
          shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm .github/scripts/*.sh
      - run: bash packaging/tests/build-deb-test.sh
```

`.github/workflows/ci.yml`에서

```yaml

  deny:
```

을 다음으로 바꾼다:

```yaml

  # LDAP 영역: 실제 OpenLDAP(slapd)에 LDAPS와 ldap://+StartTLS로 사용자 찾기·바인드, memberOf 그룹,
  # API의 영역 시험 동작과 외부 사용자 로그인. 디렉터리(시험 CA, 사용자 둘, 그룹 하나)는
  # ldap-setup.sh가 아래 OPL_TEST_LDAP_* 값으로 만든다.
  ldap:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    env:
      OPL_TEST_LDAP_URL: "ldaps://127.0.0.1:636"
      OPL_TEST_LDAP_STARTTLS_URL: "ldap://127.0.0.1:389"
      OPL_TEST_LDAP_CA: /tmp/opl-test-ldap-ca.pem
      OPL_TEST_LDAP_BASE_DN: "dc=example,dc=com"
      OPL_TEST_LDAP_BIND_DN: "cn=admin,dc=example,dc=com"
      OPL_TEST_LDAP_BIND_PASSWORD: opl-admin-pw
      OPL_TEST_LDAP_USER: alice
      OPL_TEST_LDAP_USER_PASSWORD: alice-pw
      OPL_TEST_LDAP_OTHER_USER: bob
      OPL_TEST_LDAP_OTHER_PASSWORD: bob-pw
      OPL_TEST_LDAP_GROUP: "cn=admins,ou=groups,dc=example,dc=com"
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: OpenSSL headers (native-tls)
        run: |
          sudo apt-get update
          sudo apt-get install -y libssl-dev pkg-config
      - name: slapd with LDAPS, StartTLS, two users and a group
        run: sudo -E sh .github/scripts/ldap-setup.sh
      - run: cargo test -p opl-api --features ldap-it --test ldap_it
      - name: slapd log
        if: failure()
        run: sudo journalctl -u slapd --no-pager -n 200

  deny:
```

`crates/opl-api/Cargo.toml`에서

```toml
storage-it = []
```

을 다음으로 바꾼다:

```toml
storage-it = []
# 실제 OpenLDAP(slapd)로 LDAP 영역을 확인하는 테스트. 디렉터리는 .github/scripts/ldap-setup.sh가 만들고
# OPL_TEST_LDAP_* 환경 변수로 알린다.
ldap-it = []
```

`crates/opl-api/tests/ldap_it.rs`를 만든다:

```rust
//! 실제 OpenLDAP(slapd)로 LDAP 영역 확인: LDAPS와 ldap://+StartTLS로 사용자 찾기·바인드와
//! memberOf 그룹, 틀린 자격 증명, 서비스 계정 실패·믿지 않는 CA, 그리고 API로 영역을 만들어
//! 시험 동작(틀린 서비스 계정 비밀번호는 service_bind에서 멈춤)과 외부 사용자 로그인(대문자 이름도
//! 같은 사용자). 디렉터리는 `.github/scripts/ldap-setup.sh`가 만든다.
//! `cargo test -p opl-api --features ldap-it --test ldap_it`
#![cfg(feature = "ldap-it")]

mod common;

use common::Env;
use opl_api::ldap::{LdapError, authenticate};
use opl_types::realm::{DEFAULT_GROUP_ATTR, DEFAULT_USER_FILTER, LdapRealm};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn var(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} must be set (see .github/scripts/ldap-setup.sh)"))
}

/// 시험 디렉터리. `user`는 `group`에 들고 `other`는 들지 않는다.
struct Directory {
    ldaps_url: String,
    starttls_url: String,
    ca_pem: String,
    bind_dn: String,
    bind_password: String,
    base_dn: String,
    user: (String, String),
    other: (String, String),
    group: String,
}

impl Directory {
    fn from_env() -> Self {
        let ca = var("OPL_TEST_LDAP_CA");
        Self {
            ldaps_url: var("OPL_TEST_LDAP_URL"),
            starttls_url: var("OPL_TEST_LDAP_STARTTLS_URL"),
            ca_pem: std::fs::read_to_string(&ca)
                .unwrap_or_else(|e| panic!("cannot read OPL_TEST_LDAP_CA {ca}: {e}")),
            bind_dn: var("OPL_TEST_LDAP_BIND_DN"),
            bind_password: var("OPL_TEST_LDAP_BIND_PASSWORD"),
            base_dn: var("OPL_TEST_LDAP_BASE_DN"),
            user: (
                var("OPL_TEST_LDAP_USER"),
                var("OPL_TEST_LDAP_USER_PASSWORD"),
            ),
            other: (
                var("OPL_TEST_LDAP_OTHER_USER"),
                var("OPL_TEST_LDAP_OTHER_PASSWORD"),
            ),
            group: var("OPL_TEST_LDAP_GROUP"),
        }
    }

    fn realm(&self, url: &str) -> LdapRealm {
        LdapRealm {
            url: url.into(),
            bind_dn: Some(self.bind_dn.clone()),
            bind_password: Some(self.bind_password.clone()),
            base_dn: self.base_dn.clone(),
            user_filter: DEFAULT_USER_FILTER.into(),
            group_attr: DEFAULT_GROUP_ATTR.into(),
            ca_pem: Some(self.ca_pem.clone()),
            timeout_secs: 10,
        }
    }

    fn in_group(&self, groups: &[String]) -> bool {
        groups.iter().any(|g| g.eq_ignore_ascii_case(&self.group))
    }
}

async fn login(env: &Env, user: &str, password: &str) -> (StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/auth/login", env.base))
        .json(&json!({ "username": user, "password": password, "realm": "corp" }))
        .send()
        .await
        .unwrap();
    (resp.status(), resp.json().await.unwrap())
}

#[tokio::test]
async fn a_real_directory_authenticates_users_and_maps_groups() {
    let dir = Directory::from_env();
    let (user, password) = (&dir.user.0, &dir.user.1);
    let (other, other_password) = (&dir.other.0, &dir.other.1);

    for url in [&dir.ldaps_url, &dir.starttls_url] {
        let realm = dir.realm(url);
        let got = authenticate(&realm, user, password)
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert!(
            got.dn
                .to_ascii_lowercase()
                .starts_with(&format!("uid={user},")),
            "{url}: {got:?}"
        );
        assert!(dir.in_group(&got.groups), "{url}: {got:?}");

        let got = authenticate(&realm, other, other_password)
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert!(!dir.in_group(&got.groups), "{url}: {got:?}");

        for (u, p) in [
            (user.as_str(), "wrong-password"),
            ("nobody", password.as_str()),
            ("*", password.as_str()),
        ] {
            assert_eq!(
                authenticate(&realm, u, p).await,
                Err(LdapError::InvalidCredentials),
                "{url}: {u}"
            );
        }

        // 서비스 계정 비밀번호가 틀림
        let mut bad = realm.clone();
        bad.bind_password = Some("wrong-password".into());
        let got = authenticate(&bad, user, password).await;
        assert!(
            matches!(got, Err(LdapError::Unavailable(_))),
            "{url}: {got:?}"
        );

        // 시스템 루트는 시험 CA를 믿지 않는다.
        let mut untrusted = realm.clone();
        untrusted.ca_pem = None;
        let got = authenticate(&untrusted, user, password).await;
        assert!(
            matches!(got, Err(LdapError::Unavailable(_))),
            "{url}: {got:?}"
        );
    }

    // API로 영역을 만들고 시험 동작과 로그인
    let env = Env::start().await;
    let admin = env.admin().await;
    let (status, body) = admin
        .post(
            "/realms",
            json!({
                "name": "corp",
                "backend": {
                    "kind": "ldap",
                    "url": dir.starttls_url,
                    "bind_dn": dir.bind_dn,
                    "bind_password": dir.bind_password,
                    "base_dn": dir.base_dn,
                    "ca_pem": dir.ca_pem,
                },
                "roles": { "groups": [{ "group": dir.group, "role": "admin" }] }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let (status, result) = admin
        .post(
            "/realms/corp/actions/test",
            json!({ "username": user, "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let steps: Vec<(&str, bool)> = result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s["ok"] == true))
        .collect();
    assert_eq!(
        steps,
        [
            ("connect", true),
            ("service_bind", true),
            ("search", true),
            ("user_bind", true),
            ("groups", true)
        ],
        "{result}"
    );
    assert_eq!(result["role"], "admin", "{result}");

    let (status, body) = login(&env, user, password).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], format!("{user}@corp"));
    assert_eq!(body["role"], "admin");
    assert_eq!(body["realm"], "corp");

    // 대문자로 들어와도 같은 외부 사용자다(디렉터리의 uid 비교는 대소문자를 가리지 않는다).
    let (status, body) = login(&env, &user.to_ascii_uppercase(), password).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], format!("{user}@corp"));

    // 매핑되는 그룹이 없다.
    let (status, body) = login(&env, other, other_password).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["code"], "no_role");

    let (status, body) = login(&env, user, "wrong-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "invalid_credentials");

    // 서비스 계정 비밀번호가 틀린 영역의 시험 동작은 service_bind에서 멈춘다.
    let (status, body) = admin
        .post(
            "/realms",
            json!({
                "name": "corp-bad",
                "backend": {
                    "kind": "ldap",
                    "url": dir.ldaps_url,
                    "bind_dn": dir.bind_dn,
                    "bind_password": "wrong-password",
                    "base_dn": dir.base_dn,
                    "ca_pem": dir.ca_pem,
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, result) = admin
        .post(
            "/realms/corp-bad/actions/test",
            json!({ "username": user, "password": password }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let last = result["steps"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        (last["name"].as_str(), last["ok"].as_bool()),
        (Some("service_bind"), Some(false)),
        "{result}"
    );
    assert_eq!(result["dn"], Value::Null, "{result}");
}
```

`crates/oplctl/src/cli.rs` 전체:

```rust
//! 명령 정의와 실행.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca, fingerprint,
    local,
};
use crate::config::{CliConfig, default_path};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Parser)]
#[command(name = "oplctl", version = opl_types::version::BUILD, about = "opLite command-line client")]
struct Cli {
    /// 로그인 정보 파일 (기본: ~/.config/opl/oplctl.json)
    #[arg(long, global = true, env = "OPL_CONFIG")]
    config: Option<PathBuf>,
    /// API 주소. 로그인할 때 저장한 주소 대신 쓴다.
    #[arg(long, global = true, env = "OPL_SERVER")]
    server: Option<String>,
    #[arg(short, long, global = true, value_enum, default_value = "table")]
    output: Output,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Output {
    Table,
    Json,
}

#[derive(Subcommand)]
enum Cmd {
    /// 로그인해 API 토큰을 만들고 저장한다.
    Login(LoginArgs),
    /// 저장한 API 토큰을 지운다(서버에서도).
    Logout,
    /// 클러스터 만들기, 노드 합류와 탈퇴, 상태
    Cluster {
        #[command(subcommand)]
        cmd: ClusterCmd,
    },
    Node {
        #[command(subcommand)]
        cmd: NodeCmd,
    },
    Pool {
        #[command(subcommand)]
        cmd: PoolCmd,
    },
    /// VM NIC가 연결하는 네트워크(노드에 미리 만든 Linux 브리지나 opld가 만드는 VXLAN 네트워크)
    Network {
        #[command(subcommand)]
        cmd: NetworkCmd,
    },
    /// VM NIC에 거는 방화벽 규칙 묶음. nftables가 있는 노드가 적용한다.
    SecurityGroup {
        #[command(subcommand)]
        cmd: SecurityGroupCmd,
    },
    Volume {
        #[command(subcommand)]
        cmd: VolumeCmd,
    },
    Vm {
        #[command(subcommand)]
        cmd: VmCmd,
    },
    /// VM 백업(백업 풀에 담은 디스크 복사본과 VM 설정)과 복원
    Backup {
        #[command(subcommand)]
        cmd: BackupCmd,
    },
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
    /// 다른 하이퍼바이저(VMware)의 VM 가져오기(OVA, ESXi·vCenter)
    Import {
        #[command(subcommand)]
        cmd: ImportCmd,
    },
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// 인증 영역(admin만): 바깥 LDAP 디렉터리로 로그인하게 한다.
    Realm {
        #[command(subcommand)]
        cmd: RealmCmd,
    },
    /// 클러스터 설정(업데이트 채널)
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// 구독 키와 상태. 구독은 기능을 막지 않는다.
    Subscription {
        #[command(subcommand)]
        cmd: SubscriptionCmd,
    },
    /// 옵트인 텔레메트리(기본 꺼짐). 켜면 리더가 하루 한 번 클러스터 규모를 보낸다.
    Telemetry {
        #[command(subcommand)]
        cmd: TelemetryCmd,
    },
    /// 감사 로그(admin만): 모든 노드의 바꾸는 요청·로그인·콘솔 열기를 새 것부터
    Audit(AuditArgs),
}

#[derive(Args)]
struct LoginArgs {
    #[arg(long, default_value = "admin")]
    user: String,
    /// 인증 영역. `local`이 아니면 그 영역의 사용자 이름(`@영역` 없이)으로 로그인한다.
    #[arg(long, default_value = "local")]
    realm: String,
    /// 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
    /// HTTPS 서버의 클러스터 CA 지문(`sha256:...`). 처음 로그인할 때 필요하다.
    /// `oplctl cluster init`의 출력이나 다른 노드의 `oplctl cluster status`에서 확인한다.
    #[arg(long)]
    ca_fingerprint: Option<String>,
}

/// 이 기계의 opld에 제어 소켓으로 보내는 명령의 공통 인자
#[derive(Args)]
struct LocalArgs {
    /// opld 제어 소켓
    #[arg(long, env = "OPL_SOCKET", default_value = "/run/opl/opld.sock")]
    socket: PathBuf,
}

#[derive(Subcommand)]
enum ClusterCmd {
    /// 이 노드 하나로 새 클러스터를 만든다. 관리자 비밀번호와 다음 노드의 조인 URL을 출력한다.
    Init {
        /// 클러스터 이름
        #[arg(long)]
        name: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 조인 URL의 클러스터에 더한다.
    Join {
        url: String,
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 이 노드를 클러스터에서 뺀다. 이 노드에 배정된 VM이 없어야 한다.
    Leave {
        #[command(flatten)]
        local: LocalArgs,
    },
    /// 클러스터 이름, CA 지문, 멤버
    Status,
    /// 새 노드를 더할 조인 URL을 만든다. 한 번만 쓸 수 있다.
    JoinToken {
        /// 유효 시간(초)
        #[arg(long, default_value_t = 3600)]
        ttl: u64,
    },
    /// 죽은 노드를 클러스터에서 뺀다. 되돌릴 수 없다.
    RemoveNode {
        node: String,
        /// 확인
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    List,
    Get {
        node: String,
    },
    /// 노드의 업데이트 확인 결과(올릴 수 있는 패키지)를 보인다.
    Updates {
        node: String,
        /// 먼저 노드에서 다시 확인하고(Task가 끝날 때까지 기다린다) 결과를 보인다.
        #[arg(long)]
        refresh: bool,
    },
    /// 노드의 패키지를 올린다(`apt-get dist-upgrade`). VM은 계속 돌고 opld가 잠깐 다시 시작한다. 한 번에
    /// 한 노드만 한다.
    Upgrade {
        node: String,
        /// 업그레이드 Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
}

/// `node upgrade`: 업그레이드 Task를 만든다. `wait`이면 끝날 때까지 기다리고, 아니면 Task를 따라가는
/// 방법을 알린다.
async fn node_upgrade(
    client: &Client,
    node: &str,
    wait: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .post(&format!("/nodes/{node}/upgrade"), &json!({}))
        .await?;
    if wait {
        client.wait_task_for(&task, UPGRADE_WAIT).await?;
        writeln!(out, "node {node} upgraded")?;
    } else {
        let id = cell(&task["id"]);
        writeln!(
            out,
            "upgrade of {node} started (task {id}); follow it with `oplctl task get {id}`"
        )?;
    }
    Ok(())
}

/// `node updates`의 패키지 표 열
const UPDATE_HEADERS: &[&str] = &["PACKAGE", "CURRENT", "CANDIDATE"];

/// `node updates`: `--refresh`면 다시 확인 Task를 기다린 뒤, 노드의 `status.updates`를 보인다. 확인 시각,
/// 오류, 패키지 표(새로 설치할 패키지는 CURRENT가 `-`), 잘렸는지. `-o json`이면 `status.updates` 그대로.
async fn node_updates(
    client: &Client,
    node: &str,
    refresh: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if refresh {
        let task = client
            .post(&format!("/nodes/{node}/updates/refresh"), &json!({}))
            .await?;
        client.wait_task(&task).await?;
    }
    let found = client.get(&format!("/nodes/{node}")).await?;
    let updates = &found["status"]["updates"];
    if json_out {
        return Ok(print_json(out, updates)?);
    }
    if !updates.is_object() {
        bail!("node {node} does not check for updates");
    }
    let checked = updates["checked_at"]
        .as_i64()
        .map_or_else(|| "never".to_string(), unix_time);
    writeln!(out, "checked: {checked}")?;
    if let Some(error) = updates["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let packages = updates["packages"].as_array().cloned().unwrap_or_default();
    if packages.is_empty() {
        if updates["checked_at"].is_u64() && updates["error"].is_null() {
            writeln!(out, "no updates available")?;
        }
        return Ok(());
    }
    let rows: Vec<_> = packages
        .iter()
        .map(|p| vec![cell(&p["name"]), cell(&p["current"]), cell(&p["candidate"])])
        .collect();
    table(out, UPDATE_HEADERS, &rows)?;
    if updates["truncated"] == true {
        writeln!(out, "(only the first {} packages are shown)", rows.len())?;
    }
    Ok(())
}

#[derive(Subcommand)]
enum PoolCmd {
    List,
    Get {
        pool: String,
    },
    /// 로컬 디렉터리 풀, NFS 풀, LVM-thin 풀, ZFS 풀, Ceph RBD 풀, 공유 LVM 풀을 만든다.
    Create(Box<PoolCreate>),
    /// 디렉터리 풀을 공유 스토리지로 표시하거나 노드 로컬로 되돌린다. 백업을 담는 풀로 표시하거나 끈다.
    Set(PoolSet),
    Delete {
        pool: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args(["shared", "no_shared", "backups"])))]
struct PoolSet {
    pool: String,
    /// 모든 노드가 이 경로에서 같은 파일을 보는 공유 스토리지다.
    #[arg(long, conflicts_with = "no_shared")]
    shared: bool,
    /// 노드 로컬 풀로 되돌린다. 볼륨은 만든 노드에 있는 것으로 돌아간다(그 볼륨을 다른 노드에서
    /// 쓰는 VM이나 마이그레이션 중인 VM이 있으면 거절한다).
    #[arg(long)]
    no_shared: bool,
    /// VM 백업을 담는 풀로 표시한다(true, 디렉터리·NFS 풀만)거나 끈다(false, 백업이 남아 있으면 거절한다).
    #[arg(long, value_name = "true|false")]
    backups: Option<bool>,
}

/// `pool set`의 요청 본문(`PATCH /storage-pools/{key}`). 주지 않은 값은 보내지 않는다.
fn pool_set_body(set: &PoolSet) -> Value {
    let mut body = json!({});
    if set.shared || set.no_shared {
        body["shared"] = json!(set.shared);
    }
    if let Some(backups) = set.backups {
        body["backups"] = json!(backups);
    }
    body
}

/// `pool set`이 바꾼 것: `shared, backups on`
fn pool_set_summary(set: &PoolSet) -> String {
    let mut parts = Vec::new();
    if set.shared {
        parts.push("shared");
    } else if set.no_shared {
        parts.push("node-local");
    }
    match set.backups {
        Some(true) => parts.push("backups on"),
        Some(false) => parts.push("backups off"),
        None => {}
    }
    parts.join(", ")
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["path", "nfs", "lvm_thin", "zfs", "rbd", "lvm_shared"])))]
struct PoolCreate {
    name: String,
    /// 풀 디렉터리(절대 경로)
    #[arg(long, conflicts_with = "nfs")]
    path: Option<PathBuf>,
    /// NFS 내보내기 `SERVER:/EXPORT`(IPv6는 `[ADDR]:/EXPORT`). 노드마다 마운트하는 공유 풀이다.
    #[arg(long, value_parser = parse_nfs)]
    nfs: Option<NfsSource>,
    /// `mount -o`에 넘길 쉼표 목록(예: vers=4.2,hard)
    #[arg(long, requires = "nfs", conflicts_with = "path")]
    nfs_options: Option<String>,
    /// 관리자가 준비한 VG와 thin pool `VG/THINPOOL`. 노드 로컬 풀이고 볼륨은 raw만 담는다.
    #[arg(long, value_parser = parse_lvm_thin, conflicts_with_all = ["path", "nfs", "nfs_options", "shared"])]
    lvm_thin: Option<LvmThinSource>,
    /// 관리자가 준비한 ZFS 데이터셋 `POOL/DATASET`. 노드 로컬 풀이고 볼륨은 raw zvol만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "shared"])]
    zfs: Option<String>,
    /// 관리자가 준비한 Ceph 풀과 RBD 네임스페이스 `POOL[/NAMESPACE]`. 늘 공유 풀이고 볼륨은 raw
    /// 이미지만 담는다.
    #[arg(long, value_parser = parse_rbd, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "shared"])]
    rbd: Option<RbdSource>,
    /// RBD 풀에 쓸 Ceph 클라이언트 id(`client.` 없이). 생략하면 admin.
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_user: Option<String>,
    /// RBD 풀에 쓸 ceph.conf 절대 경로. 생략하면 Ceph 기본(/etc/ceph/ceph.conf).
    #[arg(long, requires = "rbd", conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs"])]
    rbd_conf: Option<String>,
    /// 관리자가 준비한 공유 VG(lvmlockd). 늘 공유 풀이고 볼륨은 raw 두꺼운 LV만 담는다.
    #[arg(long, conflicts_with_all = ["path", "nfs", "nfs_options", "lvm_thin", "zfs", "rbd", "rbd_user", "rbd_conf", "shared"])]
    lvm_shared: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃의 포털 `HOST[:PORT]`(IPv6는 `[ADDR][:PORT]`). 노드가 로그인한다.
    #[arg(long, requires_all = ["lvm_shared", "iscsi_target"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_portal: Option<String>,
    /// 공유 VG가 놓인 iSCSI 타깃 이름(IQN)
    #[arg(long, requires_all = ["lvm_shared", "iscsi_portal"], conflicts_with_all = ["path", "nfs", "lvm_thin", "zfs", "rbd"])]
    iscsi_target: Option<String>,
    /// `--path`가 모든 노드가 같은 데이터를 보는 공유 스토리지(밖에서 마운트한 공유 디렉터리)다.
    /// NFS·RBD·공유 LVM 풀은 늘 공유다.
    #[arg(long, requires = "path", conflicts_with = "nfs")]
    shared: bool,
    /// 이 풀을 쓸 수 있는 노드 id. 생략하면 모든 노드.
    #[arg(long = "node")]
    nodes: Vec<String>,
    /// VM 백업을 담는 풀로 표시한다. 디렉터리·NFS 풀만 된다.
    #[arg(long, conflicts_with_all = ["lvm_thin", "zfs", "rbd", "lvm_shared"])]
    backups: bool,
}

/// `pool create`의 요청 본문. NFS·RBD·공유 LVM 풀의 `shared`는 적지 않는다(서버가 늘 공유로 둔다).
fn pool_create_body(args: PoolCreate) -> Value {
    let PoolCreate {
        name,
        path,
        nfs,
        nfs_options,
        lvm_thin,
        zfs,
        rbd,
        rbd_user,
        rbd_conf,
        lvm_shared,
        iscsi_portal,
        iscsi_target,
        shared,
        nodes,
        backups,
    } = args;
    let backend = match (nfs, lvm_thin, zfs, rbd, lvm_shared) {
        (Some(n), _, _, _, _) => json!({
            "kind": "nfs", "server": n.server, "export": n.export, "options": nfs_options
        }),
        (None, Some(l), _, _, _) => {
            json!({ "kind": "lvm-thin", "vg": l.vg, "thinpool": l.thinpool })
        }
        (None, None, Some(dataset), _, _) => json!({ "kind": "zfs", "dataset": dataset }),
        (None, None, None, Some(r), _) => json!({
            "kind": "rbd", "pool": r.pool, "namespace": r.namespace,
            "user": rbd_user, "conf": rbd_conf
        }),
        (None, None, None, None, Some(vg)) => {
            // 포털과 타깃은 clap이 함께 받게 한다.
            let iscsi = iscsi_portal
                .zip(iscsi_target)
                .map(|(portal, target)| json!({ "portal": portal, "target": target }));
            json!({ "kind": "lvm-shared", "vg": vg, "iscsi": iscsi })
        }
        (None, None, None, None, None) => json!({ "kind": "dir", "path": path }),
    };
    let mut spec = json!({ "backend": backend, "nodes": nodes });
    if shared {
        spec["shared"] = json!(true);
    }
    if backups {
        spec["backups"] = json!(true);
    }
    json!({ "name": name, "spec": spec })
}

/// `--nfs`로 받은 NFS 서버와 내보내기 경로
#[derive(Debug, Clone, PartialEq, Eq)]
struct NfsSource {
    server: String,
    export: String,
}

/// `nas:/srv/vm`, `[fd00::1]:/srv/vm`처럼 쓴다. 서버 이름의 나머지 검사는 API가 한다.
fn parse_nfs(s: &str) -> Result<NfsSource, String> {
    let split = match s.strip_prefix('[') {
        Some(rest) => rest.split_once("]:"),
        None => s.split_once(':'),
    };
    match split {
        Some((server, export)) if !server.is_empty() && export.starts_with('/') => Ok(NfsSource {
            server: server.into(),
            export: export.into(),
        }),
        _ => Err(format!(
            "invalid nfs source {s:?}: use SERVER:/EXPORT ([ADDR]:/EXPORT for IPv6)"
        )),
    }
}

/// `--lvm-thin`으로 받은 VG와 thin pool 이름
#[derive(Debug, Clone, PartialEq, Eq)]
struct LvmThinSource {
    vg: String,
    thinpool: String,
}

/// `vg0/tp`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_lvm_thin(s: &str) -> Result<LvmThinSource, String> {
    match s.split_once('/') {
        Some((vg, thinpool))
            if !vg.is_empty() && !thinpool.is_empty() && !thinpool.contains('/') =>
        {
            Ok(LvmThinSource {
                vg: vg.into(),
                thinpool: thinpool.into(),
            })
        }
        _ => Err(format!("invalid lvm-thin pool {s:?}: use VG/THINPOOL")),
    }
}

/// `--rbd`로 받은 Ceph 풀과 RBD 네임스페이스
#[derive(Debug, Clone, PartialEq, Eq)]
struct RbdSource {
    pool: String,
    namespace: Option<String>,
}

/// `rbd`, `rbd/opl`처럼 쓴다. 이름 규칙의 나머지 검사는 API가 한다.
fn parse_rbd(s: &str) -> Result<RbdSource, String> {
    let (pool, namespace) = match s.split_once('/') {
        Some((pool, namespace)) => (pool, Some(namespace)),
        None => (s, None),
    };
    let bad = |n: &str| n.is_empty() || n.contains('/');
    if bad(pool) || namespace.is_some_and(bad) {
        return Err(format!(
            "invalid rbd pool {s:?}: use POOL or POOL/NAMESPACE"
        ));
    }
    Ok(RbdSource {
        pool: pool.into(),
        namespace: namespace.map(Into::into),
    })
}

#[derive(Subcommand)]
enum NetworkCmd {
    List,
    Get {
        network: String,
    },
    /// 노드에 있는 Linux 브리지를 가리키는 네트워크나 VXLAN 네트워크를 만든다.
    Create(NetworkCreate),
    Delete {
        network: String,
    },
}

#[derive(Args)]
#[command(group(ArgGroup::new("kind").required(true).args(["bridge", "vxlan"])))]
struct NetworkCreate {
    name: String,
    /// 브리지 이름(예: vmbr0). 이 네트워크를 쓰는 VM은 이 브리지가 있는 노드에서만 돈다.
    #[arg(long)]
    bridge: Option<String>,
    /// VLAN 태그(1-4094). 브리지가 VLAN-aware(vlan_filtering 1)여야 한다.
    #[arg(long, conflicts_with = "vxlan")]
    vlan: Option<u16>,
    /// VXLAN 네트워크 식별자(VNI, 1-16777215). opld가 모든 노드에 브리지 oplv<VNI>를 만든다.
    #[arg(long, value_name = "VNI")]
    vxlan: Option<u32>,
    /// VXLAN 네트워크의 MTU(1280-9000, 기본 1450)
    #[arg(long, conflicts_with = "bridge")]
    mtu: Option<u16>,
}

/// `network create`의 요청 본문(`POST /networks`). 주지 않은 값은 보내지 않는다.
fn network_create_body(c: &NetworkCreate) -> Value {
    let mut spec = serde_json::Map::new();
    if let Some(bridge) = &c.bridge {
        spec.insert("bridge".into(), json!(bridge));
    }
    if let Some(vlan) = c.vlan {
        spec.insert("vlan".into(), json!(vlan));
    }
    if let Some(vni) = c.vxlan {
        spec.insert("vxlan".into(), json!({ "vni": vni }));
    }
    if let Some(mtu) = c.mtu {
        spec.insert("mtu".into(), json!(mtu));
    }
    json!({ "name": c.name, "spec": spec })
}

#[derive(Subcommand)]
enum SecurityGroupCmd {
    List,
    Get {
        group: String,
    },
    /// 보안 그룹을 만든다. 규칙은 적힌 순서대로 보고 처음 맞는 것을 따른다. 맞는 규칙이 없으면
    /// 들어오는 것은 막고 나가는 것은 보낸다.
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 규칙 `in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]`
        /// (예: "in accept tcp 22 10.0.0.0/8"). 여러 번 줄 수 있다.
        #[arg(long = "rule", value_name = "RULE", value_parser = parse_rule)]
        rules: Vec<Value>,
    },
    /// 보안 그룹을 지운다. 그룹을 쓰는 VM NIC가 없어야 한다.
    Delete {
        group: String,
    },
    /// 규칙을 더하거나 뺀다. 그룹을 쓰는 노드가 곧바로 적용한다.
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// 규칙 하나를 더한다(RULE은 `security-group create --rule`과 같다).
    Add {
        group: String,
        #[arg(value_parser = parse_rule)]
        rule: Value,
        /// 이 번호(1부터) 자리에 넣는다. 생략하면 맨 끝이다.
        #[arg(long, value_name = "N")]
        at: Option<usize>,
    },
    /// N번 규칙(1부터, `security-group get`의 순서)을 뺀다.
    Rm { group: String, n: usize },
}

/// `in accept tcp 22 10.0.0.0/8`처럼 쓴다: 방향과 동작, 그 뒤로 프로토콜·포트·원격 CIDR을 이 차례로
/// (모두 생략할 수 있다). 값의 나머지 검사(포트 범위, 프로토콜과 포트·주소의 짝)는 API가 한다.
fn parse_rule(s: &str) -> Result<Value, String> {
    let bad = || {
        format!("invalid rule {s:?}: use in|out accept|drop [tcp|udp|icmp|icmpv6] [PORTS] [CIDR]")
    };
    let mut words = s.split_whitespace().peekable();
    let direction = words
        .next()
        .filter(|w| matches!(*w, "in" | "out"))
        .ok_or_else(bad)?;
    let action = words
        .next()
        .filter(|w| matches!(*w, "accept" | "drop"))
        .ok_or_else(bad)?;
    let mut rule = json!({ "direction": direction, "action": action });
    if let Some(protocol) = words.next_if(|w| matches!(*w, "tcp" | "udp" | "icmp" | "icmpv6")) {
        rule["protocol"] = json!(protocol);
    }
    if let Some(ports) = words.next_if(|w| is_ports(w)) {
        rule["ports"] = json!(ports);
    }
    if let Some(remote) = words.next_if(|w| w.contains(['.', ':'])) {
        rule["remote"] = json!(remote);
    }
    if words.next().is_some() {
        return Err(bad());
    }
    Ok(rule)
}

/// `22`나 `8000-8080` 모양인지
fn is_ports(word: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match word.split_once('-') {
        Some((start, end)) => digits(start) && digits(end),
        None => digits(word),
    }
}

/// `security-group create`의 요청 본문(`POST /security-groups`)
fn security_group_create_body(name: &str, description: Option<&str>, rules: &[Value]) -> Value {
    let mut spec = json!({ "rules": rules });
    if let Some(description) = description {
        spec["description"] = json!(description);
    }
    json!({ "name": name, "spec": spec })
}

/// 그룹의 규칙에 `rule`을 `at`번(1부터, 생략하면 맨 끝) 자리에 넣은 목록
fn rules_with(group: &Value, rule: Value, at: Option<usize>) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = at.unwrap_or(rules.len() + 1);
    if at == 0 || at > rules.len() + 1 {
        bail!(
            "cannot add a rule at {at}: security group {} has {} rules (use 1 to {})",
            cell(&group["name"]),
            rules.len(),
            rules.len() + 1
        );
    }
    rules.insert(at - 1, rule);
    Ok(rules)
}

/// 그룹의 규칙에서 `n`번(1부터)을 뺀 목록
fn rules_without(group: &Value, n: usize) -> anyhow::Result<Vec<Value>> {
    let mut rules = group["spec"]["rules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if n == 0 || n > rules.len() {
        bail!(
            "security group {} has no rule {n} ({} rules, numbered from 1)",
            cell(&group["name"]),
            rules.len()
        );
    }
    rules.remove(n - 1);
    Ok(rules)
}

/// 그룹을 읽어 `edit`으로 만든 규칙 목록으로 바꾼다. 읽은 revision을 `If-Match`로 보내므로 그 사이
/// 다른 사람이 바꿨으면 실패한다(규칙 번호가 달라졌을 수 있다).
async fn patch_rules(
    client: &Client,
    group: &str,
    edit: impl FnOnce(&Value) -> anyhow::Result<Vec<Value>>,
) -> anyhow::Result<()> {
    let path = format!("/security-groups/{group}");
    let current = client.get(&path).await?;
    let rules = edit(&current)?;
    let r = client
        .request(Method::PATCH, &path)
        .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
        .json(&json!({ "rules": rules }));
    client.send(r).await?;
    Ok(())
}

/// `security-group list`의 열
const SECURITY_GROUP_HEADERS: &[&str] = &["NAME", "RULES", "DESCRIPTION"];

/// `security-group list`의 한 행
fn security_group_row(g: &Value) -> Vec<String> {
    vec![
        cell(&g["name"]),
        g["spec"]["rules"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        cell(&g["spec"]["description"]),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Qcow2,
    Raw,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Subcommand)]
enum VolumeCmd {
    List,
    Get {
        volume: String,
    },
    Create {
        name: String,
        #[arg(long)]
        pool: String,
        /// 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 이미지 파일(VMDK 등)을 올려 새 볼륨으로 가져온다.
    Import {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, value_enum, default_value = "vmdk")]
        source_format: SourceFormat,
        /// 생략하면 풀 종류로 정한다(LVM-thin, ZFS, RBD, 공유 LVM은 raw, 그 밖은 qcow2).
        #[arg(long, value_enum)]
        format: Option<Format>,
    },
    /// 볼륨 크기를 늘린다. 줄이지는 않는다.
    Resize {
        volume: String,
        /// 새 크기. 단위 K, M, G, T(1024 배수)를 붙일 수 있다.
        #[arg(long, value_parser = parse_size)]
        size: u64,
    },
    Delete {
        volume: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Machine {
    Virt,
    Q35,
}

#[derive(Clone, Copy, ValueEnum)]
enum Firmware {
    Uefi,
    Bios,
}

#[derive(Clone, Copy, ValueEnum)]
enum Restart {
    Never,
    OnFailure,
}

#[derive(Subcommand)]
enum VmCmd {
    List,
    Get {
        vm: String,
    },
    Create(Box<VmCreate>),
    /// VM 설정을 바꾼다. 실행 중이어도 된다.
    Set(VmSet),
    /// NIC의 보안 그룹을 바꾼다. 실행 중이어도 곧바로 적용된다. 그룹을 주지 않으면 모두 뺀다.
    NicSg {
        vm: String,
        /// NIC 번호(0부터, `vm get`의 nics 순서)
        index: usize,
        groups: Vec<String>,
    },
    Start {
        vm: String,
    },
    Stop {
        vm: String,
        /// ACPI 응답을 기다리지 않고 곧바로 끈다.
        #[arg(long)]
        force: bool,
    },
    Reboot {
        vm: String,
    },
    /// 실행 중인 VM을 멈추지 않고 다른 노드로 옮긴다(디스크가 모두 공유 풀에 있어야 한다).
    Migrate {
        vm: String,
        /// 대상 노드 이름이나 id
        node: String,
    },
    Delete {
        vm: String,
        /// 스냅샷을 하나씩 먼저 지운다.
        #[arg(long)]
        with_snapshots: bool,
    },
    /// VM을 복제해 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Clone {
        vm: String,
        /// 새 VM 이름
        name: String,
        /// 이 스냅샷(이름이나 id)의 상태에서 복제한다. 생략하면 꺼진 VM의 지금 상태에서 복제한다.
        #[arg(long)]
        snapshot: Option<String>,
        /// 새 디스크를 둘 풀. 생략하면 원본 볼륨과 같은 풀이다.
        #[arg(long)]
        pool: Option<String>,
    },
    /// VM 스냅샷(디스크, 실행 중이면 메모리, VM 설정)
    Snapshot {
        #[command(subcommand)]
        cmd: SnapshotCmd,
    },
    /// 로컬 포트에서 VNC 연결을 받아 VM 콘솔로 잇는다. Ctrl-C로 끝낸다.
    Console {
        vm: String,
        /// 0이면 비어 있는 포트를 고른다.
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
}

#[derive(Args)]
struct VmCreate {
    name: String,
    #[arg(long, default_value_t = 1)]
    sockets: u16,
    #[arg(long, default_value_t = 1)]
    cores: u16,
    /// 메모리(MiB)
    #[arg(long, default_value_t = 1024)]
    memory: u32,
    /// 기본값은 노드 아키텍처에 맞춘다(aarch64: virt, x86_64: q35).
    #[arg(long, value_enum)]
    machine: Option<Machine>,
    #[arg(long, value_enum, default_value = "uefi")]
    firmware: Firmware,
    /// 붙일 볼륨 이름. 여러 번 줄 수 있고 첫 디스크로 부팅한다.
    #[arg(long = "disk")]
    disks: Vec<String>,
    /// 연결할 네트워크 이름(`user`는 QEMU NAT). 여러 번 줄 수 있다. `,sg=A+B`를 붙이면 그 NIC에
    /// 보안 그룹을 건다(예: `lan,sg=web+db`).
    #[arg(long = "nic", value_name = "NETWORK[,sg=GROUP+...]", value_parser = parse_nic)]
    nics: Vec<NicArg>,
    #[arg(long, value_enum, default_value = "never")]
    restart: Restart,
    /// 노드 이름이나 id. 생략하면 시작할 때 스케줄러가 고른다.
    #[arg(long)]
    node: Option<String>,
    /// 만든 뒤 바로 시작하고 실행될 때까지 기다린다.
    #[arg(long)]
    start: bool,
    /// HA VM: 노드가 펜싱되면 다른 노드에서 켠다. 펜싱이 켜진 노드에서만 돈다.
    #[arg(long)]
    ha: bool,
}

/// `--nic`으로 받은 네트워크와 보안 그룹
#[derive(Debug, Clone, PartialEq, Eq)]
struct NicArg {
    network: String,
    security_groups: Vec<String>,
}

/// `lan`, `lan,sg=web+db`처럼 쓴다. 그룹이 있는지는 API가 본다.
fn parse_nic(s: &str) -> Result<NicArg, String> {
    let bad = || format!("invalid nic {s:?}: use NETWORK or NETWORK,sg=GROUP+GROUP");
    let (network, groups) = match s.split_once(',') {
        Some((network, option)) => {
            let groups = option.strip_prefix("sg=").ok_or_else(bad)?;
            let groups: Vec<String> = groups.split('+').map(str::to_string).collect();
            if groups.iter().any(|g| g.is_empty() || g.contains(',')) {
                return Err(bad());
            }
            (network, groups)
        }
        None => (s, Vec::new()),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NicArg {
        network: network.into(),
        security_groups: groups,
    })
}

/// `vm create`의 NIC 하나. 그룹이 없으면 `security_groups`를 보내지 않는다.
fn nic_body(nic: &NicArg) -> Value {
    let mut body = json!({ "network": nic.network, "model": "virtio" });
    if !nic.security_groups.is_empty() {
        body["security_groups"] = json!(nic.security_groups);
    }
    body
}

#[derive(Args)]
#[command(group(ArgGroup::new("change").required(true).args(["ha", "no_ha"])))]
struct VmSet {
    vm: String,
    /// HA VM으로 만든다. VM이 멈춰 있어야 하고 VM의 노드가 펜싱이 켜진 노드여야 한다.
    #[arg(long)]
    ha: bool,
    /// HA를 끈다.
    #[arg(long)]
    no_ha: bool,
}

#[derive(Subcommand)]
enum SnapshotCmd {
    /// 스냅샷을 만든다. 실행 중인 VM은 기본으로 메모리 상태까지 담는다.
    Create {
        vm: String,
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// 메모리 상태 없이 디스크만 담는다.
        #[arg(long)]
        no_memory: bool,
    },
    /// 스냅샷을 나무 모양으로 보여 준다. `*`가 VM의 현재 위치다.
    List {
        vm: String,
    },
    /// VM을 스냅샷 때로 되돌린다. 실행 중이면 끈다. 메모리 스냅샷이면 그 상태로 다시 시작한다.
    Revert {
        vm: String,
        name: String,
    },
    Delete {
        vm: String,
        name: String,
    },
    /// 예약 스냅샷을 설정하거나 끈다. 옵션이 없으면 지금 예약과 마지막 예약 오류를 보여 준다.
    Schedule(ScheduleArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum Every {
    Hourly,
    Daily,
    Weekly,
}

#[derive(Args)]
struct ScheduleArgs {
    vm: String,
    /// 스냅샷을 만드는 주기
    #[arg(long, value_enum, requires = "keep")]
    every: Option<Every>,
    /// 남길 예약 스냅샷 수(1-100). 넘으면 오래된 것부터 지운다.
    #[arg(long, requires = "every")]
    keep: Option<u32>,
    /// 실행 중이면 메모리 상태도 담는다.
    #[arg(long, requires = "every")]
    memory: bool,
    /// 예약을 끈다. 이미 만든 스냅샷은 남는다.
    #[arg(long, conflicts_with_all = ["every", "keep", "memory"])]
    off: bool,
}

/// 설정할 예약(`snapshot_schedule` 값). 주기를 주지 않았으면 `None`이다.
fn schedule_value(args: &ScheduleArgs) -> Option<Value> {
    let (every, keep) = args.every.zip(args.keep)?;
    Some(json!({ "every": value_name(every), "keep": keep, "memory": args.memory }))
}

#[derive(Subcommand)]
enum BackupCmd {
    /// VM을 백업한다. 실행 중이면 디스크를 잠깐 스냅샷한 때의 상태(crash-consistent)를 담는다.
    Create {
        vm: String,
        /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
        #[arg(long)]
        pool: String,
        /// 생략하면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
        #[arg(long)]
        name: Option<String>,
    },
    /// 백업 목록. VM을 지운 백업도 보인다.
    List {
        /// 이 VM(이름이나 id)의 백업만. 지운 VM은 백업에 남은 이름으로 찾는다.
        #[arg(long)]
        vm: Option<String>,
    },
    Get {
        backup: String,
    },
    /// 백업 파일과 기록을 지운다. 실패한 백업도 지운다.
    Delete {
        backup: String,
        /// 파일을 지울 노드가 없으면 기록만 지운다(파일은 백업 풀에 남는다).
        #[arg(long)]
        force: bool,
    },
    /// 백업에서 새 VM(꺼짐)을 만든다. 디스크는 새 볼륨에 복사하고 NIC는 새 MAC을 받는다.
    Restore {
        backup: String,
        /// 새 VM 이름
        #[arg(long)]
        name: String,
        /// 새 디스크를 둘 풀
        #[arg(long)]
        pool: String,
        /// 복원을 실행할 노드(이름이나 id). 생략하면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
        #[arg(long)]
        node: Option<String>,
    },
}

#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    })
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    body
}

#[derive(Subcommand)]
enum ImportCmd {
    /// VMware 등에서 내보낸 OVA를 이 서버의 노드에 올리고, 원본 VM을 읽어 VM 설정을 제안받는다.
    /// 원본 파일은 그 노드에 남고 실행도 그 노드에서 한다.
    Ova {
        file: PathBuf,
        /// 가져오기 이름
        #[arg(long)]
        name: String,
    },
    /// ESXi·vCenter의 데이터스토어에 있는 VM을 보인다. 비밀번호는 `--password-file`이나
    /// `OPL_ESXI_PASSWORD`에서, 없으면 터미널에서 묻는다. 지문을 주지 않으면 서버 인증서의 지문을 알려 준다.
    EsxiBrowse {
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// ESXi·vCenter의 꺼진 VM을 읽어 VM 설정을 제안받는다. 비밀번호는 `esxi-browse`처럼 받고 연결을
    /// 받은 노드의 메모리에만 남으므로, 그 opld를 다시 시작하면 가져오기를 다시 만들어야 한다.
    Esxi {
        /// 가져오기 이름
        name: String,
        #[command(flatten)]
        conn: EsxiArgs,
        /// 믿을 서버 인증서의 SHA-256 지문(`import esxi-browse`가 알려 준다)
        #[arg(long)]
        fingerprint: String,
        #[arg(long)]
        datastore: String,
        /// 데이터스토어 안의 `.vmx` 경로(예: `web01/web01.vmx`)
        #[arg(long)]
        vmx: String,
    },
    List,
    /// 원본 VM, 제안, 경고, 마지막 실행의 오류
    Get {
        import: String,
    },
    /// 원본 디스크를 볼륨으로 옮겨 새 VM(꺼짐)을 만든다. 주지 않은 값은 제안 값이다.
    Run(Box<ImportRunArgs>),
    /// 가져오기와 원본 파일을 지운다(만든 VM은 남는다).
    Delete {
        import: String,
        /// 원본 노드가 없거나 오프라인이면 기록만 지운다(원본 파일은 그 노드에 남는다).
        #[arg(long)]
        force: bool,
    },
}

/// ESXi·vCenter 연결. 비밀번호는 인자로 받지 않는다.
#[derive(Args)]
struct EsxiArgs {
    /// ESXi 호스트나 vCenter 주소
    #[arg(long)]
    host: String,
    #[arg(long, default_value_t = 443)]
    port: u16,
    #[arg(long)]
    user: String,
    /// vCenter면 데이터센터 이름
    #[arg(long, default_value = "ha-datacenter")]
    datacenter: String,
    /// 비밀번호가 든 파일. 없으면 `OPL_ESXI_PASSWORD`, 그것도 없으면 터미널에서 묻는다.
    #[arg(long)]
    password_file: Option<PathBuf>,
}

/// ESXi 비밀번호. `--password-file`, `OPL_ESXI_PASSWORD` 순서로 보고, 둘 다 없으면 터미널에서 묻는다.
fn esxi_password(conn: &EsxiArgs) -> anyhow::Result<String> {
    if let Some(file) = &conn.password_file {
        return read_password(Some(file), "");
    }
    match std::env::var("OPL_ESXI_PASSWORD") {
        Ok(password) => Ok(password),
        Err(_) => Ok(rpassword::prompt_password(format!(
            "password for {}@{}: ",
            conn.user, conn.host
        ))?),
    }
}

/// API의 `EsxiConnection`
fn esxi_connection(conn: &EsxiArgs, fingerprint: Option<&str>, password: &str) -> Value {
    json!({
        "host": conn.host,
        "port": conn.port,
        "user": conn.user,
        "password": password,
        "datacenter": conn.datacenter,
        "fingerprint": fingerprint,
    })
}

/// `import esxi`의 요청 본문
fn esxi_import_body(name: &str, connection: &Value, datastore: &str, vmx: &str) -> Value {
    json!({ "name": name, "connection": connection, "datastore": datastore, "vmx": vmx })
}

#[derive(Args)]
struct ImportRunArgs {
    import: String,
    /// 새 VM 이름
    #[arg(long)]
    name: String,
    /// 디스크 볼륨을 만들 풀
    #[arg(long)]
    pool: String,
    /// 원본 네트워크를 opLite 네트워크에 잇는다(예: `"VM Network=lan"`). 원본 NIC의 네트워크마다
    /// 준다. 원본 네트워크가 하나면 `NETWORK`만 줘도 된다.
    #[arg(long = "net", value_name = "SOURCE=NETWORK", value_parser = parse_net)]
    nets: Vec<NetArg>,
    #[arg(long, value_enum)]
    disk_bus: Option<DiskBus>,
    #[arg(long, value_enum)]
    nic_model: Option<NicModel>,
    #[arg(long, value_enum)]
    firmware: Option<Firmware>,
    #[arg(long)]
    sockets: Option<u16>,
    #[arg(long)]
    cores: Option<u16>,
    /// 메모리(MiB)
    #[arg(long)]
    memory: Option<u32>,
    /// 원본 MAC 주소 대신 새 MAC을 받는다.
    #[arg(long)]
    new_macs: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DiskBus {
    VirtioBlk,
    Sata,
}

#[derive(Clone, Copy, ValueEnum)]
enum NicModel {
    Virtio,
    E1000e,
}

/// `--net`으로 받은 매핑. `source`가 없으면 하나뿐인 원본 네트워크다.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NetArg {
    source: Option<String>,
    network: String,
}

fn parse_net(s: &str) -> Result<NetArg, String> {
    let bad = || format!("invalid network mapping {s:?}: use SOURCE=NETWORK or NETWORK");
    // 원본 네트워크 이름에 `=`가 있을 수 있다. opLite 네트워크 이름에는 없다.
    let (source, network) = match s.rsplit_once('=') {
        Some(("", _)) => return Err(bad()),
        Some((source, network)) => (Some(source.to_string()), network),
        None => (None, s),
    };
    if network.is_empty() {
        return Err(bad());
    }
    Ok(NetArg {
        source,
        network: network.into(),
    })
}

/// 가져오기의 원본 네트워크(제안의 네트워크 매핑 키)
fn source_networks(import: &Value) -> Vec<String> {
    import["status"]["proposal"]["networks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// `import run`의 요청 본문. `sources`는 `=` 없는 `--net`을 이을 원본 네트워크다.
fn import_run_body(args: &ImportRunArgs, sources: &[String]) -> anyhow::Result<Value> {
    let mut networks = serde_json::Map::new();
    for net in &args.nets {
        let source = match (&net.source, sources) {
            (Some(source), _) => source.clone(),
            (None, [only]) => only.clone(),
            (None, _) => bail!(
                "--net {} needs SOURCE=NETWORK: vm-import {} has {} source networks ({})",
                net.network,
                args.import,
                sources.len(),
                sources
                    .iter()
                    .map(|s| format!("{s:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        networks.insert(source, json!(net.network));
    }
    Ok(json!({
        "name": args.name,
        "pool": args.pool,
        "networks": networks,
        "disk_bus": args.disk_bus.map(value_name),
        "nic_model": args.nic_model.map(value_name),
        "firmware": args.firmware.map(value_name),
        "sockets": args.sockets,
        "cores": args.cores,
        "memory_mib": args.memory,
        "keep_macs": !args.new_macs,
    }))
}

#[derive(Subcommand)]
enum TaskCmd {
    List,
    Get { task: String },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 설정을 `키: 값` 줄로 보인다.
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing|enterprise)
    Set {
        /// 예: `updates.channel=testing`
        #[arg(required = true, value_name = "KEY=VALUE")]
        values: Vec<String>,
    },
}

/// `settings set`이 아는 키
const SETTING_KEYS: &[&str] = &["updates.channel"];

/// `settings set`의 요청 본문(`PATCH /settings`). 준 키만 보낸다.
fn settings_set_body(values: &[String]) -> anyhow::Result<Value> {
    let mut body = json!({});
    for value in values {
        let Some((key, v)) = value.split_once('=') else {
            bail!("expected KEY=VALUE, got {value:?}");
        };
        match key {
            "updates.channel" => body["updates"]["channel"] = json!(v),
            _ => bail!("unknown setting {key}; known: {}", SETTING_KEYS.join(", ")),
        }
    }
    Ok(body)
}

/// 객체의 값들을 `a.b: 값` 줄로 펼친다.
fn setting_lines(prefix: &str, value: &Value, lines: &mut Vec<String>) {
    match value.as_object() {
        Some(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                setting_lines(&key, v, lines);
            }
        }
        None => lines.push(format!("{prefix}: {}", cell(value))),
    }
}

async fn settings(
    client: &Client,
    cmd: SettingsCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SettingsCmd::Get => {
            let settings = client.get("/settings").await?;
            if json_out {
                print_json(out, &settings)?;
            } else {
                let mut lines = Vec::new();
                setting_lines("", &settings["spec"], &mut lines);
                for line in lines {
                    writeln!(out, "{line}")?;
                }
            }
        }
        SettingsCmd::Set { values } => {
            client
                .patch("/settings", &settings_set_body(&values)?)
                .await?;
            writeln!(out, "settings updated")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum SubscriptionCmd {
    /// 구독 상태를 보인다.
    Show,
    /// 구독 키를 넣는다(있던 키는 바꾼다).
    #[command(group(ArgGroup::new("source").required(true).args(["key", "file"])))]
    Set {
        /// `opl1.<payload>.<signature>`
        key: Option<String>,
        /// 키가 든 파일(`-`면 표준 입력)
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// 구독 키를 지운다.
    Remove,
}

/// 구독 상태(`SubscriptionInfo`)를 `키: 값` 줄로. 없는 값은 줄을 쓰지 않는다.
fn subscription_lines(info: &Value) -> Vec<String> {
    let mut state = cell(&info["state"]);
    if info["development"] == true {
        state.push_str(" (development key)");
    }
    let mut lines = vec![format!("state: {state}")];
    for key in ["customer", "level"] {
        if !info[key].is_null() {
            lines.push(format!("{key}: {}", cell(&info[key])));
        }
    }
    let used = cell(&info["used_nodes"]);
    lines.push(match info["nodes"].as_u64() {
        Some(limit) => format!("nodes: {used}/{limit}"),
        None => format!("nodes: {used}"),
    });
    if let Some(at) = info["expires_at"].as_u64() {
        lines.push(format!("expires: {}", opl_types::subscription::date(at)));
    }
    lines
}

fn print_subscription(
    out: &mut (dyn Write + Send),
    json_out: bool,
    info: &Value,
) -> anyhow::Result<()> {
    if json_out {
        print_json(out, info)?;
    } else {
        for line in subscription_lines(info) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

async fn subscription(
    client: &Client,
    cmd: SubscriptionCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SubscriptionCmd::Show => {
            let info = client.get("/subscription").await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Set { key, file } => {
            let key = match key {
                Some(key) => key,
                None => match file {
                    Some(path) if path.as_os_str() == "-" => {
                        read_stdin_key(&mut std::io::stdin().lock())?
                    }
                    file => read_password(file.as_ref(), "")?,
                },
            };
            let info = client
                .put("/subscription", &json!({ "key": key.trim() }))
                .await?;
            print_subscription(out, json_out, &info)?;
        }
        SubscriptionCmd::Remove => {
            client.delete("/subscription").await?;
            writeln!(out, "subscription removed")?;
        }
    }
    Ok(())
}

#[derive(Subcommand)]
enum TelemetryCmd {
    /// 텔레메트리 설정과 마지막 전송을 보인다.
    Show,
    /// 텔레메트리를 켠다. 무엇을 보내는지는 `preview`로 먼저 본다.
    Enable {
        /// 보낼 `https://` 주소. 없으면 지금 끝점(설정 또는 빌드 기본값)
        #[arg(long)]
        endpoint: Option<String>,
    },
    /// 텔레메트리를 끈다(id는 남는다).
    Disable,
    /// 지금 보낼 보고를 JSON 그대로 보인다(admin).
    Preview,
    /// 보고에 붙는 id를 새로 만든다.
    ResetId,
}

/// `telemetry enable|disable`의 요청 본문(`PATCH /settings`)
fn telemetry_body(enabled: bool, endpoint: Option<&str>) -> Value {
    let mut body = json!({ "telemetry": { "enabled": enabled } });
    if let Some(endpoint) = endpoint {
        body["telemetry"]["endpoint"] = json!(endpoint);
    }
    body
}

/// 설정 리소스의 텔레메트리 부분을 `키: 값` 줄로. 끝점이 없으면 이 oplctl 빌드의 기본 끝점을 보인다.
fn telemetry_lines(settings: &Value) -> Vec<String> {
    let spec = &settings["spec"]["telemetry"];
    let status = &settings["status"]["telemetry"];
    let endpoint = match spec["endpoint"].as_str() {
        Some(endpoint) => endpoint.to_string(),
        None if !opl_types::version::TELEMETRY_URL.is_empty() => {
            format!("{} (build default)", opl_types::version::TELEMETRY_URL)
        }
        None => "none".into(),
    };
    let last_sent = match status["last_sent_at"].as_i64() {
        Some(at) => unix_time(at),
        None => "never".into(),
    };
    vec![
        format!("enabled: {}", spec["enabled"] == true),
        format!("endpoint: {endpoint}"),
        format!("id: {}", status["id"].as_str().unwrap_or("none")),
        format!("last sent: {last_sent}"),
        format!(
            "last error: {}",
            status["last_error"].as_str().unwrap_or("none")
        ),
    ]
}

async fn telemetry(
    client: &Client,
    cmd: TelemetryCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let settings = match cmd {
        TelemetryCmd::Show => client.get("/settings").await?,
        TelemetryCmd::Enable { endpoint } => {
            client
                .patch("/settings", &telemetry_body(true, endpoint.as_deref()))
                .await?
        }
        TelemetryCmd::Disable => {
            client
                .patch("/settings", &telemetry_body(false, None))
                .await?
        }
        TelemetryCmd::Preview => {
            // 보낼 JSON 그대로 보인다.
            let report = client.get("/telemetry/preview").await?;
            print_json(out, &report)?;
            return Ok(());
        }
        TelemetryCmd::ResetId => client.post("/telemetry/reset-id", &json!({})).await?,
    };
    if json_out {
        print_json(out, &settings)?;
    } else {
        for line in telemetry_lines(&settings) {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

#[derive(Args)]
struct AuditArgs {
    /// 이 시각부터: `30m`·`1h`·`7d`(지금에서 뺌)나 RFC 3339. 기본은 `--until`의 24시간 전
    #[arg(long)]
    since: Option<String>,
    /// 이 시각까지(RFC 3339). 기본은 지금
    #[arg(long)]
    until: Option<String>,
    /// 사용자 이름(정확히 같은 것)
    #[arg(long)]
    user: Option<String>,
    /// 노드 이름
    #[arg(long)]
    node: Option<String>,
    /// 동작(`POST /vms/{key}`)이나 경로에 든 문자열
    #[arg(long)]
    action: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    #[arg(long)]
    limit: Option<u32>,
}

/// `--since`: 기간(`30m`·`1h`·`7d`, 초 `s`도)이면 지금(`now_ms`)에서 뺀 유닉스 밀리초, 아니면
/// 그대로(RFC 3339나 밀리초) 보내 서버가 푼다.
fn audit_since(s: &str, now_ms: u64) -> String {
    match audit_duration_ms(s) {
        Some(ms) => now_ms.saturating_sub(ms).to_string(),
        None => s.to_string(),
    }
}

/// `30m`·`1h`·`7d`(초 `s`도) 같은 기간의 밀리초. 기간이 아니면 None
fn audit_duration_ms(s: &str) -> Option<u64> {
    let unit = match s.chars().last()? {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return None,
    };
    let n = s[..s.len() - 1].parse::<u64>().ok()?;
    Some(n.saturating_mul(unit))
}

/// 유닉스 밀리초를 `2026-11-06T09:30:05Z`처럼(UTC)
fn unix_ms_time(ms: u64) -> String {
    match time::OffsetDateTime::from_unix_timestamp((ms / 1000) as i64) {
        Ok(t) => format!(
            "{}T{:02}:{:02}:{:02}Z",
            t.date(),
            t.hour(),
            t.minute(),
            t.second()
        ),
        Err(_) => ms.to_string(),
    }
}

/// `audit`의 표 한 줄
fn audit_row(e: &Value) -> Vec<String> {
    // 대상이 없으면 detail(예: `audit.dropped`의 수, 로그인의 `username=…`)
    let target = match (e["target"]["kind"].as_str(), e["target"]["key"].as_str()) {
        (Some(kind), Some(key)) => format!("{kind}/{key}"),
        _ => e["detail"].as_str().unwrap_or("-").to_string(),
    };
    vec![
        e["time"].as_u64().map_or_else(|| "-".into(), unix_ms_time),
        cell(&e["node"]),
        cell(&e["user"]),
        cell(&e["ip"]),
        cell(&e["action"]),
        target,
        cell(&e["status"]),
    ]
}

/// `GET /audit`를 부르고 표로 보인다. 읽지 못한 노드, 감사 로그를 끈 노드와 잘림은 표 뒤
/// `err`(stderr)에 알린다.
async fn audit(
    client: &Client,
    args: AuditArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut dyn Write,
) -> anyhow::Result<()> {
    if args.until.is_some()
        && args
            .since
            .as_deref()
            .is_some_and(|s| audit_duration_ms(s).is_some())
    {
        bail!("a relative --since cannot be combined with --until; give a time");
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let query: Vec<(&str, String)> = [
        ("since", args.since.map(|s| audit_since(&s, now_ms))),
        ("until", args.until),
        ("user", args.user),
        ("node", args.node),
        ("action", args.action),
        ("limit", args.limit.map(|n| n.to_string())),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.map(|v| (k, v)))
    .collect();
    let mut r = client.request(Method::GET, "/audit");
    if !query.is_empty() {
        r = r.query(&query);
    }
    let resp = client.send(r).await?;
    if json_out {
        print_json(out, &resp)?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = resp["entries"]
        .as_array()
        .map(|a| a.iter().map(audit_row).collect())
        .unwrap_or_default();
    table(
        out,
        &["TIME", "NODE", "USER", "IP", "ACTION", "TARGET", "STATUS"],
        &rows,
    )?;
    let unreachable: Vec<&str> = resp["unreachable"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !unreachable.is_empty() {
        writeln!(
            err,
            "warning: no audit records from: {}",
            unreachable.join(", ")
        )?;
    }
    let disabled: Vec<&str> = resp["disabled"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !disabled.is_empty() {
        writeln!(
            err,
            "note: the audit log is disabled on: {}",
            disabled.join(", ")
        )?;
    }
    if resp["truncated"] == true {
        writeln!(
            err,
            "note: more entries match; narrow the range or raise --limit"
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Role {
    Admin,
    Operator,
    Viewer,
}

#[derive(Subcommand)]
enum UserCmd {
    List,
    Create {
        name: String,
        #[arg(long, value_enum)]
        role: Role,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    /// 비밀번호를 바꾼다.
    Passwd {
        name: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
        /// 자기 비밀번호를 바꿀 때 현재 비밀번호가 든 파일. 없으면 터미널에서 묻는다.
        #[arg(long)]
        current_password_file: Option<PathBuf>,
    },
    Delete {
        name: String,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    List,
    /// 새 토큰을 만들어 출력한다. 다시 볼 수 없다.
    Create {
        name: String,
    },
    Delete {
        token: String,
    },
}

#[derive(Subcommand)]
enum RealmCmd {
    List,
    /// 영역 설정을 보인다(서비스 계정 비밀번호는 있는지만).
    Show {
        name: String,
    },
    /// LDAP 영역을 만든다.
    AddLdap(RealmAdd),
    /// LDAP 영역을 바꾼다. 준 것만 바꾼다.
    SetLdap(RealmSet),
    /// 영역을 지운다. 그 영역 사용자들의 세션과 토큰은 멈춘다.
    Remove {
        name: String,
    },
    /// 사용자 이름과 비밀번호로 디렉터리를 시험한다(사용자를 만들지 않는다).
    Test {
        name: String,
        /// 영역 안의 사용자 이름(`@영역` 없이)
        #[arg(long)]
        user: String,
        /// 비밀번호가 든 파일(`-`면 표준 입력). 없으면 터미널에서 묻는다.
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct RealmAdd {
    name: String,
    /// `ldap://host[:port]`(StartTLS) 또는 `ldaps://host[:port]`
    #[arg(long)]
    url: String,
    /// 사용자를 찾을 기준 DN
    #[arg(long)]
    base_dn: String,
    #[command(flatten)]
    ldap: LdapArgs,
}

#[derive(Args)]
struct RealmSet {
    name: String,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    base_dn: Option<String>,
    #[command(flatten)]
    ldap: LdapArgs,
    /// 저장된 서비스 계정 비밀번호를 지운다. 서비스 계정 DN이 남으면 안 된다(`--clear-bind-dn`).
    #[arg(long, conflicts_with = "bind_password_file")]
    clear_bind_password: bool,
    /// 서비스 계정 DN과 비밀번호를 지운다(익명 검색).
    #[arg(long, conflicts_with_all = ["bind_dn", "bind_password_file"])]
    clear_bind_dn: bool,
    /// CA를 지운다(시스템 루트).
    #[arg(long, conflicts_with = "ca_file")]
    clear_ca: bool,
    /// 기본 역할을 지운다(맞는 그룹이 없으면 로그인 거절).
    #[arg(long, conflicts_with = "default_role")]
    clear_default_role: bool,
    /// 그룹 → 역할 표를 비운다.
    #[arg(long, conflicts_with = "groups")]
    clear_groups: bool,
}

/// `realm add-ldap`과 `set-ldap`의 공통 선택
#[derive(Args)]
struct LdapArgs {
    /// 사용자를 찾을 때 바인드할 서비스 계정. 없으면 익명으로 찾는다.
    #[arg(long)]
    bind_dn: Option<String>,
    /// 서비스 계정 비밀번호가 든 파일(`-`면 표준 입력)
    #[arg(long)]
    bind_password_file: Option<PathBuf>,
    /// 사용자 찾기 필터. `{username}`이 로그인 이름으로 바뀐다.
    #[arg(long)]
    user_filter: Option<String>,
    /// 그룹 DN을 담은 사용자 속성(기본 memberOf)
    #[arg(long)]
    group_attr: Option<String>,
    /// 서버 인증서를 확인할 CA(PEM) 파일. 없으면 시스템 루트
    #[arg(long)]
    ca_file: Option<PathBuf>,
    /// 연결과 작업마다의 시간 제한(초)
    #[arg(long)]
    timeout: Option<u64>,
    /// 로그인 화면에 보일 이름
    #[arg(long)]
    display: Option<String>,
    /// 그룹에 든 사용자의 역할(`<그룹 DN>=<role>`, 여러 번). set-ldap에서 주면 표 전체를 바꾼다.
    #[arg(long = "group", value_name = "DN=ROLE", value_parser = parse_group_role)]
    groups: Vec<(String, Role)>,
    /// 맞는 그룹이 없을 때의 역할. 없으면 그런 사용자의 로그인을 거절한다.
    #[arg(long, value_enum)]
    default_role: Option<Role>,
}

/// `--group`의 `<그룹 DN>=<role>`. DN에 `=`가 있으므로 마지막 `=`에서 나눈다.
fn parse_group_role(s: &str) -> Result<(String, Role), String> {
    let (group, role) = s
        .rsplit_once('=')
        .ok_or_else(|| format!("invalid group {s:?}: use <group DN>=<role>"))?;
    let role = Role::from_str(role, true)
        .map_err(|_| format!("invalid role {role:?} in {s:?}: use admin, operator or viewer"))?;
    if group.trim().is_empty() {
        return Err(format!("invalid group {s:?}: use <group DN>=<role>"));
    }
    Ok((group.to_string(), role))
}

/// 파일에서 읽은 영역 값들(서비스 계정 비밀번호, CA)
#[derive(Default)]
struct LdapFiles {
    bind_password: Option<String>,
    ca_pem: Option<String>,
}

impl LdapFiles {
    fn read(args: &LdapArgs) -> anyhow::Result<Self> {
        Ok(Self {
            bind_password: args
                .bind_password_file
                .as_ref()
                .map(read_secret_file)
                .transpose()?,
            ca_pem: args
                .ca_file
                .as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()?,
        })
    }
}

/// 비밀번호 파일(`-`면 표준 입력). 끝의 줄바꿈만 뗀다.
fn read_secret_file(path: &PathBuf) -> anyhow::Result<String> {
    if path.as_os_str() == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().lock(), &mut s)
            .context("cannot read standard input")?;
        return Ok(s.trim_end_matches(['\r', '\n']).to_string());
    }
    read_password(Some(path), "")
}

fn group_roles(groups: &[(String, Role)]) -> Value {
    groups
        .iter()
        .map(|(group, role)| json!({ "group": group, "role": value_name(*role) }))
        .collect()
}

/// `realm add-ldap`의 `POST /realms` 본문. 주지 않은 선택은 서버 기본값이다.
fn realm_add_body(args: &RealmAdd, files: LdapFiles) -> Value {
    let l = &args.ldap;
    let mut backend = json!({ "kind": "ldap", "url": args.url, "base_dn": args.base_dn });
    let optional = [
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("bind_password", files.bind_password.map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
    ];
    for (key, value) in optional {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    let mut roles = json!({ "groups": group_roles(&l.groups) });
    if let Some(role) = l.default_role {
        roles["default"] = json!(value_name(role));
    }
    let mut body = json!({ "name": args.name, "backend": backend, "roles": roles });
    if let Some(display) = &l.display {
        body["display"] = json!(display);
    }
    body
}

/// `realm set-ldap`의 `PUT /realms/{name}` 본문: 지금 영역(`RealmView`)에 준 것만 바꾼다.
/// 서비스 계정 비밀번호는 주지 않으면 빼서(서버가 저장된 값을 둔다 — url·bind_dn·CA를 바꾸면
/// 서버가 400으로 다시 달라고 한다) 보내고, 지울 때는 빈 글이다.
fn realm_set_body(current: &Value, args: &RealmSet, files: LdapFiles) -> anyhow::Result<Value> {
    let l = &args.ldap;
    let mut backend = current["backend"].clone();
    if let Some(b) = backend.as_object_mut() {
        b.remove("has_bind_password");
    }
    let changes = [
        ("url", args.url.clone().map(Value::from)),
        ("base_dn", args.base_dn.clone().map(Value::from)),
        ("bind_dn", l.bind_dn.clone().map(Value::from)),
        ("user_filter", l.user_filter.clone().map(Value::from)),
        ("group_attr", l.group_attr.clone().map(Value::from)),
        ("ca_pem", files.ca_pem.map(Value::from)),
        ("timeout_secs", l.timeout.map(Value::from)),
        (
            "bind_password",
            if args.clear_bind_password || args.clear_bind_dn {
                Some(json!(""))
            } else {
                files.bind_password.map(Value::from)
            },
        ),
    ];
    for (key, value) in changes {
        if let Some(value) = value {
            backend[key] = value;
        }
    }
    if let Some(b) = backend.as_object_mut() {
        if args.clear_bind_dn {
            b.remove("bind_dn");
        }
        if args.clear_ca {
            b.remove("ca_pem");
        }
        // 서비스 계정 DN에는 비밀번호가 있어야 한다.
        if args.clear_bind_password && b.get("bind_dn").is_some_and(Value::is_string) {
            anyhow::bail!(
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
    }
    let mut roles = current["roles"].clone();
    if !l.groups.is_empty() {
        roles["groups"] = group_roles(&l.groups);
    }
    if args.clear_groups {
        roles["groups"] = json!([]);
    }
    if let Some(role) = l.default_role {
        roles["default"] = json!(value_name(role));
    }
    if args.clear_default_role
        && let Some(r) = roles.as_object_mut()
    {
        r.remove("default");
    }
    let mut body = json!({ "backend": backend, "roles": roles });
    if let Some(display) = l
        .display
        .clone()
        .or_else(|| current["display"].as_str().map(str::to_string))
    {
        body["display"] = json!(display);
    }
    Ok(body)
}

/// `realm show`의 `키: 값` 줄
fn realm_lines(realm: &Value) -> Vec<String> {
    let b = &realm["backend"];
    let set = |v: bool| if v { "set" } else { "none" };
    let mut lines = vec![
        format!("name: {}", cell(&realm["name"])),
        format!("display: {}", cell(&realm["display"])),
        format!("kind: {}", cell(&b["kind"])),
        format!("url: {}", cell(&b["url"])),
        format!("base dn: {}", cell(&b["base_dn"])),
        format!("bind dn: {}", cell(&b["bind_dn"])),
        format!("bind password: {}", set(b["has_bind_password"] == true)),
        format!("user filter: {}", cell(&b["user_filter"])),
        format!("group attribute: {}", cell(&b["group_attr"])),
        format!(
            "ca: {}",
            if b["ca_pem"].is_string() {
                "custom (PEM)"
            } else {
                "system roots"
            }
        ),
        format!("timeout: {}s", cell(&b["timeout_secs"])),
    ];
    let groups = realm["roles"]["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if groups.is_empty() {
        lines.push("groups: none".into());
    } else {
        lines.push("groups:".into());
        for g in &groups {
            lines.push(format!("  {} = {}", cell(&g["group"]), cell(&g["role"])));
        }
    }
    lines.push(format!(
        "default role: {}",
        realm["roles"]["default"].as_str().unwrap_or("none")
    ));
    lines
}

async fn realm(
    client: &Client,
    cmd: RealmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        RealmCmd::List => {
            let list = client.get("/realms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "DISPLAY", "KIND", "URL"],
                |r| {
                    vec![
                        cell(&r["name"]),
                        cell(&r["display"]),
                        cell(&r["backend"]["kind"]),
                        cell(&r["backend"]["url"]),
                    ]
                },
            )?;
        }
        RealmCmd::Show { name } => {
            let realm = client.get(&format!("/realms/{name}")).await?;
            if json_out {
                print_json(out, &realm)?;
            } else {
                for line in realm_lines(&realm) {
                    writeln!(out, "{line}")?;
                }
            }
        }
        RealmCmd::AddLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            client
                .post("/realms", &realm_add_body(&args, files))
                .await?;
            writeln!(out, "realm {} created", args.name)?;
        }
        RealmCmd::SetLdap(args) => {
            let files = LdapFiles::read(&args.ldap)?;
            let path = format!("/realms/{}", args.name);
            let current = client.get(&path).await?;
            // 읽은 뒤 다른 곳에서 바뀌었으면 412로 멈춘다.
            let r = client
                .request(Method::PUT, &path)
                .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
                .json(&realm_set_body(&current, &args, files)?);
            client.send(r).await?;
            writeln!(out, "realm {} updated", args.name)?;
        }
        RealmCmd::Remove { name } => {
            client.delete(&format!("/realms/{name}")).await?;
            writeln!(out, "realm {name} removed")?;
        }
        RealmCmd::Test {
            name,
            user,
            password_file,
        } => {
            let password = match &password_file {
                Some(f) => read_secret_file(f)?,
                None => read_password(None, "Password: ")?,
            };
            let result = client
                .post(
                    &format!("/realms/{name}/actions/test"),
                    &json!({ "username": user, "password": password }),
                )
                .await?;
            let steps = result["steps"].as_array().cloned().unwrap_or_default();
            if json_out {
                print_json(out, &result)?;
            } else {
                let rows: Vec<Vec<String>> = steps
                    .iter()
                    .map(|s| {
                        let ok = if s["ok"] == true { "ok" } else { "failed" };
                        vec![cell(&s["name"]), ok.into(), cell(&s["message"])]
                    })
                    .collect();
                table(out, &["STEP", "RESULT", "MESSAGE"], &rows)?;
                writeln!(out, "dn: {}", cell(&result["dn"]))?;
                let groups: Vec<String> = result["groups"]
                    .as_array()
                    .map(|a| a.iter().map(cell).collect())
                    .unwrap_or_default();
                writeln!(
                    out,
                    "groups: {}",
                    if groups.is_empty() {
                        "-".into()
                    } else {
                        groups.join(", ")
                    }
                )?;
                writeln!(out, "role: {}", result["role"].as_str().unwrap_or("none"))?;
            }
            if let Some(failed) = steps.iter().find(|s| s["ok"] != true) {
                bail!("realm {name}: the {} step failed", cell(&failed["name"]));
            }
        }
    }
    Ok(())
}

/// `1024`, `512M`, `10G`처럼 쓴다.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        Some('T' | 't') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid size {s:?}"))?;
    n.checked_mul(1 << shift)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

fn read_password(file: Option<&PathBuf>, prompt: &str) -> anyhow::Result<String> {
    match file {
        Some(path) => Ok(std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?
            .trim_end_matches(['\r', '\n'])
            .to_string()),
        None => Ok(rpassword::prompt_password(prompt)?),
    }
}

/// 표준 입력의 구독 키(앞뒤 공백을 다듬음). 키를 명령줄에 두지 않으려고 쓴다.
fn read_stdin_key(input: &mut dyn std::io::Read) -> anyhow::Result<String> {
    let mut key = String::new();
    input
        .read_to_string(&mut key)
        .context("cannot read the key from standard input")?;
    Ok(key.trim().to_string())
}

fn value_name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value().unwrap().get_name().to_string()
}

/// 볼륨 형식. 주지 않았으면 풀을 읽어 그 종류의 기본값으로 정한다.
async fn volume_format(
    client: &Client,
    pool: &str,
    format: Option<Format>,
) -> anyhow::Result<Format> {
    match format {
        Some(f) => Ok(f),
        None => {
            let read = client.get(&format!("/storage-pools/{pool}")).await;
            let pool = read.with_context(|| {
                format!(
                    "cannot read storage pool {pool:?} to choose a volume format; pass --format"
                )
            })?;
            Ok(pool_default_format(&pool))
        }
    }
}

/// 풀 종류의 기본 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw(스냅샷은 블록 스냅샷,
/// 공유 LVM은 스냅샷 없음), 파일 풀은 qcow2
fn pool_default_format(pool: &Value) -> Format {
    let kind = &pool["spec"]["backend"]["kind"];
    if kind == "lvm-thin" || kind == "zfs" || kind == "rbd" || kind == "lvm-shared" {
        Format::Raw
    } else {
        Format::Qcow2
    }
}

/// 명령을 실행하고 결과를 `out`에 쓴다. 인자 오류는 `clap::Error`로 돌려준다.
pub async fn run<I, T>(args: I, out: &mut (dyn Write + Send)) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::try_parse_from(args)?;
    let config_path = cli.config.clone().unwrap_or_else(default_path);
    let mut config = CliConfig::load(&config_path)?;
    let server = cli.server.clone().or(config.server.clone());

    if let Cmd::Login(args) = &cli.cmd {
        let server = server.context("--server is required for the first login")?;
        let same_server = config.server.as_deref().map(|s| s.trim_end_matches('/'))
            == Some(server.trim_end_matches('/'));
        let ca_pem = server_ca(
            &server,
            args,
            same_server.then(|| config.ca_pem.clone()).flatten(),
        )
        .await?;
        let (token, name) = login(&server, args, ca_pem.clone()).await?;
        let previous = config.token.take().zip(config.server.take());
        config.server = Some(server.clone());
        config.token = Some(token.clone());
        config.ca_pem = ca_pem.clone();
        config.save(&config_path)?;
        // 새 토큰을 저장한 뒤, 같은 서버에 다시 로그인했으면 이전 토큰은 서버에서도 지운다(실패해도 무시).
        if let Some((old, old_server)) = previous
            && old_server.trim_end_matches('/') == server.trim_end_matches('/')
            && let Some(id) = token_id(&old)
        {
            let client = Client::new(&server, Some(token), ca_pem)?;
            let _ = client.delete(&format!("/tokens/{id}")).await;
        }
        writeln!(out, "logged in to {server} as {name}")?;
        return Ok(());
    }

    if let Cmd::Cluster { cmd } = &cli.cmd
        && let Some(done) = cluster_local(cmd, out).await
    {
        return done;
    }

    let server = server.context("not logged in; run `oplctl login --server <url>`")?;
    let client = Client::new(&server, config.token.clone(), config.ca_pem.clone())?;
    let json_out = cli.output == Output::Json;
    match cli.cmd {
        Cmd::Login(_) => unreachable!(),
        Cmd::Cluster { cmd } => cluster(&client, cmd, json_out, out).await?,
        Cmd::Logout => {
            if let Some(id) = config.token.as_deref().and_then(token_id) {
                // 이미 지워진 토큰이어도 로컬에서는 지운다.
                let _ = client.delete(&format!("/tokens/{id}")).await;
            }
            config.token = None;
            config.save(&config_path)?;
            writeln!(out, "logged out")?;
        }
        Cmd::Node { cmd } => match cmd {
            NodeCmd::List => {
                let list = client.get("/nodes").await?;
                print_list(out, json_out, &list, NODE_HEADERS, node_row)?;
            }
            NodeCmd::Get { node } => {
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
            NodeCmd::Updates { node, refresh } => {
                node_updates(&client, &node, refresh, json_out, out).await?
            }
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
        },
        Cmd::Pool { cmd } => match cmd {
            PoolCmd::List => {
                let list = client.get("/storage-pools").await?;
                let nodes = if json_out {
                    Vec::new()
                } else {
                    items(&client.get("/nodes").await?)
                };
                print_list(out, json_out, &list, POOL_HEADERS, |p| pool_row(p, &nodes))?;
            }
            PoolCmd::Get { pool } => {
                let pool = client.get(&format!("/storage-pools/{pool}")).await?;
                if json_out {
                    print_json(out, &pool)?;
                } else {
                    let nodes = items(&client.get("/nodes").await?);
                    write!(out, "{}", describe_pool(&pool))?;
                    table(out, POOL_NODE_HEADERS, &pool_node_rows(&pool, &nodes))?;
                }
            }
            PoolCmd::Create(args) => {
                let name = args.name.clone();
                client
                    .post("/storage-pools", &pool_create_body(*args))
                    .await?;
                writeln!(out, "storage pool {name} created")?;
            }
            PoolCmd::Set(set) => {
                client
                    .patch(
                        &format!("/storage-pools/{}", set.pool),
                        &pool_set_body(&set),
                    )
                    .await?;
                writeln!(out, "storage pool {}: {}", set.pool, pool_set_summary(&set))?;
            }
            PoolCmd::Delete { pool } => {
                client.delete(&format!("/storage-pools/{pool}")).await?;
                writeln!(out, "storage pool {pool} deleted")?;
            }
        },
        Cmd::Network { cmd } => match cmd {
            NetworkCmd::List => {
                let list = client.get("/networks").await?;
                print_list(out, json_out, &list, NETWORK_HEADERS, network_row)?;
            }
            NetworkCmd::Get { network } => {
                print_json(out, &client.get(&format!("/networks/{network}")).await?)?
            }
            NetworkCmd::Create(c) => {
                client.post("/networks", &network_create_body(&c)).await?;
                writeln!(out, "network {} created", c.name)?;
            }
            NetworkCmd::Delete { network } => {
                client.delete(&format!("/networks/{network}")).await?;
                writeln!(out, "network {network} deleted")?;
            }
        },
        Cmd::SecurityGroup { cmd } => security_group(&client, cmd, json_out, out).await?,
        Cmd::Volume { cmd } => match cmd {
            VolumeCmd::List => {
                let list = client.get("/volumes").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["NAME", "POOL", "SIZE", "FORMAT", "ALLOCATED"],
                    |v| {
                        vec![
                            cell(&v["name"]),
                            cell(&v["spec"]["pool"]),
                            bytes(&v["spec"]["size_bytes"]),
                            cell(&v["spec"]["format"]),
                            bytes(&v["status"]["allocated_bytes"]),
                        ]
                    },
                )?;
            }
            VolumeCmd::Get { volume } => {
                print_json(out, &client.get(&format!("/volumes/{volume}")).await?)?
            }
            VolumeCmd::Create {
                name,
                pool,
                size,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let body = json!({
                    "name": name,
                    "spec": { "pool": pool, "size_bytes": size, "format": value_name(format) }
                });
                let task = client.post("/volumes", &body).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} created")?;
            }
            VolumeCmd::Import {
                name,
                pool,
                file,
                source_format,
                format,
            } => {
                let format = volume_format(&client, &pool, format).await?;
                let (src, fmt) = (value_name(source_format), value_name(format));
                let query = [
                    ("name", name.as_str()),
                    ("pool", pool.as_str()),
                    ("source_format", src.as_str()),
                    ("format", fmt.as_str()),
                ];
                // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
                let size = tokio::fs::metadata(&file)
                    .await
                    .with_context(|| format!("cannot read {}", file.display()))?
                    .len()
                    .to_string();
                let mut check = query.to_vec();
                check.push(("size", size.as_str()));
                client
                    .send(
                        client
                            .request(Method::POST, "/volumes/import/check")
                            .query(&check),
                    )
                    .await?;
                let task = client.upload("/volumes/import", &query, &file).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {name} imported")?;
            }
            VolumeCmd::Resize { volume, size } => {
                let body = json!({ "size_bytes": size });
                let task = client
                    .post(&format!("/volumes/{volume}/resize"), &body)
                    .await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} resized to {}", bytes(&size.into()))?;
            }
            VolumeCmd::Delete { volume } => {
                let task = client.delete(&format!("/volumes/{volume}")).await?;
                client.wait_task(&task).await?;
                writeln!(out, "volume {volume} deleted")?;
            }
        },
        Cmd::Vm { cmd } => vm(&client, cmd, json_out, out).await?,
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Import { cmd } => import(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List => {
                let list = client.get("/tasks").await?;
                print_list(
                    out,
                    json_out,
                    &list,
                    &["ID", "OP", "TARGET", "STATE", "ERROR"],
                    task_row,
                )?;
            }
            TaskCmd::Get { task } => {
                print_json(out, &client.get(&format!("/tasks/{task}")).await?)?
            }
        },
        Cmd::User { cmd } => match cmd {
            UserCmd::List => {
                let list = client.get("/users").await?;
                print_list(out, json_out, &list, &["NAME", "ROLE"], |u| {
                    vec![cell(&u["name"]), cell(&u["role"])]
                })?;
            }
            UserCmd::Create {
                name,
                role,
                password_file,
            } => {
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "name": name, "password": password, "role": value_name(role) });
                client.post("/users", &body).await?;
                writeln!(out, "user {name} created")?;
            }
            UserCmd::Passwd {
                name,
                password_file,
                current_password_file,
            } => {
                let me = client.get("/auth/whoami").await?;
                let current = if me["name"] == name.as_str() {
                    Some(read_password(
                        current_password_file.as_ref(),
                        "Current password: ",
                    )?)
                } else {
                    None
                };
                let password = read_password(password_file.as_ref(), "New password: ")?;
                let body = json!({ "password": password, "current_password": current });
                client.put(&format!("/users/{name}"), &body).await?;
                writeln!(out, "password of {name} changed")?;
            }
            UserCmd::Delete { name } => {
                client.delete(&format!("/users/{name}")).await?;
                writeln!(out, "user {name} deleted")?;
            }
        },
        Cmd::Token { cmd } => match cmd {
            TokenCmd::List => {
                let list = client.get("/tokens").await?;
                print_list(out, json_out, &list, &["NAME", "ID", "USER"], |t| {
                    vec![cell(&t["name"]), cell(&t["id"]), cell(&t["user"])]
                })?;
            }
            TokenCmd::Create { name } => {
                let created = client.post("/tokens", &json!({ "name": name })).await?;
                writeln!(out, "{}", cell(&created["secret"]))?;
            }
            TokenCmd::Delete { token } => {
                client.delete(&format!("/tokens/{token}")).await?;
                writeln!(out, "token {token} deleted")?;
            }
        },
        Cmd::Realm { cmd } => realm(&client, cmd, json_out, out).await?,
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
        Cmd::Subscription { cmd } => subscription(&client, cmd, json_out, out).await?,
        Cmd::Telemetry { cmd } => telemetry(&client, cmd, json_out, out).await?,
        Cmd::Audit(args) => audit(&client, args, json_out, out, &mut std::io::stderr()).await?,
    }
    Ok(())
}

async fn security_group(
    client: &Client,
    cmd: SecurityGroupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SecurityGroupCmd::List => {
            let list = client.get("/security-groups").await?;
            print_list(
                out,
                json_out,
                &list,
                SECURITY_GROUP_HEADERS,
                security_group_row,
            )?;
        }
        SecurityGroupCmd::Get { group } => print_json(
            out,
            &client.get(&format!("/security-groups/{group}")).await?,
        )?,
        SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } => {
            let body = security_group_create_body(&name, description.as_deref(), &rules);
            client.post("/security-groups", &body).await?;
            writeln!(out, "security group {name} created")?;
        }
        SecurityGroupCmd::Delete { group } => {
            client.delete(&format!("/security-groups/{group}")).await?;
            writeln!(out, "security group {group} deleted")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Add { group, rule, at },
        } => {
            patch_rules(client, &group, |g| rules_with(g, rule, at)).await?;
            writeln!(out, "security group {group}: rule added")?;
        }
        SecurityGroupCmd::Rule {
            cmd: RuleCmd::Rm { group, n },
        } => {
            patch_rules(client, &group, |g| rules_without(g, n)).await?;
            writeln!(out, "security group {group}: rule {n} removed")?;
        }
    }
    Ok(())
}

async fn backup(
    client: &Client,
    cmd: BackupCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupCmd::Create { vm, pool, name } => {
            let body = json!({ "pool": pool, "name": name });
            let task = client.post(&format!("/vms/{vm}/backups"), &body).await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            // 이름을 주지 않았으면 서버가 정한 이름을 읽는다.
            let name = match name {
                Some(name) => name,
                None => {
                    let id = cell(&task["spec"]["op"]["backup"]);
                    cell(&client.get(&format!("/backups/{id}")).await?["name"])
                }
            };
            writeln!(out, "backup {name} of vm {vm} created")?;
        }
        BackupCmd::List { vm } => {
            let mut r = client.request(Method::GET, "/backups");
            if let Some(vm) = &vm {
                r = r.query(&[("vm", vm)]);
            }
            let list = client.send(r).await?;
            print_list(out, json_out, &list, BACKUP_HEADERS, backup_row)?;
        }
        BackupCmd::Get { backup } => {
            print_json(out, &client.get(&format!("/backups/{backup}")).await?)?
        }
        BackupCmd::Delete { backup, force } => backup_delete(client, &backup, force, out).await?,
        BackupCmd::Restore {
            backup,
            name,
            pool,
            node,
        } => {
            let body = json!({ "name": name, "pool": pool, "node": node });
            let task = client
                .post(&format!("/backups/{backup}/restore"), &body)
                .await?;
            client.wait_task_for(&task, BACKUP_WAIT).await?;
            writeln!(out, "vm {name} restored from backup {backup}")?;
        }
    }
    Ok(())
}

/// 백업을 지운다. `force`로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
async fn backup_delete(
    client: &Client,
    backup: &str,
    force: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut r = client.request(Method::DELETE, &format!("/backups/{backup}"));
    if force {
        r = r.query(&[("force", "true")]);
    }
    let got = client.send(r).await?;
    if let Some(message) = got["message"].as_str() {
        writeln!(out, "{message}")?;
    } else {
        client.wait_task(&got).await?;
        writeln!(out, "backup {backup} deleted")?;
    }
    Ok(())
}

/// `backup list`의 열
const BACKUP_HEADERS: &[&str] = &["NAME", "VM", "POOL", "PHASE", "CREATED", "SIZE"];

/// `backup list`의 한 행. VM은 만든 때의 이름이고(VM을 지웠어도 남는다), SIZE는 백업 파일 크기의 합이다.
fn backup_row(b: &Value) -> Vec<String> {
    let status = &b["status"];
    let created = status["created_at"]
        .as_i64()
        .map_or_else(|| "-".to_string(), unix_time);
    let size = match status["disks"].as_array() {
        Some(disks) if status["phase"] == "ready" => bytes(
            &disks
                .iter()
                .filter_map(|d| d["stored_bytes"].as_u64())
                .sum::<u64>()
                .into(),
        ),
        _ => "-".into(),
    };
    vec![
        cell(&b["name"]),
        cell(&b["spec"]["vm_name"]),
        cell(&b["spec"]["pool"]),
        cell(&status["phase"]),
        created,
        size,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => {
            print_json(out, &client.get(&format!("/backup-jobs/{job}")).await?)?
        }
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", args.job);
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client.delete(&format!("/backup-jobs/{job}")).await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{job}/run")))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
        last,
    ]
}

async fn import(
    client: &Client,
    cmd: ImportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ImportCmd::Ova { file, name } => {
            // 파일을 보내기 전에 서버가 받을 수 있는지 확인한다.
            let size = tokio::fs::metadata(&file)
                .await
                .with_context(|| format!("cannot read {}", file.display()))?
                .len()
                .to_string();
            client
                .send(
                    client
                        .request(Method::POST, "/vm-imports/ova/check")
                        .query(&[("name", name.as_str()), ("size", size.as_str())]),
                )
                .await?;
            let file_name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{name}.ova"));
            let query = [("name", name.as_str()), ("file_name", file_name.as_str())];
            client.upload("/vm-imports/ova", &query, &file).await?;
            writeln!(
                out,
                "vm-import {name} uploaded; review it with `oplctl import get {name}`"
            )?;
        }
        ImportCmd::EsxiBrowse { conn, fingerprint } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, fingerprint.as_deref(), &password);
            esxi_browse(client, &connection, json_out, out).await?;
        }
        ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } => {
            let password = esxi_password(&conn)?;
            let connection = esxi_connection(&conn, Some(&fingerprint), &password);
            let body = esxi_import_body(&name, &connection, &datastore, &vmx);
            esxi_create(client, &body, out).await?;
        }
        ImportCmd::List => {
            let list = client.get("/vm-imports").await?;
            let (nodes, vms) = if json_out {
                (Vec::new(), Vec::new())
            } else {
                (
                    items(&client.get("/nodes").await?),
                    items(&client.get("/vms").await?),
                )
            };
            print_list(out, json_out, &list, IMPORT_HEADERS, |i| {
                import_row(i, &nodes, &vms)
            })?;
        }
        ImportCmd::Get { import } => {
            let import = client.get(&format!("/vm-imports/{import}")).await?;
            if json_out {
                print_json(out, &import)?;
            } else {
                let nodes = items(&client.get("/nodes").await?);
                let vms = if import["status"]["vm"].is_null() {
                    Vec::new()
                } else {
                    items(&client.get("/vms").await?)
                };
                print_import(out, &import, &nodes, &vms)?;
            }
        }
        ImportCmd::Run(args) => {
            // `=` 없는 --net이 있을 때만 원본 네트워크를 읽는다.
            let sources = if args.nets.iter().any(|n| n.source.is_none()) {
                source_networks(&client.get(&format!("/vm-imports/{}", args.import)).await?)
            } else {
                Vec::new()
            };
            let body = import_run_body(&args, &sources)?;
            let task = client
                .post(&format!("/vm-imports/{}/run", args.import), &body)
                .await?;
            client.wait_task_for(&task, IMPORT_WAIT).await?;
            writeln!(out, "vm {} imported", args.name)?;
        }
        ImportCmd::Delete { import, force } => {
            let mut r = client.request(Method::DELETE, &format!("/vm-imports/{import}"));
            if force {
                r = r.query(&[("force", "true")]);
            }
            let got = client.send(r).await?;
            // force로 기록만 지웠으면(200) 서버의 알림을 보이고, 아니면 Task(202)를 기다린다.
            if let Some(message) = got["message"].as_str() {
                writeln!(out, "{message}")?;
            } else {
                client.wait_task(&got).await?;
                writeln!(out, "vm-import {import} deleted")?;
            }
        }
    }
    Ok(())
}

/// ESXi 연결 오류. 믿을 지문이 없으면 서버 인증서의 지문을 확인하고 다시 하라고 알린다.
fn esxi_error(e: anyhow::Error) -> anyhow::Error {
    match e.downcast_ref::<ApiError>() {
        Some(api) if api.code == "untrusted_certificate" => {
            let fingerprint = cell(&api.details["fingerprint"]);
            anyhow::anyhow!(
                "the ESXi certificate fingerprint is {fingerprint}; check it and rerun with --fingerprint {fingerprint}"
            )
        }
        _ => e,
    }
}

/// `import esxi-browse`: 데이터스토어마다 VM 표
async fn esxi_browse(
    client: &Client,
    connection: &Value,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let got = client
        .post("/vm-imports/esxi/browse", connection)
        .await
        .map_err(esxi_error)?;
    if json_out {
        return Ok(print_json(out, &got)?);
    }
    let mut rows = Vec::new();
    for ds in got["datastores"].as_array().into_iter().flatten() {
        for vm in ds["vms"].as_array().into_iter().flatten() {
            rows.push(esxi_vm_row(&ds["name"], vm));
        }
    }
    table(out, ESXI_VM_HEADERS, &rows)?;
    Ok(())
}

/// `import esxi-browse`의 열
const ESXI_VM_HEADERS: &[&str] = &[
    "DATASTORE",
    "VMX",
    "NAME",
    "GUEST",
    "CPU",
    "MEMORY",
    "DISKS",
    "LOCKED",
];

/// `import esxi-browse`의 한 행. `.vmx`를 읽지 못한 VM은 NAME 자리에 오류를 보인다.
fn esxi_vm_row(datastore: &Value, vm: &Value) -> Vec<String> {
    let (name, guest, cpu, memory, disks) = match vm["error"].as_str() {
        Some(error) => (
            format!("error: {error}"),
            "-".into(),
            "-".into(),
            "-".into(),
            "-".into(),
        ),
        None => (
            cell(&vm["name"]),
            cell(&vm["guest_os"]),
            cell(&vm["vcpus"]),
            bytes(&vm["memory_mib"].as_u64().map(|m| m << 20).into()),
            cell(&vm["disks"]),
        ),
    };
    vec![
        cell(datastore),
        cell(&vm["vmx"]),
        name,
        guest,
        cpu,
        memory,
        disks,
        cell(&vm["locked"]),
    ]
}

/// `import esxi`: 가져오기를 만든다.
async fn esxi_create(
    client: &Client,
    body: &Value,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    client
        .post("/vm-imports/esxi", body)
        .await
        .map_err(esxi_error)?;
    writeln!(
        out,
        "vm-import {name} created; review it with `oplctl import get {name}`",
        name = cell(&body["name"])
    )?;
    Ok(())
}

/// ESXi 원본의 표시(`esxi://호스트/데이터스토어/.vmx`). OVA 원본이면 `None`이다.
fn esxi_source(i: &Value) -> Option<String> {
    let esxi = &i["spec"]["source"]["esxi"];
    esxi.is_object().then(|| {
        format!(
            "esxi://{}/{}/{}",
            cell(&esxi["host"]),
            cell(&esxi["datastore"]),
            cell(&esxi["vmx"])
        )
    })
}

/// 목록에서 id가 `id`인 항목의 이름. 없으면 id다.
fn name_of(list: &[Value], id: &Value) -> String {
    list.iter()
        .find(|v| v["id"] == *id)
        .map_or_else(|| cell(id), |v| cell(&v["name"]))
}

/// `import list`의 열
const IMPORT_HEADERS: &[&str] = &["NAME", "NODE", "SOURCE", "PHASE", "VM"];

/// `import list`의 한 행. SOURCE는 원본 VM 이름이고, 원본을 읽지 못했으면 올린 파일 이름이다.
/// ESXi 원본이면 `esxi://호스트/데이터스토어/.vmx`다.
fn import_row(i: &Value, nodes: &[Value], vms: &[Value]) -> Vec<String> {
    let status = &i["status"];
    let source = match (esxi_source(i), status["source_vm"]["name"].as_str()) {
        (Some(esxi), _) => esxi,
        (None, Some(name)) => name.to_string(),
        (None, None) => cell(&i["spec"]["source"]["ova"]["file_name"]),
    };
    vec![
        cell(&i["name"]),
        name_of(nodes, &i["spec"]["node"]),
        source,
        cell(&status["phase"]),
        name_of(vms, &status["vm"]),
    ]
}

/// `import get`: 머리 줄들, 원본 디스크와 NIC 표, 경고
fn print_import(
    out: &mut (dyn Write + Send),
    i: &Value,
    nodes: &[Value],
    vms: &[Value],
) -> anyhow::Result<()> {
    let ova = &i["spec"]["source"]["ova"];
    let status = &i["status"];
    writeln!(out, "vm-import: {}", cell(&i["name"]))?;
    writeln!(out, "node: {}", name_of(nodes, &i["spec"]["node"]))?;
    match esxi_source(i) {
        Some(esxi) => writeln!(out, "source: {esxi}")?,
        None => writeln!(
            out,
            "source: {} ({})",
            cell(&ova["file_name"]),
            bytes(&ova["size_bytes"])
        )?,
    }
    writeln!(out, "phase: {}", cell(&status["phase"]))?;
    if !status["vm"].is_null() {
        writeln!(out, "vm: {}", name_of(vms, &status["vm"]))?;
    }
    if let Some(error) = status["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    let src = &status["source_vm"];
    if src.is_object() {
        writeln!(
            out,
            "source vm: {} ({}, {}, {} vCPUs, {} cores per socket, {} MiB)",
            cell(&src["name"]),
            cell(&src["guest_os"]),
            cell(&src["firmware"]),
            cell(&src["vcpus"]),
            cell(&src["cores_per_socket"]),
            cell(&src["memory_mib"])
        )?;
    }
    let p = &status["proposal"];
    if p.is_object() {
        writeln!(
            out,
            "proposal: {} sockets x {} cores, {} MiB, {}, {} disks, {} NICs",
            cell(&p["sockets"]),
            cell(&p["cores"]),
            cell(&p["memory_mib"]),
            cell(&p["firmware"]),
            cell(&p["disk_bus"]),
            cell(&p["nic_model"])
        )?;
    }
    if src.is_object() {
        let disks: Vec<_> = src["disks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                vec![
                    cell(&d["name"]),
                    cell(&d["file"]),
                    bytes(&d["capacity_bytes"]),
                    cell(&d["controller"]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["DISK", "FILE", "SIZE", "CONTROLLER"], &disks)?;
        // PROPOSED는 이름이 같아 제안한 opLite 네트워크다.
        let nics: Vec<_> = src["nics"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                vec![
                    cell(&n["network"]),
                    cell(&n["adapter"]),
                    cell(&n["mac"]),
                    cell(&p["networks"][n["network"].as_str().unwrap_or_default()]),
                ]
            })
            .collect();
        writeln!(out)?;
        table(out, &["NETWORK", "ADAPTER", "MAC", "PROPOSED"], &nics)?;
    }
    if let Some(warnings) = status["warnings"].as_array().filter(|w| !w.is_empty()) {
        writeln!(out, "\nwarnings:")?;
        for w in warnings {
            writeln!(out, "  {}", cell(w))?;
        }
    }
    Ok(())
}

/// 이 기계의 opld에 보내는 클러스터 명령. 로그인이 필요 없다. 다른 명령이면 `None`이다.
async fn cluster_local(
    cmd: &ClusterCmd,
    out: &mut (dyn Write + Send),
) -> Option<anyhow::Result<()>> {
    let run = async {
        match cmd {
            ClusterCmd::Init { name, local: l } => {
                let r = local(&l.socket, "/init", &json!({ "cluster_name": name })).await?;
                writeln!(out, "cluster {name} created")?;
                writeln!(
                    out,
                    "admin password (shown only once): {}",
                    cell(&r["admin_password"])
                )?;
                writeln!(out, "CA fingerprint: {}", cell(&r["ca_fingerprint"]))?;
                writeln!(
                    out,
                    "join URL for the next node (single use, valid for 1 hour):"
                )?;
                writeln!(out, "  {}", cell(&r["join_url"]))?;
                writeln!(out, "log in with:")?;
                writeln!(
                    out,
                    "  oplctl login --server https://{} --ca-fingerprint {}",
                    cell(&r["api_address"]),
                    cell(&r["ca_fingerprint"])
                )?;
            }
            ClusterCmd::Join { url, local: l } => {
                let r = local(&l.socket, "/join", &json!({ "url": url })).await?;
                writeln!(
                    out,
                    "node {} joined cluster {}",
                    cell(&r["node"]),
                    cell(&r["cluster"])
                )?;
            }
            ClusterCmd::Leave { local: l } => {
                local(&l.socket, "/leave", &json!({})).await?;
                writeln!(out, "this node left the cluster")?;
            }
            _ => return Ok(false),
        }
        anyhow::Ok(true)
    };
    match run.await {
        Ok(false) => None,
        Ok(true) => Some(Ok(())),
        Err(e) => Some(Err(e)),
    }
}

/// 로그인한 서버의 API로 보내는 클러스터 명령
async fn cluster(
    client: &Client,
    cmd: ClusterCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ClusterCmd::Init { .. } | ClusterCmd::Join { .. } | ClusterCmd::Leave { .. } => {
            unreachable!("handled by cluster_local")
        }
        ClusterCmd::Status => {
            let view = client.get("/cluster").await?;
            if json_out {
                print_json(out, &view)?;
                return Ok(());
            }
            print_cluster_status(out, &view)?;
        }
        ClusterCmd::JoinToken { ttl } => {
            let created = client
                .post("/cluster/join-tokens", &json!({ "ttl_secs": ttl }))
                .await?;
            writeln!(out, "{}", cell(&created["url"]))?;
        }
        ClusterCmd::RemoveNode { node, force } => {
            if !force {
                bail!("removing a node cannot be undone; pass --force to confirm");
            }
            client.delete(&format!("/nodes/{node}")).await?;
            writeln!(out, "node {node} removed from the cluster")?;
        }
    }
    Ok(())
}

async fn vm(
    client: &Client,
    cmd: VmCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCmd::List => {
            let list = client.get("/vms").await?;
            print_list(
                out,
                json_out,
                &list,
                &["NAME", "POWER", "PHASE", "NODE", "HA", "PID", "ERROR"],
                vm_row,
            )?;
        }
        VmCmd::Get { vm } => print_json(out, &client.get(&format!("/vms/{vm}")).await?)?,
        VmCmd::Create(args) => {
            let node = match &args.node {
                Some(key) => Some(client.get(&format!("/nodes/{key}")).await?),
                None => None,
            };
            let machine = match args.machine {
                Some(m) => m,
                None => {
                    let list = client.get("/nodes").await?;
                    let nodes = list["items"].as_array().cloned().unwrap_or_default();
                    default_machine(node.as_ref(), &nodes)?
                }
            };
            let node = node.map(|n| n["id"].clone());
            let disks: Vec<Value> = args
                .disks
                .iter()
                .enumerate()
                .map(|(i, volume)| {
                    let boot = (i == 0).then_some(1);
                    json!({ "volume": volume, "bus": "virtio-blk", "boot_order": boot })
                })
                .collect();
            let nics: Vec<Value> = args.nics.iter().map(nic_body).collect();
            let body = json!({
                "name": args.name,
                "spec": {
                    "cpu": { "sockets": args.sockets, "cores": args.cores },
                    "memory_mib": args.memory,
                    "machine": value_name(machine),
                    "firmware": value_name(args.firmware),
                    "disks": disks,
                    "nics": nics,
                    "restart": value_name(args.restart),
                    "node": node,
                    "ha": args.ha,
                }
            });
            client.post("/vms", &body).await?;
            writeln!(out, "vm {} created", args.name)?;
            if args.start {
                act(client, &args.name, "start", out).await?;
            }
        }
        VmCmd::Set(args) => {
            put_spec_field(client, &args.vm, "ha", json!(args.ha)).await?;
            let state = if args.ha { "on" } else { "off" };
            writeln!(out, "vm {}: HA {state}", args.vm)?;
        }
        VmCmd::NicSg { vm, index, groups } => {
            put_spec(client, &vm, |current| {
                nic_groups_update(current, index, &groups)
            })
            .await?;
            if groups.is_empty() {
                writeln!(out, "vm {vm}: NIC {index} has no security groups")?;
            } else {
                writeln!(
                    out,
                    "vm {vm}: NIC {index} security groups {}",
                    groups.join(", ")
                )?;
            }
        }
        VmCmd::Start { vm } => act(client, &vm, "start", out).await?,
        VmCmd::Stop { vm, force } => {
            act(
                client,
                &vm,
                if force { "stop?mode=force" } else { "stop" },
                out,
            )
            .await?
        }
        VmCmd::Reboot { vm } => act(client, &vm, "reboot", out).await?,
        VmCmd::Migrate { vm, node } => {
            let task = client
                .post(&format!("/vms/{vm}/migrate"), &json!({ "node": node }))
                .await?;
            // 마이그레이션은 일반 Task 제한(10분)보다 오래 걸릴 수 있다.
            client.wait_task_for(&task, MIGRATE_WAIT).await?;
            writeln!(out, "vm {vm}: migrated to {node}")?;
        }
        VmCmd::Delete { vm, with_snapshots } => {
            if with_snapshots {
                // 내부 스냅샷은 서로 독립이라 차례는 상관없다. VM을 지울 수 없는데 스냅샷만
                // 지우지 않도록 먼저 모두 확인한다.
                let current = client.get(&format!("/vms/{vm}")).await?;
                let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                let mut items = list["items"].as_array().cloned().unwrap_or_default();
                check_deletable(&vm, &current, &items)?;
                // 지우는 사이 예약 스냅샷이 새로 생기지 않게 예약을 먼저 끄고 다시 확인한다.
                if schedule_is_on(&current) {
                    put_schedule(client, &vm, None).await?;
                    writeln!(out, "snapshot schedule of vm {vm} turned off")?;
                    let current = client.get(&format!("/vms/{vm}")).await?;
                    let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
                    items = list["items"].as_array().cloned().unwrap_or_default();
                    check_deletable(&vm, &current, &items)?;
                }
                for s in items {
                    let name = cell(&s["spec"]["name"]);
                    let path = format!("/vms/{vm}/snapshots/{}", cell(&s["id"]));
                    client.wait_task(&client.delete(&path).await?).await?;
                    writeln!(out, "snapshot {name} of vm {vm} deleted")?;
                }
            }
            client.delete(&format!("/vms/{vm}")).await?;
            writeln!(out, "vm {vm} deleted")?;
        }
        VmCmd::Clone {
            vm,
            name,
            snapshot,
            pool,
        } => {
            let body = json!({ "name": name, "snapshot": snapshot, "pool": pool });
            let task = client.post(&format!("/vms/{vm}/clone"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {name} cloned from {vm}")?;
        }
        VmCmd::Snapshot { cmd } => snapshot(client, cmd, json_out, out).await?,
        VmCmd::Console { vm, port } => {
            // 콘솔을 열 수 있는지 먼저 확인한다(없는 VM, 권한).
            client.get(&format!("/vms/{vm}")).await?;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .with_context(|| format!("cannot listen on 127.0.0.1:{port}"))?;
            let port = listener.local_addr()?.port();
            writeln!(
                out,
                "console of {vm}: vnc://127.0.0.1:{port} (Ctrl-C to quit)"
            )?;
            out.flush()?;
            tokio::select! {
                result = crate::console::serve(client.clone(), vm, listener) => result?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
    Ok(())
}

/// `task list`의 한 행. HA 복구면 OP에 죽은 노드를 붙인다(`vm-ha-recover (from A)`).
fn task_row(t: &Value) -> Vec<String> {
    let op = &t["spec"]["op"];
    let mut kind = cell(&op["kind"]);
    if op["kind"] == "vm-ha-recover" {
        kind = format!("{kind} (from {})", cell(&op["from"]));
    }
    vec![
        cell(&t["id"]),
        kind,
        cell(&t["spec"]["target"]),
        cell(&t["status"]["state"]),
        cell(&t["status"]["error"]),
    ]
}

/// `vm list`의 한 행. 마이그레이션 중이면 NODE에 대상 노드를 붙인다(`A -> B`).
fn vm_row(v: &Value) -> Vec<String> {
    let mut node = cell(&v["status"]["node"]);
    let target = &v["status"]["migration"]["node"];
    if !target.is_null() {
        node = format!("{node} -> {}", cell(target));
    }
    vec![
        cell(&v["name"]),
        cell(&v["spec"]["power"]),
        cell(&v["status"]["phase"]),
        node,
        // 옛 서버는 보내지 않는다 — HA VM이 아니다.
        (v["spec"]["ha"] == true).to_string(),
        cell(&v["status"]["pid"]),
        cell(&v["status"]["last_error"]),
    ]
}

/// `--machine`이 없을 때의 머신: 지정 노드의 arch, 아니면 arch를 아는 노드가 모두 같은 arch.
fn default_machine(node: Option<&Value>, nodes: &[Value]) -> anyhow::Result<Machine> {
    let machine = |n: &Value| match n["status"]["arch"].as_str() {
        Some("x86_64") => Some(Machine::Q35),
        Some("aarch64") => Some(Machine::Virt),
        _ => None,
    };
    if let Some(m) = node.and_then(machine) {
        return Ok(m);
    }
    let mut known = nodes.iter().filter_map(machine);
    match known.next() {
        Some(first) if known.all(|m| m == first) => Ok(first),
        _ => bail!(
            "cannot choose a machine type from the node architectures; pass --machine virt or --machine q35"
        ),
    }
}

/// 서버의 VM 삭제 조건(꺼짐을 바라고 멈췄거나 실패함)과 모든 스냅샷이 다 만들어졌는지 본다.
/// `vm delete --with-snapshots`가 아무것도 지우기 전에 쓴다.
fn check_deletable(vm: &str, current: &Value, snapshots: &[Value]) -> anyhow::Result<()> {
    let stopped = current["spec"]["power"] == "stopped"
        && matches!(
            current["status"]["phase"].as_str(),
            Some("stopped" | "failed")
        );
    if !stopped {
        bail!("VM {vm} is not stopped; stop it first");
    }
    if let Some(s) = snapshots.iter().find(|s| s["status"]["phase"] != "ready") {
        bail!(
            "snapshot {} of VM {vm} is still being created; try again later",
            cell(&s["spec"]["name"])
        );
    }
    Ok(())
}

async fn snapshot(
    client: &Client,
    cmd: SnapshotCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        SnapshotCmd::Create {
            vm,
            name,
            description,
            no_memory,
        } => {
            // 메모리를 담을지는 생략하면 서버가 정한다(실행 중이면 담는다).
            let body = json!({
                "name": name,
                "description": description.unwrap_or_default(),
                "memory": no_memory.then_some(false),
            });
            let task = client.post(&format!("/vms/{vm}/snapshots"), &body).await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} created")?;
        }
        SnapshotCmd::List { vm } => {
            let list = client.get(&format!("/vms/{vm}/snapshots")).await?;
            if json_out {
                return print_list(out, true, &list, &[], |_| vec![]);
            }
            let current = client.get(&format!("/vms/{vm}")).await?["status"]["snapshot"].clone();
            let items = list["items"].as_array().cloned().unwrap_or_default();
            let rows = snapshot_rows(&items, current.as_str());
            table(out, SNAPSHOT_HEADERS, &rows)?;
        }
        SnapshotCmd::Revert { vm, name } => {
            let path = format!("/vms/{vm}/snapshots/{name}/revert");
            let task = client.send(client.request(Method::POST, &path)).await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm {vm}: reverted to snapshot {name}")?;
        }
        SnapshotCmd::Delete { vm, name } => {
            let task = client
                .delete(&format!("/vms/{vm}/snapshots/{name}"))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "snapshot {name} of vm {vm} deleted")?;
        }
        SnapshotCmd::Schedule(args) => schedule(client, args, json_out, out).await?,
    }
    Ok(())
}

/// 예약을 보거나 바꾼다([`put_schedule`]).
async fn schedule(
    client: &Client,
    args: ScheduleArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/vms/{}", args.vm);
    let schedule = schedule_value(&args);
    if schedule.is_none() && !args.off {
        let current = client.get(&path).await?;
        if json_out {
            let view = json!({
                "snapshot_schedule": current["spec"]["snapshot_schedule"],
                "schedule_error": current["status"]["schedule_error"],
            });
            print_json(out, &view)?;
        } else {
            write!(out, "{}", describe_schedule(&current))?;
        }
        return Ok(());
    }
    put_schedule(client, &args.vm, schedule.clone()).await?;
    match schedule {
        Some(_) => writeln!(out, "vm {}: snapshot schedule set", args.vm)?,
        None => writeln!(out, "vm {}: snapshot schedule off", args.vm)?,
    }
    Ok(())
}

/// VM `vm`의 예약을 `schedule`로 바꾼다(`None`이면 끈다).
async fn put_schedule(client: &Client, vm: &str, schedule: Option<Value>) -> anyhow::Result<()> {
    put_spec_field(
        client,
        vm,
        "snapshot_schedule",
        schedule.unwrap_or(Value::Null),
    )
    .await
}

/// VM `vm`의 spec 필드 `field`를 `value`로 바꾼다.
async fn put_spec_field(
    client: &Client,
    vm: &str,
    field: &str,
    value: Value,
) -> anyhow::Result<()> {
    put_spec(client, vm, |current| {
        Ok(spec_update(current, field, value.clone()))
    })
    .await
}

/// VM `vm`을 읽어 `body`(읽은 VM → 수정 본문)로 만든 본문을 그 revision으로(`If-Match`) 다시 쓴다.
async fn put_spec(
    client: &Client,
    vm: &str,
    body: impl Fn(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<()> {
    let path = format!("/vms/{vm}");
    let mut retries = SpecRetries::default();
    loop {
        let current = client.get(&path).await?;
        let r = client
            .request(Method::PUT, &path)
            .header(reqwest::header::IF_MATCH, cell(&current["revision"]))
            .json(&body(&current)?);
        match client.send(r).await {
            Err(e) => match retries.next(&e.to_string()) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e),
            },
            Ok(_) => return Ok(()),
        }
    }
}

/// VM에 예약이 있는지
fn schedule_is_on(vm: &Value) -> bool {
    !vm["spec"]["snapshot_schedule"].is_null()
}

/// VM 수정 PUT을 다시 할 횟수
#[derive(Default)]
struct SpecRetries {
    mismatch: u32,
    busy: u32,
}

impl SpecRetries {
    /// 오류 `error`면 얼마 뒤에 다시 할지. 읽은 뒤 status만 바뀌어도 revision이 달라지므로
    /// `revision_mismatch`는 바로 세 번까지, 스냅샷·복제 Task 중이면(`vm_busy`) 1초마다 30번까지.
    fn next(&mut self, error: &str) -> Option<std::time::Duration> {
        if error.ends_with("(revision_mismatch)") && self.mismatch < 3 {
            self.mismatch += 1;
            return Some(std::time::Duration::ZERO);
        }
        if error.ends_with("(vm_busy)") && self.busy < 30 {
            self.busy += 1;
            return Some(std::time::Duration::from_secs(1));
        }
        None
    }
}

/// VM 수정 본문: 라벨과 spec은 그대로 두고 spec 필드 `field`만 `value`로 바꾼다.
fn spec_update(vm: &Value, field: &str, value: Value) -> Value {
    let mut spec = vm["spec"].clone();
    spec[field] = value;
    // 라벨이 없으면 응답에 `labels`가 없다.
    let labels = vm.get("labels").cloned().unwrap_or_else(|| json!({}));
    json!({ "labels": labels, "spec": spec })
}

/// VM 수정 본문: NIC `index`(0부터)의 보안 그룹만 `groups`로 바꾼다.
fn nic_groups_update(vm: &Value, index: usize, groups: &[String]) -> anyhow::Result<Value> {
    let mut nics = vm["spec"]["nics"].as_array().cloned().unwrap_or_default();
    let count = nics.len();
    let Some(nic) = nics.get_mut(index) else {
        bail!(
            "VM {} has no NIC {index} ({count} NICs, numbered from 0)",
            cell(&vm["name"])
        );
    };
    nic["security_groups"] = json!(groups);
    Ok(spec_update(vm, "nics", Value::Array(nics)))
}

/// `schedule: daily, keep 7, with memory`와 마지막 예약 오류가 있으면 그 줄
fn describe_schedule(vm: &Value) -> String {
    let s = &vm["spec"]["snapshot_schedule"];
    let mut text = if s.is_null() {
        "schedule: off\n".to_string()
    } else {
        let memory = if s["memory"] == true {
            ", with memory"
        } else {
            ""
        };
        format!(
            "schedule: {}, keep {}{memory}\n",
            cell(&s["every"]),
            cell(&s["keep"])
        )
    };
    let e = &vm["status"]["schedule_error"];
    if let Some(message) = e["message"].as_str() {
        text.push_str(&format!("last error: {}: {message}\n", cell(&e["code"])));
    }
    text
}

/// `vm snapshot list`의 열
const SNAPSHOT_HEADERS: &[&str] = &["", "NAME", "CREATED", "MEMORY", "SCHEDULED", "DESCRIPTION"];

/// 스냅샷 목록을 나무 모양의 표 행으로. 첫 칸은 현재 위치 표시(`*`), 형제는 만든 차례(id가 UUIDv7)다.
fn snapshot_rows(items: &[Value], current: Option<&str>) -> Vec<Vec<String>> {
    let mut items: Vec<&Value> = items.iter().collect();
    items.sort_by_key(|s| s["id"].as_str().unwrap_or_default().to_string());
    let mut rows = Vec::new();
    snapshot_subtree(&items, None, "", current, &mut rows);
    rows
}

/// `parent`의 자식들(목록에 없는 부모를 가리키면 뿌리)과 그 아래를 행으로 더한다.
fn snapshot_subtree(
    items: &[&Value],
    parent: Option<&str>,
    prefix: &str,
    current: Option<&str>,
    rows: &mut Vec<Vec<String>>,
) {
    let known = |p: &&str| items.iter().any(|i| i["id"] == *p);
    let children: Vec<&Value> = items
        .iter()
        .copied()
        .filter(|s| s["spec"]["parent"].as_str().filter(known) == parent)
        .collect();
    for (i, s) in children.iter().enumerate() {
        let (branch, next) = match (parent, i + 1 == children.len()) {
            (None, _) => ("", ""),
            (Some(_), true) => ("└─ ", "   "),
            (Some(_), false) => ("├─ ", "│  "),
        };
        let id = s["id"].as_str();
        let created = match s["status"]["created_at"].as_i64() {
            _ if s["status"]["phase"] != "ready" => "pending".to_string(),
            Some(t) => unix_time(t),
            None => "-".to_string(),
        };
        rows.push(vec![
            if id.is_some() && id == current {
                "*"
            } else {
                ""
            }
            .to_string(),
            format!("{prefix}{branch}{}", cell(&s["spec"]["name"])),
            created,
            cell(&s["spec"]["memory"]),
            (s["spec"]["scheduled"] == true).to_string(),
            cell(&s["spec"]["description"]),
        ]);
        snapshot_subtree(items, id, &format!("{prefix}{next}"), current, rows);
    }
}

/// 유닉스 초를 `2026-10-26 09:30 UTC`처럼
fn unix_time(secs: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(t) => format!("{} {:02}:{:02} UTC", t.date(), t.hour(), t.minute()),
        Err(_) => secs.to_string(),
    }
}

/// VM 동작을 요청하고 Task가 끝날 때까지 기다린다.
async fn act(
    client: &Client,
    vm: &str,
    action: &str,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let task = client
        .send(client.request(Method::POST, &format!("/vms/{vm}/actions/{action}")))
        .await?;
    client.wait_task(&task).await?;
    let verb = action.split('?').next().unwrap_or(action);
    writeln!(out, "vm {vm}: {verb} done")?;
    Ok(())
}

fn print_list(
    out: &mut (dyn Write + Send),
    json_out: bool,
    list: &Value,
    headers: &[&str],
    row: impl Fn(&Value) -> Vec<String>,
) -> anyhow::Result<()> {
    let items = list["items"].as_array().cloned().unwrap_or_default();
    if json_out {
        print_json(out, &Value::Array(items))?;
    } else {
        let rows: Vec<_> = items.iter().map(row).collect();
        table(out, headers, &rows)?;
    }
    Ok(())
}

/// `node list`의 열
const NODE_HEADERS: &[&str] = &[
    "NAME",
    "ID",
    "ADDRESS",
    "VERSION",
    "UNDERLAY",
    "ONLINE",
    "CPUS",
    "MEMORY",
    "MAINTENANCE",
    "WITNESS",
    "FENCING",
];

/// `node list`의 한 행
fn node_row(n: &Value) -> Vec<String> {
    vec![
        cell(&n["name"]),
        cell(&n["id"]),
        cell(&n["spec"]["address"]),
        // 옛 버전 노드는 기록하지 않는다.
        cell(&n["status"]["version"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
        cell(&n["status"]["underlay"]),
        cell(&n["status"]["online"]),
        cell(&n["status"]["cpus"]),
        bytes(&n["status"]["memory_mib"].as_u64().map(|m| m << 20).into()),
        cell(&n["spec"]["maintenance"]),
        // 옛 서버는 보내지 않는다 — 일반 노드다.
        (n["spec"]["witness"] == true).to_string(),
        // 펜싱이 꺼진 노드(옛 서버도)는 `ha`가 없다.
        cell(&n["status"]["ha"]["fencing"]),
    ]
}

/// `network list`의 열
const NETWORK_HEADERS: &[&str] = &["NAME", "TYPE", "BRIDGE", "VLAN/VNI", "MTU"];

/// VXLAN 네트워크의 기본 MTU(서버의 `DEFAULT_VXLAN_MTU`)
const DEFAULT_VXLAN_MTU: u64 = 1450;

/// `network list`의 한 행. VXLAN 네트워크의 브리지는 opld가 만드는 `oplv<vni>`다.
fn network_row(n: &Value) -> Vec<String> {
    let spec = &n["spec"];
    match spec["vxlan"]["vni"].as_u64() {
        Some(vni) => vec![
            cell(&n["name"]),
            "vxlan".into(),
            format!("oplv{vni}"),
            vni.to_string(),
            spec["mtu"]
                .as_u64()
                .unwrap_or(DEFAULT_VXLAN_MTU)
                .to_string(),
        ],
        None => vec![
            cell(&n["name"]),
            "bridge".into(),
            cell(&spec["bridge"]),
            cell(&spec["vlan"]),
            "-".into(),
        ],
    }
}

/// `cluster status`의 표와 경고
fn print_cluster_status(out: &mut (dyn Write + Send), view: &Value) -> anyhow::Result<()> {
    writeln!(out, "cluster: {}", cell(&view["name"]))?;
    writeln!(out, "CA fingerprint: {}", cell(&view["ca_fingerprint"]))?;
    let leader = &view["leader"];
    let members = view["members"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = members
        .iter()
        .map(|m| {
            vec![
                cell(&m["name"]),
                cell(&m["id"]),
                cell(&m["address"]),
                if m["voter"] == true {
                    "voter"
                } else {
                    "learner"
                }
                .into(),
                cell(&m["online"]),
                (&m["id"] == leader).to_string(),
                (m["witness"] == true).to_string(),
                cell(&m["fencing"]),
            ]
        })
        .collect();
    table(
        out,
        &[
            "NAME", "ID", "ADDRESS", "ROLE", "ONLINE", "LEADER", "WITNESS", "FENCING",
        ],
        &rows,
    )?;
    let voters: Vec<&Value> = members.iter().filter(|m| m["voter"] == true).collect();
    if voters.len() == 2 {
        writeln!(
            out,
            "warning: a two-node cluster stops accepting changes when either node fails; add a third node"
        )?;
    } else if voters.len() >= 3 && voters.iter().filter(|m| m["witness"] != true).count() < 2 {
        // 위트니스는 VM을 돌리지 않으므로 VM을 옮겨 받을 일반 노드가 둘 이상이어야 한다.
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
    // 버전을 모르는 멤버(옛 버전 노드, 합류 중)는 세지 않는다.
    let mut versions: Vec<&str> = members
        .iter()
        .filter_map(|m| m["version"].as_str())
        .collect();
    versions.sort_by(|a, b| opl_types::version::compare(a, b));
    versions.dedup_by(|a, b| opl_types::version::compare(a, b).is_eq());
    if versions.len() > 1 {
        writeln!(
            out,
            "warning: nodes run different opLite versions ({}); finish the upgrade on every node",
            versions.join(", ")
        )?;
    }
    Ok(())
}

/// 목록 응답의 항목들
fn items(list: &Value) -> Vec<Value> {
    list["items"].as_array().cloned().unwrap_or_default()
}

/// `pool list`의 열
const POOL_HEADERS: &[&str] = &["NAME", "KIND", "LOCATION", "NODES", "READY"];

/// `pool list`의 한 행. READY는 이 풀을 쓸 수 있는 노드(없는 노드 포함) 가운데 온라인이고 준비된
/// 노드 수다.
fn pool_row(pool: &Value, nodes: &[Value]) -> Vec<String> {
    let listed = pool["spec"]["nodes"].as_array().map_or(0, Vec::len);
    let usable = pool_nodes(pool, nodes);
    let ready = usable
        .iter()
        .filter(|n| n.is_ok_and(|n| node_online(n) && pool_state(pool, n)["ready"] == true))
        .count();
    vec![
        cell(&pool["name"]),
        cell(&pool["spec"]["backend"]["kind"]),
        pool_location(pool),
        if listed == 0 {
            "all".into()
        } else {
            listed.to_string()
        },
        format!("{ready}/{}", usable.len()),
    ]
}

/// 디렉터리 풀은 경로, NFS 풀은 `server:/export`(IPv6는 `[addr]:/export`), LVM-thin 풀은
/// `vg/thinpool`, ZFS 풀은 데이터셋, RBD 풀은 `pool[/namespace]`, 공유 LVM 풀은 `vg`(iSCSI면
/// `vg @ portal target`)
fn pool_location(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    if b["kind"] == "lvm-shared" {
        let iscsi = &b["iscsi"];
        if iscsi.is_object() {
            return format!(
                "{} @ {} {}",
                cell(&b["vg"]),
                cell(&iscsi["portal"]),
                cell(&iscsi["target"])
            );
        }
        return cell(&b["vg"]);
    }
    if b["kind"] == "rbd" {
        return match b["namespace"].as_str() {
            Some(namespace) => format!("{}/{namespace}", cell(&b["pool"])),
            None => cell(&b["pool"]),
        };
    }
    if let Some(vg) = b["vg"].as_str() {
        return format!("{vg}/{}", cell(&b["thinpool"]));
    }
    if let Some(dataset) = b["dataset"].as_str() {
        return dataset.into();
    }
    match b["server"].as_str() {
        Some(server) if server.contains(':') => format!("[{server}]:{}", cell(&b["export"])),
        Some(server) => format!("{server}:{}", cell(&b["export"])),
        None => cell(&b["path"]),
    }
}

/// 풀을 쓸 수 있는 노드. 노드 목록이 비었으면 위트니스가 아닌 모든 노드이고, 아니면 목록의 노드를
/// 그 순서대로 본다. 목록에 있지만 없는 노드는 그 id(`Err`)다.
fn pool_nodes<'a>(pool: &'a Value, nodes: &'a [Value]) -> Vec<Result<&'a Value, &'a Value>> {
    match pool["spec"]["nodes"].as_array() {
        Some(listed) if !listed.is_empty() => listed
            .iter()
            .map(|id| nodes.iter().find(|n| n["id"] == *id).ok_or(id))
            .collect(),
        _ => nodes
            .iter()
            .filter(|n| n["spec"]["witness"] != true)
            .map(Ok)
            .collect(),
    }
}

fn node_online(node: &Value) -> bool {
    node["status"]["online"] == true
}

/// 노드가 알린 풀 상태. 알리지 않았거나 같은 이름의 다른 풀(`pool_id`가 다름)의 상태면 `null`이다.
fn pool_state<'a>(pool: &Value, node: &'a Value) -> &'a Value {
    let name = pool["name"].as_str().unwrap_or_default();
    let state = &node["status"]["pools"][name];
    if state["pool_id"] == pool["id"] {
        state
    } else {
        &Value::Null
    }
}

/// `pool get`의 머리 줄들
fn describe_pool(pool: &Value) -> String {
    let b = &pool["spec"]["backend"];
    let mut text = format!(
        "pool: {}\nkind: {}\nlocation: {}\n",
        cell(&pool["name"]),
        cell(&b["kind"]),
        pool_location(pool)
    );
    if let Some(options) = b["options"].as_str() {
        text.push_str(&format!("options: {options}\n"));
    }
    // RBD 풀의 Ceph 사용자·conf(생략하면 Ceph 기본이라 적지 않는다)
    for key in ["user", "conf"] {
        if let Some(value) = b[key].as_str() {
            text.push_str(&format!("{key}: {value}\n"));
        }
    }
    text.push_str(&format!("shared: {}\n", cell(&pool["spec"]["shared"])));
    // 백업 풀만 적는다(옛 서버와 끈 풀은 보내지 않는다).
    if pool["spec"]["backups"] == true {
        text.push_str("backups: true\n");
    }
    text
}

/// `pool get`의 노드별 상태 열
const POOL_NODE_HEADERS: &[&str] = &["NODE", "READY", "SIZE", "AVAILABLE", "ERROR"];

/// 풀을 쓸 수 있는 노드마다 상태·용량·오류. 용량은 온라인이고 준비된 노드만 보인다.
fn pool_node_rows(pool: &Value, nodes: &[Value]) -> Vec<Vec<String>> {
    pool_nodes(pool, nodes)
        .into_iter()
        .map(|n| {
            let n = match n {
                Ok(n) => n,
                Err(id) => {
                    return vec![
                        cell(id),
                        "false".into(),
                        "-".into(),
                        "-".into(),
                        "node not found".into(),
                    ];
                }
            };
            let state = pool_state(pool, n);
            let online = node_online(n);
            let ready = online && state["ready"] == true;
            let size = |v: &Value| if ready { bytes(v) } else { "-".into() };
            let error = if !online {
                "node offline".into()
            } else if state.is_null() {
                "no status reported yet".into()
            } else {
                cell(&state["error"])
            };
            vec![
                cell(&n["name"]),
                ready.to_string(),
                size(&state["total_bytes"]),
                size(&state["available_bytes"]),
                error,
            ]
        })
        .collect()
}

/// 토큰 문자열 `opl_<id>_<secret>`의 id
fn token_id(token: &str) -> Option<uuid::Uuid> {
    let rest = token.strip_prefix("opl_")?;
    uuid::Uuid::try_parse(rest.split_once('_')?.0).ok()
}

/// `--server`가 HTTPS인가. 스킴은 대소문자를 가리지 않으므로 URL로 해석해 확인한다.
fn is_https(server: &str) -> bool {
    reqwest::Url::parse(server).is_ok_and(|u| u.scheme() == "https")
}

/// HTTPS 서버의 클러스터 CA. 처음이면 받아서 `--ca-fingerprint`와 맞는지 확인한다. 같은 서버에
/// 다시 로그인하면 저장한 CA를 그대로 쓴다. HTTP 서버면 없다.
async fn server_ca(
    server: &str,
    args: &LoginArgs,
    saved: Option<String>,
) -> anyhow::Result<Option<String>> {
    if !is_https(server) {
        return Ok(None);
    }
    if args.ca_fingerprint.is_none()
        && let Some(saved) = saved
    {
        return Ok(Some(saved));
    }
    let pem = fetch_ca(server).await?;
    let actual = fingerprint(&pem)?;
    match &args.ca_fingerprint {
        Some(expected) if expected.eq_ignore_ascii_case(&actual) => Ok(Some(pem)),
        Some(expected) => {
            bail!("the server's cluster CA does not match: expected {expected}, got {actual}")
        }
        None => bail!(
            "the server's cluster CA fingerprint is {actual}; check it (from `oplctl cluster init` \
             or `oplctl cluster status` on a node you trust) and log in again with --ca-fingerprint {actual}"
        ),
    }
}

/// 세션으로 로그인해 이 CLI가 쓸 API 토큰을 만든다. 세션은 바로 닫는다. 토큰과 서버가 정한
/// 사용자 이름(영역 사용자는 `<사용자>@<영역>`)을 돌려준다.
async fn login(
    server: &str,
    args: &LoginArgs,
    ca_pem: Option<String>,
) -> anyhow::Result<(String, String)> {
    let password = read_password(args.password_file.as_ref(), "Password: ")?;
    let anon = Client::new(server, None, ca_pem)?;
    let mut body = json!({ "username": args.user, "password": password });
    if args.realm != opl_types::realm::LOCAL_REALM {
        body["realm"] = json!(args.realm);
    }
    let resp = anon
        .http
        .post(anon.url("/auth/login"))
        .json(&body)
        .send()
        .await
        .with_context(|| format!("cannot reach {server}"))?;
    if !resp.status().is_success() {
        let body: Value = resp.json().await.unwrap_or_default();
        bail!("login failed: {}", cell(&body["message"]));
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .context("server did not return a session")?
        .to_string();
    let user: Value = resp.json().await.unwrap_or_default();
    let user = user["name"].as_str().unwrap_or(&args.user).to_string();
    let name = format!("oplctl-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let with_cookie = |r: reqwest::RequestBuilder| r.header(reqwest::header::COOKIE, &cookie);
    let created = anon
        .send(with_cookie(anon.request(Method::POST, "/tokens")).json(&json!({ "name": name })))
        .await?;
    let _ = anon
        .send(with_cookie(anon.request(Method::POST, "/auth/logout")))
        .await;
    let token = created["secret"]
        .as_str()
        .map(str::to_string)
        .context("server did not return a token")?;
    Ok((token, user))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("512M"), Ok(512 << 20));
        assert_eq!(parse_size("10g"), Ok(10 << 30));
        assert!(parse_size("ten").is_err());
        assert!(parse_size("99999999999T").is_err());
    }

    #[test]
    fn extracts_token_id() {
        let id = uuid::Uuid::now_v7();
        assert_eq!(token_id(&format!("opl_{}_abc", id.simple())), Some(id));
        assert_eq!(token_id("garbage"), None);
    }

    #[test]
    fn https_scheme_is_case_insensitive() {
        assert!(is_https("https://host:8443"));
        assert!(is_https("HTTPS://host"));
        assert!(!is_https("http://host"));
        assert!(!is_https("HTTP://host"));
    }

    #[test]
    fn parses_snapshot_and_resize_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "snapshot",
            "create",
            "web",
            "before",
            "--no-memory",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Snapshot { cmd: SnapshotCmd::Create { vm, name, description: None, no_memory: true } } }
                if vm == "web" && name == "before"
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "vm", "delete", "web", "--with-snapshots"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Delete {
                    with_snapshots: true,
                    ..
                }
            }
        ));
        let cli =
            Cli::try_parse_from(["oplctl", "volume", "resize", "root", "--size", "20G"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume { cmd: VolumeCmd::Resize { size, .. } } if size == 20 << 30
        ));
        assert!(Cli::try_parse_from(["oplctl", "volume", "resize", "root"]).is_err());
    }

    #[test]
    fn default_machine_follows_node_arch() {
        let node = |arch: Value| json!({ "status": { "arch": arch } });
        let (x86, arm, unknown) = (
            node(json!("x86_64")),
            node(json!("aarch64")),
            node(Value::Null),
        );
        // 지정 노드의 arch가 먼저다.
        let all = [x86.clone(), arm.clone()];
        assert_eq!(default_machine(Some(&arm), &all).unwrap(), Machine::Virt);
        assert_eq!(default_machine(Some(&x86), &all).unwrap(), Machine::Q35);
        // 지정 노드의 arch를 모르거나 지정하지 않았으면 arch를 아는 노드가 모두 같을 때 그것이다.
        let same = [arm.clone(), arm.clone(), unknown.clone()];
        assert_eq!(
            default_machine(Some(&unknown), &same).unwrap(),
            Machine::Virt
        );
        assert_eq!(default_machine(None, &same).unwrap(), Machine::Virt);
        assert_eq!(
            default_machine(None, std::slice::from_ref(&x86)).unwrap(),
            Machine::Q35
        );
        // 섞였거나 아는 노드가 없으면 --machine을 달라고 한다.
        for nodes in [&all[..], &[unknown.clone()][..], &[][..]] {
            let err = default_machine(None, nodes).unwrap_err().to_string();
            assert!(err.contains("--machine"), "{err}");
        }
        assert!(default_machine(Some(&unknown), &all).is_err());
    }

    #[test]
    fn parses_clone_and_schedule_commands() {
        let cli = Cli::try_parse_from([
            "oplctl",
            "vm",
            "clone",
            "web",
            "copy",
            "--snapshot",
            "base",
            "--pool",
            "fast",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Clone { vm, name, snapshot: Some(s), pool: Some(p) } }
                if vm == "web" && name == "copy" && s == "base" && p == "fast"
        ));
        let schedule = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "snapshot", "schedule", "web"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.cmd) {
                Ok(Cmd::Vm {
                    cmd:
                        VmCmd::Snapshot {
                            cmd: SnapshotCmd::Schedule(s),
                        },
                }) => Ok(s),
                Ok(_) => panic!("not a schedule command"),
                Err(e) => Err(e),
            }
        };
        let s = schedule(&["--every", "daily", "--keep", "7", "--memory"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "daily", "keep": 7, "memory": true }))
        );
        let s = schedule(&["--every", "hourly", "--keep", "3"]).unwrap();
        assert_eq!(
            schedule_value(&s),
            Some(json!({ "every": "hourly", "keep": 3, "memory": false }))
        );
        // 옵션이 없으면 보기만 한다.
        let s = schedule(&[]).unwrap();
        assert!(!s.off && schedule_value(&s).is_none());
        assert!(schedule(&["--off"]).unwrap().off);
        // 주기와 보관 개수는 함께 준다. 끄기는 다른 옵션과 함께 쓸 수 없다.
        for bad in [
            &["--every", "daily"][..],
            &["--keep", "7"],
            &["--memory"],
            &["--off", "--every", "daily", "--keep", "7"],
            &["--every", "monthly", "--keep", "7"],
        ] {
            assert!(schedule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn spec_update_keeps_the_rest_of_the_vm() {
        let vm = json!({
            "name": "web",
            "revision": 9,
            "labels": { "team": "a" },
            "spec": { "memory_mib": 256, "snapshot_schedule": null },
            "status": { "phase": "running" },
        });
        let schedule = json!({ "every": "weekly", "keep": 2, "memory": false });
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", schedule.clone()),
            json!({
                "labels": { "team": "a" },
                "spec": { "memory_mib": 256, "snapshot_schedule": schedule },
            })
        );
        // 라벨이 없는 VM은 응답에 `labels`가 없다. 끄면 null이다.
        let mut vm = vm;
        vm.as_object_mut().unwrap().remove("labels");
        vm["spec"]["snapshot_schedule"] = schedule;
        assert_eq!(
            spec_update(&vm, "snapshot_schedule", Value::Null),
            json!({ "labels": {}, "spec": { "memory_mib": 256, "snapshot_schedule": null } })
        );
        // `vm set --ha`는 ha만 바꾼다.
        assert_eq!(
            spec_update(&vm, "ha", json!(true))["spec"],
            json!({ "memory_mib": 256, "snapshot_schedule": vm["spec"]["snapshot_schedule"], "ha": true })
        );
    }

    #[test]
    fn parses_vm_ha_commands() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web", "--ha"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "create", "web"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Vm { cmd: VmCmd::Create(c) } if !c.ha));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Set(VmSet { vm, ha: true, no_ha: false }) } if vm == "web"
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "set", "web", "--no-ha"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm {
                cmd: VmCmd::Set(VmSet {
                    ha: false,
                    no_ha: true,
                    ..
                })
            }
        ));
        // 바꿀 것이 하나는 있어야 하고, 둘을 함께 줄 수 없다.
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "set", "web", "--ha", "--no-ha"]).is_err());
    }

    #[test]
    fn parses_vm_migrate() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "migrate", "web", "n2"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::Migrate { vm, node } } if vm == "web" && node == "n2"
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "migrate", "web"]).is_err());
    }

    /// 마이그레이션 중인 VM은 PHASE가 migrating이고 NODE에 대상 노드가 붙는다.
    #[test]
    fn vm_list_shows_migration_target() {
        let vm = json!({
            "name": "web",
            "spec": { "power": "running" },
            "status": {
                "phase": "migrating",
                "node": "id-a",
                "pid": 42,
                "migration": { "node": "id-b", "task": "t1" },
            },
        });
        assert_eq!(
            vm_row(&vm),
            [
                "web",
                "running",
                "migrating",
                "id-a -> id-b",
                "false",
                "42",
                "-"
            ]
        );
        // 마이그레이션이 없으면(필드 없음) 지금 노드만
        let mut vm = vm;
        vm["status"].as_object_mut().unwrap().remove("migration");
        vm["status"]["phase"] = json!("running");
        assert_eq!(
            vm_row(&vm),
            ["web", "running", "running", "id-a", "false", "42", "-"]
        );
        // HA VM
        vm["spec"]["ha"] = json!(true);
        assert_eq!(vm_row(&vm)[4], "true");
    }

    #[test]
    fn task_row_shows_where_ha_moved_the_vm_from() {
        let task = json!({
            "id": "t1",
            "spec": { "op": { "kind": "vm-ha-recover", "from": "id-a" }, "target": "vm-1", "node": "id-b" },
            "status": { "state": "succeeded", "error": null },
        });
        assert_eq!(
            task_row(&task),
            ["t1", "vm-ha-recover (from id-a)", "vm-1", "succeeded", "-"]
        );
        // 다른 종류는 이름만
        let mut task = task;
        task["spec"]["op"] = json!({ "kind": "vm-start" });
        assert_eq!(task_row(&task)[1], "vm-start");
    }

    #[test]
    fn delete_with_snapshots_turns_a_schedule_off() {
        let on = json!({ "spec": { "snapshot_schedule": { "every": "daily", "keep": 7, "memory": false } } });
        assert!(schedule_is_on(&on));
        assert!(!schedule_is_on(
            &json!({ "spec": { "snapshot_schedule": null } })
        ));
        // 예약 필드가 없는(예약을 모르는) 서버
        assert!(!schedule_is_on(&json!({ "spec": {} })));
    }

    #[test]
    fn spec_update_retries_mismatch_and_busy() {
        use std::time::Duration;
        let mut r = SpecRetries::default();
        for _ in 0..3 {
            assert_eq!(r.next("changed (revision_mismatch)"), Some(Duration::ZERO));
        }
        assert_eq!(r.next("changed (revision_mismatch)"), None);
        // 바쁘면 1초마다 30번(30초)까지 기다린다.
        for _ in 0..30 {
            assert_eq!(
                r.next("VM web is busy (vm_busy)"),
                Some(Duration::from_secs(1))
            );
        }
        assert_eq!(r.next("VM web is busy (vm_busy)"), None);
        // 다른 오류는 다시 하지 않는다.
        let mut r = SpecRetries::default();
        assert_eq!(r.next("VM web not found (not_found)"), None);
        // 실행 중인 VM의 HA 켜기는 서버의 409 문구를 그대로 보인다.
        assert_eq!(
            r.next("cannot turn on HA for VM \"web\" while it is running; turn on HA while the VM is stopped (vm_running)"),
            None
        );
    }

    #[test]
    fn describes_the_schedule_and_its_last_error() {
        let vm = |schedule: Value, error: Value| json!({ "spec": { "snapshot_schedule": schedule }, "status": { "schedule_error": error } });
        assert_eq!(
            describe_schedule(&vm(Value::Null, Value::Null)),
            "schedule: off\n"
        );
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "daily", "keep": 7, "memory": true }),
                Value::Null
            )),
            "schedule: daily, keep 7, with memory\n"
        );
        // 마지막 예약 오류가 있으면 한 줄 더
        assert_eq!(
            describe_schedule(&vm(
                json!({ "every": "hourly", "keep": 1, "memory": false }),
                json!({ "code": "vm_busy", "message": "VM web is busy" })
            )),
            "schedule: hourly, keep 1\nlast error: vm_busy: VM web is busy\n"
        );
    }

    #[test]
    fn delete_with_snapshots_checks_everything_first() {
        let vm = |power: &str, phase: &str| json!({ "spec": { "power": power }, "status": { "phase": phase } });
        let snap = |name: &str, phase: &str| json!({ "spec": { "name": name }, "status": { "phase": phase } });
        let ready = [snap("a", "ready"), snap("b", "ready")];
        for (power, phase) in [("stopped", "stopped"), ("stopped", "failed")] {
            check_deletable("web", &vm(power, phase), &ready).unwrap();
        }
        // 서버의 VM 삭제 조건과 같다. 시작·정지 중도 지울 수 없다.
        for (power, phase) in [
            ("running", "running"),
            ("running", "stopped"),
            ("stopped", "running"),
            ("stopped", "stopping"),
            ("running", "starting"),
        ] {
            let err = check_deletable("web", &vm(power, phase), &ready).unwrap_err();
            assert!(
                err.to_string().contains("is not stopped"),
                "{power} {phase}: {err}"
            );
        }
        // 만드는 중인 스냅샷이 하나라도 있으면 지우지 않는다.
        let err = check_deletable(
            "web",
            &vm("stopped", "stopped"),
            &[snap("a", "ready"), snap("b", "pending")],
        )
        .unwrap_err();
        assert!(err.to_string().contains("snapshot b of VM web"), "{err}");
    }

    #[test]
    fn snapshot_list_is_a_tree_with_the_current_position() {
        let id = |n: u8| format!("01900000-0000-7000-8000-0000000000{n:02}");
        let snap = |n: u8, name: &str, parent: Option<u8>, memory: bool, description: &str| {
            json!({
                "id": id(n),
                "spec": { "name": name, "parent": parent.map(id), "memory": memory, "description": description },
                "status": { "phase": "ready", "created_at": 1_793_000_000 + u64::from(n) * 60 },
            })
        };
        let mut pending = snap(6, "new", Some(5), false, "");
        pending["status"] = json!({ "phase": "pending" });
        let mut auto = snap(7, "auto-20261026-0740", Some(2), false, "");
        auto["spec"]["scheduled"] = json!(true);
        // 순서를 섞어도 만든 차례로 놓는다. 목록에 없는 부모(4)는 뿌리로 본다.
        let items = [
            snap(3, "c", Some(1), false, ""),
            snap(2, "b", Some(1), true, ""),
            snap(1, "base", None, false, "clean install"),
            snap(5, "orphan", Some(4), true, ""),
            auto,
            pending,
        ];
        let rows = snapshot_rows(&items, Some(&id(2)));
        let mut out = Vec::new();
        table(&mut out, SNAPSHOT_HEADERS, &rows).unwrap();
        // 예약으로 만든 스냅샷은 SCHEDULED가 true다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "   NAME                      CREATED               MEMORY  SCHEDULED  DESCRIPTION\n\
             \x20  base                      2026-10-26 07:34 UTC  false   false      clean install\n\
             *  ├─ b                      2026-10-26 07:35 UTC  true    false\n\
             \x20  │  └─ auto-20261026-0740  2026-10-26 07:40 UTC  false   true\n\
             \x20  └─ c                      2026-10-26 07:36 UTC  false   false\n\
             \x20  orphan                    2026-10-26 07:38 UTC  true    false\n\
             \x20  └─ new                    pending               false   false\n"
        );
    }

    #[test]
    fn parses_pool_set() {
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "set", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Set(s),
                } => (s.pool.clone(), pool_set_body(&s), pool_set_summary(&s)),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--shared"]).unwrap(),
            ("nas".into(), json!({ "shared": true }), "shared".into())
        );
        assert_eq!(
            set(&["--no-shared"]).unwrap(),
            (
                "nas".into(),
                json!({ "shared": false }),
                "node-local".into()
            )
        );
        // 백업 표시는 따로 바꾸거나 공유 여부와 함께 바꾼다. 주지 않은 값은 보내지 않는다.
        assert_eq!(
            set(&["--backups", "true"]).unwrap(),
            (
                "nas".into(),
                json!({ "backups": true }),
                "backups on".into()
            )
        );
        assert_eq!(
            set(&["--no-shared", "--backups", "false"]).unwrap().1,
            json!({ "shared": false, "backups": false })
        );
        assert_eq!(
            set(&["--shared", "--backups", "false"]).unwrap().2,
            "shared, backups off"
        );
        // 하나는 꼭, 공유와 노드 로컬은 함께 못 준다, 백업은 true나 false
        assert!(set(&[]).is_err());
        assert!(set(&["--shared", "--no-shared"]).is_err());
        assert!(set(&["--backups"]).is_err());
        assert!(set(&["--backups", "yes"]).is_err());
    }

    #[test]
    fn pool_create_marks_backup_pools() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "bk"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            body(&["--path", "/srv/bk", "--backups"]).unwrap()["spec"],
            json!({ "backend": { "kind": "dir", "path": "/srv/bk" }, "nodes": [], "backups": true })
        );
        assert_eq!(
            body(&["--nfs", "nas:/bk", "--backups"]).unwrap()["spec"]["backups"],
            json!(true)
        );
        // 끈 풀은 보내지 않는다.
        assert!(body(&["--path", "/srv/bk"]).unwrap()["spec"]["backups"].is_null());
        // 블록 풀은 백업을 담지 못한다.
        for args in [
            &["--lvm-thin", "vg0/tp", "--backups"][..],
            &["--zfs", "tank/opl", "--backups"],
            &["--rbd", "rbd", "--backups"],
            &["--lvm-shared", "san", "--backups"],
        ] {
            assert!(body(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn describes_a_backup_pool() {
        let pool = json!({
            "name": "bk",
            "spec": { "backend": { "kind": "dir", "path": "/srv/bk" }, "backups": true }
        });
        assert_eq!(
            describe_pool(&pool),
            "pool: bk\nkind: dir\nlocation: /srv/bk\nshared: -\nbackups: true\n"
        );
    }

    #[test]
    fn parses_backup_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Backup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk"]).unwrap(),
            BackupCmd::Create { vm, pool, name: None } if vm == "web" && pool == "bk"
        ));
        assert!(matches!(
            parse(&["create", "web", "--pool", "bk", "--name", "before-upgrade"]).unwrap(),
            BackupCmd::Create { name: Some(n), .. } if n == "before-upgrade"
        ));
        assert!(matches!(
            parse(&["list", "--vm", "web"]).unwrap(),
            BackupCmd::List { vm: Some(v) } if v == "web"
        ));
        assert!(matches!(
            parse(&["restore", "web-1", "--name", "web2", "--pool", "local", "--node", "n1"])
                .unwrap(),
            BackupCmd::Restore { backup, name, pool, node: Some(node) }
                if backup == "web-1" && name == "web2" && pool == "local" && node == "n1"
        ));
        // 백업 풀과 복원의 새 이름·풀은 꼭 준다.
        for args in [
            &["create", "web"][..],
            &["restore", "web-1", "--pool", "local"],
            &["restore", "web-1", "--name", "web2"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_list_shows_the_vm_name_and_stored_size() {
        let ready = json!({
            "name": "web-20261104-030000",
            "spec": { "vm": "x", "vm_name": "web", "pool": "bk" },
            "status": {
                "phase": "ready", "created_at": 1_793_761_200,
                "disks": [
                    { "name": "disk0", "stored_bytes": 3u64 << 30 },
                    { "name": "disk1", "stored_bytes": 1u64 << 29 }
                ]
            }
        });
        assert_eq!(
            backup_row(&ready),
            [
                "web-20261104-030000",
                "web",
                "bk",
                "ready",
                "2026-11-04 03:00 UTC",
                "3.5GiB"
            ]
        );
        // 만드는 중이거나 실패한 백업은 크기가 없다.
        let pending = json!({
            "name": "b", "spec": { "vm_name": "web", "pool": "bk" },
            "status": { "phase": "pending", "created_at": null, "disks": [] }
        });
        assert_eq!(backup_row(&pending)[3..], ["pending", "-", "-"]);
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
    }

    #[test]
    fn parses_pool_create_commands() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "nas"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.path, c.nfs, c.nfs_options, c.shared),
                _ => unreachable!(),
            })
        };
        let nfs = |server: &str, export: &str| NfsSource {
            server: server.into(),
            export: export.into(),
        };
        assert_eq!(
            create(&["--path", "/srv/pool"]).unwrap(),
            (Some(PathBuf::from("/srv/pool")), None, None, false)
        );
        // 디렉터리 풀은 공유 스토리지(밖에서 마운트한 공유 디렉터리)라고 적을 수 있다.
        assert!(create(&["--path", "/mnt/shared", "--shared"]).unwrap().3);
        assert_eq!(
            create(&["--nfs", "nas.lan:/srv/vm", "--nfs-options", "vers=4.2,hard"]).unwrap(),
            (
                None,
                Some(nfs("nas.lan", "/srv/vm")),
                Some("vers=4.2,hard".into()),
                false
            )
        );
        // IPv6는 대괄호로 감싼다.
        assert_eq!(
            create(&["--nfs", "[fd00::1]:/srv/vm"]).unwrap().1,
            Some(nfs("fd00::1", "/srv/vm"))
        );
        // 둘 중 하나만, 옵션은 --nfs와 함께, 내보내기는 절대 경로, NFS 풀은 늘 공유라 --shared가 없다
        for args in [
            &[][..],
            &["--nfs", "nas:/srv/vm", "--shared"],
            &["--shared"],
            &["--path", "/srv/pool", "--nfs", "nas:/srv/vm"],
            &["--path", "/srv/pool", "--nfs-options", "hard"],
            &["--nfs", "nas"],
            &["--nfs", "nas:srv"],
            &["--nfs", ":/srv"],
            &["--nfs", "fd00::1:/srv"],
            &["--nfs", "[fd00::1/srv"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 브리지와 VXLAN 중 정확히 하나. VLAN은 브리지와, MTU는 VXLAN과만. 주지 않은 값은 보내지 않는다.
    #[test]
    fn parses_network_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "network", "create", "lan"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Network {
                    cmd: NetworkCmd::Create(c),
                } => network_create_body(&c),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--bridge", "vmbr0"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } })
        );
        assert_eq!(
            create(&["--bridge", "vmbr0", "--vlan", "10"]).unwrap(),
            json!({ "name": "lan", "spec": { "bridge": "vmbr0", "vlan": 10 } })
        );
        assert_eq!(
            create(&["--vxlan", "100"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 } } })
        );
        assert_eq!(
            create(&["--vxlan", "100", "--mtu", "1400"]).unwrap(),
            json!({ "name": "lan", "spec": { "vxlan": { "vni": 100 }, "mtu": 1400 } })
        );
        for args in [
            &[][..],
            &["--bridge", "vmbr0", "--vxlan", "100"],
            &["--vxlan", "100", "--vlan", "10"],
            &["--bridge", "vmbr0", "--mtu", "1400"],
            &["--vlan", "10"],
            &["--mtu", "1400"],
            &["--vxlan", "-1"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 종류, 붙는 브리지(VXLAN은 관리 이름), VLAN이나 VNI, VXLAN MTU(없으면 기본값)
    #[test]
    fn network_list_shows_kind_bridge_and_mtu() {
        let rows: Vec<_> = [
            json!({ "name": "lan", "spec": { "bridge": "vmbr0" } }),
            json!({ "name": "dmz", "spec": { "bridge": "vmbr0", "vlan": 20 } }),
            json!({ "name": "ov", "spec": { "vxlan": { "vni": 100 } } }),
            json!({ "name": "jumbo", "spec": { "vxlan": { "vni": 7 }, "mtu": 8950 } }),
        ]
        .iter()
        .map(network_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, NETWORK_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   TYPE    BRIDGE   VLAN/VNI  MTU\n\
             lan    bridge  vmbr0    -         -\n\
             dmz    bridge  vmbr0    20        -\n\
             ov     vxlan   oplv100  100       1450\n\
             jumbo  vxlan   oplv7    7         8950\n"
        );
    }

    #[test]
    fn parses_lvm_thin_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.lvm_thin,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-thin", "vg0/tp"]).unwrap(),
            Some(LvmThinSource {
                vg: "vg0".into(),
                thinpool: "tp".into()
            })
        );
        // 다른 종류·--shared와 함께 못 쓰고, VG와 thin pool이 모두 있어야 한다
        for args in [
            &["--lvm-thin", "vg0/tp", "--path", "/srv/pool"][..],
            &["--lvm-thin", "vg0/tp", "--nfs", "nas:/srv/vm"],
            &["--lvm-thin", "vg0/tp", "--shared"],
            &["--lvm-thin", "vg0/tp", "--nfs-options", "hard"],
            &["--lvm-thin", "vg0"],
            &["--lvm-thin", "/tp"],
            &["--lvm-thin", "vg0/"],
            &["--lvm-thin", "vg0/tp/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
        // 종류(--path, --nfs, --lvm-thin, --zfs, --rbd, --lvm-shared) 중 하나가 있어야 한다.
        let err = create(&[]).expect_err("a pool kind is required");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        let msg = err.to_string();
        for kind in [
            "--path",
            "--nfs",
            "--lvm-thin",
            "--zfs",
            "--rbd",
            "--lvm-shared",
        ] {
            assert!(msg.contains(kind), "{msg}");
        }
        assert!(create(&["--node", "a"]).is_err());
        assert!(create(&["--path", "/srv/p"]).is_ok());
        assert!(create(&["--nfs", "nas:/srv/vm"]).is_ok());
    }

    #[test]
    fn parses_zfs_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "fast"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => c.zfs,
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--zfs", "tank/opl"]).unwrap(),
            Some("tank/opl".to_string())
        );
        assert_eq!(
            create(&["--zfs", "tank"]).unwrap(),
            Some("tank".to_string())
        );
        // 다른 종류·--shared와 함께 못 쓴다. 데이터셋 이름의 검사는 API가 한다.
        for args in [
            &["--zfs", "tank/opl", "--path", "/srv/pool"][..],
            &["--zfs", "tank/opl", "--nfs", "nas:/srv/vm"],
            &["--zfs", "tank/opl", "--nfs-options", "hard"],
            &["--zfs", "tank/opl", "--lvm-thin", "vg0/tp"],
            &["--zfs", "tank/opl", "--shared"],
            &["--zfs"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn parses_rbd_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.rbd, c.rbd_user, c.rbd_conf),
                _ => unreachable!(),
            })
        };
        let rbd = |pool: &str, namespace: Option<&str>| RbdSource {
            pool: pool.into(),
            namespace: namespace.map(Into::into),
        };
        assert_eq!(
            create(&["--rbd", "rbd"]).unwrap(),
            (Some(rbd("rbd", None)), None, None)
        );
        assert_eq!(
            create(&[
                "--rbd",
                "rbd/opl",
                "--rbd-user",
                "opl",
                "--rbd-conf",
                "/etc/ceph/opl.conf"
            ])
            .unwrap(),
            (
                Some(rbd("rbd", Some("opl"))),
                Some("opl".into()),
                Some("/etc/ceph/opl.conf".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 사용자·conf는 --rbd와 함께만 쓴다.
        // 이름 규칙의 나머지 검사는 API가 한다.
        for args in [
            &["--rbd", "rbd", "--path", "/srv/pool"][..],
            &["--rbd", "rbd", "--nfs", "nas:/srv/vm"],
            &["--rbd", "rbd", "--nfs-options", "hard"],
            &["--rbd", "rbd", "--lvm-thin", "vg0/tp"],
            &["--rbd", "rbd", "--zfs", "tank/opl"],
            &["--rbd", "rbd", "--shared"],
            &["--path", "/srv/pool", "--rbd-user", "opl"],
            &["--path", "/srv/pool", "--rbd-conf", "/etc/ceph/ceph.conf"],
            &["--rbd"],
            &["--rbd", ""],
            &["--rbd", "/opl"],
            &["--rbd", "rbd/"],
            &["--rbd", "rbd/opl/x"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// `pool create`가 보내는 요청 본문. RBD 풀은 네임스페이스·사용자·conf를 담고 `shared`는 적지
    /// 않는다.
    #[test]
    fn pool_create_sends_the_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "ceph"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        let args = [
            "--rbd",
            "rbd/opl",
            "--rbd-user",
            "opl",
            "--rbd-conf",
            "/etc/ceph/x.conf",
        ];
        assert_eq!(
            body(&args),
            json!({
                "name": "ceph",
                "spec": {
                    "backend": {
                        "kind": "rbd", "pool": "rbd", "namespace": "opl",
                        "user": "opl", "conf": "/etc/ceph/x.conf"
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--rbd", "rbd", "--node", "n1"])["spec"],
            json!({
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": null, "user": null, "conf": null
                },
                "nodes": ["n1"]
            })
        );
    }

    #[test]
    fn parses_lvm_shared_pool_create() {
        let create = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => (c.lvm_shared, c.iscsi_portal, c.iscsi_target),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            create(&["--lvm-shared", "san"]).unwrap(),
            (Some("san".into()), None, None)
        );
        assert_eq!(
            create(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "10.0.0.5:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ])
            .unwrap(),
            (
                Some("san".into()),
                Some("10.0.0.5:3260".into()),
                Some("iqn.2003-01.org.example:san".into())
            )
        );
        // 다른 종류·--shared(늘 공유)와 함께 못 쓰고, 포털·타깃은 --lvm-shared와 함께, 둘이 함께
        // 쓴다. 이름 규칙의 검사는 API가 한다.
        for args in [
            &["--lvm-shared", "san", "--path", "/srv/pool"][..],
            &["--lvm-shared", "san", "--nfs", "nas:/srv/vm"],
            &["--lvm-shared", "san", "--nfs-options", "hard"],
            &["--lvm-shared", "san", "--lvm-thin", "vg0/tp"],
            &["--lvm-shared", "san", "--zfs", "tank/opl"],
            &["--lvm-shared", "san", "--rbd", "rbd"],
            &["--lvm-shared", "san", "--rbd-user", "opl"],
            &["--lvm-shared", "san", "--shared"],
            &["--lvm-shared", "san", "--iscsi-portal", "nas"],
            &[
                "--lvm-shared",
                "san",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &[
                "--path",
                "/srv/pool",
                "--iscsi-portal",
                "nas",
                "--iscsi-target",
                "iqn.2003-01.org.example:san",
            ],
            &["--lvm-shared"],
        ] {
            assert!(create(args).is_err(), "{args:?}");
        }
    }

    /// 공유 LVM 풀은 VG와 iSCSI 타깃(없으면 null)을 담고 `shared`는 적지 않는다.
    #[test]
    fn pool_create_sends_the_lvm_shared_backend() {
        let body = |args: &[&str]| {
            let mut argv = vec!["oplctl", "pool", "create", "san"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Pool {
                    cmd: PoolCmd::Create(c),
                } => pool_create_body(*c),
                _ => unreachable!(),
            }
        };
        assert_eq!(
            body(&[
                "--lvm-shared",
                "san",
                "--iscsi-portal",
                "[fd00::5]:3260",
                "--iscsi-target",
                "iqn.2003-01.org.example:san"
            ]),
            json!({
                "name": "san",
                "spec": {
                    "backend": {
                        "kind": "lvm-shared", "vg": "san",
                        "iscsi": { "portal": "[fd00::5]:3260", "target": "iqn.2003-01.org.example:san" }
                    },
                    "nodes": []
                }
            })
        );
        assert_eq!(
            body(&["--lvm-shared", "san", "--node", "n1"])["spec"],
            json!({
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": ["n1"]
            })
        );
    }

    /// 받은 요청 줄을 `seen`에 남기고 경로에 맞는 JSON을 200으로 돌려주는 작은 HTTP 서버
    async fn fake_api(
        reply: fn(&str) -> Value,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        fake_api_status(move |path| (200, reply(path))).await
    }

    /// `fake_api`와 같고 응답 상태 코드도 정한다.
    async fn fake_api_status(
        reply: impl Fn(&str) -> (u16, Value) + Send + 'static,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0; 4096];
                let n = sock.read(&mut buf).await.unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(line);
                let (status, body) = reply(&path);
                let body = body.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        (client, seen)
    }

    /// `backup delete --force`는 `?force=true`로 보내고, 기록만 지웠으면 서버의 알림을 보인다.
    /// 아니면 Task를 기다린다.
    #[tokio::test]
    async fn backup_delete_force_shows_the_server_message() {
        let cli = Cli::try_parse_from(["oplctl", "backup", "delete", "b1", "--force"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Backup {
                cmd: BackupCmd::Delete { force: true, .. }
            }
        ));

        let (client, seen) =
            fake_api(|_| json!({ "message": "backup \"b1\" deleted; its files are left" })).await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", true, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/backups/b1?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup \"b1\" deleted; its files are left\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        backup_delete(&client, "b1", false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/backups/b1 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
        );
    }

    /// 풀을 읽지 못하면 --format을 주라고 알린다. 주면 풀을 읽지 않는다.
    #[tokio::test]
    async fn volume_format_explains_an_unreadable_pool() {
        // 아무도 듣지 않는 포트
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new(&format!("http://127.0.0.1:{port}"), None, None).unwrap();
        let err = volume_format(&client, "fast", None).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.starts_with(
                "cannot read storage pool \"fast\" to choose a volume format; pass --format: "
            ),
            "{msg}"
        );
        let format = volume_format(&client, "fast", Some(Format::Raw)).await;
        assert_eq!(format.unwrap(), Format::Raw);
    }

    #[test]
    fn volume_format_defaults_to_pool_kind() {
        let pool = |kind: &str| json!({ "spec": { "backend": { "kind": kind } } });
        assert_eq!(pool_default_format(&pool("lvm-thin")), Format::Raw);
        assert_eq!(pool_default_format(&pool("zfs")), Format::Raw);
        assert_eq!(pool_default_format(&pool("rbd")), Format::Raw);
        assert_eq!(pool_default_format(&pool("lvm-shared")), Format::Raw);
        assert_eq!(pool_default_format(&pool("dir")), Format::Qcow2);
        assert_eq!(pool_default_format(&pool("nfs")), Format::Qcow2);
        // --format를 주지 않으면 풀을 읽어 정하고, 주면 그대로 쓴다.
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "create", "root", "--pool", "fast", "--size", "1G",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Create { format: None, .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "oplctl", "volume", "import", "root", "--pool", "fast", "--file", "a.vmdk", "--format",
            "raw",
        ])
        .unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Volume {
                cmd: VolumeCmd::Import {
                    format: Some(Format::Raw),
                    ..
                }
            }
        ));
    }

    #[test]
    fn pool_list_and_details_show_each_node() {
        let node = |name: &str, online: bool, pools: Value| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "status": { "online": online, "pools": pools },
            })
        };
        let gib = 1u64 << 30;
        let state = |ready: bool, error: Value, total: u64, available: u64| json!({ "pool_id": "id-nas", "ready": ready, "error": error, "total_bytes": total, "available_bytes": available });
        let nodes = [
            node(
                "a",
                true,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            node(
                "b",
                true,
                json!({ "nas": state(false, json!("mount failed"), 0, 0) }),
            ),
            node("c", true, json!({})),
            // 준비됐다고 알렸지만 꺼진 노드는 준비로 세지 않는다.
            node(
                "d",
                false,
                json!({ "nas": state(true, Value::Null, 100 * gib, 40 * gib) }),
            ),
            // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
            node(
                "e",
                true,
                json!({ "nas": { "pool_id": "id-old", "ready": true, "error": null } }),
            ),
            // 위트니스는 풀을 쓰지 않으므로 "모든 노드"에 들지 않는다.
            json!({
                "id": "id-w",
                "name": "w",
                "spec": { "witness": true },
                "status": { "online": true, "pools": {} },
            }),
        ];
        let nas = json!({
            "id": "id-nas",
            "name": "nas",
            "spec": {
                "backend": { "kind": "nfs", "server": "fd00::1", "export": "/srv/vm", "options": "hard" },
                "nodes": [],
                "shared": true,
            },
        });
        // 노드 목록의 노드를 그 순서대로 본다. 없는 노드도 센다.
        let local = json!({
            "id": "id-local",
            "name": "local",
            "spec": {
                "backend": { "kind": "dir", "path": "/srv/pool" },
                "nodes": ["id-gone", "id-b"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&nas, &nodes),
            ["nas", "nfs", "[fd00::1]:/srv/vm", "all", "1/5"]
        );
        assert_eq!(
            pool_row(&local, &nodes),
            ["local", "dir", "/srv/pool", "2", "0/2"]
        );

        let mut out = describe_pool(&nas).into_bytes();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&nas, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "pool: nas\n\
             kind: nfs\n\
             location: [fd00::1]:/srv/vm\n\
             options: hard\n\
             shared: true\n\
             NODE  READY  SIZE      AVAILABLE  ERROR\n\
             a     true   100.0GiB  40.0GiB    -\n\
             b     false  -         -          mount failed\n\
             c     false  -         -          no status reported yet\n\
             d     false  -         -          node offline\n\
             e     false  -         -          no status reported yet\n"
        );
        assert_eq!(
            describe_pool(&local),
            "pool: local\nkind: dir\nlocation: /srv/pool\nshared: false\n"
        );
        let fast = json!({
            "id": "id-fast",
            "name": "fast",
            "spec": {
                "backend": { "kind": "lvm-thin", "vg": "vg0", "thinpool": "tp" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&fast, &nodes),
            ["fast", "lvm-thin", "vg0/tp", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&fast),
            "pool: fast\nkind: lvm-thin\nlocation: vg0/tp\nshared: false\n"
        );
        let tank = json!({
            "id": "id-tank",
            "name": "tank",
            "spec": {
                "backend": { "kind": "zfs", "dataset": "tank/opl" },
                "nodes": ["id-a"],
                "shared": false,
            },
        });
        assert_eq!(
            pool_row(&tank, &nodes),
            ["tank", "zfs", "tank/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&tank),
            "pool: tank\nkind: zfs\nlocation: tank/opl\nshared: false\n"
        );
        // RBD 풀의 위치는 `POOL[/NAMESPACE]`, 상세는 사용자·conf가 있으면 보인다.
        let ceph = json!({
            "id": "id-ceph",
            "name": "ceph",
            "spec": {
                "backend": { "kind": "rbd", "pool": "rbd" },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&ceph, &nodes),
            ["ceph", "rbd", "rbd", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&ceph),
            "pool: ceph\nkind: rbd\nlocation: rbd\nshared: true\n"
        );
        let opl = json!({
            "id": "id-opl",
            "name": "opl",
            "spec": {
                "backend": {
                    "kind": "rbd", "pool": "rbd", "namespace": "opl", "user": "opl",
                    "conf": "/etc/ceph/opl.conf"
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&opl, &nodes),
            ["opl", "rbd", "rbd/opl", "1", "0/1"]
        );
        assert_eq!(
            describe_pool(&opl),
            "pool: opl\nkind: rbd\nlocation: rbd/opl\nuser: opl\nconf: /etc/ceph/opl.conf\n\
             shared: true\n"
        );
        // 공유 LVM 풀의 위치는 `VG`, iSCSI면 `VG @ PORTAL TARGET`
        let fc = json!({
            "id": "id-fc",
            "name": "fc",
            "spec": {
                "backend": { "kind": "lvm-shared", "vg": "san", "iscsi": null },
                "nodes": [],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&fc, &nodes),
            ["fc", "lvm-shared", "san", "all", "0/5"]
        );
        assert_eq!(
            describe_pool(&fc),
            "pool: fc\nkind: lvm-shared\nlocation: san\nshared: true\n"
        );
        let san = json!({
            "id": "id-san",
            "name": "san",
            "spec": {
                "backend": {
                    "kind": "lvm-shared", "vg": "san",
                    "iscsi": { "portal": "10.0.0.5:3260", "target": "iqn.2003-01.org.example:san" }
                },
                "nodes": ["id-a"],
                "shared": true,
            },
        });
        assert_eq!(
            pool_row(&san, &nodes),
            [
                "san",
                "lvm-shared",
                "san @ 10.0.0.5:3260 iqn.2003-01.org.example:san",
                "1",
                "0/1"
            ]
        );
        assert_eq!(
            describe_pool(&san),
            "pool: san\nkind: lvm-shared\nlocation: san @ 10.0.0.5:3260 \
             iqn.2003-01.org.example:san\nshared: true\n"
        );
        let mut out = Vec::new();
        table(&mut out, POOL_NODE_HEADERS, &pool_node_rows(&local, &nodes)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NODE     READY  SIZE  AVAILABLE  ERROR\n\
             id-gone  false  -     -          node not found\n\
             b        false  -     -          no status reported yet\n"
        );
    }

    #[test]
    fn node_list_shows_witnesses_and_fencing() {
        let node = |name: &str, witness: Option<bool>| {
            let mut spec = json!({ "address": "10.0.0.1:8006", "maintenance": false });
            if let Some(w) = witness {
                spec["witness"] = json!(w);
            }
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "spec": spec,
                "status": { "online": true, "cpus": 4, "memory_mib": 1024 },
            })
        };
        let mut a = node("a", Some(false));
        a["status"]["ha"] = json!({ "fencing": "watchdog", "recover_after_secs": 65 });
        a["status"]["underlay"] = json!("10.0.0.1");
        a["status"]["version"] = json!("0.2.0");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
            .iter()
            .map(node_row)
            .collect();
        let mut out = Vec::new();
        table(&mut out, NODE_HEADERS, &rows).unwrap();
        // 옛 서버는 witness를 보내지 않는다 — 일반 노드다. 펜싱이 꺼진 노드는 `ha`가 없다.
        // underlay가 없는 노드(위트니스, 옛 서버)는 `-`다.
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        VERSION  UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  0.2.0    10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -        -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -        -         true    4     1.0GiB  false        false    -\n"
        );
    }

    #[test]
    fn cluster_status_shows_witnesses_and_warns() {
        let member = |name: &str, voter: bool, witness: bool| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": voter,
                "online": true,
                "witness": witness,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        // 일반 노드 둘과 위트니스면 투표 멤버가 셋이라 경고가 없다. 펜싱이 꺼졌으면(null) 또는 옛
        // 서버라 `fencing`이 없으면 `-`다.
        let mut a = member("a", true, false);
        a["fencing"] = json!("process");
        let mut b = member("b", true, false);
        b["fencing"] = Value::Null;
        assert_eq!(
            status(vec![a, b, member("w", true, true)]),
            "cluster: lab\nCA fingerprint: ab:cd\n\
             NAME  ID    ADDRESS  ROLE   ONLINE  LEADER  WITNESS  FENCING\n\
             a     id-a  a:8006   voter  true    true    false    process\n\
             b     id-b  b:8006   voter  true    false   false    -\n\
             w     id-w  w:8006   voter  true    false   true     -\n"
        );
        const TWO: &str = "warning: a two-node cluster";
        const HA: &str = "warning: HA needs at least two non-witness nodes";
        // 투표 멤버 둘이면 위트니스가 있어도 2노드 경고다.
        let out = status(vec![member("a", true, false), member("w", true, true)]);
        assert!(out.contains(TWO) && !out.contains(HA), "{out}");
        // 투표 멤버가 셋 이상이어도 일반 노드가 하나뿐이면 HA 경고다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
        ]);
        assert!(out.contains(HA) && !out.contains(TWO), "{out}");
        // learner는 투표 멤버로 세지 않는다.
        let out = status(vec![
            member("a", true, false),
            member("w", true, true),
            member("v", true, true),
            member("b", false, false),
        ]);
        assert!(out.contains(HA), "{out}");
        // 옛 서버는 witness를 보내지 않는다 — 모두 일반 노드로 본다.
        let mut old = member("c", true, false);
        old.as_object_mut().unwrap().remove("witness");
        let out = status(vec![
            member("a", true, false),
            member("b", true, false),
            old,
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 버전을 아는 멤버의 버전이 둘 이상이면 낮은 것부터 한 번씩 적어 경고한다. 모르는 멤버는 세지 않는다.
    #[test]
    fn cluster_status_warns_about_mixed_versions() {
        let member = |name: &str, version: Option<&str>| {
            json!({
                "id": format!("id-{name}"),
                "name": name,
                "address": format!("{name}:8006"),
                "cluster_address": format!("{name}:8007"),
                "voter": true,
                "online": true,
                "witness": false,
                "version": version,
            })
        };
        let status = |members: Vec<Value>| {
            let view = json!({
                "name": "lab",
                "ca_fingerprint": "ab:cd",
                "leader": "id-a",
                "node": "id-a",
                "members": members,
            });
            let mut out = Vec::new();
            print_cluster_status(&mut out, &view).unwrap();
            String::from_utf8(out).unwrap()
        };
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.9.0")),
            member("c", Some("0.10.0")),
            member("d", Some("0.10.0~rc1")),
            member("e", None),
        ]);
        assert!(
            out.ends_with(
                "warning: nodes run different opLite versions (0.9.0, 0.10.0~rc1, 0.10.0); \
                 finish the upgrade on every node\n"
            ),
            "{out}"
        );
        let out = status(vec![
            member("a", Some("0.10.0")),
            member("b", Some("0.10.0")),
            member("c", None),
        ]);
        assert!(!out.contains("warning"), "{out}");
    }

    /// 방향과 동작은 꼭, 프로토콜·포트·CIDR은 이 차례로 생략할 수 있다.
    #[test]
    fn parses_rules() {
        assert_eq!(
            parse_rule("in accept tcp 22 10.0.0.0/8"),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "tcp",
                "ports": "22", "remote": "10.0.0.0/8"
            }))
        );
        assert_eq!(
            parse_rule("out drop tcp 25"),
            Ok(json!({ "direction": "out", "action": "drop", "protocol": "tcp", "ports": "25" }))
        );
        assert_eq!(
            parse_rule("in accept icmp"),
            Ok(json!({ "direction": "in", "action": "accept", "protocol": "icmp" }))
        );
        assert_eq!(
            parse_rule("  in   accept udp 8000-8080 "),
            Ok(json!({
                "direction": "in", "action": "accept", "protocol": "udp", "ports": "8000-8080"
            }))
        );
        assert_eq!(
            parse_rule("out accept 2001:db8::/32"),
            Ok(json!({ "direction": "out", "action": "accept", "remote": "2001:db8::/32" }))
        );
        assert_eq!(
            parse_rule("in drop"),
            Ok(json!({ "direction": "in", "action": "drop" }))
        );
        // 포트 범위와 프로토콜·포트의 짝은 서버가 본다.
        assert_eq!(
            parse_rule("in accept 99999"),
            Ok(json!({ "direction": "in", "action": "accept", "ports": "99999" }))
        );
        for bad in [
            "",
            "in",
            "accept in",
            "up accept",
            "in allow",
            "in accept sctp",
            "in accept tcp 22 10.0.0.0/8 extra",
            "in accept 10.0.0.0/8 tcp",
            "in accept tcp 10.0.0.0/8 22",
            "in accept tcp ssh",
            "in accept tcp 22-",
            "in accept tcp -22",
            "IN accept",
        ] {
            let err = parse_rule(bad).unwrap_err();
            assert!(err.contains("in|out accept|drop"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parses_security_group_commands() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["oplctl", "security-group"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::SecurityGroup { cmd } => cmd,
                _ => unreachable!(),
            })
        };
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&[
            "create",
            "web",
            "--description",
            "web servers",
            "--rule",
            "in accept tcp 443",
            "--rule",
            "in accept icmp",
        ])
        .unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [
                    { "direction": "in", "action": "accept", "protocol": "tcp", "ports": "443" },
                    { "direction": "in", "action": "accept", "protocol": "icmp" },
                ]
            }})
        );
        // 설명이 없으면 보내지 않는다. 규칙 없는 그룹도 된다.
        let SecurityGroupCmd::Create {
            name,
            description,
            rules,
        } = parse(&["create", "empty"]).unwrap()
        else {
            panic!("not create")
        };
        assert_eq!(
            security_group_create_body(&name, description.as_deref(), &rules),
            json!({ "name": "empty", "spec": { "rules": [] } })
        );
        assert!(parse(&["create", "web", "--rule", "in maybe"]).is_err());
        assert!(matches!(
            parse(&["rule", "add", "web", "out drop tcp 25", "--at", "2"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Add { group, rule, at: Some(2) } }
                if group == "web" && rule["ports"] == "25"
        ));
        assert!(matches!(
            parse(&["rule", "rm", "web", "3"]).unwrap(),
            SecurityGroupCmd::Rule { cmd: RuleCmd::Rm { group, n: 3 } } if group == "web"
        ));
        assert!(parse(&["rule", "add", "web", "sideways accept"]).is_err());
        assert!(parse(&["rule", "rm", "web", "first"]).is_err());
        assert!(matches!(
            parse(&["get", "web"]).unwrap(),
            SecurityGroupCmd::Get { group } if group == "web"
        ));
        assert!(matches!(
            parse(&["delete", "web"]).unwrap(),
            SecurityGroupCmd::Delete { group } if group == "web"
        ));
    }

    /// 규칙 번호는 1부터다. 범위 밖이면 아무것도 바꾸지 않고 실패한다.
    #[test]
    fn adds_and_removes_rules_by_number() {
        let rule =
            |n: u16| json!({ "direction": "in", "action": "accept", "ports": n.to_string() });
        let group = json!({ "name": "web", "spec": { "rules": [rule(1), rule(2)] } });
        assert_eq!(
            rules_with(&group, rule(9), None).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(1)).unwrap(),
            vec![rule(9), rule(1), rule(2)]
        );
        assert_eq!(
            rules_with(&group, rule(9), Some(3)).unwrap(),
            vec![rule(1), rule(2), rule(9)]
        );
        for at in [0, 4] {
            let err = rules_with(&group, rule(9), Some(at))
                .unwrap_err()
                .to_string();
            assert!(err.contains("use 1 to 3"), "{err}");
        }
        assert_eq!(rules_without(&group, 1).unwrap(), vec![rule(2)]);
        assert_eq!(rules_without(&group, 2).unwrap(), vec![rule(1)]);
        for n in [0, 3] {
            let err = rules_without(&group, n).unwrap_err().to_string();
            assert!(err.contains(&format!("has no rule {n}")), "{err}");
        }
        // 규칙이 없는 그룹은 응답에 rules가 비어 있다.
        let empty = json!({ "name": "e", "spec": { "rules": [] } });
        assert_eq!(rules_with(&empty, rule(1), None).unwrap(), vec![rule(1)]);
        assert!(rules_without(&empty, 1).is_err());
    }

    #[test]
    fn security_group_list_shows_rule_counts() {
        let rows: Vec<_> = [
            json!({ "name": "web", "spec": {
                "description": "web servers",
                "rules": [{ "direction": "in", "action": "accept" }, { "direction": "out", "action": "drop" }]
            }}),
            json!({ "name": "empty", "spec": { "rules": [] } }),
        ]
        .iter()
        .map(security_group_row)
        .collect();
        let mut out = Vec::new();
        table(&mut out, SECURITY_GROUP_HEADERS, &rows).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME   RULES  DESCRIPTION\n\
             web    2      web servers\n\
             empty  0      -\n"
        );
    }

    /// `--nic NETWORK[,sg=A+B]`. 그룹이 없으면 `security_groups`를 보내지 않는다.
    #[test]
    fn parses_nics_with_security_groups() {
        let nics = |args: &[&str]| {
            let mut argv = vec!["oplctl", "vm", "create", "web"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::Vm {
                    cmd: VmCmd::Create(c),
                } => c.nics.iter().map(nic_body).collect::<Vec<_>>(),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            nics(&[
                "--nic",
                "user",
                "--nic",
                "lan,sg=web+db",
                "--nic",
                "dmz,sg=web"
            ])
            .unwrap(),
            vec![
                json!({ "network": "user", "model": "virtio" }),
                json!({ "network": "lan", "model": "virtio", "security_groups": ["web", "db"] }),
                json!({ "network": "dmz", "model": "virtio", "security_groups": ["web"] }),
            ]
        );
        for bad in [
            "",
            ",sg=web",
            "lan,",
            "lan,sg=",
            "lan,sg=web+",
            "lan,sg=+web",
            "lan,group=web",
            "lan,sg=web,sg=db",
        ] {
            assert!(nics(&["--nic", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_vm_nic_sg() {
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "1", "a", "b"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { vm, index: 1, groups } }
                if vm == "web" && groups == ["a", "b"]
        ));
        let cli = Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "0"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Vm { cmd: VmCmd::NicSg { index: 0, groups, .. } } if groups.is_empty()
        ));
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web"]).is_err());
        assert!(Cli::try_parse_from(["oplctl", "vm", "nic-sg", "web", "-1"]).is_err());
    }

    /// 그 NIC의 그룹만 바꾸고 나머지(다른 NIC, 다른 필드, 라벨)는 그대로 둔다.
    #[test]
    fn nic_groups_update_changes_one_nic() {
        let vm = json!({
            "name": "web",
            "revision": 4,
            "labels": { "team": "a" },
            "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["old"] },
                ],
            },
        });
        let groups = ["web".to_string(), "db".to_string()];
        assert_eq!(
            nic_groups_update(&vm, 1, &groups).unwrap(),
            json!({ "labels": { "team": "a" }, "spec": {
                "memory_mib": 256,
                "nics": [
                    { "network": "user", "model": "virtio" },
                    { "network": "lan", "model": "virtio", "mac": "52:54:00:00:00:01", "security_groups": ["web", "db"] },
                ],
            }})
        );
        assert_eq!(
            nic_groups_update(&vm, 1, &[]).unwrap()["spec"]["nics"][1]["security_groups"],
            json!([])
        );
        let err = nic_groups_update(&vm, 2, &groups).unwrap_err().to_string();
        assert_eq!(err, "VM web has no NIC 2 (2 NICs, numbered from 0)");
    }

    /// `import` 명령의 인자
    fn parse_import(args: &[&str]) -> Result<ImportCmd, clap::Error> {
        let mut argv = vec!["oplctl", "import"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::Import { cmd } => cmd,
            _ => unreachable!(),
        })
    }

    /// `import run`의 인자를 요청 본문으로. `sources`는 가져오기의 원본 네트워크다.
    fn import_run(args: &[&str], sources: &[&str]) -> anyhow::Result<Value> {
        let mut argv = vec!["run", "web01"];
        argv.extend_from_slice(args);
        let ImportCmd::Run(run) = parse_import(&argv)? else {
            unreachable!()
        };
        let sources: Vec<String> = sources.iter().map(|s| s.to_string()).collect();
        import_run_body(&run, &sources)
    }

    #[test]
    fn parses_import_commands() {
        assert!(matches!(
            parse_import(&["ova", "/tmp/web01.ova", "--name", "web01"]).unwrap(),
            ImportCmd::Ova { file, name } if file == std::path::Path::new("/tmp/web01.ova") && name == "web01"
        ));
        assert!(matches!(parse_import(&["list"]).unwrap(), ImportCmd::List));
        assert!(matches!(
            parse_import(&["get", "web01"]).unwrap(),
            ImportCmd::Get { import } if import == "web01"
        ));
        assert!(matches!(
            parse_import(&["delete", "web01", "--force"]).unwrap(),
            ImportCmd::Delete { import, force: true } if import == "web01"
        ));
        // 가져오기 이름, 새 VM 이름과 풀은 꼭 준다.
        for args in [
            &["ova", "/tmp/web01.ova"][..],
            &["run", "web01", "--pool", "local"],
            &["run", "web01", "--name", "web"],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--disk-bus",
                "ide",
            ],
            &[
                "run",
                "web01",
                "--name",
                "web",
                "--pool",
                "local",
                "--net",
                "VM Network=",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "=lan",
            ],
            &[
                "run", "web01", "--name", "web", "--pool", "local", "--net", "",
            ],
        ] {
            assert!(parse_import(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn import_run_builds_the_body() {
        // 주지 않은 값은 비워 두어 서버가 제안 값을 쓴다.
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "VM Network=lan",
                    "--net",
                    "DMZ=dmz",
                    "--net",
                    "a=b=lan"
                ],
                &["DMZ", "VM Network", "a=b"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan", "DMZ": "dmz", "a=b": "lan" },
                "disk_bus": null, "nic_model": null, "firmware": null,
                "sockets": null, "cores": null, "memory_mib": null,
                "keep_macs": true
            })
        );
        assert_eq!(
            import_run(
                &[
                    "--name",
                    "web",
                    "--pool",
                    "local",
                    "--net",
                    "lan",
                    "--disk-bus",
                    "sata",
                    "--nic-model",
                    "e1000e",
                    "--firmware",
                    "bios",
                    "--sockets",
                    "2",
                    "--cores",
                    "4",
                    "--memory",
                    "8192",
                    "--new-macs"
                ],
                &["VM Network"]
            )
            .unwrap(),
            json!({
                "name": "web", "pool": "local",
                "networks": { "VM Network": "lan" },
                "disk_bus": "sata", "nic_model": "e1000e", "firmware": "bios",
                "sockets": 2, "cores": 4, "memory_mib": 8192,
                "keep_macs": false
            })
        );
        assert_eq!(
            import_run(
                &["--name", "web", "--pool", "p", "--disk-bus", "virtio-blk"],
                &[]
            )
            .unwrap()["disk_bus"],
            "virtio-blk"
        );
        // `=` 없는 --net은 원본 네트워크가 하나일 때만 쓴다.
        let err = import_run(
            &["--name", "web", "--pool", "p", "--net", "lan"],
            &["A", "B"],
        );
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 2 source networks (\"A\", \"B\")"
        );
        let err = import_run(&["--name", "web", "--pool", "p", "--net", "lan"], &[]);
        assert_eq!(
            err.unwrap_err().to_string(),
            "--net lan needs SOURCE=NETWORK: vm-import web01 has 0 source networks ()"
        );
    }

    fn sample_import() -> Value {
        json!({
            "id": "11111111-1111-1111-1111-111111111111",
            "name": "web01",
            "spec": {
                "node": "n-1",
                "source": { "ova": { "file_name": "web01.ova", "size_bytes": 3u64 << 29 } }
            },
            "status": {
                "phase": "ready",
                "source_vm": {
                    "name": "web01-src", "guest_os": "otherLinux64Guest", "firmware": "uefi",
                    "vcpus": 2, "cores_per_socket": 2, "memory_mib": 1024,
                    "disks": [
                        { "name": "disk0", "file": "web01-disk1.vmdk", "capacity_bytes": 10u64 << 30, "controller": "scsi" }
                    ],
                    "nics": [
                        { "network": "VM Network", "adapter": "vmxnet3", "mac": "00:50:56:aa:bb:cc" },
                        { "network": "DMZ", "adapter": "e1000" }
                    ]
                },
                "proposal": {
                    "sockets": 1, "cores": 2, "memory_mib": 1024, "firmware": "uefi",
                    "disk_bus": "virtio-blk", "nic_model": "virtio",
                    "networks": { "DMZ": null, "VM Network": "VM Network" }
                },
                "warnings": ["first warning", "second warning"],
                "error": "checksum mismatch for web01-disk1.vmdk"
            }
        })
    }

    #[test]
    fn source_networks_are_the_proposal_keys() {
        assert_eq!(source_networks(&sample_import()), ["DMZ", "VM Network"]);
        assert!(source_networks(&json!({ "status": {} })).is_empty());
    }

    #[test]
    fn import_list_shows_node_source_and_vm_names() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let vms = [json!({ "id": "v-1", "name": "web" })];
        assert_eq!(
            import_row(&sample_import(), &nodes, &vms),
            ["web01", "node1", "web01-src", "ready", "-"]
        );
        // 만든 VM은 이름으로, 지운 VM·빠진 노드는 id로 보인다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        assert_eq!(import_row(&done, &nodes, &vms)[3..], ["done", "web"]);
        assert_eq!(
            import_row(&done, &[], &[])[1..],
            ["n-1", "web01-src", "done", "v-1"]
        );
        // 원본을 읽지 못한 가져오기는 파일 이름을 보인다.
        let mut failed = sample_import();
        failed["status"] = json!({ "phase": "failed" });
        assert_eq!(import_row(&failed, &nodes, &vms)[2], "web01.ova");
    }

    #[test]
    fn import_get_shows_the_source_proposal_and_warnings() {
        let nodes = [json!({ "id": "n-1", "name": "node1" })];
        let mut out = Vec::new();
        print_import(&mut out, &sample_import(), &nodes, &[]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
vm-import: web01
node: node1
source: web01.ova (1.5GiB)
phase: ready
error: checksum mismatch for web01-disk1.vmdk
source vm: web01-src (otherLinux64Guest, uefi, 2 vCPUs, 2 cores per socket, 1024 MiB)
proposal: 1 sockets x 2 cores, 1024 MiB, uefi, virtio-blk disks, virtio NICs

DISK   FILE              SIZE     CONTROLLER
disk0  web01-disk1.vmdk  10.0GiB  scsi

NETWORK     ADAPTER  MAC                PROPOSED
VM Network  vmxnet3  00:50:56:aa:bb:cc  VM Network
DMZ         e1000    -                  -

warnings:
  first warning
  second warning
"
        );
        // 끝난 가져오기는 만든 VM을, 경고가 없으면 경고 줄을 뺀다.
        let mut done = sample_import();
        done["status"]["phase"] = json!("done");
        done["status"]["vm"] = json!("v-1");
        done["status"]["warnings"] = json!([]);
        done["status"]["error"] = Value::Null;
        let mut out = Vec::new();
        print_import(
            &mut out,
            &done,
            &nodes,
            &[json!({ "id": "v-1", "name": "web" })],
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("phase: done\nvm: web\nsource vm: "), "{text}");
        assert!(
            !text.contains("warnings") && !text.contains("error"),
            "{text}"
        );
    }

    /// `import ova`는 크기로 먼저 확인하고 실제 파일 이름과 함께 올린다.
    #[tokio::test]
    async fn import_ova_checks_then_uploads_with_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        // 본문이 없어야 작은 시험 서버가 요청 줄만 읽고 답할 수 있다.
        let file = dir.path().join("web01-export.ova");
        std::fs::write(&file, b"").unwrap();
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let cmd = ImportCmd::Ova {
            file,
            name: "web01".into(),
        };
        let mut out = Vec::new();
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/vm-imports/ova/check?name=web01&size=0 HTTP/1.1",
                "POST /api/v1/vm-imports/ova?name=web01&file_name=web01-export.ova HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 uploaded; review it with `oplctl import get web01`\n"
        );
    }

    /// `=` 없는 `--net`이면 가져오기를 읽어 원본 네트워크를 찾고, 실행 Task를 기다린다.
    #[tokio::test]
    async fn import_run_reads_the_import_for_a_bare_net_and_waits() {
        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else if path.ends_with("/run") {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            } else {
                json!({ "status": { "proposal": { "networks": { "VM Network": null } } } })
            }
        })
        .await;
        let run = |args: &[&str]| {
            let mut argv = vec![
                "oplctl", "import", "run", "web01", "--name", "web", "--pool", "p",
            ];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).unwrap().cmd {
                Cmd::Import { cmd } => cmd,
                _ => unreachable!(),
            }
        };
        let mut out = Vec::new();
        import(&client, run(&["--net", "lan"]), false, &mut out)
            .await
            .unwrap();
        // SOURCE=NETWORK만 주면 읽지 않는다.
        import(&client, run(&["--net", "VM Network=lan"]), false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/vm-imports/web01 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "POST /api/v1/vm-imports/web01/run HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm web imported\nvm web imported\n"
        );
    }

    /// `import delete --force`는 `?force=true`로 보내고 서버의 알림을 보인다. 아니면 Task를 기다린다.
    #[tokio::test]
    async fn import_delete_force_shows_the_server_message() {
        let (client, seen) = fake_api(|_| {
            json!({ "message": "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)" })
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: true,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["DELETE /api/v1/vm-imports/web01?force=true HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import \"web01\" deleted; its files are left on node \"n1\" (x)\n"
        );

        let (client, seen) = fake_api(|path| {
            if path.starts_with("/api/v1/tasks/") {
                json!({ "id": "t1", "status": { "state": "succeeded" } })
            } else {
                json!({ "id": "t1", "spec": {}, "status": { "state": "pending" } })
            }
        })
        .await;
        let mut out = Vec::new();
        let cmd = ImportCmd::Delete {
            import: "web01".into(),
            force: false,
        };
        import(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "DELETE /api/v1/vm-imports/web01 HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
        assert_eq!(String::from_utf8(out).unwrap(), "vm-import web01 deleted\n");
    }

    #[test]
    fn parses_import_esxi_commands() {
        let ImportCmd::EsxiBrowse { conn, fingerprint } =
            parse_import(&["esxi-browse", "--host", "esx1", "--user", "root"]).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fingerprint, None);
        // 포트와 데이터센터는 ESXi 기본값이다.
        assert_eq!(
            esxi_connection(&conn, fingerprint.as_deref(), "secret"),
            json!({
                "host": "esx1", "port": 443, "user": "root", "password": "secret",
                "datacenter": "ha-datacenter", "fingerprint": null
            })
        );
        let ImportCmd::Esxi {
            name,
            conn,
            fingerprint,
            datastore,
            vmx,
        } = parse_import(&[
            "esxi",
            "web01",
            "--host",
            "vc1",
            "--port",
            "8443",
            "--user",
            "admin@vsphere.local",
            "--datacenter",
            "DC1",
            "--datastore",
            "datastore1",
            "--vmx",
            "web01/web01.vmx",
            "--fingerprint",
            "AB:CD",
        ])
        .unwrap()
        else {
            unreachable!()
        };
        assert_eq!(
            esxi_import_body(
                &name,
                &esxi_connection(&conn, Some(&fingerprint), "pw"),
                &datastore,
                &vmx
            ),
            json!({
                "name": "web01",
                "connection": {
                    "host": "vc1", "port": 8443, "user": "admin@vsphere.local", "password": "pw",
                    "datacenter": "DC1", "fingerprint": "AB:CD"
                },
                "datastore": "datastore1",
                "vmx": "web01/web01.vmx"
            })
        );
        // 만들기에는 지문·데이터스토어·.vmx가 꼭 있어야 하고, 비밀번호는 인자로 받지 않는다.
        let full = [
            "esxi",
            "web01",
            "--host",
            "esx1",
            "--user",
            "root",
            "--datastore",
            "ds",
            "--vmx",
            "a/a.vmx",
            "--fingerprint",
            "AB",
        ];
        assert!(parse_import(&full).is_ok());
        for skip in ["--datastore", "--vmx", "--fingerprint", "--host", "--user"] {
            let i = full.iter().position(|a| *a == skip).unwrap();
            let args: Vec<&str> = full[..i].iter().chain(&full[i + 2..]).copied().collect();
            assert!(parse_import(&args).is_err(), "{args:?}");
        }
        let mut with_password = full.to_vec();
        with_password.extend(["--password", "x"]);
        assert!(parse_import(&with_password).is_err());
        assert!(parse_import(&["esxi-browse", "--host", "esx1"]).is_err());

        // 비밀번호 파일(끝의 줄바꿈은 뺀다)
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("esxi-password");
        std::fs::write(&file, "VMware1!\n").unwrap();
        let mut with_file = full.to_vec();
        with_file.extend(["--password-file", file.to_str().unwrap()]);
        let ImportCmd::Esxi { conn, .. } = parse_import(&with_file).unwrap() else {
            unreachable!()
        };
        assert_eq!(esxi_password(&conn).unwrap(), "VMware1!");
    }

    fn sample_browse() -> Value {
        json!({
            "datastores": [
                {
                    "name": "datastore1",
                    "vms": [
                        {
                            "vmx": "web01/web01.vmx", "name": "web01", "guest_os": "ubuntu-64",
                            "vcpus": 2, "memory_mib": 1024, "disks": 1, "locked": false, "error": null
                        },
                        {
                            "vmx": "db/db.vmx", "name": "db", "guest_os": "", "vcpus": 0,
                            "memory_mib": 0, "disks": 0, "locked": true,
                            "error": "memsize is missing"
                        }
                    ]
                },
                { "name": "empty", "vms": [] }
            ]
        })
    }

    /// `import esxi-browse`는 연결을 보내고 데이터스토어의 VM을 표로 보인다. 읽지 못한 VM은 NAME
    /// 자리에 오류를 보인다.
    #[tokio::test]
    async fn import_esxi_browse_shows_the_vms() {
        let (client, seen) = fake_api(|_| sample_browse()).await;
        let mut out = Vec::new();
        let conn = json!({ "host": "esx1" });
        esxi_browse(&client, &conn, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi/browse HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\
DATASTORE   VMX              NAME                       GUEST      CPU  MEMORY  DISKS  LOCKED
datastore1  web01/web01.vmx  web01                      ubuntu-64  2    1.0GiB  1      false
datastore1  db/db.vmx        error: memsize is missing  -          -    -       -      true
"
        );
        // --json이면 응답 그대로
        let mut out = Vec::new();
        esxi_browse(&client, &conn, true, &mut out).await.unwrap();
        let printed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(printed, sample_browse());
    }

    /// 지문을 주지 않았거나(409 `untrusted_certificate`) 틀리면 서버 인증서의 지문으로 다시 하라고 알린다.
    #[tokio::test]
    async fn import_esxi_explains_an_untrusted_certificate() {
        let (client, _) = fake_api_status(|_| {
            (
                409,
                json!({
                    "code": "untrusted_certificate",
                    "message": "verify the ESXi certificate fingerprint",
                    "details": { "fingerprint": "AB:CD:EF" }
                }),
            )
        })
        .await;
        let conn = json!({ "host": "esx1" });
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        let body = esxi_import_body("web01", &conn, "ds", "a/a.vmx");
        let err = esxi_create(&client, &body, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "the ESXi certificate fingerprint is AB:CD:EF; check it and rerun with --fingerprint AB:CD:EF"
        );
        // 다른 오류는 서버의 메시지와 코드 그대로
        let (client, _) = fake_api_status(|_| {
            (
                400,
                json!({ "code": "esxi_auth", "message": "ESXi rejected the user name or password" }),
            )
        })
        .await;
        let err = esxi_browse(&client, &conn, false, &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "ESXi rejected the user name or password (esxi_auth)"
        );
    }

    /// `import esxi`는 가져오기를 만들고 검토 명령을 알린다.
    #[tokio::test]
    async fn import_esxi_creates_the_import() {
        let (client, seen) = fake_api(|_| json!({ "name": "web01" })).await;
        let mut out = Vec::new();
        let body = esxi_import_body("web01", &json!({}), "ds", "a/a.vmx");
        esxi_create(&client, &body, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/vm-imports/esxi HTTP/1.1"]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-import web01 created; review it with `oplctl import get web01`\n"
        );
    }

    fn sample_esxi_import() -> Value {
        let mut i = sample_import();
        i["spec"]["source"] = json!({ "esxi": {
            "host": "esx1.example.com", "port": 443, "datacenter": "ha-datacenter",
            "datastore": "datastore1", "vmx": "web01/web01.vmx", "user": "root",
            "fingerprint": "AB:CD"
        } });
        i
    }

    /// ESXi에서 가져오면 SOURCE는 `esxi://호스트/데이터스토어/.vmx`다.
    #[test]
    fn import_list_and_get_show_an_esxi_source() {
        let url = "esxi://esx1.example.com/datastore1/web01/web01.vmx";
        assert_eq!(import_row(&sample_esxi_import(), &[], &[])[2], url);
        let mut out = Vec::new();
        print_import(&mut out, &sample_esxi_import(), &[], &[]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.starts_with(&format!(
                "vm-import: web01\nnode: n-1\nsource: {url}\nphase: ready\n"
            )),
            "{text}"
        );
    }

    /// `node updates`는 확인 시각, 오류, 패키지 표, 잘렸는지를 보인다. `--refresh`면 다시 확인 Task를
    /// 기다린 뒤 읽는다.
    #[tokio::test]
    async fn node_updates_shows_the_check() {
        fn reply(path: &str) -> Value {
            match path {
                "/api/v1/nodes/n1/updates/refresh" => json!({ "id": "t1" }),
                "/api/v1/tasks/t1" => json!({ "id": "t1", "status": { "state": "succeeded" } }),
                "/api/v1/nodes/n1" => json!({
                    "name": "n1",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000,
                        "packages": [
                            { "name": "oplite", "current": "0.2.0", "candidate": "0.2.1" },
                            { "name": "linux-image-6.12.48+deb13-amd64", "current": null, "candidate": "6.12.48-1" }
                        ],
                        "truncated": true,
                        "error": null
                    } }
                }),
                "/api/v1/nodes/n2" => json!({
                    "name": "n2",
                    "status": { "updates": {
                        "checked_at": 1_793_000_000, "packages": [],
                        "error": "apt-get update -q failed (exit status: 100): E: no network"
                    } }
                }),
                "/api/v1/nodes/n3" => json!({
                    "name": "n3",
                    "status": { "updates": { "checked_at": 1_793_000_000, "packages": [], "error": null } }
                }),
                "/api/v1/nodes/n4" => json!({
                    "name": "n4",
                    "status": { "updates": { "checked_at": null, "packages": [], "error": null } }
                }),
                _ => json!({ "name": "w", "status": { "online": true } }),
            }
        }
        let (client, seen) = fake_api(reply).await;
        let run = |args: &'static [&'static str], json_out: bool| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Updates { node, refresh },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_updates(&client, &node, refresh, json_out, &mut out)
                    .await
                    .map(|()| String::from_utf8(out).unwrap())
            }
        };

        let text = run(&["oplctl", "node", "updates", "n1", "--refresh"], false)
            .await
            .unwrap();
        assert_eq!(
            text,
            "checked: 2026-10-26 07:33 UTC\n\
             PACKAGE                          CURRENT  CANDIDATE\n\
             oplite                           0.2.0    0.2.1\n\
             linux-image-6.12.48+deb13-amd64  -        6.12.48-1\n\
             (only the first 2 packages are shown)\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/updates/refresh HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1",
                "GET /api/v1/nodes/n1 HTTP/1.1"
            ]
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n2"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\n\
             error: apt-get update -q failed (exit status: 100): E: no network\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n3"], false)
                .await
                .unwrap(),
            "checked: 2026-10-26 07:33 UTC\nno updates available\n"
        );
        assert_eq!(
            run(&["oplctl", "node", "updates", "n4"], false)
                .await
                .unwrap(),
            "checked: never\n"
        );
        let err = run(&["oplctl", "node", "updates", "w"], false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "node w does not check for updates");
        let shown: Value = serde_json::from_str(
            &run(&["oplctl", "node", "updates", "n3"], true)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(shown["packages"], json!([]));
    }

    /// `node upgrade`는 업그레이드 Task를 만들고 따라가는 방법을 알린다. `--wait`이면 Task를 기다린다.
    #[tokio::test]
    async fn node_upgrade_starts_the_task() {
        let (client, seen) = fake_api(|path| match path {
            "/api/v1/nodes/n1/upgrade" => json!({ "id": "t1" }),
            _ => json!({ "id": "t1", "status": { "state": "succeeded" } }),
        })
        .await;
        let run = |args: &'static [&'static str]| {
            let client = client.clone();
            async move {
                let Cmd::Node {
                    cmd: NodeCmd::Upgrade { node, wait },
                } = Cli::try_parse_from(args).unwrap().cmd
                else {
                    unreachable!()
                };
                let mut out = Vec::new();
                node_upgrade(&client, &node, wait, &mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }
        };

        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1"]).await,
            "upgrade of n1 started (task t1); follow it with `oplctl task get t1`\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            ["POST /api/v1/nodes/n1/upgrade HTTP/1.1"]
        );
        seen.lock().unwrap().clear();
        assert_eq!(
            run(&["oplctl", "node", "upgrade", "n1", "--wait"]).await,
            "node n1 upgraded\n"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "POST /api/v1/nodes/n1/upgrade HTTP/1.1",
                "GET /api/v1/tasks/t1 HTTP/1.1"
            ]
        );
    }

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
    #[test]
    fn settings_set_body_maps_known_keys() {
        assert_eq!(
            settings_set_body(&["updates.channel=testing".into()]).unwrap(),
            json!({ "updates": { "channel": "testing" } })
        );
        let err = settings_set_body(&["updates.mirror=x".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown setting updates.mirror; known: updates.channel"
        );
        let err = settings_set_body(&["updates.channel".into()]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected KEY=VALUE, got \"updates.channel\""
        );
    }

    /// `settings get`은 spec을 `키: 값` 줄로, `-o json`이면 리소스 그대로 보인다. `set`은 PATCH한다.
    #[tokio::test]
    async fn settings_get_and_set() {
        let (client, seen) = fake_api(|_| {
            json!({
                "id": "00000000-0000-0000-0000-000000000000", "name": "cluster", "revision": 0,
                "spec": { "updates": { "channel": "stable" } }, "status": {}
            })
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "settings", "get"][..],
            &["oplctl", "settings", "set", "updates.channel=testing"][..],
        ] {
            let Cmd::Settings { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            settings(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "updates.channel: stable\nsettings updated\n"
        );

        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "get"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        let mut out = Vec::new();
        settings(&client, cmd, true, &mut out).await.unwrap();
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["spec"]["updates"]["channel"], "stable");

        // 모르는 키는 요청을 보내지 않는다.
        let Cmd::Settings { cmd } = Cli::try_parse_from(["oplctl", "settings", "set", "x=1"])
            .unwrap()
            .cmd
        else {
            unreachable!()
        };
        assert!(
            settings(&client, cmd, false, &mut Vec::new())
                .await
                .is_err()
        );
        assert_eq!(seen.lock().unwrap().len(), 3);
    }

    /// `subscription show`는 상태를 줄로 보인다. 한도가 없으면 쓰는 노드 수만.
    #[test]
    fn subscription_lines_show_the_state() {
        let active = json!({
            "state": "active", "id": "sub-1", "customer": "Example Corp", "level": "premium",
            "nodes": 3, "used_nodes": 2, "expires_at": 1_700_000_000u64, "development": true
        });
        assert_eq!(
            subscription_lines(&active),
            [
                "state: active (development key)",
                "customer: Example Corp",
                "level: premium",
                "nodes: 2/3",
                "expires: 2023-11-14",
            ]
        );
        let expired = json!({
            "state": "expired", "id": "sub-1", "customer": "C", "level": "standard",
            "nodes": 1, "used_nodes": 1, "expires_at": 0, "development": false
        });
        assert_eq!(subscription_lines(&expired)[0], "state: expired");
        assert_eq!(
            subscription_lines(&json!({ "state": "none", "used_nodes": 1 })),
            ["state: none", "nodes: 1"]
        );
    }

    /// `subscription set --file -`은 표준 입력에서 키를 읽는다(앞뒤 공백·줄바꿈을 다듬음).
    #[test]
    fn subscription_key_can_come_from_stdin() {
        let args = ["oplctl", "subscription", "set", "--file", "-"];
        let Cmd::Subscription {
            cmd: SubscriptionCmd::Set { key: None, file },
        } = Cli::try_parse_from(args).unwrap().cmd
        else {
            panic!("parse --file -")
        };
        assert_eq!(file.unwrap().to_str(), Some("-"));
        let mut input = std::io::Cursor::new(" opl1.a.b\r\n");
        assert_eq!(read_stdin_key(&mut input).unwrap(), "opl1.a.b");
    }

    /// `subscription set`은 키(인자나 파일)를 PUT하고 상태를 보인다. `remove`는 DELETE한다.
    #[tokio::test]
    async fn subscription_set_show_and_remove() {
        let (client, seen) = fake_api(|_| {
            json!({ "state": "active", "customer": "C", "level": "standard", "nodes": 2,
                    "used_nodes": 1, "expires_at": 1_700_000_000u64, "development": false })
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("key");
        std::fs::write(&file, "opl1.a.b\r\n").unwrap();
        let file = file.to_str().unwrap();
        let mut out = Vec::new();
        for args in [
            &["oplctl", "subscription", "show"][..],
            &["oplctl", "subscription", "set", "opl1.a.b"][..],
            &["oplctl", "subscription", "set", "--file", file][..],
            &["oplctl", "subscription", "remove"][..],
        ] {
            let Cmd::Subscription { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            subscription(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "PUT /api/v1/subscription HTTP/1.1",
                "DELETE /api/v1/subscription HTTP/1.1",
            ]
        );
        let shown =
            "state: active\ncustomer: C\nlevel: standard\nnodes: 1/2\nexpires: 2023-11-14\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}subscription removed\n")
        );

        // 키나 파일 중 하나만 준다.
        for args in [
            &["oplctl", "subscription", "set"][..],
            &["oplctl", "subscription", "set", "k", "--file", "f"][..],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    /// `telemetry show`는 켜짐·끝점·id·마지막 전송·오류를 줄로 보인다.
    #[test]
    fn telemetry_lines_show_the_state() {
        let settings = json!({
            "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
            "status": { "telemetry": {
                "id": "6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last_sent_at": 1_761_471_000, "last_error": "HTTP 500"
            } }
        });
        assert_eq!(
            telemetry_lines(&settings),
            [
                "enabled: true",
                "endpoint: https://t.example/r",
                "id: 6f0c3b7e-8d2a-4c1e-9b5f-0a1b2c3d4e5f",
                "last sent: 2025-10-26 09:30 UTC",
                "last error: HTTP 500",
            ]
        );
        let default_endpoint = if opl_types::version::TELEMETRY_URL.is_empty() {
            "endpoint: none".to_string()
        } else {
            format!(
                "endpoint: {} (build default)",
                opl_types::version::TELEMETRY_URL
            )
        };
        assert_eq!(
            telemetry_lines(&json!({ "spec": { "updates": {} }, "status": {} })),
            [
                "enabled: false".to_string(),
                default_endpoint,
                "id: none".into(),
                "last sent: never".into(),
                "last error: none".into(),
            ]
        );
    }

    #[test]
    fn telemetry_bodies_send_only_what_was_given() {
        assert_eq!(
            telemetry_body(true, None),
            json!({ "telemetry": { "enabled": true } })
        );
        assert_eq!(
            telemetry_body(true, Some("https://t.example/r")),
            json!({ "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } })
        );
        assert_eq!(
            telemetry_body(false, None),
            json!({ "telemetry": { "enabled": false } })
        );
    }

    /// `telemetry`의 하위 명령이 부르는 API. preview는 받은 JSON을 그대로 예쁘게 보인다.
    #[tokio::test]
    async fn telemetry_commands_call_the_api() {
        let (client, seen) = fake_api(|path| {
            if path == "/api/v1/telemetry/preview" {
                json!({ "schema": 1, "vms": { "total": 2 } })
            } else {
                json!({
                    "spec": { "telemetry": { "enabled": true, "endpoint": "https://t.example/r" } },
                    "status": { "telemetry": { "id": "abc" } }
                })
            }
        })
        .await;
        let mut out = Vec::new();
        for args in [
            &["oplctl", "telemetry", "show"][..],
            &[
                "oplctl",
                "telemetry",
                "enable",
                "--endpoint",
                "https://t.example/r",
            ][..],
            &["oplctl", "telemetry", "disable"][..],
            &["oplctl", "telemetry", "reset-id"][..],
            &["oplctl", "telemetry", "preview"][..],
        ] {
            let Cmd::Telemetry { cmd } = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            telemetry(&client, cmd, false, &mut out).await.unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "PATCH /api/v1/settings HTTP/1.1",
                "POST /api/v1/telemetry/reset-id HTTP/1.1",
                "GET /api/v1/telemetry/preview HTTP/1.1",
            ]
        );
        let shown = "enabled: true\nendpoint: https://t.example/r\nid: abc\nlast sent: never\nlast error: none\n";
        let preview = "{\n  \"schema\": 1,\n  \"vms\": {\n    \"total\": 2\n  }\n}\n";
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{shown}{shown}{shown}{shown}{preview}")
        );
    }

    /// `--since`의 기간(`30m`·`1h`·`7d`)은 지금에서 뺀 유닉스 밀리초로, 나머지는 그대로 보낸다.
    #[test]
    fn audit_since_turns_durations_into_milliseconds() {
        let now = 1_762_400_000_000;
        assert_eq!(audit_since("30m", now), (now - 30 * 60_000).to_string());
        assert_eq!(audit_since("1h", now), (now - 3_600_000).to_string());
        assert_eq!(audit_since("7d", now), (now - 7 * 86_400_000).to_string());
        assert_eq!(audit_since("90s", now), (now - 90_000).to_string());
        assert_eq!(
            audit_since("2026-11-06T00:00:00Z", now),
            "2026-11-06T00:00:00Z"
        );
        assert_eq!(audit_since("1762400000000", now), "1762400000000");
        assert_eq!(audit_since("xd", now), "xd");
    }

    /// `audit`는 거르기를 쿼리로 보내고 표를 보인다. 읽지 못한 노드와 잘림은 stderr에 알린다.
    /// `-o json`은 응답 그대로이고 stderr에 쓰지 않는다.
    #[tokio::test]
    async fn audit_shows_a_table_and_warnings() {
        let (client, seen) = fake_api(|_| {
            json!({
                "entries": [
                    { "id": "019a0000-0000-7000-8000-000000000002", "time": 1_762_387_205_123u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "token", "ip": "10.0.0.5", "method": "POST",
                      "path": "/vms/web-1/actions/start", "action": "POST /vms/{key}/actions/{action}",
                      "target": { "kind": "vms", "key": "web-1" }, "status": 202,
                      "duration_ms": 12, "detail": null },
                    { "id": "019a0000-0000-7000-8000-000000000001", "time": 1_762_387_200_000u64,
                      "node": "node-b", "user": null, "user_id": null, "role": null,
                      "auth": "none", "ip": "10.0.0.9", "method": "POST", "path": "/auth/login",
                      "action": "POST /auth/login", "target": null, "status": 401,
                      "duration_ms": 3, "detail": "username=bob" },
                    { "id": "019a0000-0000-7000-8000-000000000000", "time": 1_762_387_100_000u64,
                      "node": "node-a", "user": "alice", "user_id": null, "role": "admin",
                      "auth": "session", "ip": "10.0.0.5", "method": "POST", "path": "/auth/logout",
                      "action": "POST /auth/logout", "target": null, "status": 204,
                      "duration_ms": 1, "detail": null }
                ],
                "unreachable": ["node-c", "node-d"],
                "disabled": ["node-e"],
                "truncated": true
            })
        })
        .await;
        let args = [
            "oplctl",
            "audit",
            "--since",
            "2026-11-06T00:00:00Z",
            "--until",
            "1762400000000",
            "--user",
            "alice",
            "--node",
            "node-a",
            "--action",
            "start",
            "--limit",
            "5",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, false, &mut out, &mut err).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/audit?since=2026-11-06T00%3A00%3A00Z&until=1762400000000\
              &user=alice&node=node-a&action=start&limit=5 HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "TIME                  NODE    USER   IP        ACTION                            TARGET        STATUS\n\
             2025-11-06T00:00:05Z  node-a  alice  10.0.0.5  POST /vms/{key}/actions/{action}  vms/web-1     202\n\
             2025-11-06T00:00:00Z  node-b  -      10.0.0.9  POST /auth/login                  username=bob  401\n\
             2025-11-05T23:58:20Z  node-a  alice  10.0.0.5  POST /auth/logout                 -             204\n"
        );
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: no audit records from: node-c, node-d\n\
             note: the audit log is disabled on: node-e\n\
             note: more entries match; narrow the range or raise --limit\n"
        );

        // 거르기가 없으면 쿼리도 없다(서버 기본값: 지난 24시간, 200개).
        let Cmd::Audit(a) = Cli::try_parse_from(["oplctl", "audit"]).unwrap().cmd else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        audit(&client, a, true, &mut out, &mut err).await.unwrap();
        assert_eq!(seen.lock().unwrap()[1], "GET /api/v1/audit HTTP/1.1");
        let shown: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(shown["unreachable"], json!(["node-c", "node-d"]));
        assert_eq!(shown["entries"][1]["detail"], "username=bob");
        assert!(err.is_empty());

        // 기간 --since는 --until과 함께 줄 수 없다(서버에 보내지 않는다).
        let args = [
            "oplctl",
            "audit",
            "--since",
            "1h",
            "--until",
            "1762400000000",
        ];
        let Cmd::Audit(a) = Cli::try_parse_from(args).unwrap().cmd else {
            unreachable!()
        };
        let e = audit(&client, a, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "a relative --since cannot be combined with --until; give a time"
        );
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// `fake_api_bodies`가 남긴 요청
    #[derive(Debug)]
    struct SeenRequest {
        line: String,
        if_match: Option<String>,
        body: Value,
    }

    /// `fake_api`와 같고 요청의 `if-match`와 JSON 본문도 남긴다. 응답에는 세션 쿠키를 붙인다.
    async fn fake_api_bodies(
        reply: fn(&str) -> Value,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<SeenRequest>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = vec![0; 4096];
                // 머리와 content-length만큼의 본문을 다 읽는다.
                let (head, body) = loop {
                    let n = sock.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "connection closed mid-request");
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = head
                        .lines()
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break (head.to_string(), body.to_string());
                    }
                };
                let line = head.lines().next().unwrap_or("").to_string();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let if_match = head.lines().find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("if-match")
                        .then(|| v.trim().to_string())
                });
                log.lock().unwrap().push(SeenRequest {
                    line,
                    if_match,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                });
                let body = reply(&path).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nset-cookie: opl_session=s1; Path=/\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        (base, seen)
    }

    fn realm_cmd(args: &[&str]) -> Result<RealmCmd, clap::Error> {
        let mut all = vec!["oplctl", "realm"];
        all.extend_from_slice(args);
        match Cli::try_parse_from(all)?.cmd {
            Cmd::Realm { cmd } => Ok(cmd),
            _ => unreachable!(),
        }
    }

    /// `realm add-ldap`은 url·base DN이 필요하고, `--group`은 마지막 `=`에서 나눈다.
    #[test]
    fn parses_realm_commands() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--group",
            "cn=admins,dc=example,dc=com=admin",
            "--group",
            "cn=staff,dc=example,dc=com=Viewer",
        ])
        .unwrap() else {
            panic!("add-ldap")
        };
        assert_eq!(add.ldap.groups.len(), 2);
        assert_eq!(add.ldap.groups[0].0, "cn=admins,dc=example,dc=com");
        assert!(matches!(add.ldap.groups[0].1, Role::Admin));
        assert!(matches!(add.ldap.groups[1].1, Role::Viewer));

        assert_eq!(
            parse_group_role("cn=a,dc=x=root").unwrap_err(),
            "invalid role \"root\" in \"cn=a,dc=x=root\": use admin, operator or viewer"
        );
        assert!(parse_group_role("admins").is_err());
        assert!(parse_group_role("=admin").is_err());

        for args in [
            // url·base DN이 없다.
            &["add-ldap", "corp", "--url", "ldaps://h"][..],
            &["add-ldap", "corp", "--base-dn", "dc=x"][..],
            // 비밀번호를 주면서 지울 수는 없다.
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-password-file",
                "f",
            ][..],
            // 지우기와 설정은 함께 줄 수 없다.
            &["set-ldap", "corp", "--clear-bind-dn", "--bind-dn", "cn=x"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-dn",
                "--bind-password-file",
                "f",
            ][..],
            &["set-ldap", "corp", "--clear-ca", "--ca-file", "f"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-default-role",
                "--default-role",
                "viewer",
            ][..],
            &[
                "set-ldap",
                "corp",
                "--clear-groups",
                "--group",
                "cn=x=admin",
            ][..],
            &["test", "corp"][..],
        ] {
            assert!(realm_cmd(args).is_err(), "{args:?}");
        }
        let RealmCmd::Test { user, .. } = realm_cmd(&["test", "corp", "--user", "alice"]).unwrap()
        else {
            panic!("test")
        };
        assert_eq!(user, "alice");
    }

    /// `add-ldap`의 본문은 준 것만 담는다(나머지는 서버 기본값).
    #[test]
    fn realm_add_body_sends_only_what_is_given() {
        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldap://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
        ])
        .unwrap() else {
            unreachable!()
        };
        assert_eq!(
            realm_add_body(&add, LdapFiles::default()),
            json!({
                "name": "corp",
                "backend": { "kind": "ldap", "url": "ldap://ldap.example.com", "base_dn": "dc=example,dc=com" },
                "roles": { "groups": [] }
            })
        );

        let RealmCmd::AddLdap(add) = realm_cmd(&[
            "add-ldap",
            "corp",
            "--url",
            "ldaps://ldap.example.com",
            "--base-dn",
            "dc=example,dc=com",
            "--bind-dn",
            "cn=svc,dc=example,dc=com",
            "--user-filter",
            "(sAMAccountName={username})",
            "--group-attr",
            "isMemberOf",
            "--timeout",
            "5",
            "--display",
            "Corp",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "viewer",
        ])
        .unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("svc-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        assert_eq!(
            realm_add_body(&add, files),
            json!({
                "name": "corp",
                "display": "Corp",
                "backend": {
                    "kind": "ldap", "url": "ldaps://ldap.example.com", "base_dn": "dc=example,dc=com",
                    "bind_dn": "cn=svc,dc=example,dc=com", "bind_password": "svc-pw",
                    "user_filter": "(sAMAccountName={username})", "group_attr": "isMemberOf",
                    "ca_pem": "PEM", "timeout_secs": 5
                },
                "roles": {
                    "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                    "default": "viewer"
                }
            })
        );
    }

    fn sample_realm() -> Value {
        json!({
            "id": "019a0000-0000-7000-8000-000000000001", "name": "corp", "revision": 4,
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 10, "has_bind_password": true
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        })
    }

    /// `set-ldap`은 지금 영역에 준 것만 바꾼다. 비밀번호는 주지 않으면 빼고(서버가 둔다),
    /// `--clear-bind-password`면 빈 글이다. `--group`을 주면 표 전체를 바꾼다.
    #[test]
    fn realm_set_body_changes_only_what_is_given() {
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp", "--timeout", "3"]).unwrap()
        else {
            unreachable!()
        };
        let kept = json!({
            "display": "Corp",
            "backend": {
                "kind": "ldap", "url": "ldaps://ldap.example.com", "bind_dn": "cn=svc,dc=example,dc=com",
                "base_dn": "dc=example,dc=com", "user_filter": "(uid={username})",
                "group_attr": "memberOf", "timeout_secs": 3
            },
            "roles": {
                "groups": [{ "group": "cn=admins,dc=example,dc=com", "role": "admin" }],
                "default": "viewer"
            }
        });
        assert_eq!(
            realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap(),
            kept
        );

        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--url",
            "ldap://other.example.com",
            "--clear-bind-dn",
            "--clear-ca",
            "--group",
            "cn=ops,dc=example,dc=com=operator",
            "--default-role",
            "admin",
            "--display",
            "Corporate",
        ])
        .unwrap() else {
            unreachable!()
        };
        let mut with_ca = sample_realm();
        with_ca["backend"]["ca_pem"] = json!("PEM");
        let body = realm_set_body(&with_ca, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["display"], "Corporate");
        assert_eq!(body["backend"]["url"], "ldap://other.example.com");
        // --clear-bind-dn은 저장된 비밀번호도 지운다.
        assert_eq!(body["backend"]["bind_password"], "");
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
        assert!(body["backend"].get("ca_pem").is_none(), "{body}");
        assert_eq!(
            body["roles"],
            json!({
                "groups": [{ "group": "cn=ops,dc=example,dc=com", "role": "operator" }],
                "default": "admin"
            })
        );

        // 새 비밀번호와 CA
        let RealmCmd::SetLdap(set) = realm_cmd(&["set-ldap", "corp"]).unwrap() else {
            unreachable!()
        };
        let files = LdapFiles {
            bind_password: Some("new-pw".into()),
            ca_pem: Some("PEM".into()),
        };
        let body = realm_set_body(&sample_realm(), &set, files).unwrap();
        assert_eq!(body["backend"]["bind_password"], "new-pw");
        assert_eq!(body["backend"]["ca_pem"], "PEM");

        // 그룹 표와 기본 역할 지우기
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-groups", "--clear-default-role"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert_eq!(body["roles"], json!({ "groups": [] }));

        // 서비스 계정 DN이 남는데 비밀번호만 지울 수는 없다(새 DN을 줘도).
        for args in [
            &["set-ldap", "corp", "--clear-bind-password"][..],
            &[
                "set-ldap",
                "corp",
                "--clear-bind-password",
                "--bind-dn",
                "cn=other",
            ][..],
        ] {
            let RealmCmd::SetLdap(set) = realm_cmd(args).unwrap() else {
                unreachable!()
            };
            let e = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap_err();
            assert_eq!(
                e.to_string(),
                "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)"
            );
        }
        // DN이 없는 영역이나 DN도 지우면 된다.
        let mut anon = sample_realm();
        anon["backend"].as_object_mut().unwrap().remove("bind_dn");
        let RealmCmd::SetLdap(set) =
            realm_cmd(&["set-ldap", "corp", "--clear-bind-password"]).unwrap()
        else {
            unreachable!()
        };
        let body = realm_set_body(&anon, &set, LdapFiles::default()).unwrap();
        assert_eq!(body["backend"]["bind_password"], "");
        let RealmCmd::SetLdap(set) = realm_cmd(&[
            "set-ldap",
            "corp",
            "--clear-bind-password",
            "--clear-bind-dn",
        ])
        .unwrap() else {
            unreachable!()
        };
        let body = realm_set_body(&sample_realm(), &set, LdapFiles::default()).unwrap();
        assert!(body["backend"].get("bind_dn").is_none(), "{body}");
    }

    /// url·bind_dn·CA를 바꾸면서 비밀번호를 주지 않으면 서버의 400을 그대로 보인다.
    #[tokio::test]
    async fn realm_set_shows_the_server_refusal() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (client, seen) = fake_api_status(move |_| {
            if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                (200, sample_realm())
            } else {
                (
                    400,
                    json!({
                        "code": "invalid_request",
                        "message": "bind_password must be given again when url, bind_dn or ca_pem changes"
                    }),
                )
            }
        })
        .await;
        let e = realm(
            &client,
            realm_cmd(&["set-ldap", "corp", "--url", "ldaps://other.example.com"]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert!(
            e.to_string()
                .contains("bind_password must be given again when url, bind_dn or ca_pem changes"),
            "{e}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1"
            ]
        );
    }

    #[test]
    fn realm_show_lines() {
        assert_eq!(
            realm_lines(&sample_realm()).join("\n"),
            "name: corp\n\
             display: Corp\n\
             kind: ldap\n\
             url: ldaps://ldap.example.com\n\
             base dn: dc=example,dc=com\n\
             bind dn: cn=svc,dc=example,dc=com\n\
             bind password: set\n\
             user filter: (uid={username})\n\
             group attribute: memberOf\n\
             ca: system roots\n\
             timeout: 10s\n\
             groups:\n  cn=admins,dc=example,dc=com = admin\n\
             default role: viewer"
        );
        let mut bare = sample_realm();
        bare["backend"]["has_bind_password"] = json!(false);
        bare["backend"]["ca_pem"] = json!("PEM");
        bare["roles"] = json!({ "groups": [] });
        let lines = realm_lines(&bare);
        for line in [
            "bind password: none",
            "ca: custom (PEM)",
            "groups: none",
            "default role: none",
        ] {
            assert!(lines.contains(&line.to_string()), "{line}: {lines:?}");
        }
    }

    /// 영역 명령이 부르는 API: 파일의 비밀번호·CA는 본문에, `set-ldap`은 읽은 revision을
    /// If-Match로, `test`는 단계 표를 보이고 실패한 단계가 있으면 오류다.
    #[tokio::test]
    async fn realm_commands_call_the_api() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/realms" => json!({ "revision": 7, "items": [sample_realm()] }),
            "/api/v1/realms/corp/actions/test" => json!({
                "steps": [
                    { "name": "connect", "ok": true, "message": "connected to ldaps://ldap.example.com" },
                    { "name": "service_bind", "ok": true, "message": "bound as cn=svc,dc=example,dc=com" },
                    { "name": "search", "ok": true, "message": "found uid=alice,dc=example,dc=com" },
                    { "name": "user_bind", "ok": true, "message": "the password is correct" },
                    { "name": "groups", "ok": true, "message": "2 groups" }
                ],
                "dn": "uid=alice,dc=example,dc=com",
                "groups": ["cn=admins,dc=example,dc=com", "cn=staff,dc=example,dc=com"],
                "role": "admin"
            }),
            "/api/v1/realms/broken/actions/test" => json!({
                "steps": [{ "name": "connect", "ok": false, "message": "connection refused" }],
                "dn": null, "groups": [], "role": null
            }),
            _ => sample_realm(),
        })
        .await;
        let client = Client::new(&base, None, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("pw");
        std::fs::write(&secret, "svc-pw\n").unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "-----BEGIN CERTIFICATE-----\nX\n").unwrap();
        let (secret, ca) = (secret.to_str().unwrap(), ca.to_str().unwrap());

        let mut out = Vec::new();
        for args in [
            &["list"][..],
            &[
                "add-ldap",
                "corp",
                "--url",
                "ldaps://ldap.example.com",
                "--base-dn",
                "dc=example,dc=com",
                "--bind-dn",
                "cn=svc,dc=example,dc=com",
                "--bind-password-file",
                secret,
                "--ca-file",
                ca,
            ][..],
            &["set-ldap", "corp", "--bind-password-file", secret][..],
            &["remove", "corp"][..],
            &["test", "corp", "--user", "alice", "--password-file", secret][..],
        ] {
            realm(&client, realm_cmd(args).unwrap(), false, &mut out)
                .await
                .unwrap();
        }
        let e = realm(
            &client,
            realm_cmd(&["test", "broken", "--user", "a", "--password-file", secret]).unwrap(),
            false,
            &mut Vec::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.to_string(), "realm broken: the connect step failed");

        let seen = seen.lock().unwrap();
        let lines: Vec<&str> = seen.iter().map(|r| r.line.as_str()).collect();
        assert_eq!(
            lines,
            [
                "GET /api/v1/realms HTTP/1.1",
                "POST /api/v1/realms HTTP/1.1",
                "GET /api/v1/realms/corp HTTP/1.1",
                "PUT /api/v1/realms/corp HTTP/1.1",
                "DELETE /api/v1/realms/corp HTTP/1.1",
                "POST /api/v1/realms/corp/actions/test HTTP/1.1",
                "POST /api/v1/realms/broken/actions/test HTTP/1.1",
            ]
        );
        assert_eq!(seen[1].body["backend"]["bind_password"], "svc-pw");
        assert_eq!(
            seen[1].body["backend"]["ca_pem"],
            "-----BEGIN CERTIFICATE-----\nX\n"
        );
        assert_eq!(seen[3].if_match.as_deref(), Some("4"));
        assert_eq!(seen[3].body["backend"]["bind_password"], "svc-pw");
        assert!(seen[3].body["backend"].get("has_bind_password").is_none());
        assert_eq!(
            seen[5].body,
            json!({ "username": "alice", "password": "svc-pw" })
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME  DISPLAY  KIND  URL\n\
             corp  Corp     ldap  ldaps://ldap.example.com\n\
             realm corp created\n\
             realm corp updated\n\
             realm corp removed\n\
             STEP          RESULT  MESSAGE\n\
             connect       ok      connected to ldaps://ldap.example.com\n\
             service_bind  ok      bound as cn=svc,dc=example,dc=com\n\
             search        ok      found uid=alice,dc=example,dc=com\n\
             user_bind     ok      the password is correct\n\
             groups        ok      2 groups\n\
             dn: uid=alice,dc=example,dc=com\n\
             groups: cn=admins,dc=example,dc=com, cn=staff,dc=example,dc=com\n\
             role: admin\n"
        );
    }

    /// `login --realm`은 로그인 본문에 영역을 넣고(local이면 넣지 않음) 서버가 정한 이름을 돌려준다.
    #[tokio::test]
    async fn login_sends_the_realm() {
        let (base, seen) = fake_api_bodies(|path| match path {
            "/api/v1/auth/login" => json!({ "name": "alice@corp", "role": "admin" }),
            "/api/v1/tokens" => json!({ "secret": "opl_t_s" }),
            _ => json!({}),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let pw = dir.path().join("pw");
        std::fs::write(&pw, "alice-pw\n").unwrap();
        let pw = pw.to_str().unwrap();
        for (realm, expected) in [(Some("corp"), Some("corp")), (None, None)] {
            let mut args = vec!["oplctl", "login", "--user", "Alice", "--password-file", pw];
            if let Some(realm) = realm {
                args.extend(["--realm", realm]);
            }
            let Cmd::Login(args) = Cli::try_parse_from(args).unwrap().cmd else {
                unreachable!()
            };
            assert_eq!(
                login(&base, &args, None).await.unwrap(),
                ("opl_t_s".to_string(), "alice@corp".to_string())
            );
            let seen = seen.lock().unwrap();
            let sent = &seen[seen.len() - 3];
            assert_eq!(sent.line, "POST /api/v1/auth/login HTTP/1.1");
            assert_eq!(sent.body["username"], "Alice");
            assert_eq!(sent.body["password"], "alice-pw");
            assert_eq!(sent.body.get("realm").and_then(Value::as_str), expected);
        }
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
```

- [ ] **Step 2: 실행 권한**

Run: `chmod 0755 .github/scripts/ldap-setup.sh`
Expected: 출력 없음

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1343개, 실패 0

Run: `shellcheck .github/scripts/ldap-setup.sh`
Expected: 경고 없음 (shellcheck가 없으면 CI `shell` 작업이 본다)

Run: `ruby -ryaml -e 'YAML.load_file(%q(.github/workflows/ci.yml))'`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates .github
git commit -m "feat(oplctl): oplctl realm과 실제 OpenLDAP 시험 — oplctl realm list|show|add-ldap|set-ldap|remove|test(add-ldap은 --url·--base-dn에 --bind-dn·--bind-password-file(파일|-)·--user-filter·--group-attr·--ca-file·--timeout·--display·--group <DN>=<role>(마지막 =에서 나눔, 여러 번)·--default-role, 준 것만 본문에; set-ldap은 영역을 읽어 준 것만 바꾸고 읽은 revision을 If-Match로 PUT, 비밀번호는 주지 않으면 빼서 서버가 두고 --clear-bind-password면 \"\", --group을 주면 표 전체를 바꿈; show는 bind password: set|none·ca: custom (PEM)|system roots; test는 단계 표·dn·groups·role을 보이고 실패한 단계가 있으면 오류, 비밀번호 파일이 없으면 터미널에서 물음). oplctl login --realm <이름>(기본 local — local이면 본문에 realm을 넣지 않음), 로그인 메시지는 서버가 정한 이름(alice@corp). fake_api 시험(본문·If-Match를 남기는 fake_api_bodies). opl-api 기능 ldap-it와 tests/ldap_it.rs(OPL_TEST_LDAP_* 환경 변수의 slapd에 LDAPS·StartTLS로 찾기·바인드·memberOf, 틀린 자격 증명, 서비스 계정 실패·믿지 않는 CA, API의 시험 동작과 외부 사용자 로그인·no_role). .github/scripts/ldap-setup.sh(POSIX sh: debconf로 slapd, openssl 시험 CA·127.0.0.1 서버 인증서로 LDAPS·StartTLS, memberof 오버레이, 사용자 둘·그룹 하나, ldapsearch·ldapwhoami로 확인)와 CI 작업 ldap, shell 작업의 shellcheck에 .github/scripts"
```

---

### Task 6: 웹: 인증 영역 화면과 영역 로그인

로그인 화면의 영역 고르기(영역이 local뿐이면 숨김), `/realms`(admin만): 목록, LDAP 영역 만들기·고치기(그룹 → 역할 표, 비밀번호 규칙을 저장 전에 검사), 지우기, 시험 결과 표. 사용자 목록의 영역 열, 외부 사용자는 비밀번호 바꾸기 없음. 오류 문구 ko·en.

**Files:**
- Create: `web/src/realms/RealmsPage.test.tsx`
- Create: `web/src/realms/RealmsPage.tsx`
- Create: `web/src/realms/realms.ts`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/accounts/ChangePasswordDialog.test.tsx`
- Modify: `web/src/accounts/UsersPage.test.tsx`
- Modify: `web/src/accounts/UsersPage.tsx`
- Modify: `web/src/auth/AuthProvider.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/pages/LoginPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.test.tsx` 전체:

```tsx
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse, ws } from "msw";

import { App } from "@/App";
import { safeNext } from "@/auth/RequireAuth";
import { apiUrl, server } from "@/test/server";

const admin = { user: "00000000-0000-0000-0000-000000000001", name: "admin", role: "admin" };

function signedOut() {
  let signedIn = false;
  const logins: unknown[] = [];
  server.use(
    http.get(apiUrl("/auth/whoami"), () =>
      signedIn
        ? HttpResponse.json(admin)
        : HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }),
    ),
    http.post(apiUrl("/auth/login"), async ({ request }) => {
      const body = (await request.json()) as { password: string };
      logins.push(body);
      if (body.password !== "admin-password") {
        return HttpResponse.json(
          { code: "invalid_credentials", message: "wrong" },
          { status: 401 },
        );
      }
      signedIn = true;
      return HttpResponse.json({ id: admin.user, name: "admin", role: "admin" });
    }),
    http.post(apiUrl("/auth/logout"), () => {
      signedIn = false;
      return new HttpResponse(null, { status: 204 });
    }),
    ...[
      "/vms",
      "/volumes",
      "/storage-pools",
      "/nodes",
      "/tasks",
      "/networks",
      "/snapshots",
      "/security-groups",
      "/backups",
      "/backup-jobs",
      "/vm-imports",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
    http.get(apiUrl("/version"), () => HttpResponse.json({ version: "0.9.0~ci7" })),
    // 영역은 로그인 없이 읽는다. 기본은 로컬뿐이다.
    http.get(apiUrl("/auth/realms"), () =>
      HttpResponse.json([{ name: "local", display: null, kind: "local" }]),
    ),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: admin.user, members: [] }),
    ),
  );
  return logins;
}

/** 로컬과 LDAP 영역 `corp`가 있는 서버 */
function withRealms() {
  server.use(
    http.get(apiUrl("/auth/realms"), () =>
      HttpResponse.json([
        { name: "local", display: null, kind: "local" },
        { name: "corp", display: "Corp directory", kind: "ldap" },
        { name: "lab", display: null, kind: "ldap" },
      ]),
    ),
  );
}

it("signs in, returns to the requested page and signs out", async () => {
  const logins = signedOut();
  window.history.pushState({}, "", "/somewhere");
  const user = userEvent.setup();
  render(<App />);

  await screen.findByRole("button", { name: "Sign in" });
  expect(window.location.pathname).toBe("/login");
  expect(screen.getByRole("heading", { level: 1, name: "opLite" })).toBeInTheDocument();
  // 로그인하기 전에도 서버 버전이 보인다.
  expect(await screen.findByText("opLite 0.9.0~ci7")).toBeInTheDocument();
  // 영역이 로컬뿐이면 고르지 않는다.
  expect(screen.queryByLabelText("Realm")).not.toBeInTheDocument();
  await user.type(screen.getByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "nope");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Wrong username or password.");
  expect(screen.getByLabelText("Password")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByLabelText("Password")).toHaveAccessibleDescription(
    "Wrong username or password.",
  );

  await user.clear(screen.getByLabelText("Password"));
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
  expect(window.location.pathname).toBe("/somewhere");
  // 로컬 로그인에는 영역을 보내지 않는다.
  expect(logins.at(-1)).toEqual({ username: "admin", password: "admin-password" });
  expect(await screen.findByText("lab")).toBeInTheDocument();
  expect(await screen.findByRole("status")).toHaveTextContent("Live");

  await user.click(screen.getByRole("button", { name: "Account (admin)" }));
  await user.click(await screen.findByRole("menuitem", { name: "Sign out" }));
  expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
  // 직접 로그아웃하면 다음 사용자를 앞 사용자의 화면으로 보내지 않는다.
  expect(window.location.pathname).toBe("/login");
  expect(window.location.search).toBe("");
});

it("signs in to the chosen realm", async () => {
  const logins = signedOut();
  withRealms();
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  const realm = await screen.findByLabelText("Realm");
  expect(realm).toHaveValue("local");
  expect(
    within(realm)
      .getAllByRole("option")
      .map((o) => [o.getAttribute("value"), o.textContent]),
  ).toEqual([
    ["local", "Local users"],
    ["corp", "Corp directory"],
    ["lab", "lab"],
  ]);
  await user.selectOptions(realm, "corp");
  await user.type(screen.getByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByText("lab")).toBeInTheDocument();
  expect(logins).toEqual([{ username: "alice", password: "admin-password", realm: "corp" }]);
});

it("explains realm sign-in errors", async () => {
  signedOut();
  withRealms();
  const answers = [
    HttpResponse.json(
      { code: "no_role", message: "no role is mapped for this user" },
      { status: 403 },
    ),
    HttpResponse.json(
      { code: "realm_unavailable", message: "the directory of realm corp is unavailable" },
      { status: 503 },
    ),
  ];
  server.use(http.post(apiUrl("/auth/login"), () => answers.shift()));
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  await user.selectOptions(await screen.findByLabelText("Realm"), "corp");
  await user.type(screen.getByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), "secret-123");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Your directory account has no role in opLite. Ask an administrator to map one of your groups.",
  );
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Cannot reach the directory of this realm. Try again later or ask an administrator.",
  );
});

it("says when sign-in attempts are throttled", async () => {
  signedOut();
  server.use(
    http.post(apiUrl("/auth/login"), () =>
      HttpResponse.json(
        { code: "too_many_attempts", message: "try again in 30 seconds" },
        { status: 429, headers: { "Retry-After": "30" } },
      ),
    ),
  );
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  await user.type(await screen.findByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Too many attempts. Try again later.");
});

it("does not show the sign-in form while checking the session", async () => {
  signedOut();
  let answer = () => {};
  server.use(
    http.get(
      apiUrl("/auth/whoami"),
      () =>
        new Promise<Response>((resolve) => {
          answer = () =>
            resolve(HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }));
        }),
    ),
  );
  window.history.pushState({}, "", "/login");
  render(<App />);
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(screen.queryByRole("button", { name: "Sign in" })).not.toBeInTheDocument();
  answer();
  expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
});

it("only returns to pages on this site", () => {
  expect(safeNext("/vms/web-01?tab=console")).toBe("/vms/web-01?tab=console");
  expect(safeNext("//evil.example")).toBe("/");
  expect(safeNext("https://evil.example")).toBe("/");
  expect(safeNext(null)).toBe("/");
  // 브라우저는 `\`를 `/`로 읽고 탭·줄바꿈을 지워 `//evil.example`로 만든다.
  expect(safeNext("/\\evil.example")).toBe("/");
  expect(safeNext("/\t/evil.example")).toBe("/");
  expect(safeNext("/\n/evil.example")).toBe("/");
  expect(safeNext("vms")).toBe("/");
  expect(safeNext("/vms#console")).toBe("/vms#console");
});
```

`web/src/App.tsx`에서

```tsx
import { NotFoundPage } from "@/pages/NotFoundPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
```

을 다음으로 바꾼다:

```tsx
import { NotFoundPage } from "@/pages/NotFoundPage";
import { RealmsPage } from "@/realms/RealmsPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "users", element: <UsersPage /> },
            { path: "tokens", element: <TokensPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "users", element: <UsersPage /> },
            { path: "realms", element: <RealmsPage /> },
            { path: "tokens", element: <TokensPage /> },
```

`web/src/accounts/ChangePasswordDialog.test.tsx`에서

```tsx
  expect(router.state.location.search).toBe("");
});
```

을 다음으로 바꾼다:

```tsx
  expect(router.state.location.search).toBe("");
});

it("does not offer a password change to external users", async () => {
  // 외부 사용자(`<이름>@<영역>`)의 비밀번호는 디렉터리가 맡는다.
  renderPage(<AppLayout />, { as: { ...ADMIN, name: "alice@corp", role: "operator" } });
  await userEvent.click(await screen.findByRole("button", { name: "Account (alice@corp)" }));
  expect(await screen.findByRole("menuitem", { name: "Sign out" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Change my password" })).not.toBeInTheDocument();
});
```

`web/src/accounts/UsersPage.test.tsx`에서

```tsx
    expect(screen.queryByRole("cell", { name: "kim" })).not.toBeInTheDocument(),
  );
});
```

을 다음으로 바꾼다:

```tsx
    expect(screen.queryByRole("cell", { name: "kim" })).not.toBeInTheDocument(),
  );
});

it("shows the realm of each user and no password reset for external users", async () => {
  const ALICE = {
    id: "00000000-0000-0000-0000-0000000000a1",
    name: "alice@corp",
    realm: "corp",
    role: "operator",
    revision: 3,
  };
  users([ME, KIM, ALICE]);
  renderPage(<UsersPage />);
  const table = await screen.findByRole("table", { name: "Users" });
  expect(within(table).getByRole("columnheader", { name: "Realm" })).toBeInTheDocument();
  // 로컬 사용자는 `local`이다.
  const kim = (await within(table).findByText("kim")).closest("tr")!;
  expect(within(kim).getByRole("cell", { name: "local" })).toBeInTheDocument();
  const alice = within(table).getByText("alice@corp").closest("tr")!;
  expect(within(alice).getByRole("cell", { name: "corp" })).toBeInTheDocument();

  // 외부 사용자의 비밀번호는 디렉터리가 맡는다. 역할 바꾸기와 지우기는 그대로다.
  await userEvent.click(within(alice).getByRole("button", { name: "Actions for alice@corp" }));
  expect(await screen.findByRole("menuitem", { name: "Change role" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Delete" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Reset password" })).not.toBeInTheDocument();
  await userEvent.keyboard("{Escape}");

  await userEvent.click(within(kim).getByRole("button", { name: "Actions for kim" }));
  expect(await screen.findByRole("menuitem", { name: "Reset password" })).toBeInTheDocument();
});
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
            <TableHead>{t("users.role")}</TableHead>
            <TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("users.role")}</TableHead>
            <TableHead>{t("users.realm")}</TableHead>
            <TableHead>
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
          {(users.data ?? []).length === 0 && (
            <TableMessage colSpan={3} query={users} empty={t("users.empty")} />
          )}
```

을 다음으로 바꾼다:

```tsx
          {(users.data ?? []).length === 0 && (
            <TableMessage colSpan={4} query={users} empty={t("users.empty")} />
          )}
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
                <TableCell>{t(`roles.${user.role}`)}</TableCell>
                <TableCell className="text-right">
```

을 다음으로 바꾼다:

```tsx
                <TableCell>{t(`roles.${user.role}`)}</TableCell>
                <TableCell>{user.realm ?? "local"}</TableCell>
                <TableCell className="text-right">
```

`web/src/accounts/UsersPage.tsx`에서

```tsx
                      </ReasonMenuItem>
                      <ReasonMenuItem
                        reason={self ? "users.reason.selfPassword" : null}
                        onSelect={() => setDialog({ kind: "password", user })}
                      >
                        {t("users.resetPassword")}
                      </ReasonMenuItem>
                      <DropdownMenuSeparator />
```

을 다음으로 바꾼다:

```tsx
                      </ReasonMenuItem>
                      {/* 외부 사용자의 비밀번호는 영역의 디렉터리가 맡는다. */}
                      {!user.realm && (
                        <ReasonMenuItem
                          reason={self ? "users.reason.selfPassword" : null}
                          onSelect={() => setDialog({ kind: "password", user })}
                        >
                          {t("users.resetPassword")}
                        </ReasonMenuItem>
                      )}
                      <DropdownMenuSeparator />
```

`web/src/auth/AuthProvider.tsx`에서

```tsx
  signedOut: boolean;
  login(username: string, password: string): Promise<void>;
  logout(): Promise<void>;
```

을 다음으로 바꾼다:

```tsx
  signedOut: boolean;
  /** `realm`이 없으면 로컬 사용자로 로그인한다. */
  login(username: string, password: string, realm?: string): Promise<void>;
  logout(): Promise<void>;
```

`web/src/auth/AuthProvider.tsx`에서

```tsx

  const login = useCallback(async (username: string, password: string) => {
    const mine = ++epoch.current;
    await call(api.POST("/auth/login", { body: { username, password } }));
    const p = await call(api.GET("/auth/whoami"));
```

을 다음으로 바꾼다:

```tsx

  const login = useCallback(async (username: string, password: string, realm?: string) => {
    const mine = ++epoch.current;
    await call(api.POST("/auth/login", { body: { username, password, ...(realm && { realm }) } }));
    const p = await call(api.GET("/auth/whoami"));
```

`web/src/i18n/en.json`에서

```json
    "invalid_subscription_key": "The subscription key is not valid: {{message}}",
    "telemetry_endpoint_required": "Set a telemetry endpoint first. This build has no default endpoint."
  },
```

을 다음으로 바꾼다:

```json
    "invalid_subscription_key": "The subscription key is not valid: {{message}}",
    "telemetry_endpoint_required": "Set a telemetry endpoint first. This build has no default endpoint.",
    "no_role": "Your directory account has no role in opLite. Ask an administrator to map one of your groups.",
    "realm_unavailable": "Cannot reach the directory of this realm. Try again later or ask an administrator.",
    "external_user": "This user signs in through a realm. Change the password in the directory."
  },
```

`web/src/i18n/en.json`에서

```json
    "users": "Users",
    "tokens": "API tokens",
```

을 다음으로 바꾼다:

```json
    "users": "Users",
    "realms": "Realms",
    "tokens": "API tokens",
```

`web/src/i18n/en.json`에서

```json
    "submit": "Sign in",
    "version": "opLite {{version}}"
  },
```

을 다음으로 바꾼다:

```json
    "submit": "Sign in",
    "version": "opLite {{version}}",
    "realm": "Realm",
    "localRealm": "Local users"
  },
```

`web/src/i18n/en.json`에서

```json
    "role": "Role",
    "you": "(you)",
```

을 다음으로 바꾼다:

```json
    "role": "Role",
    "realm": "Realm",
    "you": "(you)",
```

`web/src/i18n/en.json`에서

```json
      "selfDelete": "Only another administrator can delete your account."
    }
```

을 다음으로 바꾼다:

```json
      "selfDelete": "Only another administrator can delete your account."
    }
  },
  "realms": {
    "pageHint": "Users of a realm sign in with their directory account as <name>@<realm>. Their role comes from their groups at each sign-in.",
    "adminOnly": "Only administrators can manage realms.",
    "empty": "No realms. Only local users can sign in.",
    "display": "Display name",
    "displayHint": "Shown on the sign-in page. Leave empty to show the name.",
    "kind": "Kind",
    "kinds": {
      "ldap": "LDAP"
    },
    "url": "URL",
    "urlHint": "ldap:// always uses StartTLS. ldaps:// uses TLS from the start.",
    "baseDn": "Base DN",
    "bindDn": "Bind DN",
    "bindDnHint": "Service account used to find users. Leave empty to search anonymously.",
    "bindPassword": "Bind password",
    "bindPasswordSaved": "A password is saved. Leave this empty to keep it.",
    "clearBindPassword": "Clear the saved password",
    "userFilter": "User filter",
    "userFilterHint": "{username} is replaced with the escaped sign-in name.",
    "groupAttr": "Group attribute",
    "caPem": "CA certificate (PEM)",
    "caPemHint": "Trust only this CA for the directory's certificate. Leave empty to use the system roots.",
    "timeout": "Timeout (seconds)",
    "groups": "Group to role mapping",
    "groupsHint": "Group DNs match without regard to case. A user in several groups gets the highest role.",
    "groupN": "Group {{n}}",
    "groupRoleN": "Role of group {{n}}",
    "removeGroupN": "Remove group {{n}}",
    "addGroup": "Add group",
    "defaultRole": "Default role",
    "noDefaultRole": "None (refuse sign-in)",
    "createTitle": "Create an LDAP realm",
    "editTitle": "Edit realm {{name}}",
    "formDescription": "The role is set again from the mapping each time a user signs in.",
    "editNamed": "Edit {{name}}",
    "created": "Created realm {{name}}.",
    "saved": "Saved realm {{name}}.",
    "deleted": "Deleted realm {{name}}.",
    "deleteTitle": "Delete realm {{name}}?",
    "deleteDescription": "Users of this realm can no longer sign in, and their sessions and API tokens stop working. The users stay; creating a realm with the same name lets them in again.",
    "test": "Test",
    "testNamed": "Test {{name}}",
    "testTitle": "Test realm {{name}}",
    "testDescription": "Checks each step of signing in as a directory user. No user or session is created.",
    "steps": "Steps",
    "step": "Step",
    "result": "Result",
    "message": "Message",
    "ok": "OK",
    "failed": "Failed",
    "stepNames": {
      "connect": "Connect",
      "service_bind": "Service account bind",
      "search": "Find the user",
      "user_bind": "User bind",
      "groups": "Groups"
    },
    "dn": "DN",
    "groupsFound": "Groups",
    "noRole": "No role: sign-in would be refused.",
    "errors": {
      "local": "local is reserved for local users.",
      "taken": "A realm with this name already exists.",
      "url": "Use ldap://host[:port] (with StartTLS) or ldaps://host[:port].",
      "required": "Enter a value.",
      "userFilter": "Include {username} exactly once.",
      "bindDn": "Enter a bind DN to use a bind password.",
      "bindPasswordNeeded": "Enter a bind password for the bind DN, or clear the bind DN to search anonymously.",
      "bindPasswordAgain": "Enter the bind password again: the URL, bind DN or CA certificate changed.",
      "clearNeedsNoBindDn": "To clear the saved password, also clear the bind DN.",
      "clearSaved": "The bind DN is empty: choose \"Clear the saved password\" to search anonymously.",
      "timeout": "Use a whole number of seconds from 1 to 30.",
      "group": "Enter a group DN or remove the row."
    }
```

`web/src/i18n/ko.json`에서

```json
    "invalid_subscription_key": "구독 키가 올바르지 않습니다: {{message}}",
    "telemetry_endpoint_required": "먼저 텔레메트리 끝점을 정하세요. 이 빌드에는 기본 끝점이 없습니다."
  },
```

을 다음으로 바꾼다:

```json
    "invalid_subscription_key": "구독 키가 올바르지 않습니다: {{message}}",
    "telemetry_endpoint_required": "먼저 텔레메트리 끝점을 정하세요. 이 빌드에는 기본 끝점이 없습니다.",
    "no_role": "디렉터리 계정에 opLite 역할이 없습니다. 관리자에게 그룹의 역할 매핑을 요청하세요.",
    "realm_unavailable": "이 영역의 디렉터리에 연결할 수 없습니다. 잠시 뒤 다시 시도하거나 관리자에게 문의하세요.",
    "external_user": "영역으로 로그인하는 사용자입니다. 비밀번호는 디렉터리에서 바꾸세요."
  },
```

`web/src/i18n/ko.json`에서

```json
    "users": "사용자",
    "tokens": "API 토큰",
```

을 다음으로 바꾼다:

```json
    "users": "사용자",
    "realms": "인증 영역",
    "tokens": "API 토큰",
```

`web/src/i18n/ko.json`에서

```json
    "submit": "로그인",
    "version": "opLite {{version}}"
  },
```

을 다음으로 바꾼다:

```json
    "submit": "로그인",
    "version": "opLite {{version}}",
    "realm": "인증 영역",
    "localRealm": "로컬 사용자"
  },
```

`web/src/i18n/ko.json`에서

```json
    "role": "역할",
    "you": "(나)",
```

을 다음으로 바꾼다:

```json
    "role": "역할",
    "realm": "영역",
    "you": "(나)",
```

`web/src/i18n/ko.json`에서

```json
      "selfDelete": "내 계정은 다른 관리자가 삭제합니다."
    }
```

을 다음으로 바꾼다:

```json
      "selfDelete": "내 계정은 다른 관리자가 삭제합니다."
    }
  },
  "realms": {
    "pageHint": "영역의 사용자는 디렉터리 계정으로 로그인하고 <이름>@<영역>이 됩니다. 역할은 로그인할 때마다 그룹으로 정해집니다.",
    "adminOnly": "인증 영역은 관리자만 관리할 수 있습니다.",
    "empty": "영역이 없습니다. 로컬 사용자만 로그인합니다.",
    "display": "표시 이름",
    "displayHint": "로그인 화면에 보입니다. 비우면 이름이 보입니다.",
    "kind": "종류",
    "kinds": {
      "ldap": "LDAP"
    },
    "url": "URL",
    "urlHint": "ldap://는 늘 StartTLS를 쓰고, ldaps://는 처음부터 TLS를 씁니다.",
    "baseDn": "기준 DN",
    "bindDn": "바인드 DN",
    "bindDnHint": "사용자를 찾을 서비스 계정입니다. 비우면 익명으로 찾습니다.",
    "bindPassword": "바인드 비밀번호",
    "bindPasswordSaved": "저장된 비밀번호가 있습니다. 비워 두면 그대로 둡니다.",
    "clearBindPassword": "저장된 비밀번호 지우기",
    "userFilter": "사용자 필터",
    "userFilterHint": "{username}은 이스케이프한 로그인 이름으로 바뀝니다.",
    "groupAttr": "그룹 속성",
    "caPem": "CA 인증서(PEM)",
    "caPemHint": "디렉터리 인증서를 이 CA로만 확인합니다. 비우면 시스템 루트를 씁니다.",
    "timeout": "시간 제한(초)",
    "groups": "그룹별 역할",
    "groupsHint": "그룹 DN은 대소문자를 가리지 않고 비교합니다. 여러 그룹에 속하면 가장 높은 역할을 받습니다.",
    "groupN": "그룹 {{n}}",
    "groupRoleN": "그룹 {{n}}의 역할",
    "removeGroupN": "그룹 {{n}} 빼기",
    "addGroup": "그룹 더하기",
    "defaultRole": "기본 역할",
    "noDefaultRole": "없음(로그인 거절)",
    "createTitle": "LDAP 영역 만들기",
    "editTitle": "영역 {{name}} 고치기",
    "formDescription": "역할은 사용자가 로그인할 때마다 매핑으로 다시 정해집니다.",
    "editNamed": "{{name}} 고치기",
    "created": "영역 {{name}}을(를) 만들었습니다.",
    "saved": "영역 {{name}}을(를) 저장했습니다.",
    "deleted": "영역 {{name}}을(를) 삭제했습니다.",
    "deleteTitle": "영역 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "이 영역의 사용자는 더 로그인할 수 없고, 세션과 API 토큰도 멈춥니다. 사용자는 남으며 같은 이름의 영역을 다시 만들면 다시 로그인할 수 있습니다.",
    "test": "시험",
    "testNamed": "{{name}} 시험",
    "testTitle": "영역 {{name}} 시험",
    "testDescription": "디렉터리 사용자로 로그인하는 단계를 하나씩 확인합니다. 사용자나 세션은 만들지 않습니다.",
    "steps": "단계",
    "step": "단계",
    "result": "결과",
    "message": "내용",
    "ok": "성공",
    "failed": "실패",
    "stepNames": {
      "connect": "연결",
      "service_bind": "서비스 계정 바인드",
      "search": "사용자 찾기",
      "user_bind": "사용자 바인드",
      "groups": "그룹"
    },
    "dn": "DN",
    "groupsFound": "그룹",
    "noRole": "역할 없음: 로그인이 거절됩니다.",
    "errors": {
      "local": "local은 로컬 사용자용으로 예약된 이름입니다.",
      "taken": "같은 이름의 영역이 이미 있습니다.",
      "url": "ldap://호스트[:포트](StartTLS) 또는 ldaps://호스트[:포트] 형식으로 쓰세요.",
      "required": "값을 입력하세요.",
      "userFilter": "{username}을 정확히 한 번 넣으세요.",
      "bindDn": "바인드 비밀번호를 쓰려면 바인드 DN을 입력하세요.",
      "bindPasswordNeeded": "바인드 DN에는 바인드 비밀번호가 필요합니다. 익명으로 찾으려면 바인드 DN을 비우세요.",
      "bindPasswordAgain": "URL·바인드 DN·CA 인증서를 바꿨으니 바인드 비밀번호를 다시 넣으세요.",
      "clearNeedsNoBindDn": "저장된 비밀번호를 지우려면 바인드 DN도 비우세요.",
      "clearSaved": "바인드 DN을 비웠으니 익명으로 찾으려면 \"저장된 비밀번호 지우기\"를 고르세요.",
      "timeout": "1~30 사이의 정수(초)로 쓰세요.",
      "group": "그룹 DN을 입력하거나 행을 빼세요."
    }
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.getByRole("link", { name: "Audit log" })).toHaveAttribute("href", "/audit");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("link", { name: "Audit log" })).toHaveAttribute("href", "/audit");
  expect(screen.getByRole("link", { name: "Realms" })).toHaveAttribute("href", "/realms");
});
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Audit log" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Audit log" })).not.toBeInTheDocument();
  // 인증 영역도 계정 종류처럼 admin만 다룬다.
  expect(screen.queryByRole("link", { name: "Realms" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  HardDrive,
  Import,
```

을 다음으로 바꾼다:

```tsx
  HardDrive,
  IdCard,
  Import,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
  { to: "/tokens", key: "nav.tokens", icon: KeyRound, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/users", key: "nav.users", icon: Users, end: false, kind: "user" },
  { to: "/realms", key: "nav.realms", icon: IdCard, end: false, kind: "realm" },
  { to: "/tokens", key: "nav.tokens", icon: KeyRound, end: false },
```

`web/src/layout/AppLayout.tsx`에서

```tsx
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => setChanging(true)}>
            <Lock />
            {t("accounts.changePassword")}
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void logout()}>
```

을 다음으로 바꾼다:

```tsx
          <DropdownMenuSeparator />
          {/* 외부 사용자(이름에 `@<영역>`)의 비밀번호는 영역의 디렉터리가 맡는다. */}
          {!me.name.includes("@") && (
            <DropdownMenuItem onSelect={() => setChanging(true)}>
              <Lock />
              {t("accounts.changePassword")}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem onSelect={() => void logout()}>
```

`web/src/pages/LoginPage.tsx`에서

```tsx
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";
```

을 다음으로 바꾼다:

```tsx
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";
```

`web/src/pages/LoginPage.tsx`에서

```tsx
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
```

을 다음으로 바꾼다:

```tsx
  const [password, setPassword] = useState("");
  const [realm, setRealm] = useState("local");
  const [error, setError] = useState<string | null>(null);
```

`web/src/pages/LoginPage.tsx`에서

```tsx
  });
```

을 다음으로 바꾼다:

```tsx
  });
  // 고를 영역도 로그인 없이 읽는다. 읽지 못하면(옛 서버) 로컬로만 로그인한다.
  const realms = useQuery({
    queryKey: ["login-realms"],
    queryFn: () => call(api.GET("/auth/realms")),
    retry: false,
  });
  const choices = realms.data ?? [];
```

`web/src/pages/LoginPage.tsx`에서

```tsx
    try {
      await login(username, password);
    } catch (err) {
```

을 다음으로 바꾼다:

```tsx
    try {
      await login(username, password, realm === "local" ? undefined : realm);
    } catch (err) {
```

`web/src/pages/LoginPage.tsx`에서

```tsx
          <form onSubmit={submit} className="flex flex-col gap-4">
            <div className="flex flex-col gap-2">
```

을 다음으로 바꾼다:

```tsx
          <form onSubmit={submit} className="flex flex-col gap-4">
            {/* 영역이 로컬뿐이면 고르지 않는다. */}
            {choices.length > 1 && (
              <div className="flex flex-col gap-2">
                <Label htmlFor="realm">{t("login.realm")}</Label>
                <NativeSelect
                  id="realm"
                  className="w-full"
                  value={realm}
                  onChange={(e) => setRealm(e.target.value)}
                >
                  {choices.map((r) => (
                    <NativeSelectOption key={r.name} value={r.name}>
                      {r.kind === "local" ? t("login.localRealm") : (r.display ?? r.name)}
                    </NativeSelectOption>
                  ))}
                </NativeSelect>
              </div>
            )}
            <div className="flex flex-col gap-2">
```

`web/src/realms/RealmsPage.test.tsx`를 만든다:

```tsx
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { RealmsPage } from "@/realms/RealmsPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const CORP = {
  id: "00000000-0000-0000-0000-0000000000c1",
  name: "corp",
  revision: 7,
  display: "Corp directory",
  backend: {
    kind: "ldap",
    url: "ldaps://ldap.corp.example",
    base_dn: "dc=corp,dc=example",
    bind_dn: "cn=svc,dc=corp,dc=example",
    has_bind_password: true,
    user_filter: "(&(objectClass=person)(uid={username}))",
    group_attr: "memberOf",
    timeout_secs: 10,
  },
  roles: {
    groups: [{ group: "cn=ops,ou=groups,dc=corp,dc=example", role: "operator" }],
    default: "viewer",
  },
};

const LAB = {
  ...CORP,
  id: "00000000-0000-0000-0000-0000000000c2",
  name: "lab",
  revision: 3,
  display: undefined,
  backend: { ...CORP.backend, url: "ldap://10.0.0.5:389", has_bind_password: false },
  roles: { groups: [] },
};

/** 영역 목록. 돌려받은 함수로 다음 읽기의 목록을 바꾼다. */
function realms(list: unknown[]) {
  let items = list;
  server.use(http.get(apiUrl("/realms"), () => HttpResponse.json({ revision: 1, items })));
  return (next: unknown[]) => {
    items = next;
  };
}

it("lists realms by name", async () => {
  realms([LAB, CORP]);
  renderPage(<RealmsPage />);
  const table = await screen.findByRole("table", { name: "Realms" });
  await within(table).findByText("corp");
  const rows = within(table).getAllByRole("row");
  expect(
    rows.map((r) =>
      within(r)
        .queryAllByRole("cell")
        .map((c) => c.textContent),
    ),
  ).toEqual([
    [],
    ["corp", "Corp directory", "LDAP", "ldaps://ldap.corp.example", expect.any(String)],
    ["lab", "—", "LDAP", "ldap://10.0.0.5:389", expect.any(String)],
  ]);
});

it("tells other roles that only admins manage realms", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/realms"), () => {
      reads += 1;
      return HttpResponse.json({ revision: 1, items: [CORP] });
    }),
  );
  renderPage(<RealmsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("Only administrators can manage realms.")).toBeInTheDocument();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(reads).toBe(0);
});

it("creates an LDAP realm with a group mapping", async () => {
  const change = realms([CORP]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/realms"), async ({ request }) => {
      body = await request.json();
      change([CORP, LAB]);
      return HttpResponse.json(LAB, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await screen.findByText("corp");
  await user.click(screen.getByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create an LDAP realm" });
  // 서버의 기본값으로 시작한다.
  expect(within(dialog).getByLabelText("User filter")).toHaveValue(
    "(&(objectClass=person)(uid={username}))",
  );
  expect(within(dialog).getByLabelText("Group attribute")).toHaveValue("memberOf");
  expect(within(dialog).getByLabelText("Timeout (seconds)")).toHaveValue("10");

  // `local`은 예약된 이름이고, 주소와 base DN이 있어야 하며, 시간 제한은 30초까지다.
  await user.type(within(dialog).getByLabelText("Name"), "local");
  await user.clear(within(dialog).getByLabelText("Timeout (seconds)"));
  await user.type(within(dialog).getByLabelText("Timeout (seconds)"), "31");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("local is reserved for local users.")).toBeVisible();
  expect(within(dialog).getByText("Use a whole number of seconds from 1 to 30.")).toBeVisible();
  expect(
    within(dialog).getByText("Use ldap://host[:port] (with StartTLS) or ldaps://host[:port]."),
  ).toBeVisible();
  expect(body).toBeUndefined();

  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "lab");
  await user.type(within(dialog).getByLabelText("Display name"), "Lab LDAP");
  await user.type(within(dialog).getByLabelText("URL"), "ldap://10.0.0.5:389");
  await user.type(within(dialog).getByLabelText("Base DN"), "dc=lab,dc=example");
  await user.type(within(dialog).getByLabelText("Bind DN"), "cn=svc,dc=lab,dc=example");
  await user.type(within(dialog).getByLabelText("Bind password"), "svc-secret");
  await user.type(within(dialog).getByLabelText("CA certificate (PEM)"), "-----BEGIN CERT");
  await user.clear(within(dialog).getByLabelText("Timeout (seconds)"));
  await user.type(within(dialog).getByLabelText("Timeout (seconds)"), "5");
  // 그룹 셋을 더하고 가운데 것을 뺀다. 빈 그룹 행의 오류는 입력의 설명이다.
  for (let i = 0; i < 3; i++) {
    await user.click(within(dialog).getByRole("button", { name: "Add group" }));
  }
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Group 1")).toHaveAccessibleDescription(
    "Enter a group DN or remove the row.",
  );
  expect(within(dialog).getByLabelText("Group 1")).toBeInvalid();
  expect(body).toBeUndefined();
  await user.type(within(dialog).getByLabelText("Group 1"), "cn=admins,dc=lab,dc=example");
  await user.selectOptions(within(dialog).getByLabelText("Role of group 1"), "admin");
  await user.type(within(dialog).getByLabelText("Group 2"), "cn=gone,dc=lab,dc=example");
  await user.type(within(dialog).getByLabelText("Group 3"), "cn=ops,dc=lab,dc=example");
  await user.selectOptions(within(dialog).getByLabelText("Role of group 3"), "operator");
  await user.click(within(dialog).getByRole("button", { name: "Remove group 2" }));
  await user.selectOptions(within(dialog).getByLabelText("Default role"), "viewer");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));

  expect(await screen.findByText("Created realm lab.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "lab",
    display: "Lab LDAP",
    backend: {
      kind: "ldap",
      url: "ldap://10.0.0.5:389",
      base_dn: "dc=lab,dc=example",
      bind_dn: "cn=svc,dc=lab,dc=example",
      bind_password: "svc-secret",
      user_filter: "(&(objectClass=person)(uid={username}))",
      group_attr: "memberOf",
      ca_pem: "-----BEGIN CERT",
      timeout_secs: 5,
    },
    roles: {
      groups: [
        { group: "cn=admins,dc=lab,dc=example", role: "admin" },
        { group: "cn=ops,dc=lab,dc=example", role: "operator" },
      ],
      default: "viewer",
    },
  });
  expect(await screen.findByText("lab")).toBeInTheDocument();
});

it("keeps, replaces or clears the bind password when editing", async () => {
  realms([CORP]);
  const sent: { ifMatch: string | null; body: { backend: Record<string, unknown> } }[] = [];
  server.use(
    http.put(apiUrl("/realms/corp"), async ({ request }) => {
      sent.push({
        ifMatch: request.headers.get("If-Match"),
        body: (await request.json()) as { backend: Record<string, unknown> },
      });
      return HttpResponse.json(CORP);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);

  async function edit() {
    await user.click(await screen.findByRole("button", { name: "Edit corp" }));
    return screen.findByRole("dialog", { name: "Edit realm corp" });
  }
  async function save(dialog: HTMLElement) {
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }

  // 비워 두면 보내지 않아 저장된 비밀번호가 남는다.
  let dialog = await edit();
  expect(within(dialog).getByLabelText("Name")).toHaveValue("corp");
  expect(within(dialog).getByLabelText("Name")).toHaveAttribute("readonly");
  expect(within(dialog).getByLabelText("Bind password")).toHaveValue("");
  expect(
    within(dialog).getByText("A password is saved. Leave this empty to keep it."),
  ).toBeVisible();
  expect(within(dialog).getByLabelText("Group 1")).toHaveValue(
    "cn=ops,ou=groups,dc=corp,dc=example",
  );
  expect(within(dialog).getByLabelText("Default role")).toHaveValue("viewer");
  await user.clear(within(dialog).getByLabelText("Display name"));
  await save(dialog);
  expect(await screen.findByText("Saved realm corp.")).toBeInTheDocument();
  expect(sent[0]!.ifMatch).toBe("7");
  expect(sent[0]!.body).toEqual({
    backend: {
      kind: "ldap",
      url: "ldaps://ldap.corp.example",
      base_dn: "dc=corp,dc=example",
      bind_dn: "cn=svc,dc=corp,dc=example",
      user_filter: "(&(objectClass=person)(uid={username}))",
      group_attr: "memberOf",
      timeout_secs: 10,
    },
    roles: {
      groups: [{ group: "cn=ops,ou=groups,dc=corp,dc=example", role: "operator" }],
      default: "viewer",
    },
  });

  // 새 비밀번호를 넣으면 바꾼다.
  dialog = await edit();
  await user.type(within(dialog).getByLabelText("Bind password"), "new-secret");
  await save(dialog);
  expect(sent[1]!.body.backend.bind_password).toBe("new-secret");

  // 지우기를 고르면 빈 글을 보낸다(입력은 막힌다). 바인드 DN도 비워야 한다(익명 검색).
  dialog = await edit();
  await user.click(within(dialog).getByLabelText("Clear the saved password"));
  expect(within(dialog).getByLabelText("Bind password")).toBeDisabled();
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText("To clear the saved password, also clear the bind DN."),
  ).toBeVisible();
  expect(sent).toHaveLength(2);
  await user.clear(within(dialog).getByLabelText("Bind DN"));
  await save(dialog);
  expect(sent[2]!.body.backend.bind_password).toBe("");
  expect(sent[2]!.body.backend).not.toHaveProperty("bind_dn");
  expect(sent).toHaveLength(3);
});

it("asks for the bind password again when the server changes", async () => {
  realms([CORP, LAB]);
  const sent: { backend: Record<string, unknown> }[] = [];
  server.use(
    http.put(apiUrl("/realms/corp"), async ({ request }) => {
      sent.push((await request.json()) as { backend: Record<string, unknown> });
      return HttpResponse.json(CORP);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  const again = "Enter the bind password again: the URL, bind DN or CA certificate changed.";

  // URL·바인드 DN·CA 가운데 하나라도 바꾸면 비밀번호를 다시 넣어야 한다(서버의 400과 같은 규칙).
  for (const [label, value] of [
    ["URL", "ldaps://other.example"],
    ["Bind DN", "cn=other,dc=corp,dc=example"],
    ["CA certificate (PEM)", "-----BEGIN CERT"],
  ] as const) {
    await user.click(await screen.findByRole("button", { name: "Edit corp" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit realm corp" });
    await user.clear(within(dialog).getByLabelText(label));
    await user.type(within(dialog).getByLabelText(label), value);
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(within(dialog).getByText(again)).toBeVisible();
    expect(sent).toHaveLength(0);
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }

  // 바인드 DN만 비우면 비밀번호를 다시 넣을 것이 아니라 저장된 비밀번호 지우기를 골라야 한다.
  await user.click(await screen.findByRole("button", { name: "Edit corp" }));
  const cleared = await screen.findByRole("dialog", { name: "Edit realm corp" });
  await user.clear(within(cleared).getByLabelText("Bind DN"));
  await user.click(within(cleared).getByRole("button", { name: "Save" }));
  expect(
    within(cleared).getByText(
      'The bind DN is empty: choose "Clear the saved password" to search anonymously.',
    ),
  ).toBeVisible();
  expect(within(cleared).queryByText(again)).not.toBeInTheDocument();
  expect(sent).toHaveLength(0);
  await user.click(within(cleared).getByLabelText("Clear the saved password"));
  await user.click(within(cleared).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(cleared).not.toBeInTheDocument());
  expect(sent[0]!.backend.bind_password).toBe("");
  expect(sent[0]!.backend).not.toHaveProperty("bind_dn");

  // 새 비밀번호와 함께면 보낸다.
  await user.click(await screen.findByRole("button", { name: "Edit corp" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit realm corp" });
  await user.clear(within(dialog).getByLabelText("URL"));
  await user.type(within(dialog).getByLabelText("URL"), "ldaps://other.example");
  await user.type(within(dialog).getByLabelText("Bind password"), "svc-secret");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(sent[1]!.backend).toMatchObject({
    url: "ldaps://other.example",
    bind_password: "svc-secret",
  });

  // 바인드 DN이 있는데 저장된 비밀번호가 없으면 넣어야 한다.
  await user.click(await screen.findByRole("button", { name: "Edit lab" }));
  const lab = await screen.findByRole("dialog", { name: "Edit realm lab" });
  await user.click(within(lab).getByRole("button", { name: "Save" }));
  expect(
    within(lab).getByText(
      "Enter a bind password for the bind DN, or clear the bind DN to search anonymously.",
    ),
  ).toBeVisible();
  expect(within(lab).getByLabelText("Bind password")).toBeInvalid();
});

it("deletes a realm after confirming", async () => {
  const change = realms([CORP, LAB]);
  let deleted = false;
  server.use(
    http.delete(apiUrl("/realms/lab"), () => {
      deleted = true;
      change([CORP]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Delete lab" }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent("Delete realm lab?");
  await user.click(within(confirm).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted realm lab.")).toBeInTheDocument();
  expect(deleted).toBe(true);
  await waitFor(() => expect(screen.queryByRole("cell", { name: "lab" })).not.toBeInTheDocument());
});

it("tests a realm and shows each step", async () => {
  realms([CORP]);
  const bodies: unknown[] = [];
  const results = [
    {
      steps: [
        { name: "connect", ok: true, message: "connected" },
        { name: "service_bind", ok: true, message: "bound as cn=svc" },
        { name: "search", ok: true, message: "found 1 entry" },
        { name: "user_bind", ok: true, message: "password accepted" },
        { name: "groups", ok: true, message: "2 groups" },
      ],
      dn: "uid=alice,ou=people,dc=corp,dc=example",
      groups: ["cn=ops,ou=groups,dc=corp,dc=example", "cn=all,ou=groups,dc=corp,dc=example"],
      role: "operator",
    },
    {
      steps: [
        { name: "connect", ok: true, message: "connected" },
        { name: "service_bind", ok: false, message: "invalid credentials" },
      ],
      dn: null,
      groups: [],
      role: null,
    },
  ];
  server.use(
    http.post(apiUrl("/realms/corp/actions/test"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(results.shift());
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Test corp" }));
  const dialog = await screen.findByRole("dialog", { name: "Test realm corp" });
  await user.type(within(dialog).getByLabelText("Username"), "alice");
  await user.type(within(dialog).getByLabelText("Password"), "alice-secret");
  await user.click(within(dialog).getByRole("button", { name: "Test" }));

  const steps = await within(dialog).findByRole("table", { name: "Steps" });
  expect(
    within(steps)
      .getAllByRole("row")
      .slice(1)
      .map((r) =>
        within(r)
          .getAllByRole("cell")
          .map((c) => c.textContent),
      ),
  ).toEqual([
    ["Connect", "OK", "connected"],
    ["Service account bind", "OK", "bound as cn=svc"],
    ["Find the user", "OK", "found 1 entry"],
    ["User bind", "OK", "password accepted"],
    ["Groups", "OK", "2 groups"],
  ]);
  expect(within(dialog).getByText("uid=alice,ou=people,dc=corp,dc=example")).toBeVisible();
  expect(within(dialog).getByText("cn=all,ou=groups,dc=corp,dc=example")).toBeVisible();
  expect(within(dialog).getByText("Operator")).toBeVisible();
  expect(bodies).toEqual([{ username: "alice", password: "alice-secret" }]);

  // 실패한 단계에서 멈추고, 역할이 없으면 로그인이 거절된다고 알린다.
  await user.click(within(dialog).getByRole("button", { name: "Test" }));
  await waitFor(() =>
    expect(within(dialog).getByRole("table", { name: "Steps" })).toHaveTextContent(
      "invalid credentials",
    ),
  );
  expect(within(dialog).getByRole("table", { name: "Steps" })).toHaveTextContent("Failed");
  expect(within(dialog).getByText("No role: sign-in would be refused.")).toBeVisible();
  // 빈 DN과 그룹은 같은 "—"(고정폭 글꼴이 아님)
  const blanks = within(dialog).getAllByText("—");
  expect(blanks).toHaveLength(2);
  for (const blank of blanks) {
    expect(blank).not.toHaveClass("font-mono");
  }
  // 영역 시험은 사용자를 만들지 않는다. 대화상자는 닫을 때까지 남는다.
  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
});
```

`web/src/realms/RealmsPage.tsx`를 만든다:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FlaskConical, Pencil, Plus, Trash2, X } from "lucide-react";
import { useRef, useState, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Role } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { focusBack } from "@/components/focusBack";
import { FormDialog } from "@/components/FormDialog";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import {
  checkForm,
  emptyForm,
  formOf,
  type RealmForm,
  type RealmTestResult,
  type RealmView,
  requestOf,
} from "@/realms/realms";

const ROLES: readonly Role[] = ["admin", "operator", "viewer"];

type Dialogs = { kind: "create" } | { kind: "edit" | "test"; realm: RealmView };

/**
 * 인증 영역: 목록과 LDAP 영역 만들기·고치기·지우기·시험. admin만 다룬다. 서비스 계정 비밀번호가 있어
 * watch로 받지 않으므로 화면을 열 때와 바꾼 뒤에 다시 읽는다.
 */
export function RealmsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const admin = can(me.role, "read", "realm");
  const queryClient = useQueryClient();
  const realms = useQuery({
    queryKey: ["realms"],
    queryFn: () => call(api.GET("/realms")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    enabled: admin,
  });
  const [dialog, setDialog] = useState<Dialogs | null>(null);
  const [deleting, setDeleting] = useState<RealmView | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const close = () => setDialog(null);
  const changed = () => queryClient.invalidateQueries({ queryKey: ["realms"] });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. 관리자가 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.realms")}</h1>
        <p className="text-sm text-muted-foreground">{t("realms.adminOnly")}</p>
      </div>
    );
  }

  const list = realms.data ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="realms-title" className="text-2xl font-semibold">
          {t("nav.realms")}
        </h1>
        <Button
          disabled={!realms.isSuccess}
          onClick={(e) => {
            opener.current = e.currentTarget;
            setDialog({ kind: "create" });
          }}
        >
          <Plus />
          {t("common.create")}
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">{t("realms.pageHint")}</p>
      <Table aria-labelledby="realms-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("realms.display")}</TableHead>
            <TableHead>{t("realms.kind")}</TableHead>
            <TableHead>{t("realms.url")}</TableHead>
            <TableHead>
              <span className="sr-only">{t("vms.actions")}</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={5} query={realms} empty={t("realms.empty")} />
          )}
          {list.map((realm) => (
            <TableRow key={realm.id}>
              <TableCell className="font-medium">{realm.name}</TableCell>
              <TableCell>{realm.display ?? "—"}</TableCell>
              <TableCell>{t(`realms.kinds.${realm.backend.kind}`)}</TableCell>
              <TableCell className="font-mono text-xs">{realm.backend.url}</TableCell>
              <TableCell className="text-right whitespace-nowrap">
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={t("realms.testNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ kind: "test", realm });
                  }}
                >
                  <FlaskConical />
                  {t("realms.test")}
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="ml-2"
                  aria-label={t("realms.editNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDialog({ kind: "edit", realm });
                  }}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={t("common.deleteNamed", { name: realm.name })}
                  onClick={(e) => {
                    opener.current = e.currentTarget;
                    setDeleting(realm);
                    setDeleteOpen(true);
                  }}
                >
                  <Trash2 />
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {(dialog?.kind === "create" || dialog?.kind === "edit") && (
        <RealmDialog
          realm={dialog.kind === "edit" ? dialog.realm : null}
          taken={list.map((r) => r.name)}
          returnFocus={opener}
          onClose={close}
          onDone={changed}
        />
      )}
      {dialog?.kind === "test" && (
        <TestDialog realm={dialog.realm} returnFocus={opener} onClose={close} />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("realms.deleteTitle", { name: deleting?.name })}
        description={t("realms.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const realm = deleting!;
          await call(api.DELETE("/realms/{key}", { params: { path: { key: realm.name } } }));
          toast.success(t("realms.deleted", { name: realm.name }));
          await changed();
        }}
      />
    </div>
  );
}

function RoleOptions() {
  const { t } = useTranslation();
  return ROLES.map((role) => (
    <NativeSelectOption key={role} value={role}>
      {t(`roles.${role}`)}
    </NativeSelectOption>
  ));
}

/** LDAP 영역 만들기(`realm`이 null)와 고치기. 열 때만 그린다. */
function RealmDialog({
  realm,
  taken,
  returnFocus,
  onClose,
  onDone,
}: {
  realm: RealmView | null;
  /** 있는 영역 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
  onDone(): Promise<void>;
}) {
  const { t } = useTranslation();
  const [form, setForm] = useState<RealmForm>(() => (realm ? formOf(realm) : emptyForm()));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<RealmForm>) => setForm((f) => ({ ...f, ...part }));
  // 행을 빼면 행 번호로 단 오류가 다른 행을 가리키므로 지운다.
  const changeGroups = (groups: RealmForm["groups"]) => {
    change({ groups });
    setErrors({});
  };
  const saved = realm?.backend.has_bind_password ?? false;

  async function submit() {
    const found = checkForm(form, realm ? null : taken, realm);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const body = requestOf(form, realm !== null);
    if (realm) {
      await call(
        api.PUT("/realms/{key}", {
          params: { path: { key: realm.name }, header: { "If-Match": realm.revision } },
          body,
        }),
      );
      toast.success(t("realms.saved", { name: realm.name }));
    } else {
      await call(api.POST("/realms", { body }));
      toast.success(t("realms.created", { name: form.name }));
    }
    await onDone();
  }

  const text = (id: string, key: keyof RealmForm, label: string, hint?: string) => (
    <Field id={id} label={label} error={errors[key]} hint={hint}>
      {(a11y) => (
        <Input
          id={id}
          autoComplete="off"
          spellCheck={false}
          value={form[key] as string}
          onChange={(e) => change({ [key]: e.target.value })}
          {...a11y}
        />
      )}
    </Field>
  );

  return (
    <FormDialog
      title={realm ? t("realms.editTitle", { name: realm.name }) : t("realms.createTitle")}
      description={t("realms.formDescription")}
      submitLabel={realm ? t("common.save") : t("common.create")}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {/* 항목이 많으므로 대화상자 안에서 넘긴다. */}
      <div className="-mx-1 flex max-h-[60svh] flex-col gap-4 overflow-y-auto px-1">
        <Field id="realm-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="realm-name"
              autoComplete="off"
              readOnly={realm !== null}
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        {text("realm-display", "display", t("realms.display"), t("realms.displayHint"))}
        {text("realm-url", "url", t("realms.url"), t("realms.urlHint"))}
        {text("realm-base-dn", "baseDn", t("realms.baseDn"))}
        {text("realm-bind-dn", "bindDn", t("realms.bindDn"), t("realms.bindDnHint"))}
        <Field
          id="realm-bind-password"
          label={t("realms.bindPassword")}
          error={errors.bindPassword}
          hint={saved ? t("realms.bindPasswordSaved") : undefined}
        >
          {(a11y) => (
            <Input
              id="realm-bind-password"
              type="password"
              autoComplete="new-password"
              disabled={form.clearBindPassword}
              value={form.bindPassword}
              onChange={(e) => change({ bindPassword: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        {saved && (
          <div className="flex items-center gap-2">
            <Checkbox
              id="realm-clear-password"
              checked={form.clearBindPassword}
              onCheckedChange={(on) => change({ clearBindPassword: on === true })}
            />
            <Label htmlFor="realm-clear-password">{t("realms.clearBindPassword")}</Label>
          </div>
        )}
        {text(
          "realm-user-filter",
          "userFilter",
          t("realms.userFilter"),
          t("realms.userFilterHint"),
        )}
        {text("realm-group-attr", "groupAttr", t("realms.groupAttr"))}
        <Field id="realm-ca" label={t("realms.caPem")} hint={t("realms.caPemHint")}>
          {(a11y) => (
            <textarea
              id="realm-ca"
              rows={3}
              spellCheck={false}
              autoComplete="off"
              value={form.caPem}
              onChange={(e) => change({ caPem: e.target.value })}
              className="w-full min-w-0 rounded-lg border border-input bg-transparent px-2.5 py-1 font-mono text-xs break-all outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-50 dark:bg-input/30"
              {...a11y}
            />
          )}
        </Field>
        <Field id="realm-timeout" label={t("realms.timeout")} error={errors.timeout}>
          {(a11y) => (
            <Input
              id="realm-timeout"
              inputMode="numeric"
              value={form.timeout}
              onChange={(e) => change({ timeout: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-1.5 text-sm font-medium">{t("realms.groups")}</legend>
          <p className="text-xs text-muted-foreground">{t("realms.groupsHint")}</p>
          {form.groups.map((row, i) => {
            const n = i + 1;
            const error = errors[`group-${i}`];
            const errorId = `realm-group-${i}-error`;
            return (
              <div key={i} className="flex flex-col gap-1">
                <div className="flex items-center gap-2">
                  <Input
                    className="min-w-0 flex-1"
                    aria-label={t("realms.groupN", { n })}
                    aria-invalid={error ? true : undefined}
                    aria-describedby={error ? errorId : undefined}
                    spellCheck={false}
                    placeholder="cn=ops,ou=groups,dc=example,dc=com"
                    value={row.group}
                    onChange={(e) =>
                      changeGroups(
                        form.groups.map((g, j) => (j === i ? { ...g, group: e.target.value } : g)),
                      )
                    }
                  />
                  {/* 역할 이름이 다 보이게 내용 너비를 지킨다. */}
                  <NativeSelect
                    className="min-w-28 shrink-0"
                    aria-label={t("realms.groupRoleN", { n })}
                    value={row.role}
                    onChange={(e) =>
                      changeGroups(
                        form.groups.map((g, j) =>
                          j === i ? { ...g, role: e.target.value as Role } : g,
                        ),
                      )
                    }
                  >
                    <RoleOptions />
                  </NativeSelect>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-sm"
                    aria-label={t("realms.removeGroupN", { n })}
                    onClick={() => changeGroups(form.groups.filter((_, j) => j !== i))}
                  >
                    <X />
                  </Button>
                </div>
                {error && (
                  <p id={errorId} className="text-xs text-destructive">
                    {t(error)}
                  </p>
                )}
              </div>
            );
          })}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            onClick={() => changeGroups([...form.groups, { group: "", role: "viewer" }])}
          >
            <Plus />
            {t("realms.addGroup")}
          </Button>
        </fieldset>
        <Field id="realm-default-role" label={t("realms.defaultRole")}>
          <NativeSelect
            id="realm-default-role"
            className="w-full"
            value={form.defaultRole}
            onChange={(e) => change({ defaultRole: e.target.value as Role | "" })}
          >
            <NativeSelectOption value="">{t("realms.noDefaultRole")}</NativeSelectOption>
            <RoleOptions />
          </NativeSelect>
        </Field>
      </div>
    </FormDialog>
  );
}

/**
 * 영역 시험: 디렉터리 사용자로 단계마다 확인한다. 사용자·세션을 만들지 않는다. 결과를 보이도록 시험한 뒤에도
 * 열어 둔다.
 */
function TestDialog({
  realm,
  returnFocus,
  onClose,
}: {
  realm: RealmView;
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<RealmTestResult | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setResult(
        await call(
          api.POST("/realms/{key}/actions/test", {
            params: { path: { key: realm.name } },
            body: { username, password },
          }),
        ),
      );
    } catch (err) {
      setResult(null);
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className="sm:max-w-2xl" onCloseAutoFocus={(e) => focusBack(e, returnFocus)}>
        <form onSubmit={(e) => void submit(e)} className="flex min-w-0 flex-col gap-4">
          <DialogHeader>
            <DialogTitle>{t("realms.testTitle", { name: realm.name })}</DialogTitle>
            <DialogDescription>{t("realms.testDescription")}</DialogDescription>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <fieldset disabled={busy} className="grid gap-4 sm:grid-cols-2">
            <Field id="realm-test-username" label={t("login.username")}>
              <Input
                id="realm-test-username"
                autoComplete="off"
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </Field>
            <Field id="realm-test-password" label={t("login.password")}>
              <Input
                id="realm-test-password"
                type="password"
                autoComplete="off"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </Field>
          </fieldset>
          {result && <TestResult result={result} />}
          <DialogFooter>
            <Button type="button" variant="outline" disabled={busy} onClick={onClose}>
              {t("common.done")}
            </Button>
            <Button type="submit" disabled={busy || !username}>
              {t("realms.test")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function TestResult({ result }: { result: RealmTestResult }) {
  const { t } = useTranslation();
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <Table aria-label={t("realms.steps")}>
        <TableHeader>
          <TableRow>
            <TableHead>{t("realms.step")}</TableHead>
            <TableHead>{t("realms.result")}</TableHead>
            <TableHead>{t("realms.message")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {result.steps.map((step) => (
            <TableRow key={step.name}>
              <TableCell>
                {t(`realms.stepNames.${step.name}`, { defaultValue: step.name })}
              </TableCell>
              <TableCell className={step.ok ? "" : "text-destructive"}>
                {t(step.ok ? "realms.ok" : "realms.failed")}
              </TableCell>
              <TableCell className="whitespace-normal break-words">{step.message}</TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">{t("realms.dn")}</dt>
        <dd className={result.dn ? "font-mono text-xs break-all" : undefined}>
          {result.dn ?? "—"}
        </dd>
        <dt className="text-muted-foreground">{t("realms.groupsFound")}</dt>
        <dd>
          {result.groups.length === 0 ? (
            "—"
          ) : (
            <ul className="font-mono text-xs break-all">
              {result.groups.map((g) => (
                <li key={g}>{g}</li>
              ))}
            </ul>
          )}
        </dd>
        <dt className="text-muted-foreground">{t("users.role")}</dt>
        <dd>{result.role ? t(`roles.${result.role}`) : t("realms.noRole")}</dd>
      </dl>
    </div>
  );
}
```

`web/src/realms/realms.ts`를 만든다:

```ts
import type { components } from "@/api/schema";
import type { Role } from "@/api/types";
import { validName } from "@/vms/form";

type Schemas = components["schemas"];

export type RealmView = Schemas["RealmView"];
export type RealmRequest = Schemas["RealmRequest"];
export type RealmTestResult = Schemas["RealmTestResult"];

/** 서버의 기본값과 같다(`opl_types::realm`). */
export const DEFAULT_USER_FILTER = "(&(objectClass=person)(uid={username}))";
export const DEFAULT_GROUP_ATTR = "memberOf";
export const DEFAULT_TIMEOUT_SECS = 10;

export interface GroupRow {
  group: string;
  role: Role;
}

/** LDAP 영역 만들기·고치기 폼. 입력 그대로의 글이다. */
export interface RealmForm {
  name: string;
  display: string;
  url: string;
  baseDn: string;
  bindDn: string;
  /** 새 비밀번호. 고칠 때 비우면 저장된 값을 그대로 둔다. */
  bindPassword: string;
  /** 고칠 때 저장된 비밀번호를 지운다. */
  clearBindPassword: boolean;
  userFilter: string;
  groupAttr: string;
  caPem: string;
  timeout: string;
  groups: GroupRow[];
  /** 빈 글이면 기본 역할이 없다(맞는 그룹이 없으면 로그인을 거절한다). */
  defaultRole: Role | "";
}

export function emptyForm(): RealmForm {
  return {
    name: "",
    display: "",
    url: "",
    baseDn: "",
    bindDn: "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: DEFAULT_USER_FILTER,
    groupAttr: DEFAULT_GROUP_ATTR,
    caPem: "",
    timeout: String(DEFAULT_TIMEOUT_SECS),
    groups: [],
    defaultRole: "",
  };
}

export function formOf(realm: RealmView): RealmForm {
  const ldap = realm.backend;
  return {
    name: realm.name,
    display: realm.display ?? "",
    url: ldap.url,
    baseDn: ldap.base_dn,
    bindDn: ldap.bind_dn ?? "",
    bindPassword: "",
    clearBindPassword: false,
    userFilter: ldap.user_filter,
    groupAttr: ldap.group_attr,
    caPem: ldap.ca_pem ?? "",
    timeout: String(ldap.timeout_secs),
    groups: (realm.roles.groups ?? []).map((g) => ({ ...g })),
    defaultRole: realm.roles.default ?? "",
  };
}

/**
 * 서버의 `RealmSpec::validate`와 PUT의 비밀번호 규칙과 같은 검사. 값은 번역 키다. 그룹 행의 오류는 `group-<번호>`.
 * `realm`은 고칠 때의 지금 영역이다.
 */
export function checkForm(
  form: RealmForm,
  taken: string[] | null,
  realm: RealmView | null = null,
): Record<string, string> {
  const errors: Record<string, string> = {};
  // 만들 때만 이름을 본다(`taken`이 있으면 만들기).
  if (taken) {
    if (!validName(form.name)) {
      errors.name = "forms.name";
    } else if (form.name === "local") {
      errors.name = "realms.errors.local";
    } else if (taken.includes(form.name)) {
      errors.name = "realms.errors.taken";
    }
  }
  if (!/^ldaps?:\/\/\[?[^/?:\]]+/.test(form.url.trim())) {
    errors.url = "realms.errors.url";
  }
  if (!form.baseDn.trim()) {
    errors.baseDn = "realms.errors.required";
  }
  if (form.userFilter.split("{username}").length !== 2) {
    errors.userFilter = "realms.errors.userFilter";
  }
  if (!form.groupAttr.trim()) {
    errors.groupAttr = "realms.errors.required";
  }
  const bindDn = form.bindDn.trim();
  const saved = realm?.backend.has_bind_password ?? false;
  if (form.bindPassword && !form.clearBindPassword && !bindDn) {
    errors.bindPassword = "realms.errors.bindDn";
  }
  if (form.clearBindPassword && bindDn) {
    // 바인드 DN에는 비밀번호가 있어야 한다.
    errors.bindDn = "realms.errors.clearNeedsNoBindDn";
  } else if (bindDn && !form.bindPassword && !saved) {
    errors.bindPassword = "realms.errors.bindPasswordNeeded";
  } else if (!bindDn && saved && !form.clearBindPassword && !form.bindPassword) {
    // 바인드 DN을 비우면 익명 검색이므로 저장된 비밀번호를 지워야 한다(다시 넣을 것이 아니다).
    errors.bindPassword = "realms.errors.clearSaved";
  } else if (realm && saved && !form.clearBindPassword && !form.bindPassword) {
    // 저장된 비밀번호를 다른 서버·계정으로 보내지 않는다(서버도 400).
    const ldap = realm.backend;
    if (
      form.url.trim() !== ldap.url ||
      bindDn !== (ldap.bind_dn ?? "") ||
      form.caPem.trim() !== (ldap.ca_pem ?? "")
    ) {
      errors.bindPassword = "realms.errors.bindPasswordAgain";
    }
  }
  const timeout = Number(form.timeout);
  if (!/^\d+$/.test(form.timeout.trim()) || timeout < 1 || timeout > 30) {
    errors.timeout = "realms.errors.timeout";
  }
  form.groups.forEach((g, i) => {
    if (!g.group.trim()) {
      errors[`group-${i}`] = "realms.errors.group";
    }
  });
  return errors;
}

/**
 * 폼을 요청 본문으로. 고칠 때(`editing`) 비밀번호를 비워 두면 `bind_password`를 빼서 서버가 저장된 값을 두고,
 * 지우기를 골랐으면 빈 글을 보낸다.
 */
export function requestOf(form: RealmForm, editing: boolean): RealmRequest {
  const bindDn = form.bindDn.trim();
  const display = form.display.trim();
  const password = form.clearBindPassword && editing ? "" : form.bindPassword || undefined;
  return {
    ...(!editing && { name: form.name }),
    ...(display && { display }),
    backend: {
      kind: "ldap",
      url: form.url.trim(),
      base_dn: form.baseDn.trim(),
      ...(bindDn && { bind_dn: bindDn }),
      ...(password !== undefined && { bind_password: password }),
      user_filter: form.userFilter.trim(),
      group_attr: form.groupAttr.trim(),
      ...(form.caPem.trim() && { ca_pem: form.caPem.trim() }),
      timeout_secs: Number(form.timeout),
    },
    roles: {
      groups: form.groups.map((g) => ({ group: g.group.trim(), role: g.role })),
      ...(form.defaultRole && { default: form.defaultRole }),
    },
  };
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 인증 영역 화면과 영역 로그인 — 로그인 화면은 인증 없이 GET /auth/realms를 읽어 local 말고도 영역이 있으면 영역 고르기(기본 local, local은 \"로컬 사용자\", 나머지는 표시 이름이나 이름; 읽지 못하면 숨김)를 보이고 고른 영역을 로그인 요청의 realm으로(local이면 보내지 않음). 오류 문구 errors.no_role·realm_unavailable·external_user. 새 화면 /realms(메뉴 nav.realms \"인증 영역\"/\"Realms\"는 realm을 읽을 수 있는 admin에게만, 다른 역할은 주소로 와도 읽지 않고 안내): 이름 순 목록(이름, 표시 이름, 종류, URL), LDAP 영역 만들기·고치기 대화상자(URL, 기준 DN, 바인드 DN, 바인드 비밀번호 — 고칠 때 비우면 필드를 빼 저장된 값을 두고 \"저장된 비밀번호 지우기\"면 \"\", 사용자 필터·그룹 속성·시간 제한은 서버 기본값으로 시작, CA PEM, 그룹 → 역할 표 더하기·빼기, 기본 역할(없으면 로그인 거절); 서버 검증과 같은 입력 검사, 고치기는 읽은 revision을 If-Match로), 지우기 확인, 시험 대화상자(사용자·비밀번호 → 단계 표, DN, 그룹, 정해질 역할 — 닫을 때까지 남음). 사용자 목록에 영역 열(없으면 local), 외부 사용자는 비밀번호 재설정 메뉴가 없고 외부 사용자 본인의 계정 메뉴에는 비밀번호 바꾸기가 없음(이름에 @). ko·en"
```

---

### Task 7: 문서

README 새 절 "외부 인증(LDAP)"(영역, 외부 사용자 이름과 역할 매핑, OpenLDAP·Active Directory 설정 예, TLS, 비밀번호 규칙, 로그인 단계와 오류, 시험 동작, oplctl·웹, 영역을 지울 때, 한계, `ldap_it` 실행), 엔터프라이즈 설계 §3.6 "구현(계획 35)"과 §7·§9, 처음 분해 문서의 하위 프로젝트 10 줄.

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-06-enterprise-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`README.md`에서

```markdown
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
```

을 다음으로 바꾼다:

```markdown
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, VMware 가져오기(OVA), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업, 감사 로그(admin),
사용자(admin), 인증 영역(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
`oplctl cluster join`을 실행한다(클러스터 화면이 명령을 만들어 준다). 이미지는 지금 접속한 노드로 올라가므로
```

`README.md`에서

```markdown

## 감사 로그
```

을 다음으로 바꾼다:

````markdown

## 외부 인증(LDAP)

인증 영역(`Realm`)을 만들면 로컬 사용자 말고도 바깥 LDAP 디렉터리(OpenLDAP, Active Directory 등)의 계정으로
로그인할 수 있다. 영역은 admin만 보고 만들고 고친다(operator·viewer는 읽기도 **403**). 영역 이름은 리소스 이름
규칙(1~63자의 영문 소문자·숫자·`-`)이고 `local`은 로컬 사용자를 뜻하는 예약 이름이다. UUID로 읽히는 이름(예:
`0192f0c1-5d2e-7000-8000-000000000000`)도 API가 id로 읽어 이름으로 부를 수 없으므로 **400**이다.

- **외부 사용자.** 영역 `corp`에 `alice`로 처음 로그인하면 opLite 사용자 `alice@corp`가 생긴다(로컬 사용자 `alice`와는
  다른 사용자다). 이름의 사용자 부분은 소문자로 바꾸고(디렉터리에도 소문자로 찾는다) 1~64자의 `a-z`·`0-9`·`.`·`_`·`-`만
  된다. 이 형식이 아닌 계정은 로그인할 수 없다(**401**). 비밀번호는 opLite에 저장하지 않고 늘 디렉터리가 확인한다.
  외부 사용자의 비밀번호를 바꾸려 하면 **400** `external_user`다(웹에는 그 메뉴가 없다). 외부 사용자도 API 토큰을
  만들 수 있다. 외부 사용자를 지우면(`oplctl user delete alice@corp`) 세션과 토큰이 함께 끝나고, 디렉터리가 받아 주면
  다음 로그인에 다시 생긴다.
- **역할 매핑.** 로그인할 때마다 사용자 항목의 그룹 속성(기본 `memberOf`) 값들을 영역의 그룹 → 역할 표와 비교한다.
  앞뒤 공백을 떼고 ASCII 대소문자만 무시한 문자열 비교다(DN을 정규화하지 않으므로 `cn=ops, dc=example`과
  `cn=ops,dc=example`은 다르다 — `oplctl realm test`가 보이는 값을 그대로 쓴다). 여러 그룹이 맞으면 가장 높은 역할
  (admin > operator > viewer), 맞는 것이 없으면 기본 역할, 그것도 없으면 **403** `no_role`("no role is mapped for this
  user")이다. 정해진 역할이 지금과 다르면 사용자를 고치고 그 사용자의 다른 로그인 세션을 끝낸다(같으면 아무것도 쓰지
  않는다). admin이 외부 사용자의 역할을 바꿀 수는 있지만 다음 로그인에 매핑으로 다시 정해진다.

```bash
# OpenLDAP(memberof 오버레이): 서비스 계정으로 찾는다. --group은 <그룹 DN>=<role>(마지막 =에서 나눔, 여러 번)
oplctl realm add-ldap corp --display "Corp LDAP" \
  --url ldap://ldap.example.com --base-dn ou=people,dc=example,dc=com \
  --bind-dn cn=oplite,ou=services,dc=example,dc=com --bind-password-file /root/corp-bind.pw \
  --ca-file /etc/ssl/certs/corp-ca.pem \
  --group cn=admins,ou=groups,dc=example,dc=com=admin \
  --group cn=ops,ou=groups,dc=example,dc=com=operator --default-role viewer
# Active Directory: sAMAccountName으로 찾는다(그룹은 memberOf 그대로)
oplctl realm add-ldap ad --url ldaps://dc1.corp.example.com --base-dn "DC=corp,DC=example,DC=com" \
  --bind-dn "CN=svc-oplite,OU=Service Accounts,DC=corp,DC=example,DC=com" --bind-password-file /root/ad-bind.pw \
  --user-filter "(&(objectClass=user)(sAMAccountName={username}))" \
  --group "CN=oplite-admins,OU=Groups,DC=corp,DC=example,DC=com=admin"
oplctl realm list
# NAME  DISPLAY    KIND  URL
# corp  Corp LDAP  ldap  ldap://ldap.example.com
oplctl realm show corp            # 설정(bind password: set|none, ca: custom (PEM)|system roots, 그룹 표, 기본 역할)
oplctl realm test corp --user alice   # 비밀번호는 터미널에서 묻는다(--password-file <파일|->)
# STEP          RESULT  MESSAGE
# connect       ok      connected to ldap://ldap.example.com with StartTLS
# service_bind  ok      bound as cn=oplite,ou=services,dc=example,dc=com
# search        ok      found uid=alice,ou=people,dc=example,dc=com
# user_bind     ok      bound as uid=alice,ou=people,dc=example,dc=com
# groups        ok      2 value(s) of memberOf
# dn: uid=alice,ou=people,dc=example,dc=com
# groups: cn=admins,ou=groups,dc=example,dc=com, cn=staff,ou=groups,dc=example,dc=com
# role: admin
oplctl realm set-ldap corp --timeout 5            # 준 것만 바꾼다(읽은 revision으로 — 그 사이 바뀌었으면 412)
oplctl realm set-ldap corp --bind-password-file /root/corp-bind.pw   # 새 서비스 계정 비밀번호
# URL·바인드 DN·CA를 바꿀 때는 비밀번호도 다시 준다(주지 않으면 서버가 400)
oplctl realm set-ldap corp --url ldaps://ldap2.example.com --bind-password-file /root/corp-bind.pw
oplctl realm set-ldap corp --clear-bind-dn          # 익명 검색으로(저장된 비밀번호도 지움)
oplctl realm set-ldap corp --clear-ca --clear-default-role --clear-groups   # 시스템 루트, 기본 역할·그룹 표 없음
oplctl realm remove corp
oplctl login --server https://10.0.0.11:8006 --realm corp --user alice   # 기본 --realm local
```

- **설정 항목**(API `backend`의 필드, oplctl 선택):
  - `url`(`--url`): `ldap://호스트[:포트]` 또는 `ldaps://호스트[:포트]`. 서버 하나만 쓴다.
  - `base_dn`(`--base-dn`): 사용자를 찾을 기준 DN(그 아래 전체, subtree).
  - `bind_dn`·`bind_password`(`--bind-dn`, `--bind-password-file <파일|->`): 사용자를 찾을 서비스 계정. 둘은 함께 있어야
    한다 — `bind_dn`에 비밀번호가 없거나 빈 글이면 **400**("bind_dn needs bind_password; remove bind_dn for an anonymous
    search"), 비밀번호만 있어도 **400**이고, 둘 다 없을 때만 익명으로 찾는다. 비밀번호는 API 응답에 없고
    `has_bind_password`로 있는지만 보인다. 바꾸기(`PUT`)에서 필드를 빼면 저장된 값을 두는데, `url`·`bind_dn`·`ca_pem`이
    모두 그대로일 때만이다: 하나라도 바꾸면서 빼면 **400** `invalid_request`("bind_password must be given again when url,
    bind_dn or ca_pem changes" — 저장된 비밀번호를 다른 서버나 계정으로 보내지 않는다). `""`이면 지우므로 `bind_dn`도 함께
    빼야 한다(`--clear-bind-dn`, 웹은 "저장된 비밀번호 지우기"와 바인드 DN 비우기).
  - 서버는 저장·비교하기 전에 `url`·`bind_dn`의 앞뒤 공백을 떼고, `ca_pem`은 CRLF를 LF로 바꾼 뒤 앞뒤 공백을 뗀다(만들기·바꾸기
    모두). 그래서 끝 줄바꿈이 있는 CA 파일(`--ca-file`)로 만든 영역을 웹에서 고쳐도 같은 CA로 보고 비밀번호를 그대로 둔다.
  - `user_filter`(`--user-filter`, 기본 `(&(objectClass=person)(uid={username}))`): `{username}`이 정확히 한 번 있어야
    하고, 로그인 이름을 RFC 4515로 이스케이프해 넣는다. Active Directory는 `(&(objectClass=user)(sAMAccountName={username}))`,
    UPN으로 찾으려면 `(userPrincipalName={username}@corp.example.com)`처럼 쓴다. 로그인할 수 있는 사람을 줄이는 조건도
    필터에 넣는다(예: AD의 `(memberOf:1.2.840.113556.1.4.1941:=CN=oplite,…)`).
  - `group_attr`(`--group-attr`, 기본 `memberOf`): 그룹 DN을 담은 사용자 속성. 서비스 계정(또는 익명)이 읽을 수 있어야 한다.
    OpenLDAP는 memberof 오버레이가 있어야 `memberOf`가 생긴다.
  - `ca_pem`(`--ca-file`): 서버 인증서를 확인할 CA(PEM). 있으면 그 인증서들만 믿고, 없으면 시스템 루트를 쓴다.
  - `timeout_secs`(`--timeout`, 기본 10, 1~30): 연결과 LDAP 작업마다의 시간 제한(초). 확인 전체(연결·바인드·검색·바인드)는
    `timeout_secs × 4 + 1`초(최대 121초) 안에 끝나고, 넘으면 **503**이다(시험 동작은 그 단계가 "timed out").
  - `display`(`--display`): 로그인 화면에 보일 이름. `roles`: `groups`(`--group`, `set-ldap`에서 주면 표 전체를 바꾼다)와
    `default`(`--default-role`).
  - `set-ldap`은 영역을 읽어 준 것만 바꾼다. 없애는 선택은 `--clear-bind-dn`(바인드 DN과 저장된 비밀번호 — 익명 검색),
    `--clear-ca`(시스템 루트), `--clear-default-role`, `--clear-groups`이고, 같은 값을 정하는 선택과 함께 줄 수 없다.
    `--clear-bind-password`는 바인드 DN이 남으면 보내지 않고 오류다("--clear-bind-password needs --clear-bind-dn (or give
    --bind-password-file)"). URL·바인드 DN·CA를 바꾸면서 `--bind-password-file`을 주지 않으면 서버의 **400**을 그대로 보인다.
  - 나쁜 값(형식이 아닌 URL, 빈 기준 DN, `{username}`이 없거나 둘, 시간 제한 범위 밖, 빈 그룹 DN 등)과 이름 `local`은 **400**
    `invalid_request`다.
- **TLS.** 평문으로는 바인드하지 않는다. `ldap://`는 늘 StartTLS를 쓰고(서버가 StartTLS를 하지 못하면 연결 실패),
  `ldaps://`는 처음부터 TLS다. 서버 인증서는 CA와 **호스트 이름**을 모두 확인한다: URL의 호스트(이름이나 IP)가 인증서의
  SAN에 있어야 한다. 시스템 루트는 노드의 인증서 저장소(Debian: `ca-certificates`)이고, 읽지 못하면 믿는 루트가 없어 모든
  연결이 실패한다. 사설 CA면 `--ca-file`을 준다.
- **로그인 순서.** 빈 비밀번호는 디렉터리에 묻지 않고 거절한다(익명 바인드로 통과하는 것을 막는다). 연결 → 서비스 계정
  바인드(없으면 생략) → `base_dn` 아래에서 `user_filter`로 찾기(`group_attr`만 읽음, 크기 제한 2·시간 제한 `timeout_secs`;
  서버가 크기 제한을 지키지 않아도 둘째 항목에서 멈추고 검색을 버린다) → 찾은 항목이 정확히 하나가 아니면(결과 코드 4
  sizeLimitExceeded도 여럿으로 봄) 실패 → 그 DN과 비밀번호로 바인드 → 그룹 값으로 역할 → 사용자 만들기·고치기 → 세션.
  끝나면 연결을 끊는다(unbind). 사용자 확인이 자격 증명 실패로 끝나면 시작부터 적어도 300ms 뒤에 답한다. 디렉터리가
  300ms 안에 답하면 없는 사용자와 틀린 비밀번호의 응답 시간 차가 가려지지만, 그보다 느린 디렉터리에서는 차이가 남을 수
  있다(시험 동작은 기다리지 않는다).
- **오류.** 없는 사용자, 여러 항목, 틀린 비밀번호(LDAP 결과 49)는 모두 로컬과 같은 **401** `invalid_credentials`다.
  연결·TLS 실패, 믿지 않는 인증서, 서비스 계정 바인드·검색 실패, 사용자 바인드의 49가 아닌 실패는 **503**
  `realm_unavailable`("the directory of realm corp is unavailable")이고 원인은 opld 로그(`journalctl -u opld`)에 경고로
  남는다. 모르는 영역은 **400** `invalid_request`, 매핑되는 역할이 없으면 **403** `no_role`이다. 롤링 업그레이드 중(옛 opld
  노드가 있을 때) 영역 로그인과 영역 쓰기는 **409** `cluster_upgrading`이다(로컬 로그인은 된다).
- **시도 제한.** 로컬 로그인과 같은 기록이고 키는 외부 사용자 이름(`alice@corp`)과 IP다. **401뿐 아니라 403 `no_role`과
  503 `realm_unavailable`도 실패로 센다**: 디렉터리가 멈춘 동안 되풀이해 로그인하면 잠시 막힐 수 있다. 형식이 아닌 이름은
  IP만 센다.
- **감사 로그.** 영역 로그인의 `detail`은 `username=alice realm=corp`이다(이름 형식이 아니면 `<invalid>`). 영역 만들기·고치기·
  지우기·시험은 다른 바꾸는 요청처럼 적히고, 서비스 계정·사용자 비밀번호는 어디에도 넣지 않는다.
- **시험 동작.** `oplctl realm test`(웹: 영역의 "시험", API `POST /api/v1/realms/{key}/actions/test {"username","password"}`,
  admin)는 로그인과 같은 순서를 `connect`·`service_bind`·`search`·`user_bind`·`groups` 단계로 돌려 단계마다 성공·실패와
  내용, 찾은 DN, 그룹 값, 정해질 역할을 보인다. 실패한 단계에서 멈춘다. 사용자·세션을 만들지 않고 시도 제한에도 세지
  않으며 비밀번호는 응답에 없다. oplctl은 실패한 단계가 있으면 오류로 끝난다(역할만 없으면 성공 — `role: none`).
- **웹.** 로그인 화면은 `local` 말고도 영역이 있으면 영역 고르기를 보인다(기본 "로컬 사용자", 영역은 표시 이름이나 이름).
  "인증 영역" 메뉴(admin에게만, 사용자 다음): 목록(이름, 표시 이름, 종류, URL), LDAP 영역 만들기·고치기(바인드 비밀번호는
  비워 두면 그대로, "저장된 비밀번호 지우기"로 지움 — 서버와 같은 규칙으로 URL·바인드 DN·CA를 바꾸면 비밀번호를 다시
  넣으라고(바인드 DN만 비웠으면 "저장된 비밀번호 지우기"를 고르라고), 바인드 DN에 비밀번호가 없거나 지우기에 바인드 DN이
  남으면 저장 전에 알린다, 그룹 → 역할 표 더하기·빼기,
  기본 역할), 지우기, 시험 대화상자(단계 표,
  DN, 그룹, 정해질 역할). 사용자 목록에 영역 열(로컬 사용자는 `local`)이 있다.
- **API.** `GET`·`POST /api/v1/realms`, `GET`·`PUT`·`DELETE /api/v1/realms/{key}`(`PUT`은 통째로 바꾸고 `If-Match`를 따른다,
  이름은 바꿀 수 없다), 로그인 전 화면용 `GET /api/v1/auth/realms`(인증 없음, `[{"name","display","kind"}]`, `local`이 먼저이고
  나머지는 이름 순), `POST /api/v1/auth/login {"username","password","realm"}`(`realm`이 없거나 `local`이면 로컬 로그인,
  영역 로그인의 `username`은 `@영역` 없는 이름). 영역은 watch로 내보내지 않는다(서비스 계정 비밀번호가 들어 있다).
- **영역을 지울 때.** 그 영역의 사용자는 로그인할 수 없고(모르는 영역 **400**), 모든 노드에서 그 사용자들의 세션과 API 토큰이
  다음 요청부터 **401**이다. `alice@corp` 같은 사용자와 토큰은 남으므로 같은 이름의 영역을 다시 만들면 다시 쓸 수 있다
  — 새 영역의 디렉터리·매핑과 상관없이 **옛 세션(12시간 안)과 토큰이 지울 때의 역할 그대로 되살아난다**(아래 한계).
  완전히 없애려면 사용자도 지운다. 이미 열린 콘솔(WebSocket)은 끊기지 않는다.

한계:

- 디렉터리는 로그인할 때만 본다. 디렉터리에서 계정을 막거나 지우거나 그룹을 빼도 이미 있는 세션(최대 12시간)과 API 토큰은
  남고 역할은 다음 로그인에 바뀐다. 바로 막으려면 opLite에서 그 사용자를 지운다(토큰도 함께 지워진다).
- 그룹은 사용자 항목의 속성 값만 본다. 그룹 항목을 찾는 방식(`posixGroup`의 `memberUid` 등)과 중첩 그룹 풀기(AD의
  `memberOf`는 직접 속한 그룹만)는 없다. DN은 문자열로만 비교한다.
- 서버 주소는 하나다(장애 조치·여러 서버 없음). 참조(referral)는 따라가지 않는다. 단순 바인드만 한다(SASL·Kerberos·클라이언트
  인증서 없음).
- 서비스 계정 비밀번호는 Raft에 평문으로 있다(구독 키와 같이 노드의 root만 읽는다).
- 영역을 지우고 같은 이름으로 다시 만들면(다른 디렉터리를 가리켜도) 옛 영역 사용자의 세션과 API 토큰이 지울 때의 역할로
  다시 동작한다. 영역을 다른 용도로 다시 쓰기 전에는 그 영역의 사용자(`oplctl user list`의 `<사용자>@<영역>`)를 먼저 지운다.
- 같은 사용자가 거의 동시에 두 번 로그인하고 그 사이 디렉터리의 그룹이 바뀌면, 옛 그룹을 읽은 로그인이 나중에 써서 역할이
  옛 것으로 되돌아갈 수 있다(드문 경쟁 — 다음 로그인에 바로잡힌다).
- 사용자 부분이 `a-z`·`0-9`·`.`·`_`·`-`가 아닌 계정(공백·`@`·한글 등)은 로그인할 수 없다. 사용자·그룹 동기화, 외부 사용자의
  2단계 인증, SSO(OIDC)는 없다.

## 감사 로그
````

`README.md`에서

```markdown
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다), 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
```

을 다음으로 바꾼다:

```markdown
  경로는 `null`이다. `detail`은 핸들러가 남긴 짧은 문자열이다: 로그인은 `username=<이름>`(이름 형식이 아니면
  `username=<invalid>` — 비밀번호를 이름 칸에 넣었을 수 있다, 영역 로그인은 뒤에 ` realm=<영역>`), 구독 키 넣기는 `id=<키 id>`. 비밀번호·키·토큰은
  어디에도 넣지 않는다. 로그인에 성공하면 그 사용자, 로그아웃은 세션의 사용자가 `user`다. `path`·`target.key`·
```

`README.md`에서

````markdown
cargo test -p opld --features ha-it --test ha
```
````

을 다음으로 바꾼다:

````markdown
cargo test -p opld --features ha-it --test ha
# Linux(Ubuntu·Debian), root: 실제 OpenLDAP. ldap-setup.sh가 이 기계의 slapd 설정과 디렉터리를 지우고 늘 새 데이터베이스로
# 다시 설정한다(다시 돌려도 된다 — 옛 데이터베이스는 /var/backups로 옮겨짐). 버려도 되는 기계에서만: CI(`CI=true`)가 아니면
# OPL_LDAP_SETUP_WIPE=1을 주어야 돌고, 없으면 설명을 찍고 끝난다(CI `ldap` 작업).
# 스크립트와 시험이 같은 값을 읽는다(아래는 CI ldap 작업의 값. 스크립트는 없으면 이 값을 쓰지만 시험은 모두 있어야 한다).
export OPL_TEST_LDAP_URL=ldaps://127.0.0.1:636 OPL_TEST_LDAP_STARTTLS_URL=ldap://127.0.0.1:389 \
  OPL_TEST_LDAP_CA=/tmp/opl-test-ldap-ca.pem OPL_TEST_LDAP_BASE_DN=dc=example,dc=com \
  OPL_TEST_LDAP_BIND_DN=cn=admin,dc=example,dc=com OPL_TEST_LDAP_BIND_PASSWORD=opl-admin-pw \
  OPL_TEST_LDAP_USER=alice OPL_TEST_LDAP_USER_PASSWORD=alice-pw OPL_TEST_LDAP_OTHER_USER=bob \
  OPL_TEST_LDAP_OTHER_PASSWORD=bob-pw OPL_TEST_LDAP_GROUP=cn=admins,ou=groups,dc=example,dc=com
sudo -E OPL_LDAP_SETUP_WIPE=1 sh .github/scripts/ldap-setup.sh
cargo test -p opl-api --features ldap-it --test ldap_it
```
````

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP), 계획 36(OIDC SSO), 계획 37(원격 클러스터), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 35~38은 남았다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 10은 계획 34(감사 로그: 노드마다 라우터 미들웨어가 바꾸는 API 요청·로그인·로그아웃·콘솔 열기를 `{data_dir}/audit/audit.log`(JSON Lines, 0600, 크기·개수 회전, 1초 묶음 fsync)와 tracing 대상 `opl_audit`(journald)에 적음, 동작 이름은 `"<METHOD> <라우트 템플릿>"`, 노드 사이 RPC `ReadAudit`로 모든 노드를 모아 읽는 `GET /api/v1/audit`(admin), opld `[audit]`, `oplctl audit`, 웹 `/audit`, 완료), 계획 35(인증 영역과 LDAP: 새 리소스 `Realm`(LDAP, admin만, 서비스 계정 비밀번호는 응답·watch에 없음), `ldap3`(rustls)로 StartTLS·LDAPS만 쓰는 바인드·사용자 찾기·그룹 속성으로 역할 매핑, 영역 로그인이 외부 사용자 `<사용자>@<영역>`을 만들거나 역할을 고침(`no_role`·`realm_unavailable`·`external_user`), 영역을 지우면 그 사용자의 세션·토큰이 멈춤, 시험 동작, `oplctl realm`·`login --realm`, 웹 로그인 영역 고르기와 `/realms`, 가짜 LDAP 서버 시험과 CI `ldap` 작업의 실제 slapd 시험, 완료), 계획 36(OIDC SSO), 계획 37(원격 클러스터), 계획 38(원격 복제·DR)로 나눴다(`2026-11-06-enterprise-design.md`). 계획 36~38은 남았다.
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown

## 4. OIDC SSO (계획 36)
```

을 다음으로 바꾼다:

```markdown

### 3.6 구현(계획 35)

- 타입(`opl_types::realm`): `Realm = Resource<RealmSpec, RealmStatus>`, `Kind::Realm`(`"realm"`), `RealmSpec{display, backend, roles}`(`deny_unknown_fields`, `display`는 None이면 쓰지 않고 `roles`는 serde 기본값), `RealmBackend::Ldap(LdapRealm)`(`#[serde(tag = "kind", rename_all = "snake_case")]`), `LdapRealm{url, bind_dn, bind_password, base_dn, user_filter, group_attr, ca_pem, timeout_secs}`(`DEFAULT_USER_FILTER` `(&(objectClass=person)(uid={username}))`·`DEFAULT_GROUP_ATTR` `memberOf`·`DEFAULT_TIMEOUT_SECS` 10이 serde 기본값, `deny_unknown_fields`), `RoleMapping{groups: Vec<GroupRole{group, role}>, default}`와 `role_for(&[String])`(앞뒤 공백을 떼고 ASCII 대소문자 무시, 맞는 것 중 가장 높은 역할, 맞는 매핑이 있으면 `default`보다 우선, 없으면 `default`). `RealmSpec::validate() -> Result<(), RealmError>`: URL은 `ldap://`·`ldaps://`에 호스트(IPv6 `[…]` 포함), `base_dn`·`group_attr` 비어 있지 않음, `user_filter`에 `{username}` 정확히 한 번, `bind_password`는 `bind_dn`이 있어야, `bind_dn`에는 비어 있지 않은 `bind_password`가 있어야(`BindDnWithoutPassword` "bind_dn needs bind_password; remove bind_dn for an anonymous search" — 익명 검색은 둘 다 없을 때만), `timeout_secs` 1~30(전체 마감 `timeout_secs × 4 + 1초`가 최대 121초), 그룹 매핑의 `group` 비어 있지 않음. `validate_realm_name`(리소스 이름 규칙, `LOCAL_REALM` `"local"` 예약, UUID로 풀리는 이름은 `LooksLikeId` "invalid name …: names must not look like an id" — API가 id로 읽어 이름으로 부를 수 없음). `RealmError`의 영어 문구가 400 `invalid_request`의 메시지다. 설계와 다른 점: 설계의 `timeout`은 `timeout_secs`. openapi 기능에서 realm 타입에 `ToSchema`. `LdapRealm`의 `Debug`는 손으로 써서 `bind_password`를 `Some("***")`로 가린다(그것을 담은 `RealmSpec`·`RealmRequest`도).
- 사용자(`opl_types::auth`): `UserSpec.realm: Option<String>`(None이면 쓰지 않아 로컬 사용자는 계획 34의 JSON 그대로 — 옛 노드가 읽음), 외부 사용자의 `password_hash`는 `""`(argon2 PHC가 아니라 로컬 확인은 패닉 없이 늘 실패). `validate_external_name`(`<사용자>@<영역>`, 사용자 부분 1~64자 `[a-z0-9._-]`, 영역 부분 `validate_name`), `external_name(user, realm)`(사용자 부분을 ASCII 소문자로 바꾼 뒤 검사, 오류는 기존 `NameError` — 문구가 로컬 이름용이라 사용자에게 보이지 않음).
- 영역 API(`opl_api::realms`): `GET`·`POST /api/v1/realms`, `GET`·`PUT`·`DELETE /api/v1/realms/{key}`(tag `auth`). 본문 `RealmRequest{name?, display?, backend, roles}`(POST·PUT 공용, `deny_unknown_fields`; `name`은 만들 때 필요하고 바꿀 때 주면 지금 이름과 같아야 함; 빈 `display`는 None). 응답 `RealmView{id, name, revision, display?, backend: RealmBackendView::Ldap(LdapRealmView{…, has_bind_password}), roles}`. 만들기·바꾸기 모두 저장·비교 전에 `url`·`bind_dn`의 앞뒤 공백을 떼고 `ca_pem`은 CRLF를 LF로 바꿔 앞뒤 공백을 뗀다(끝 줄바꿈이 있는 `--ca-file` CA와 웹이 다듬어 보낸 같은 CA를 같은 값으로 봄). `PUT`은 통째로 바꾼다: `backend.bind_password`를 빼면 저장된 값을 두는데 `url`·`bind_dn`·`ca_pem`이 모두 그대로일 때만이고, 저장된 비밀번호가 있는데 하나라도 바뀌었으면 400 `invalid_request`("bind_password must be given again when url, bind_dn or ca_pem changes" — 저장된 비밀번호를 다른 서버·계정으로 보내지 않음), `""`이면 지움(그러면 `bind_dn`도 없어야 검증을 통과). 바뀐 것이 없으면 쓰지 않고 200. `If-Match`가 다르면 412, 없으면 읽은 revision에 쓰고 충돌하면 다시 읽어 되풀이(보안 그룹과 같음, 10번 넘으면 409 `busy`). 쓰기(POST·PUT·DELETE)는 모두 `require_current` 뒤. 권한: `RoleAuthorizer`의 계정 종류(`User|ApiToken|JoinToken`)에 `Kind::Realm`을 더해 읽기·쓰기 모두 admin만(웹 `permissions.ts`도 같게). `Realm`은 `WATCH_KINDS`에 넣지 않는다(설계의 "watch에서 지움" 대신 — 웹은 바꾼 뒤 다시 읽음). `GET /api/v1/auth/realms`(`security(())`): `RealmSummary{name, display(null 가능), kind: RealmKind(local|ldap)}`, `local`이 먼저, 나머지는 이름 순.
- LDAP 클라이언트(`opl_api::ldap`, 워크스페이스 의존 `ldap3` 0.12.1 `default-features = false, features = ["tls-rustls-ring"]` — 새 패키지는 `ldap3`·`lber`·`rustls-native-certs`뿐): `authenticate(&LdapRealm, username, password) -> Result<LdapUser{dn, groups}, LdapError{InvalidCredentials, Unavailable(String)}>`. 빈 비밀번호는 연결하지 않고 `InvalidCredentials`. 연결은 `LdapConnSettings::set_starttls(true)`(`ldap://`는 StartTLS, `ldaps://`는 처음부터 TLS)와 `set_conn_timeout`, `ca_pem`이 있으면 `opl_peer::pki::certs`로 읽은 인증서만 루트인 rustls(ring) 설정, 없으면 ldap3 기본 설정(`rustls-native-certs`의 시스템 루트 — 리눅스에서 읽지 못하면 빈 목록이라 모든 연결이 실패). 호스트 이름 확인은 rustls 기본. 서비스 바인드(`bind_dn`이 없으면 생략 — 검증이 `bind_dn`에 비밀번호를 요구) 실패 → `Unavailable`. `base_dn` subtree 스트림 검색(`SearchOptions` `sizelimit(2)`·`timelimit(timeout_secs)`, 필터는 `user_filter`의 `{username}`을 `ldap3::ldap_escape`로, 속성은 `group_attr`만, `EntriesOnly`로 참조·중간 응답을 뺌): 둘째 항목에서 멈추고 `abandon`(서버가 크기 제한을 지키지 않아도), 항목이 정확히 1개가 아니거나 결과 코드 4(sizeLimitExceeded)면 `InvalidCredentials`, 그 밖의 검색 실패는 `Unavailable`. 사용자 바인드 결과 49는 `InvalidCredentials`, 그 밖은 `Unavailable`. 그룹은 속성 이름을 ASCII 대소문자 무시로 맞춘 값들. 모든 작업에 `timeout_secs`, 끝나면 unbind. `authenticate`와 `test` 전체는 `timeout_secs × 4 + 1초` 마감(`tokio::time::timeout`; 작업마다의 제한 안에 참조를 끝없이 보내는 서버 같은 경우)이고 넘으면 `Unavailable("timed out")`(`test`는 걸린 단계를 실패로 적음). `authenticate`가 `InvalidCredentials`로 끝나면 시작부터 적어도 300ms(`MIN_FAILURE`)가 되도록 기다린다(디렉터리가 300ms 안에 답하면 없는 사용자와 틀린 비밀번호의 시간 차를 가린다 — 더 느린 디렉터리에서는 차이가 남을 수 있음; `test`는 기다리지 않음). 설계와 다른 점: 검색 실패·사용자 바인드의 49가 아닌 실패·나쁜 `ca_pem`도 503이다.
- 시험 동작: `POST /api/v1/realms/{key}/actions/test`(`RealmTestRequest{username, password}`, 권한은 `Action::Operate` on `Realm` → admin만) → `ldap::test(&RealmSpec, …) -> RealmTestResult{steps: Vec<RealmTestStep{name, ok, message}>, dn, groups, role}`(단계 상수 `STEP_CONNECT`·`STEP_SERVICE_BIND`·`STEP_SEARCH`·`STEP_USER_BIND`·`STEP_GROUPS`, 실패한 단계에서 멈춤, `groups`·`role`은 모두 성공했을 때만). `authenticate`와 같은 흐름이지만 빈 비밀번호도 연결해 `user_bind` 단계에서 실패로 보인다. 사용자·세션을 만들지 않고 시도 제한에 세지 않으며 `require_current`도 보지 않는다(쓰지 않음). 비밀번호는 응답·로그·감사에 없다(`RealmTestRequest`의 `Debug`도 가림).
- 로그인(`users::login`): `LoginRequest.realm: Option<String>`(serde 기본값). 없거나 `"local"`이면 `login_local`(계획 34 그대로 — 외부 사용자 이름은 `validate_name`에서 401). 영역이면 `login_realm`: 감사 `detail`을 `username=<u> realm=<r>`(사용자 부분이 형식이 아니거나 영역이 이름 형식이 아니면 그 자리에 `<invalid>`)로 먼저 적고 → `external_name`이 안 되면 `begin_ip`·401 → 영역이 없으면 400 `invalid_request`("unknown realm …", 시도로 세지 않음) → `attempts.begin(<외부 이름>, ip)` → `ldap::authenticate`(디렉터리에는 소문자로 바꾼 사용자 부분을 물음): `InvalidCredentials`는 로컬과 같은 401 `invalid_credentials`, `Unavailable`은 `warn!`(원인) 뒤 503 `realm_unavailable`("the directory of realm <r> is unavailable") → `role_for`가 None이면 403 `no_role` → `require_current` → `upsert_external`(없으면 `User{realm: Some, password_hash: "", role}` 만들기, 역할이 다르면 역할과 `session_epoch += 1`, 같으면 쓰지 않음; `AlreadyExists`·`NameTaken`·`RevisionMismatch`는 다시 읽어 한 번 더 — 같은 사용자의 거의 동시 로그인 사이에 디렉터리 그룹이 바뀌면 옛 그룹을 읽은 쪽이 나중에 써서 역할이 되돌아갈 수 있다: 드문 경쟁이고 다음 로그인에 바로잡히므로 코드로 막지 않음) → `attempts.success`. 설계와 다른 점: 503·403·409도 시도 제한의 실패로 남는다(`begin`이 미리 센 것을 `success`로만 뺌). 세션 쿠키·감사 주체는 로컬과 같은 코드.
- 외부 사용자: `PUT /users/{key}`에 `password`가 있으면 400 `external_user`("the password of an external user is managed by its realm"), 역할은 바꿀 수 있으나 다음 로그인에 매핑으로 다시 정해짐. `UserView.realm`(None이면 쓰지 않음). `AppState::principal_for`와 세션·토큰 인증은 사용자의 `realm`이 있는데 그 `Realm`이 없으면 401(`realm_exists`) — 영역을 지우면 그 사용자의 세션·토큰이 모든 노드에서 멈추고, 같은 이름으로 다시 만들면 이어 쓴다. `delete_user`는 그대로(토큰도 지움).
- oplctl: `realm list`(NAME·DISPLAY·KIND·URL)`|show <이름>`(`bind password: set|none`, `ca: custom (PEM)|system roots`, 그룹 표, `default role`)`|add-ldap <이름> --url --base-dn [--bind-dn] [--bind-password-file <파일|->] [--user-filter] [--group-attr] [--ca-file] [--timeout] [--display] [--group <DN>=<role>]... [--default-role]`(준 것만 본문에, `--group`은 마지막 `=`에서 나눔)`|set-ldap <이름> (같은 선택) [--clear-bind-password] [--clear-bind-dn] [--clear-ca] [--clear-default-role] [--clear-groups]`(영역을 읽어 준 것만 바꾸고 읽은 revision을 `If-Match`로 `PUT`, 비밀번호는 주지 않으면 빼서 서버가 둠 — URL·바인드 DN·CA를 바꾸며 주지 않으면 서버의 400을 그대로 보임, `--group`을 주면 표 전체를 바꿈; `--clear-bind-dn`은 `bind_dn`을 빼고 `bind_password: ""`, 각 `--clear-*`는 같은 값을 정하는 선택과 clap `conflicts_with`, `--clear-bind-password`만 주고 `bind_dn`이 남으면 보내지 않고 "--clear-bind-password needs --clear-bind-dn (or give --bind-password-file)")`|remove <이름>|test <이름> --user <이름> [--password-file <파일|->]`(단계 표·`dn`·`groups`·`role`, 실패한 단계가 있으면 오류 종료 — 역할 없음만은 성공, 비밀번호 파일이 없으면 터미널에서 물음). `login --realm <이름>`(기본 `local`, `local`이면 본문에 `realm`을 넣지 않음, 출력의 이름은 서버가 정한 `alice@corp`). 설계와 다른 점: 설계의 `realm set`은 `set-ldap`이다.
- 웹: 로그인 화면이 인증 없이 `GET /auth/realms`를 읽어 `local` 말고도 있으면 영역 고르기(기본 "로컬 사용자", 읽지 못하면 숨김), 고른 영역을 `realm`으로(`local`이면 보내지 않음). 오류 문구 `errors.no_role`·`errors.realm_unavailable`·`errors.external_user`(ko·en). `web/src/realms/RealmsPage.tsx`·`realms.ts`, 라우트 `/realms`, 메뉴 `nav.realms` "인증 영역"/"Realms"(사용자 다음, admin만 — 다른 역할이 주소로 오면 읽지 않고 안내): 목록, LDAP 영역 만들기·고치기 대화상자(서버 검증과 같은 입력 검사, 바인드 비밀번호는 비우면 필드를 빼고 "저장된 비밀번호 지우기"면 `""` — 서버와 같은 규칙으로 저장 전에 검사: 저장된 비밀번호가 있고 URL·바인드 DN·CA를 바꾸며 비밀번호 칸이 비면 "다시 넣으세요"(바인드 DN을 비운 경우는 "저장된 비밀번호 지우기를 고르세요"), 시간 제한 1~30, 바인드 DN에 비밀번호도 저장된 것도 없으면 오류, 지우기는 바인드 DN도 비워야, 그룹 → 역할 표(행 오류는 `aria-describedby`, 역할 선택은 내용 너비), 기본 역할, 고치기는 읽은 revision을 `If-Match`로; 고치기·지우기·시험 API는 id가 아니라 이름으로 불러 감사 대상이 `realms/<이름>`), 지우기 확인, 시험 대화상자(단계 표·DN·그룹·정해질 역할). 사용자 목록의 영역 열(없으면 `local`), 외부 사용자는 비밀번호 재설정 메뉴가 없고 계정 메뉴의 "내 비밀번호 바꾸기"는 이름에 `@`가 있으면 숨김. 설계와 다른 점: 영역 화면은 "설정 아래"가 아니라 사용자 다음의 메뉴 항목이다.
- 시험: opl-types 단위(JSON 왕복·기본값·모르는 필드·검증 각 경우·`role_for`·이름 규칙·`realm` 없는 `UserSpec`이 계획 34 모양), `RoleAuthorizer` 단위, 가짜 LDAP 서버 `tests/common/ldap.rs`(`lber`·`bytes` dev 의존; `FakeLdap::start(Mode::Ldaps|Mode::StartTls|Mode::StartTlsRefused, Directory)`, `truncate_at(n)`·`send_endless_references()`, `size_limits()`·`abandons()`, `url`·`ca_pem`, `Directory::with`, `Person::new(uid, pw, &[groups])`, `set_groups`, `SERVICE_DN`·`SERVICE_PASSWORD`·`BASE_DN`, `connections()`·`binds()`·`searches()`·`assertions()` 세기; 시험 CA가 서명한 127.0.0.1 인증서, 필터는 등호와 `&`만), `tests/realms.rs`(CRUD, 권한 403, `local`·UUID 모양 이름·검증 400, `url`·`bind_dn`·`ca_pem` 다듬기(끝 줄바꿈·CRLF CA로 만든 뒤 다듬은 CA로 PUT하면 비밀번호 유지), `has_bind_password`와 비밀번호 유지·지우기·URL·바인드 DN·CA를 바꾸며 빼면 400·`bind_dn`과 `""`는 400, 옛 노드 409, 인증 없는 `/auth/realms`, 감사), `tests/ldap.rs`(LDAPS·StartTLS 성공, 익명 검색, 틀린·빈 비밀번호(바인드가 가지 않음)·없는 사용자·두 항목, 이스케이프, 서비스 바인드 실패·닫힌 포트·믿지 않는 CA는 Unavailable, 크기 제한 2와 50개 항목에서 멈춤·abandon·결과 코드 4, 전체 마감, `ldaps://localhost`(인증서는 127.0.0.1)와 StartTLS 거절은 바인드 없이 Unavailable, 자격 증명 실패는 300ms 이상, 시험 동작의 단계와 권한, 응답에 비밀번호 없음), `tests/ldap_login.rs`(첫 로그인에 `alice@corp`·두 번째는 revision 그대로, 그룹이 바뀌면 역할과 옛 세션 401, 403·503·400 구분, 외부 이름 키의 시도 제한·`no_role` 5번 뒤 429, 역할이 바뀌면 다른 세션은 끝나고 토큰은 새 역할, 외부 사용자 비밀번호 400, 토큰과 영역 지우기·다시 만들기, 겹친 첫 로그인에 사용자 하나, 옛 노드 409, 감사 `realm=corp`), `tests/openapi.rs`, oplctl 단위와 `fake_api_bodies`(본문·`If-Match`), 웹 Vitest(`RealmsPage.test.tsx`, `App.test.tsx`의 로그인 영역, `UsersPage`·`ChangePasswordDialog`·`AppLayout`·`permissions`). 실제 OpenLDAP: opl-api 기능 `ldap-it`의 `tests/ldap_it.rs`(`OPL_TEST_LDAP_*` — LDAPS·StartTLS 찾기·바인드·memberOf, 틀린 자격 증명, 서비스 계정 실패·믿지 않는 CA, API의 시험 동작(틀린 서비스 계정 비밀번호는 `service_bind`에서 멈춤)과 외부 사용자 로그인(대문자 이름도 같은 사용자)·`no_role`), `.github/scripts/ldap-setup.sh`(POSIX sh, root, 이 기계의 slapd 설정과 디렉터리를 지우므로 `CI=true`나 `OPL_LDAP_SETUP_WIPE=1`이 아니면 설명을 찍고 종료 1: debconf로 slapd를 늘 `dpkg-reconfigure`해 새 데이터베이스에서 시작 — 다시 돌려도 됨, openssl 시험 CA와 127.0.0.1 서버 인증서로 LDAPS(636)·StartTLS(389), `memberof` 모듈(`olcModuleList`의 `olcModuleLoad`를 받아 `grep -qi memberof`로 감지)과 오버레이, `ou=people`의 사용자 둘과 `ou=groups`의 `groupOfNames` 하나, `ldapsearch`·`ldapwhoami`로 확인; 기준 DN은 `dc=…`만, 바인드 DN은 `cn=admin,<기준 DN>`, 그룹은 `ou=groups` 아래여야 함), CI 작업 `ldap`(ubuntu-latest, `sudo -E sh .github/scripts/ldap-setup.sh` 뒤 `cargo test -p opl-api --features ldap-it --test ldap_it`, 실패하면 slapd 저널), `shell` 작업의 shellcheck에 `.github/scripts/*.sh`.

## 4. OIDC SSO (계획 36)
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
| `no_role` | 403 | 외부 사용자의 그룹·클레임에 맞는 역할이 없음 |
| `realm_unavailable` | 503 | LDAP 서버·IdP에 닿지 못함, 서비스 바인드 실패 |
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `sso_failed` | (리디렉트) | OIDC 콜백 확인 실패 |
```

을 다음으로 바꾼다:

```markdown
| `no_role` | 403 | 외부 사용자의 그룹·클레임에 맞는 역할이 없음 |
| `realm_unavailable` | 503 | LDAP 서버·IdP에 닿지 못함(연결·TLS·믿지 않는 인증서), 서비스 바인드·검색 실패, 사용자 바인드의 49가 아닌 실패 |
| `external_user` | 400 | 외부 사용자의 비밀번호 바꾸기 |
| `invalid_request` | 400 | 모르는 영역으로 로그인, 영역 이름 `local`, 영역 검증 실패(계획 35) |
| `sso_failed` | (리디렉트) | OIDC 콜백 확인 실패 |
```

`docs/superpowers/specs/2026-11-06-enterprise-design.md`에서

```markdown
- 외부 사용자를 디렉터리에서 지워도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다.
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 노드 수만큼 등록해야 한다.
```

을 다음으로 바꾼다:

```markdown
- 외부 사용자를 디렉터리에서 지워도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다.
- (계획 35) 영역을 지우고 같은 이름으로 다시 만들면 옛 영역 사용자의 세션(12시간 안)과 API 토큰이 지울 때의 역할로 되살아난다(새 영역이 다른 디렉터리를 가리켜도) — 다시 쓰기 전에 그 영역의 사용자를 지운다. 같은 사용자의 거의 동시 로그인 사이에 그룹이 바뀌면 역할이 옛 것으로 되돌아갈 수 있다(다음 로그인에 바로잡힘). 디렉터리는 로그인할 때만 보므로 그룹·계정 변화는 다음 로그인에 반영된다(바로 막으려면 opLite에서 사용자를 지움 — 토큰도 지워짐). 영역을 지워도 열린 콘솔(WebSocket)은 끊기지 않는다. 503 `realm_unavailable`·403 `no_role`도 시도 제한의 실패로 세므로 디렉터리가 멈춘 동안 되풀이한 로그인은 잠시 막힐 수 있다. 그룹은 사용자 항목의 속성 값만 보고(그룹 항목 찾기·중첩 그룹 풀기 없음) DN은 정규화 없이 문자열로 비교한다. 서버 주소는 하나이고 참조를 따라가지 않으며 단순 바인드만 한다. 사용자 부분이 `[a-z0-9._-]`(1~64자, 대문자는 소문자로)가 아닌 계정은 로그인할 수 없다. 시스템 루트를 읽지 못한 노드는 `ca_pem` 없는 영역에 연결하지 못한다. 서비스 계정 비밀번호는 Raft에 평문이다.
- 노드마다 주소가 다르면 IdP에 리디렉트 URI를 노드 수만큼 등록해야 한다.
```

- [ ] **Step 2: 확인**

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add README.md docs
git commit -m "docs: 외부 인증(LDAP) — README \"외부 인증(LDAP)\" 절(영역 개념과 admin 권한, 외부 사용자 이름 <사용자>@<영역>과 소문자·문자 규칙, 그룹 → 역할 매핑과 가장 높은 역할·기본 역할·no_role, 역할이 바뀌면 세션 끝남과 admin이 바꾼 역할은 다음 로그인에 다시 정해짐, OpenLDAP·Active Directory 예와 oplctl realm list|show|add-ldap|set-ldap|remove|test·login --realm, 설정 항목과 bind_password 유지·지우기 규칙, TLS(StartTLS 필수·호스트 이름 확인·CA·시스템 루트), 로그인 순서, 401·503·400·403·409 오류, 503·403도 세는 시도 제한, 감사 detail의 realm, 시험 동작, 웹, API, 영역을 지울 때, 한계)과 웹 UI 목록·감사 로그 detail·개발의 ldap-it 실행 방법 갱신, 엔터프라이즈 설계 §3.6 구현(계획 35)(set-ldap, 영역 화면 위치, watch 제외, 503 범위, 시도 제한 등 설계와 다른 점)과 §7 오류·§9 알려진 틈, 노드 코어 설계 §1.1에 계획 35 완료"
```

---

## 완료 기준 (계획 35)

- `oplctl realm add-ldap corp …`으로 만든 영역에 대해 `oplctl realm test corp --user alice`가 단계별로 성공하고, 웹 로그인 화면에서 "Example Corp"를 골라 디렉터리 비밀번호로 들어오면 사용자 목록에 `alice@corp`(영역 corp, 그룹으로 정한 역할)가 보인다.
- 틀린 비밀번호·없는 사용자·빈 비밀번호는 같은 401, 매핑 없음은 403 `no_role`, 디렉터리가 꺼져 있으면 503 `realm_unavailable`이다.
- 영역 응답·목록·감사 로그 어디에도 서비스 계정 비밀번호가 없고, URL·bind DN·CA를 바꾸면 비밀번호를 다시 넣어야 한다.
- CI `ldap` 작업이 실제 slapd(LDAPS·StartTLS·memberOf)로 로그인 시험을 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 `typecheck`·`lint`·`test`·`build`, shellcheck가 통과한다.

## 알고 받아들인 제한

- 디렉터리에서 사용자를 지우거나 그룹에서 빼도 이미 있는 세션(최대 12시간)과 API 토큰은 남는다(관리자가 지움). 역할은 다음 로그인 때 바뀐다.
- 영역을 지우고 같은 이름으로 다시 만들면 옛 세션·토큰이 되살아난다.
- 디렉터리가 300ms보다 늦게 답하면 없는 사용자와 틀린 비밀번호의 응답 시간 차가 드러날 수 있다.
- 같은 사용자의 로그인이 거의 동시에 겹치면 드물게 옛 역할이 다시 쓰일 수 있다.
- 그 밖의 제한은 설계 문서 §3.6·§9와 README "외부 인증(LDAP)" 절에 있다.
