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
