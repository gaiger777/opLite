# opLite 계획 32: 버전과 업데이트 채널 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 노드마다 opLite 버전을 보이고, 버전이 낮은 노드의 합류를 막는다. 클러스터 설정 `settings`로 업데이트 채널(stable·testing)을 고른다. 각 노드는 opld가 관리하는 APT 소스로 업데이트를 확인한다. 관리자는 API·oplctl·웹에서 노드를 하나씩 업그레이드한다. APT 저장소를 만드는 스크립트도 둔다. 하위 프로젝트 9(설치·업데이트·구독)의 두 번째 계획이다.

**Architecture:**
- **버전:** 계획 31의 `opl_types::version::BUILD`를 노드 상태·`/api/v1/version`·합류 요청에 싣는다. 비교는 Debian 규칙의 작은 구현이다.
- **설정:** Raft에 둔 새 리소스 종류 `settings`(이름 `cluster` 하나). 옛 노드는 모르는 종류가 든 Raft 항목을 풀지 못하므로, 모든 노드가 버전을 보고하기 전에는 새 종류·새 Task를 쓰지 않는다(409 `cluster_upgrading`).
- **업데이트 확인:** opld가 노드마다 소스 파일을 쓰고 `apt-get -s dist-upgrade`의 `Inst` 줄을 읽어 노드 상태에 쓴다. 다른 노드에 일을 시키는 길은 Task뿐이라 "다시 확인"도 Task다.
- **업그레이드:** 그 노드의 Task가 `systemd-run`으로 별도 유닛에서 apt를 돌리고 지켜본다. 패키지 업그레이드로 opld가 다시 시작해도 유닛은 계속 돌고, 새 opld가 같은 유닛을 이어서 본다.
- **저장소:** `apt-ftparchive`와 `gpg`로 만드는 정적 디렉터리. 어디에 올릴지는 범위 밖이다.

**Tech Stack:** Rust 2024(MSRV 1.88), axum·utoipa, openraft 클러스터 상태, tokio, clap. 웹은 React·TypeScript·react-query·vitest·msw. 셸은 POSIX sh와 bash(시험). APT(deb822 소스, apt-ftparchive, gpg), systemd(`systemd-run`, `systemctl show`). CI는 GitHub Actions(ubuntu-latest, Docker `debian:13`, KVM). 새 Rust 크레이트·npm 패키지는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md` (§3와 §3.8 "구현(계획 32)", §5)
**선행 계획:** 계획 1~31 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| 노드 버전, `/api/v1/version`, 합류 버전 거절, 혼합 버전 경고 | 자동 롤링 업그레이드, 업그레이드 뒤 재부팅 |
| `settings` 리소스와 업데이트 채널(stable·testing) | `enterprise` 채널과 구독 (계획 33) |
| opld `[updates]`, 소스 파일 관리, 업데이트 확인, 다시 확인 Task | 감사 로그(코드베이스에 없음) |
| 업그레이드 Task(한 번에 한 노드, 재시작 뒤 이어 보기) | 업그레이드 취소·시간 제한, 다운그레이드 |
| APT 저장소 스크립트, 패키지 키링, CI 저장소 시험 | 저장소 호스팅 |
| oplctl `node updates|upgrade`, `settings get|set`, 웹 화면 | 텔레메트리 (계획 33) |

## 실행 환경

- **이 Mac에서 하는 것:** Rust·웹 변경과 시험, 셸 시험(`packaging/tests/*-test.sh` 가운데 `deb-test.sh`·`repo-test.sh` 밖의 것). 진짜 `apt-get`·`systemd-run`이 없으므로 Rust 시험은 가짜 스크립트로 한다.
- **이 Mac에서 못 하는 것:** Docker 컨테이너 시험(`deb-test.sh`·`repo-test.sh`)과 ISO E2E는 CI(`ci.yml`의 `package`, `iso.yml`)가 한다.
- **shellcheck:** 없으면 CI `shell` 작업이 본다.

## 설계 결정

- **옛 노드와의 호환:** `settings` 종류와 `node-update-check`·`node-upgrade` Task는 옛 노드가 풀지 못한다. 모든 노드의 `status.version`이 있어야(계획 32 이상) 쓰고, 그 전에는 409 `cluster_upgrading`. 버전을 보내지 않는 옛 노드는 버전이 있는 클러스터에 합류할 수 없다. 그래서 처음 계획 32로 올릴 때는 노드마다 손으로 `apt`/`dpkg`로 올린다.
- **다시 확인은 Task:** 다른 노드로 HTTP를 전달하는 장치가 없어서, 다시 확인 요청은 그 노드가 실행하는 Task로 만든다.
- **업그레이드 판단:** `--collect`로 끝난 유닛은 곧 사라진다. 그래서 스크립트가 로그에 시작 줄과 종료 코드 줄을 남기고, 유닛이 없으면 이 줄로 판단한다(시작 줄이 없으면 다시 시작, 종료 줄이 없으면 중단으로 실패, `dpkg --audit`는 안내 문구에만).
- **apt 잠금:** 업그레이드 동안 확인 잠금을 잡고, 다시 확인은 409 또는 기다리지 않고 끝낸다. apt에는 `DPkg::Lock::Timeout`을 준다(업그레이드 600초, 확인 60초).
- **소스 파일:** 첫 줄 `# managed by opld; edits are overwritten`가 없는 파일은 건드리지 않는다. 키링이 없으면 쓰지 않는다(없는 키링을 가리키는 소스는 그 호스트의 모든 `apt-get update`를 깨뜨린다).
- **웹:** 노드 상세 페이지가 없으므로 업데이트는 노드 표에서 여는 대화상자다.

## 파일 구조

```
crates/opl-types/src/{version.rs, node.rs, settings.rs, task.rs, resource.rs, lib.rs}
crates/opl-api/src/{cluster.rs, settings.rs, updates.rs, tasks.rs, watch.rs, lib.rs, …}
crates/opl-api/tests/{cluster.rs, settings.rs, updates.rs, openapi.rs, common/{mod.rs, fake_apt.rs}, …}
crates/opld/src/{config.rs, bootstrap.rs, member.rs, join.rs}, crates/oplctl/src/{cli.rs, client.rs}
packaging/repo/build-repo.sh, packaging/deb/{build-deb.sh, postrm}, packaging/tests/{build-repo-test.sh, repo-test.sh, …}
web/src/{nodes/NodeUpdatesDialog.tsx, settings/SettingsPage.tsx, lib/version.ts, …}
.github/workflows/{ci.yml, iso.yml}, README.md, docs/superpowers/specs/…
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

`web/src/api/schema.d.ts`는 싣지 않는다. API가 바뀌는 Task의 "생성 파일" 단계에서 `pnpm -C web gen:api`로 만든다. 셸 스크립트의 들여쓰기 탭은 그대로 옮긴다. 새 실행 파일은 "실행 권한" 단계에서 `chmod 0755`를 한다.

---

### Task 1: 노드 버전과 합류 버전 검사

`opl_types::version::compare`(Debian 버전 규칙: 에포크, 업스트림, 리비전, `~`가 가장 낮음)와 `SUITE`·`APT_URL`(빌드 때 `OPL_APT_URL`). 로그인 없이 쓰는 `GET /api/v1/version`. `NodeStatus.version`과 `GET /cluster` 멤버의 `version`(노드가 등록할 때 빌드 버전을 쓴다). 합류 요청이 버전을 싣고, 받는 노드는 클러스터의 가장 낮은 노드 버전보다 낮거나(버전을 모르는 옛 노드 포함 — 버전이 있는 클러스터라면) 409 `version_too_old`로 거절한다(토큰은 쓰지 않음). oplctl `node list`의 VERSION 열과 `cluster status`의 혼합 버전 경고.

**Files:**
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/tests/cluster.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/ha.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/version.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/join.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs` 전체:

```rust
//! 클러스터 정보, 노드 합류(조인 토큰, CSR 서명), 노드 제거.
//!
//! 새 노드는 조인 URL을 받아 이 노드의 `POST /cluster/join`을 부른다. 토큰을 지워 한 번만 쓰게 하고,
//! 노드의 CSR에 클러스터 CA로 서명하고, Node 리소스를 만들고, 노드를 learner로 더한다. 새 노드는
//! 받은 인증서로 노드 간 서버를 띄우고 로그를 따라잡은 뒤 스스로 투표 멤버 승격을 요청한다.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use opl_cluster::ClusterError;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_peer::pki::{ClusterCa, fingerprint};
use opl_types::cluster::{ClusterInfo, ClusterSpec, JoinToken, JoinTokenSpec};
use opl_types::node::{Fencing, NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::version;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal, constant_time_eq, random_hex, sha256_hex};
use crate::error::{ApiError, Json};
use crate::resources::{check_name, lookup};
use crate::state::AppState;

/// 조인 토큰의 기본 유효 시간
pub const DEFAULT_JOIN_TTL: Duration = Duration::from_secs(3600);
/// 조인 토큰의 최대 유효 시간
const MAX_JOIN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const JOIN_TOKEN_PREFIX: &str = "oplj_";
const JOIN_SCHEME: &str = "opl-join://";

/// `opl-join://<API 주소>?token=<토큰>&ca=<CA 지문>`. `oplctl cluster join`에 그대로 넘긴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinUrl {
    /// 합류 요청을 받을 노드의 API 주소(`host:port`)
    pub address: String,
    pub token: String,
    /// 클러스터 CA 인증서의 지문(`sha256:<16진수>`). 새 노드는 이것으로 상대를 확인한다.
    pub ca_fingerprint: String,
}

impl fmt::Display for JoinUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{JOIN_SCHEME}{}?token={}&ca={}",
            self.address, self.token, self.ca_fingerprint
        )
    }
}

impl FromStr for JoinUrl {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "invalid join URL {s:?}: expected {JOIN_SCHEME}<host:port>?token=...&ca=sha256:..."
            )
        };
        let rest = s.trim().strip_prefix(JOIN_SCHEME).ok_or_else(bad)?;
        let (address, query) = rest.split_once('?').ok_or_else(bad)?;
        let (mut token, mut ca) = (None, None);
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("token", v)) => token = Some(v.to_string()),
                Some(("ca", v)) => ca = Some(v.to_string()),
                _ => return Err(bad()),
            }
        }
        if host_of(address).is_none() {
            return Err(bad());
        }
        Ok(JoinUrl {
            address: address.to_string(),
            token: token.ok_or_else(bad)?,
            ca_fingerprint: ca.filter(|c| c.starts_with("sha256:")).ok_or_else(bad)?,
        })
    }
}

/// `host:port`나 `[v6]:port`의 호스트. 포트가 숫자가 아니면 `None`이다.
pub fn host_of(address: &str) -> Option<&str> {
    let (host, port) = address.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// 인증서 SAN에 넣을 수 있는 호스트인지. 특수 주소가 아닌 IP나 RFC 1123 호스트 이름(와일드카드, 끝의 점 없이)
fn valid_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        let broadcast = matches!(ip, IpAddr::V4(v4) if v4.is_broadcast());
        return !ip.is_unspecified() && !ip.is_multicast() && !broadcast;
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 클러스터 정보 리소스. 클러스터를 만들 때 하나 생긴다.
fn cluster_info(state: &AppState) -> Result<ClusterInfo, ApiError> {
    state
        .cluster
        .list::<ClusterSpec>()?
        .1
        .into_iter()
        .next()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_not_initialized",
                "the cluster has not been initialized",
            )
        })
}

fn ca_fingerprint(info: &ClusterInfo) -> Result<String, ApiError> {
    fingerprint(&info.spec.ca_cert_pem).map_err(ApiError::internal)
}

/// 클러스터 멤버 한 노드
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MemberView {
    pub id: Uuid,
    /// Node 리소스 이름. 아직 없으면(합류 중) 비어 있다.
    pub name: Option<String>,
    /// API 주소
    pub address: Option<String>,
    /// 노드 간 통신 주소
    pub cluster_address: String,
    /// 투표 멤버인지(아니면 learner)
    pub voter: bool,
    pub online: bool,
    /// 위트니스 노드인지(VM을 돌리지 않는 투표 멤버)
    pub witness: bool,
    /// 노드의 HA 펜싱. 펜싱이 꺼졌거나 Node가 아직 없으면 null이다.
    pub fencing: Option<Fencing>,
    /// 노드의 opld 빌드 버전. 옛 버전 노드이거나 Node가 아직 없으면 null이다.
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClusterView {
    pub name: String,
    /// 클러스터 CA 인증서의 지문. `oplctl login --ca-fingerprint`에 쓴다.
    pub ca_fingerprint: String,
    pub leader: Option<Uuid>,
    /// 이 응답을 보낸 노드의 id. 볼륨 업로드처럼 받은 노드에서 처리하는 요청에 쓴다.
    pub node: Uuid,
    pub members: Vec<MemberView>,
}

/// 클러스터 이름, CA 지문, 멤버(이 노드가 아는 Raft 멤버십)
#[utoipa::path(get, path = "/cluster", tag = "cluster",
    responses((status = 200, body = ClusterView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_cluster(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<ClusterView>, ApiError> {
    state.authorize(&principal, Action::Read, ClusterSpec::KIND)?;
    let info = cluster_info(&state)?;
    let members = state.cluster.members();
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let view = |id: NodeId, addr: &String, voter: bool| {
        let node = nodes.iter().find(|n| n.id == id.0);
        MemberView {
            id: id.0,
            name: node.map(|n| n.name.clone()),
            address: node.map(|n| n.spec.address.clone()),
            cluster_address: addr.clone(),
            voter,
            online: node.is_some_and(|n| n.status.online),
            witness: node.is_some_and(|n| n.spec.witness),
            fencing: node.and_then(|n| n.status.ha).map(|ha| ha.fencing),
            version: node.and_then(|n| n.status.version.clone()),
        }
    };
    let mut list: Vec<MemberView> = members
        .voters
        .iter()
        .map(|(id, addr)| view(*id, addr, true))
        .collect();
    list.extend(
        members
            .learners
            .iter()
            .map(|(id, addr)| view(*id, addr, false)),
    );
    Ok(Json(ClusterView {
        name: info.name.clone(),
        ca_fingerprint: ca_fingerprint(&info)?,
        leader: members.leader.map(|id| id.0),
        node: state.node,
        members: list,
    }))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct VersionInfo {
    /// 이 노드의 opld 빌드 버전
    pub version: String,
}

/// 이 노드의 빌드 버전. 인증 없이 읽는다(로그인 화면과 oplctl이 서버 버전을 본다).
#[utoipa::path(get, path = "/version", tag = "cluster", security(()),
    responses((status = 200, body = VersionInfo)))]
pub async fn get_version() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: version::BUILD.to_string(),
    })
}

/// 클러스터 CA 인증서(PEM). 인증 없이 읽는다. 받는 쪽은 반드시 지문을 확인해야 한다.
#[utoipa::path(get, path = "/cluster/ca", tag = "cluster", security(()),
    responses((status = 200, description = "CA certificate", content_type = "application/x-pem-file", body = String),
              (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn get_ca(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let info = cluster_info(&state)?;
    Ok((
        [(CONTENT_TYPE, "application/x-pem-file")],
        info.spec.ca_cert_pem,
    ))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenRequest {
    /// 유효 시간(초). 기본 1시간, 최대 7일
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedJoinToken {
    /// 새 노드에서 `oplctl cluster join`에 넘길 URL. 다시 볼 수 없다.
    pub url: String,
    /// 만료 시각(유닉스 초)
    pub expires_at: u64,
}

/// 새 토큰과 그 조인 URL(이 노드의 API 주소). 토큰은 한 번만 쓸 수 있다.
pub async fn issue_join_token(
    state: &AppState,
    ttl: Duration,
) -> Result<CreatedJoinToken, ApiError> {
    let info = cluster_info(state)?;
    let me = state
        .cluster
        .get::<NodeSpec>(state.node)?
        .ok_or_else(|| ApiError::internal("this node has no Node resource"))?;
    let secret = random_hex(32);
    let expires_at = now_secs() + ttl.as_secs();
    let mut token: JoinToken = Resource::new(
        "",
        JoinTokenSpec {
            secret_hash: sha256_hex(&secret),
            expires_at,
        },
    );
    token.name = token.id.to_string();
    state.cluster.create(&token).await?;
    let url = JoinUrl {
        address: me.spec.address,
        token: format!("{JOIN_TOKEN_PREFIX}{}_{secret}", token.id.simple()),
        ca_fingerprint: ca_fingerprint(&info)?,
    };
    Ok(CreatedJoinToken {
        url: url.to_string(),
        expires_at,
    })
}

#[utoipa::path(post, path = "/cluster/join-tokens", tag = "cluster",
    request_body = JoinTokenRequest,
    responses((status = 201, body = CreatedJoinToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_join_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<JoinTokenRequest>,
) -> Result<(StatusCode, Json<CreatedJoinToken>), ApiError> {
    state.authorize(&principal, Action::Write, JoinTokenSpec::KIND)?;
    let ttl = req.ttl_secs.map_or(DEFAULT_JOIN_TTL, Duration::from_secs);
    if ttl.is_zero() || ttl > MAX_JOIN_TTL {
        return Err(ApiError::bad_request(
            "ttl_secs must be between 1 and 604800",
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(issue_join_token(&state, ttl).await?),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    /// 조인 URL의 토큰
    pub token: String,
    /// 새 노드의 id(Raft 노드 id, Node 리소스 id)
    pub node: Uuid,
    /// 새 노드의 이름
    pub name: String,
    /// 새 노드의 API 주소(`host:port`)
    pub address: String,
    /// 새 노드의 노드 간 통신 주소(`host:port`)
    pub cluster_address: String,
    /// 새 노드가 만든 키의 CSR(PEM)
    pub csr_pem: String,
    /// 새 노드가 위트니스(투표만 하고 VM을 돌리지 않는 노드)인지
    #[serde(default)]
    pub witness: bool,
    /// 새 노드의 opld 빌드 버전. 옛 버전 노드는 보내지 않는다.
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JoinResponse {
    pub cluster: String,
    pub ca_pem: String,
    /// 새 노드의 인증서(PEM)
    pub cert_pem: String,
    /// 이 노드의 노드 간 통신 주소. 새 노드는 여기로 승격을 요청한다.
    pub via: String,
}

/// 합류하는 노드가 클러스터의 가장 낮은 노드 버전(위트니스 포함, 버전을 적은 노드만, 합류하는 노드
/// 자신의 옛 기록은 빼고)보다 낮으면 거절한다. 버전을 보내지 않는 옛 노드도 버전을 적은 노드가
/// 있으면 거절한다. 새 노드가 옛 노드가 모르는 상태를 쓰지 않게 한다.
fn check_version(state: &AppState, joining: Uuid, version: Option<&str>) -> Result<(), ApiError> {
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let oldest = nodes
        .iter()
        .filter(|n| n.id != joining)
        .filter_map(|n| n.status.version.as_deref())
        .min_by(|a, b| version::compare(a, b));
    let Some(oldest) = oldest else {
        return Ok(());
    };
    let runs = match version {
        Some(version) if version::compare(version, oldest).is_lt() => version,
        Some(_) => return Ok(()),
        None => "an unknown version",
    };
    Err(ApiError::conflict(
        "version_too_old",
        format!("this node runs {runs}; the cluster runs {oldest} or newer; upgrade it first"),
    ))
}

fn invalid_token() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_token",
        "invalid or expired join token",
    )
}

/// 새 노드의 합류 요청. 조인 토큰으로 인증한다(사용자 인증 없음).
#[utoipa::path(post, path = "/cluster/join", tag = "cluster", security(()),
    request_body = JoinRequest,
    responses((status = 200, body = JoinResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn join(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    // 토큰부터 확인해, 토큰 없는 요청에는 클러스터에 대해 아무것도 알려 주지 않는다.
    let (id, secret) = req
        .token
        .strip_prefix(JOIN_TOKEN_PREFIX)
        .and_then(|rest| rest.split_once('_'))
        .and_then(|(id, secret)| Some((Uuid::try_parse(id).ok()?, secret)))
        .ok_or_else(invalid_token)?;
    let token = state
        .cluster
        .get::<JoinTokenSpec>(id)?
        .ok_or_else(invalid_token)?;
    if !constant_time_eq(&sha256_hex(secret), &token.spec.secret_hash)
        || token.spec.expires_at <= now_secs()
    {
        return Err(invalid_token());
    }

    check_version(&state, req.node, req.version.as_deref())?;
    check_name(&req.name)?;
    let (Some(api_host), Some(cluster_host)) = (
        host_of(&req.address).filter(|h| valid_host(h)),
        host_of(&req.cluster_address).filter(|h| valid_host(h)),
    ) else {
        return Err(ApiError::bad_request(
            "address and cluster_address must be host:port with an IP address or a host name",
        ));
    };
    if req.node.is_nil() {
        return Err(ApiError::bad_request("node id must not be nil"));
    }
    // learner이면 합류하다 멈춘 노드다. 같은 이름의 오프라인 노드만 새 토큰으로 다시 합류할 수 있다.
    // 그래야 토큰을 가진 쪽이 승격을 기다리는 살아 있는 노드의 id를 가로채지 못한다.
    let members = state.cluster.members();
    let learner_addr = members.learners.get(&NodeId(req.node)).cloned();
    let rejoin_ok = || -> Result<bool, ApiError> {
        Ok(state
            .cluster
            .get::<NodeSpec>(req.node)?
            .is_some_and(|n| n.name == req.name && !n.status.online))
    };
    if members.voters.contains_key(&NodeId(req.node)) || (learner_addr.is_some() && !rejoin_ok()?) {
        return Err(ApiError::conflict(
            "already_member",
            format!("node {} is already a cluster member", req.node),
        ));
    }
    if let Some(other) = state.cluster.get_by_name::<NodeSpec>(&req.name)?
        && other.id != req.node
    {
        return Err(ApiError::conflict(
            "name_taken",
            format!("node name {:?} is already taken", req.name),
        ));
    }
    let info = cluster_info(&state)?;
    let me = state
        .cluster
        .members()
        .addr(NodeId(state.node))
        .map(str::to_string)
        .ok_or_else(|| ApiError::internal("this node is not in the membership it knows"))?;

    let mut sans = vec![api_host.to_string()];
    if cluster_host != api_host {
        sans.push(cluster_host.to_string());
    }
    sans.extend(["localhost".into(), "127.0.0.1".into(), "::1".into()]);
    let ca = ClusterCa {
        cert_pem: info.spec.ca_cert_pem.clone(),
        key_pem: info.spec.ca_key_pem.clone(),
    };
    let cert_pem = ca
        .sign_node(&req.csr_pem, req.node, &sans)
        .map_err(|e| ApiError::bad_request(format!("cannot sign the CSR: {e}")))?;

    // 토큰을 지운다. 동시에 같은 토큰을 쓴 요청은 하나만 성공한다.
    match state
        .cluster
        .delete::<JoinTokenSpec>(token.id, Some(token.revision))
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(_)) => return Err(invalid_token()),
        Err(e) => return Err(e.into()),
    }

    // 앞서 합류하다 실패한 같은 노드면 Node 리소스를 다시 쓴다.
    // 온라인으로 기록해 첫 핑 전에 노드가 제거되지 않게 한다. 핑이 계속 실패하면 리더가 오프라인으로 바꾼다.
    // 버전은 보낸 값으로 적어, 등록 전에도 버전 검사가 이 노드를 센다.
    let spec = NodeSpec {
        address: req.address.clone(),
        maintenance: false,
        witness: req.witness,
    };
    match state.cluster.get::<NodeSpec>(req.node)? {
        Some(node) => {
            state
                .cluster
                .update_spec(node.id, None, node.labels, &spec)
                .await?;
            let status = NodeStatus {
                online: true,
                version: req.version.clone(),
                ..node.status
            };
            state
                .cluster
                .update_status::<NodeSpec>(node.id, &status)
                .await?;
        }
        None => {
            let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(req.name.clone(), spec);
            node.id = req.node;
            node.status.online = true;
            node.status.version = req.version.clone();
            state.cluster.create(&node).await?;
        }
    }
    // add_learner는 이미 있는 노드의 주소를 바꾸지 않으므로, 주소가 바뀐 learner는 먼저 뺀다.
    if learner_addr.is_some_and(|addr| addr != req.cluster_address) {
        state
            .cluster
            .request(LeaderRequest::Remove {
                id: NodeId(req.node),
            })
            .await?;
    }
    state
        .cluster
        .request(LeaderRequest::AddLearner {
            id: NodeId(req.node),
            addr: req.cluster_address.clone(),
        })
        .await?;
    tracing::info!(node = %req.name, id = %req.node, "node joined as learner");
    Ok(Json(JoinResponse {
        cluster: info.name,
        ca_pem: info.spec.ca_cert_pem,
        cert_pem,
        via: me,
    }))
}

/// 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다.
/// 그 노드에 배정된 VM은 그대로 남는다.
#[utoipa::path(delete, path = "/nodes/{key}", tag = "nodes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Removed"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.id == state.node || node.status.online {
        return Err(ApiError::conflict(
            "node_online",
            format!(
                "node {:?} is online; run `oplctl cluster leave` on it instead",
                node.name
            ),
        ));
    }
    state
        .cluster
        .request(LeaderRequest::Remove {
            id: NodeId(node.id),
        })
        .await?;
    match state.cluster.delete::<NodeSpec>(node.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => {}
        Err(e) => return Err(e.into()),
    }
    tracing::info!(node = %node.name, "node removed from the cluster");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_round_trips() {
        let url = JoinUrl {
            address: "[::1]:8006".into(),
            token: "oplj_abc_def".into(),
            ca_fingerprint: "sha256:00ff".into(),
        };
        let s = url.to_string();
        assert_eq!(s, "opl-join://[::1]:8006?token=oplj_abc_def&ca=sha256:00ff");
        assert_eq!(s.parse::<JoinUrl>().unwrap(), url);
        for bad in [
            "https://h:1?token=a&ca=sha256:00",
            "opl-join://h?token=a&ca=sha256:00",
            "opl-join://h:1?token=a",
            "opl-join://h:1?token=a&ca=md5:00",
            "opl-join://h:1?token=a&ca=sha256:00&x=1",
        ] {
            assert!(bad.parse::<JoinUrl>().is_err(), "{bad}");
        }
    }

    #[test]
    fn host_of_handles_v6_and_names() {
        assert_eq!(host_of("10.0.0.5:8006"), Some("10.0.0.5"));
        assert_eq!(host_of("[::1]:8006"), Some("::1"));
        assert_eq!(host_of("node1.lan:8007"), Some("node1.lan"));
        assert_eq!(host_of("node1.lan"), None);
        assert_eq!(host_of(":8006"), None);
        assert_eq!(host_of("h:port"), None);
    }

    #[test]
    fn valid_host_accepts_ips_and_host_names_only() {
        for good in ["node1.example", "localhost", "10.0.0.5", "::1", "a-b.c1"] {
            assert!(valid_host(good), "{good}");
        }
        let long_label = format!("{}.example", "a".repeat(64));
        let long_name = vec!["a".repeat(63); 4].join(".");
        for bad in [
            "*.corp.example",
            "evil host/x",
            "0.0.0.0",
            "::",
            "224.0.0.1",
            "ff02::1",
            "255.255.255.255",
            &long_label,
            &long_name,
            "-a.example",
            "a-.example",
            "node1.example.",
            "a..b",
            "노드.example",
        ] {
            assert!(!valid_host(bad), "{bad}");
        }
        assert_eq!(host_of("[::1]:8006").filter(|h| valid_host(h)), Some("::1"));
    }
}
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(cluster::get_ca))
        .routes(routes!(cluster::create_join_token))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(cluster::get_ca))
        .routes(routes!(cluster::get_version))
        .routes(routes!(cluster::create_join_token))
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            firewall: false,
        };
```

을 다음으로 바꾼다:

```rust
            firewall: false,
            version: None,
        };
```

`crates/opl-api/tests/cluster.rs`에서

```rust
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::node::{Fencing, NodeHa, NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use reqwest::StatusCode;
```

`crates/opl-api/tests/cluster.rs`에서

```rust
            witness: false,
        },
```

을 다음으로 바꾼다:

```rust
            witness: false,
            version: Some(BUILD.into()),
        },
```

`crates/opl-api/tests/cluster.rs`에서

```rust
}

#[tokio::test]
async fn only_the_same_offline_node_rejoins() {
```

을 다음으로 바꾼다:

```rust
}

/// 빌드 버전은 로그인 없이 읽는다.
#[tokio::test]
async fn version_is_served_without_login() {
    let env = Env::start().await;
    let (status, body) = env.anonymous().get("/version").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "version": BUILD }));
}

/// 이 노드의 기록된 버전을 바꾼다.
async fn set_version(env: &Env, node: Uuid, version: &str) {
    let mut status = env.cluster.get::<NodeSpec>(node).unwrap().unwrap().status;
    status.version = Some(version.into());
    env.cluster
        .update_status::<NodeSpec>(node, &status)
        .await
        .unwrap();
}

/// 클러스터의 가장 낮은 노드 버전(위트니스·오프라인 노드도)보다 낮은 노드와 버전을 보내지 않는
/// 옛 노드는 토큰을 쓰지 않고 거절한다. 같거나 높은 버전은 합류한다. 멤버 목록에 버전이 보인다.
#[tokio::test]
async fn a_node_older_than_the_cluster_is_refused() {
    let env = Env::start().await;
    let anon = env.anonymous();
    set_version(&env, env.node, "1.2.0").await;
    let mut witness: Resource<NodeSpec, NodeStatus> = Resource::new(
        "w",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    witness.status.version = Some("1.1.0".into());
    env.cluster.create(&witness).await.unwrap();

    let token = new_token(&env).await;
    for old in ["1.0.5", "1.1.0~rc1"] {
        let (mut req, _) = join_request(&token, "node2");
        req.version = Some(old.into());
        let (status, body) = anon.post("/cluster/join", json!(req)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "version_too_old");
        assert_eq!(
            body["message"],
            format!("this node runs {old}; the cluster runs 1.1.0 or newer; upgrade it first")
        );
    }
    // 거절은 토큰을 쓰지 않는다.
    let (mut req, node) = join_request(&token, "node2");
    req.version = Some("1.1.0".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (mut req, node3) = join_request(&new_token(&env).await, "node3");
    req.version = Some("1:0.1".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (mut req, _) = join_request(&new_token(&env).await, "node4");
    req.version = None;
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "version_too_old");
    assert_eq!(
        body["message"],
        "this node runs an unknown version; the cluster runs 1.1.0 or newer; upgrade it first"
    );

    // 다시 합류하는 노드 자신의 옛 기록은 세지 않는다.
    mark_offline(&env, node).await;
    set_version(&env, node, "0.1.0").await;
    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.node = node;
    req.version = Some("1.1.0".into());
    let (status, body) = anon.post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (_, view) = env.admin().await.get("/cluster").await;
    let version_of = |id: Uuid| {
        view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.to_string())
            .unwrap()["version"]
            .clone()
    };
    assert_eq!(version_of(env.node), "1.2.0", "{view}");
    // 합류 때 보낸 버전을 등록 전부터 적는다. 다시 합류하면 옛 기록을 바꾼다.
    assert_eq!(version_of(node3), "1:0.1", "{view}");
    assert_eq!(version_of(node), "1.1.0", "{view}");
}

/// 노드 버전을 하나도 적지 않은 옛 클러스터는 버전을 보내지 않는 노드도 받는다.
#[tokio::test]
async fn a_cluster_without_versions_takes_a_node_without_one() {
    let env = Env::start().await;
    let mut status = env
        .cluster
        .get::<NodeSpec>(env.node)
        .unwrap()
        .unwrap()
        .status;
    status.version = None;
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
    let (mut req, _) = join_request(&new_token(&env).await, "node2");
    req.version = None;
    let (status, body) = env.anonymous().post("/cluster/join", json!(req)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn only_the_same_offline_node_rejoins() {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            firewall: false,
        };
```

을 다음으로 바꾼다:

```rust
            firewall: false,
            version: None,
        };
```

`crates/opl-api/tests/ha.rs`에서

```rust
        firewall: false,
    };
```

을 다음으로 바꾼다:

```rust
        firewall: false,
        version: None,
    };
```

`crates/opl-api/tests/migrate.rs`에서

```rust
        firewall: false,
    };
```

을 다음으로 바꾼다:

```rust
        firewall: false,
        version: None,
    };
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/auth/login",
        "/vms",
```

을 다음으로 바꾼다:

```rust
        "/auth/login",
        "/version",
        "/vms",
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            firewall: false,
        };
```

을 다음으로 바꾼다:

```rust
            firewall: false,
            version: None,
        };
```

`crates/opl-types/src/node.rs`에서

```rust
    pub firewall: bool,
}
```

을 다음으로 바꾼다:

```rust
    pub firewall: bool,
    /// 이 노드에서 도는 opld의 빌드 버전. 노드가 등록할 때 쓴다(옛 버전 노드는 None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub version: Option<String>,
}
```

`crates/opl-types/src/node.rs`에서

```rust
    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn status_version_round_trips_and_is_omitted_when_none() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("version").is_none(), "{none}");
        let status = NodeStatus {
            version: Some("0.2.0~ci7".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["version"], "0.2.0~ci7");
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

`crates/opl-types/src/version.rs`에서

```rust
//! 빌드 버전.
```

을 다음으로 바꾼다:

```rust
//! 빌드 버전과 Debian 규칙의 버전 비교.

use std::cmp::Ordering;
```

`crates/opl-types/src/version.rs`에서

```rust
    _ => env!("CARGO_PKG_VERSION"),
};

#[cfg(test)]
```

을 다음으로 바꾼다:

```rust
    _ => env!("CARGO_PKG_VERSION"),
};

/// opLite APT 저장소의 Debian 배포판(suite)
pub const SUITE: &str = "trixie";

/// 이 빌드에 넣은 opLite APT 저장소 URL(빌드 때 환경 변수 `OPL_APT_URL`). 없으면 빈 문자열이다.
pub const APT_URL: &str = match option_env!("OPL_APT_URL") {
    Some(v) => v,
    None => "",
};

/// 두 버전을 Debian 규칙으로 비교한다: `[에포크:]업스트림[-리비전]`, `~`는 끝보다도 낮다.
/// 형식에 맞지 않는 문자열도 패닉하지 않고 같은 규칙으로 비교한다.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (ae, au, ar) = split(a);
    let (be, bu, br) = split(b);
    compare_number(ae, be)
        .then_with(|| compare_part(au.as_bytes(), bu.as_bytes()))
        .then_with(|| compare_part(ar.as_bytes(), br.as_bytes()))
}

/// 에포크(숫자가 아니면 없는 것으로 본다), 업스트림, 리비전(마지막 `-` 뒤)
fn split(v: &str) -> (&str, &str, &str) {
    let (epoch, rest) = match v.split_once(':') {
        Some((e, rest)) if !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit()) => (e, rest),
        _ => ("0", v),
    };
    let (upstream, revision) = rest.rsplit_once('-').unwrap_or((rest, ""));
    (epoch, upstream, revision)
}

/// 비숫자 부분 문자의 순서: `~` < 끝·숫자 < 문자 < 그 밖
fn order(c: Option<u8>) -> i32 {
    match c {
        Some(b'~') => -1,
        None => 0,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => i32::from(c),
        Some(c) => i32::from(c) + 256,
    }
}

/// 숫자 문자열을 수로 비교한다(앞 0 무시, 길이에 제한 없음).
fn compare_number(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// 업스트림·리비전을 비숫자 부분과 숫자 부분을 번갈아 비교한다(dpkg `verrevcmp`).
fn compare_part(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while a.get(i).is_some_and(|c| !c.is_ascii_digit())
            || b.get(j).is_some_and(|c| !c.is_ascii_digit())
        {
            let (x, y) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        let (x, ni) = digits(a, i);
        let (y, nj) = digits(b, j);
        let ord = compare_number(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
        (i, j) = (ni, nj);
    }
    Ordering::Equal
}

/// `from`에서 시작하는 숫자 부분과 그 끝 위치
fn digits(s: &[u8], from: usize) -> (&str, usize) {
    let end = from + s[from..].iter().take_while(|c| c.is_ascii_digit()).count();
    (std::str::from_utf8(&s[from..end]).unwrap_or(""), end)
}

#[cfg(test)]
```

`crates/opl-types/src/version.rs`에서

```rust
        assert_eq!(BUILD, want);
    }
}
```

을 다음으로 바꾼다:

```rust
        assert_eq!(BUILD, want);
    }

    #[test]
    fn apt_url_is_the_build_variable_or_empty() {
        assert_eq!(APT_URL, option_env!("OPL_APT_URL").unwrap_or(""));
        assert_eq!(SUITE, "trixie");
    }

    #[test]
    fn compares_like_dpkg() {
        use Ordering::{Equal, Greater, Less};
        for (a, b, want) in [
            // 같음, 앞 0, 없는 리비전은 0과 같다.
            ("1.2.3", "1.2.3", Equal),
            ("1.01", "1.1", Equal),
            ("1.0", "1.0-0", Equal),
            ("0:1.0", "1.0", Equal),
            // 숫자는 수로 비교한다.
            ("0.10.0", "0.9.0", Greater),
            ("1.0~ci12", "1.0~ci5", Greater),
            ("1.2", "1.2.1", Less),
            // `~`는 끝보다 낮고, 끝은 문자보다, 문자는 그 밖의 문자보다 낮다.
            ("1.0~rc1", "1.0", Less),
            ("1.0~~", "1.0~", Less),
            ("1.0", "1.0a", Less),
            ("1.0a", "1.0+", Less),
            ("1.0", "1.0+1", Less),
            ("1.0b", "1.0a", Greater),
            // 에포크가 먼저다.
            ("1:0.1", "2.0", Greater),
            ("2:1.0", "10:0.1", Less),
            // 리비전은 마지막 `-` 뒤다.
            ("1.0-1", "1.0-2", Less),
            ("1.0-10", "1.0-9", Greater),
            ("1.0-rc-2", "1.0-rc-10", Less),
            ("1.0-1", "1.0", Greater),
            // 아주 큰 수도 넘치지 않는다.
            (
                "99999999999999999999999",
                "99999999999999999999998",
                Greater,
            ),
        ] {
            assert_eq!(compare(a, b), want, "{a} vs {b}");
            assert_eq!(compare(b, a), want.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn odd_strings_do_not_panic() {
        for a in ["", ":", "a:b", "-", "1:", "~", "1.0-", ":-:", "é1", "1:2:3"] {
            for b in ["", "1.0", "~", a] {
                let _ = compare(a, b);
            }
            assert_eq!(compare(a, a), Ordering::Equal, "{a}");
        }
        assert_eq!(compare("", "1"), Ordering::Less);
        assert_eq!(compare("~", ""), Ordering::Less);
    }
}
```

`crates/oplctl/src/cli.rs`에서

```rust
    "ADDRESS",
    "UNDERLAY",
```

을 다음으로 바꾼다:

```rust
    "ADDRESS",
    "VERSION",
    "UNDERLAY",
```

`crates/oplctl/src/cli.rs`에서

```rust
        cell(&n["spec"]["address"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
```

을 다음으로 바꾼다:

```rust
        cell(&n["spec"]["address"]),
        // 옛 버전 노드는 기록하지 않는다.
        cell(&n["status"]["version"]),
        // VXLAN 패킷을 주고받는 주소. 위트니스와 옛 서버는 없다.
```

`crates/oplctl/src/cli.rs`에서

```rust
        writeln!(out, "warning: HA needs at least two non-witness nodes")?;
    }
```

을 다음으로 바꾼다:

```rust
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
```

`crates/oplctl/src/cli.rs`에서

```rust
        a["status"]["underlay"] = json!("10.0.0.1");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
```

을 다음으로 바꾼다:

```rust
        a["status"]["underlay"] = json!("10.0.0.1");
        a["status"]["version"] = json!("0.2.0");
        let rows: Vec<_> = [a, node("w", Some(true)), node("old", None)]
```

`crates/oplctl/src/cli.rs`에서

```rust
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -         true    4     1.0GiB  false        false    -\n"
        );
```

을 다음으로 바꾼다:

```rust
            String::from_utf8(out).unwrap(),
            "NAME  ID      ADDRESS        VERSION  UNDERLAY  ONLINE  CPUS  MEMORY  MAINTENANCE  WITNESS  FENCING\n\
             a     id-a    10.0.0.1:8006  0.2.0    10.0.0.1  true    4     1.0GiB  false        false    watchdog\n\
             w     id-w    10.0.0.1:8006  -        -         true    4     1.0GiB  false        true     -\n\
             old   id-old  10.0.0.1:8006  -        -         true    4     1.0GiB  false        false    -\n"
        );
```

`crates/oplctl/src/cli.rs`에서

```rust
            old,
        ]);
```

을 다음으로 바꾼다:

```rust
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
```

`crates/opld/src/bootstrap.rs`에서

```rust
use opl_types::resource::Resource;
use opl_types::vm::Arch;
```

을 다음으로 바꾼다:

```rust
use opl_types::resource::Resource;
use opl_types::version::BUILD;
use opl_types::vm::Arch;
```

`crates/opld/src/bootstrap.rs`에서

```rust
            firewall: host.firewall.is_some(),
        };
```

을 다음으로 바꾼다:

```rust
            firewall: host.firewall.is_some(),
            version: Some(BUILD.to_string()),
        };
```

`crates/opld/src/bootstrap.rs`에서

```rust
            arch: Some(Arch::native()),
            ..NodeStatus::default()
```

을 다음으로 바꾼다:

```rust
            arch: Some(Arch::native()),
            version: Some(BUILD.to_string()),
            ..NodeStatus::default()
```

`crates/opld/src/bootstrap.rs`에서

```rust

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
```

을 다음으로 바꾼다:

```rust

    /// 등록할 때 이 빌드의 버전을 쓴다(옛 버전으로 적힌 값도 바꾼다).
    #[tokio::test]
    async fn registration_records_the_build_version() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let version = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .version
        };
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        update_node_status(&cluster, |status| {
            status.version = Some("0.0.1".into());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(version().as_deref(), Some(BUILD));
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
```

`crates/opld/src/bootstrap.rs`에서

```rust
                arch: Some(Arch::native()),
                ..NodeStatus::default()
```

을 다음으로 바꾼다:

```rust
                arch: Some(Arch::native()),
                version: Some(BUILD.to_string()),
                ..NodeStatus::default()
```

`crates/opld/src/join.rs`에서

```rust
            witness: config.witness,
        })
```

을 다음으로 바꾼다:

```rust
            witness: config.witness,
            version: Some(opl_types::version::BUILD.to_string()),
        })
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1142개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: 노드 버전과 합류 버전 검사 — opl_types::version::compare(Debian 규칙: 에포크, 업스트림, 마지막 - 뒤 리비전, ~ < 끝 < 문자 < 그 밖, 숫자는 앞 0을 무시한 수 비교, 이상한 문자열도 패닉 없음)·SUITE(\"trixie\")·APT_URL(빌드 때 OPL_APT_URL, 없으면 \"\"), 인증 없는 GET /api/v1/version(VersionInfo, OpenAPI·schema.d.ts), NodeStatus.version(없으면 빼고 씀, register_node·register_witness가 BUILD를 씀), 클러스터 멤버 목록 MemberView.version, JoinRequest.version(opld join::request가 BUILD를 보냄)과 토큰 검사 뒤 이름 검사 앞에서 클러스터의 가장 낮은 노드 버전(위트니스·오프라인 포함, 합류하는 노드 자신의 옛 기록 제외)보다 낮으면 409 version_too_old(토큰은 쓰지 않음), oplctl node list의 VERSION 열(ADDRESS 다음)과 cluster status의 혼합 버전 경고(버전 순, 중복 없이, 버전 모르는 멤버 제외)"
```

---

### Task 2: 클러스터 설정 Settings와 업데이트 채널

새 리소스 종류 `settings`(이름은 언제나 `cluster`, `spec.updates.channel` = `stable`|`testing`, watch 종류에도). `GET /api/v1/settings`(로그인한 누구나, 없으면 기본값), `PATCH /api/v1/settings`(admin, 기존 Patch 관례, 처음 쓰기에서 만듦). 옛 노드는 모르는 종류가 든 Raft 항목을 풀지 못하므로, 버전을 모르는 노드가 있는 동안 새 종류를 쓰는 요청은 409 `cluster_upgrading`으로 막는다. oplctl `settings get|set`.

**Files:**
- Create: `crates/opl-api/src/settings.rs`
- Create: `crates/opl-api/tests/settings.rs`
- Create: `crates/opl-types/src/settings.rs`
- Modify: `crates/opl-api/src/cluster.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`
- Modify: `crates/oplctl/src/cli.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/cluster.rs`에서

```rust

fn invalid_token() -> ApiError {
```

을 다음으로 바꾼다:

```rust

/// 버전을 적지 않은 옛 노드(위트니스 포함)가 남아 있으면 새 종류의 상태를 쓰지 않고 409
/// `cluster_upgrading`으로 거절한다. 버전을 적지 않는 옛 opld는 새 종류의 상태를 풀지 못한다.
pub(crate) fn require_upgraded(cluster: &opl_cluster::Cluster) -> Result<(), ApiError> {
    let mut old: Vec<String> = cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| n.status.version.is_none())
        .map(|n| n.name)
        .collect();
    if old.is_empty() {
        return Ok(());
    }
    old.sort();
    Err(ApiError::conflict(
        "cluster_upgrading",
        format!(
            "some nodes run an older opLite ({}); finish the upgrade on every node first",
            old.join(", ")
        ),
    ))
}

fn invalid_token() -> ApiError {
```

`crates/opl-api/src/lib.rs`에서

```rust
mod security_groups;
mod snapshots;
```

을 다음으로 바꾼다:

```rust
mod security_groups;
mod settings;
mod snapshots;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(cluster::join))
        .routes(routes!(r::list_pools, r::create_pool))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(cluster::join))
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(r::list_pools, r::create_pool))
```

`crates/opl-api/src/settings.rs`를 만든다:

```rust
//! 클러스터 설정 `GET/PATCH /api/v1/settings`. 리소스가 없으면 기본값으로 보고 처음 PATCH가 만든다.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::{Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::if_match;
use crate::state::AppState;

/// 저장된 설정. 없으면 기본값을 담은 리소스(id는 nil UUID, revision 0)다.
pub fn current(cluster: &Cluster) -> Result<Settings, ClusterError> {
    Ok(cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)?
        .unwrap_or_else(|| Resource {
            id: Uuid::nil(),
            ..Resource::new(SETTINGS_NAME, SettingsSpec::default())
        }))
}

/// 설정에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub updates: Option<UpdateSettingsPatch>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub channel: Option<Channel>,
}

/// 클러스터 설정. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/settings", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    Ok(Json(current(&state.cluster)?))
}

/// 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다.
#[utoipa::path(patch, path = "/settings", tag = "settings",
    params(("If-Match" = Option<u64>, Header)),
    request_body = SettingsPatch,
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    Json(req): Json<SettingsPatch>,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let Some(updates) = req.updates else {
        return Err(ApiError::bad_request("give updates"));
    };
    crate::cluster::require_upgraded(&state.cluster)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let settings = current(&state.cluster)?;
        if let Some(expected) = expected
            && expected != settings.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: settings.revision,
            })
            .into());
        }
        let mut spec = settings.spec.clone();
        if let Some(channel) = updates.channel {
            spec.updates.channel = channel;
        }
        if spec == settings.spec {
            return Ok(Json(settings));
        }
        let written = if settings.revision == 0 {
            // 아직 없다. 다른 요청이 먼저 만들었으면 다시 읽는다.
            let mut created = Resource::new(SETTINGS_NAME, spec);
            match state.cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(Json(created));
                }
                Err(e) => e,
            }
        } else {
            match state
                .cluster
                .update_spec(
                    settings.id,
                    Some(settings.revision),
                    settings.labels.clone(),
                    &spec,
                )
                .await
            {
                Ok(_) => return Ok(Json(current(&state.cluster)?)),
                Err(e) => e,
            }
        };
        // If-Match 0으로 만들려 했는데 다른 요청이 먼저 만들었다.
        if let (ClusterError::Rejected(Rejection::NameTaken { .. }), Some(expected)) =
            (&written, expected)
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current(&state.cluster)?.revision,
            })
            .into());
        }
        match written {
            ClusterError::Rejected(Rejection::NameTaken { .. })
            | ClusterError::Rejected(Rejection::RevisionMismatch { .. })
                if expected.is_none() =>
            {
                continue;
            }
            e => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the settings kept changing while being updated; try again",
    ))
}
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 11] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 12] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::VmImport,
];
```

을 다음으로 바꾼다:

```rust
    Kind::VmImport,
    Kind::Settings,
];
```

`crates/opl-api/src/watch.rs`에서

```rust
    #[test]
    fn security_groups_are_watchable() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn settings_are_watchable() {
        let kinds = parse_kinds("settings").unwrap();
        assert!(kinds.contains(&Kind::Settings));
    }

    #[test]
    fn security_groups_are_watchable() {
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
};
use opl_types::vm::Arch;
```

을 다음으로 바꾼다:

```rust
};
use opl_types::version::BUILD;
use opl_types::vm::Arch;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
        };
        cluster.create(&me).await.unwrap();
```

을 다음으로 바꾼다:

```rust
        };
        // 이 노드는 지금 빌드를 돌린다.
        me.status.version = Some(BUILD.into());
        cluster.create(&me).await.unwrap();
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/version",
        "/vms",
```

을 다음으로 바꾼다:

```rust
        "/version",
        "/settings",
        "/vms",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(doc["components"]["schemas"]["Resource_VmImportSpec_VmImportStatus"].is_object());
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
```

을 다음으로 바꾼다:

```rust
    assert!(doc["components"]["schemas"]["Resource_VmImportSpec_VmImportStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_SettingsSpec_SettingsStatus"].is_object());
    assert_eq!(
        doc["paths"]["/settings"]["patch"]["requestBody"]["content"]["application/json"]["schema"]
            ["$ref"],
        "#/components/schemas/SettingsPatch"
    );
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
```

`crates/opl-api/tests/settings.rs`를 만든다:

```rust
//! 클러스터 설정 API: 없으면 기본값, 처음 PATCH가 만듦, 요청 검사와 권한.

mod common;

use common::Env;
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::settings::{Channel, SETTINGS_NAME, SettingsSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

#[tokio::test]
async fn settings_default_until_the_first_patch_creates_them() {
    let env = Env::start().await;
    let admin = env.admin().await;

    // 리소스가 없으면 기본값을 리소스 모양으로 보인다.
    let (status, got) = admin.get("/settings").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], "00000000-0000-0000-0000-000000000000");
    assert_eq!(got["name"], SETTINGS_NAME);
    assert_eq!(got["revision"], 0);
    assert_eq!(got["spec"], json!({ "updates": { "channel": "stable" } }));
    assert!(
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .is_none()
    );

    // 처음 PATCH가 만든다.
    let (status, created) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["name"], SETTINGS_NAME);
    assert_eq!(created["spec"]["updates"]["channel"], "testing");
    let stored = env
        .cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)
        .unwrap()
        .unwrap();
    assert_eq!(stored.spec.updates.channel, Channel::Testing);
    assert_eq!(created["id"], stored.id.to_string());
    assert_eq!(created["revision"], stored.revision);

    // 다음 PATCH는 같은 리소스를 바꾼다.
    let (status, updated) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "stable" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["id"], created["id"]);
    assert_eq!(updated["spec"]["updates"]["channel"], "stable");
    assert!(updated["revision"].as_u64() > created["revision"].as_u64());
    let (_, got) = admin.get("/settings").await;
    assert_eq!(got, updated);

    // 같은 값이면 쓰지 않는다.
    let (status, same) = admin
        .patch("/settings", json!({ "updates": {} }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{same}");
    assert_eq!(same["revision"], updated["revision"]);
}

#[tokio::test]
async fn settings_patch_checks_the_request() {
    let env = Env::start().await;
    let admin = env.admin().await;
    assert_error(
        &admin.patch("/settings", json!({}), None).await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &admin
            .patch(
                "/settings",
                json!({ "telemetry": { "enabled": true } }),
                None,
            )
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &admin
            .patch("/settings", json!({ "updates": { "mirror": "x" } }), None)
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    assert_error(
        &admin
            .patch(
                "/settings",
                json!({ "updates": { "channel": "enterprise" } }),
                None,
            )
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_request",
    );
    // If-Match는 읽은 revision과 맞아야 한다(없는 설정은 0).
    let body = json!({ "updates": { "channel": "testing" } });
    assert_error(
        &admin.patch("/settings", body.clone(), Some(5)).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    let (status, created) = admin.patch("/settings", body.clone(), Some(0)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let revision = created["revision"].as_u64().unwrap();
    let body = json!({ "updates": { "channel": "stable" } });
    assert_error(
        &admin
            .patch("/settings", body.clone(), Some(revision - 1))
            .await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    let (status, got) = admin.patch("/settings", body, Some(revision)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["updates"]["channel"], "stable");
}

#[tokio::test]
async fn only_admins_change_settings() {
    let env = Env::start().await;
    let body = json!({ "updates": { "channel": "testing" } });
    for role in ["operator", "viewer"] {
        let client = env.user(role, role).await;
        let (status, got) = client.get("/settings").await;
        assert_eq!(status, StatusCode::OK, "{got}");
        assert_eq!(got["spec"]["updates"]["channel"], "stable");
        assert_error(
            &client.patch("/settings", body.clone(), None).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    assert_error(
        &env.anonymous().get("/settings").await,
        StatusCode::UNAUTHORIZED,
        "unauthorized",
    );
    assert!(
        env.cluster
            .get_by_name::<SettingsSpec>(SETTINGS_NAME)
            .unwrap()
            .is_none()
    );
}

/// 같은 If-Match 0으로 함께 만들면 하나만 만들고 나머지는 412다(409 name_taken이 아니다).
/// 요청이 정말 겹치도록 여러 스레드에서 돈다.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_patches_with_if_match_lose_with_412() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let body = json!({ "updates": { "channel": "testing" } });
    let got = futures_util::future::join_all(
        (0..32).map(|_| admin.patch("/settings", body.clone(), Some(0))),
    )
    .await;
    let created = got.iter().filter(|g| g.0 == StatusCode::OK).count();
    assert_eq!(created, 1, "{got:?}");
    for g in got.iter().filter(|g| g.0 != StatusCode::OK) {
        assert_error(g, StatusCode::PRECONDITION_FAILED, "revision_mismatch");
    }
}

/// 버전을 적지 않은 옛 노드가 있으면 옛 노드가 풀지 못하는 설정을 쓰지 않는다.
#[tokio::test]
async fn settings_wait_until_every_node_is_upgraded() {
    let env = Env::start().await;
    let admin = env.admin().await;
    let mut olds = Vec::new();
    for name in ["old", "ancient"] {
        let old = Resource::new(
            name,
            NodeSpec {
                address: "10.0.0.9:8006".into(),
                maintenance: false,
                witness: true,
            },
        );
        env.cluster.create(&old).await.unwrap();
        olds.push(old.id);
    }
    let body = json!({ "updates": { "channel": "testing" } });
    let got = admin.patch("/settings", body.clone(), None).await;
    assert_error(&got, StatusCode::CONFLICT, "cluster_upgrading");
    assert_eq!(
        got.1["message"],
        "some nodes run an older opLite (ancient, old); finish the upgrade on every node first"
    );

    for old in olds {
        let mut status = env.cluster.get::<NodeSpec>(old).unwrap().unwrap().status;
        status.version = Some("0.1.0".into());
        env.cluster
            .update_status::<NodeSpec>(old, &status)
            .await
            .unwrap();
    }
    let (status, got) = admin.patch("/settings", body, None).await;
    assert_eq!(status, StatusCode::OK, "{got}");
}
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod security_group;
pub mod snapshot;
```

을 다음으로 바꾼다:

```rust
pub mod security_group;
pub mod settings;
pub mod snapshot;
```

`crates/opl-types/src/resource.rs`에서

```rust
    VmImport,
}
```

을 다음으로 바꾼다:

```rust
    VmImport,
    Settings,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::VmImport => "vm-import",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::VmImport => "vm-import",
            Kind::Settings => "settings",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::VmImport,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::VmImport,
            Kind::Settings,
        ] {
```

`crates/opl-types/src/settings.rs`를 만든다:

```rust
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
```

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: TokenCmd,
    },
```

을 다음으로 바꾼다:

```rust
        cmd: TokenCmd,
    },
    /// 클러스터 설정(업데이트 채널)
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
```

`crates/oplctl/src/cli.rs`에서

```rust
}

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

을 다음으로 바꾼다:

```rust
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// 설정을 `키: 값` 줄로 보인다.
    Get,
    /// 설정을 바꾼다. 키: `updates.channel`(stable|testing)
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

#[derive(Clone, Copy, ValueEnum)]
enum Role {
```

`crates/oplctl/src/cli.rs`에서

```rust
        },
    }
```

을 다음으로 바꾼다:

```rust
        },
        Cmd::Settings { cmd } => settings(&client, cmd, json_out, out).await?,
    }
```

`crates/oplctl/src/cli.rs`에서

```rust
    }

    #[test]
    fn cli_definition_is_valid() {
```

을 다음으로 바꾼다:

```rust
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

    #[test]
    fn cli_definition_is_valid() {
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1154개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: 클러스터 설정 Settings와 업데이트 채널 — opl_types::settings(Kind::Settings \"settings\", 이름은 언제나 SETTINGS_NAME \"cluster\", SettingsSpec{updates: UpdateSettings{channel: Channel}}, Channel stable(기본)|testing 소문자, SettingsStatus{}, 모두 deny_unknown_fields), watch 종류에 settings, GET /api/v1/settings(로그인한 누구나, 없으면 nil id·revision 0·기본 spec의 리소스 모양, opl_api::settings::current), PATCH /api/v1/settings(admin만, SettingsPatch{updates:{channel}} 기존 Patch 관례 — 빈 패치·모르는 필드·잘못된 채널 400, 없으면 create·있으면 update_spec, If-Match는 없는 설정이면 0, 없으면 RevisionMismatch·NameTaken에 10번 다시 뒤 409 busy, 같은 값이면 쓰지 않음; 감사 로그는 없음), OpenAPI·schema.d.ts, oplctl settings get(`updates.channel: stable` 줄, -o json이면 리소스)·settings set KEY=VALUE…(지금은 updates.channel만, 모르는 키는 `unknown setting KEY; known: updates.channel`)"
```

---

### Task 3: 노드 업데이트 확인

opld `[updates]` 설정(apt-get이 있으면 켜짐). 켜진 노드는 저장소 URL이 있고 키링이 있으면 opld가 관리 표시를 단 deb822 소스 파일을 쓰고(시작할 때와 채널이 바뀔 때), 주기적으로 `apt-get update`와 `apt-get -s dist-upgrade`로 업데이트를 확인해 `NodeStatus.updates`에 쓴다. 다른 노드로 HTTP를 전달하는 장치가 없으므로 "다시 확인"(`POST /api/v1/nodes/{key}/updates/refresh`)은 그 노드에서 도는 Task `node-update-check`다. oplctl `node updates [--refresh]`. 시험은 가짜 `apt-get` 스크립트로 한다.

**Files:**
- Create: `crates/opl-api/src/updates.rs`
- Create: `crates/opl-api/tests/common/fake_apt.rs`
- Create: `crates/opl-api/tests/updates.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/scheduler.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/tests/common/mod.rs`
- Modify: `crates/opl-api/tests/ha.rs`
- Modify: `crates/opl-api/tests/migrate.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/two_nodes.rs`
- Modify: `crates/opl-types/src/node.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/opld/src/bootstrap.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/tests/common/mod.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
mod tasks;
pub mod users;
```

을 다음으로 바꾼다:

```rust
mod tasks;
mod updates;
pub mod users;
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
```

을 다음으로 바꾼다:

```rust
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use updates::{KEYRING, UpdateAgent, Updates, UpdatesConfig};
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(r::get_node, r::update_node, cluster::delete_node))
        .routes(routes!(cluster::get_cluster))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(r::get_node, r::update_node, cluster::delete_node))
        .routes(routes!(updates::refresh_updates))
        .routes(routes!(cluster::get_cluster))
```

`crates/opl-api/src/scheduler.rs`에서

```rust
            version: None,
        };
```

을 다음으로 바꾼다:

```rust
            version: None,
            updates: None,
        };
```

`crates/opl-api/src/tasks.rs`에서

```rust
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::vm_imports::ImportCredentials;
```

을 다음으로 바꾼다:

```rust
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::updates::Updates;
use crate::vm_imports::ImportCredentials;
```

`crates/opl-api/src/tasks.rs`에서

```rust
    pub import_credentials: Arc<ImportCredentials>,
}
```

을 다음으로 바꾼다:

```rust
    pub import_credentials: Arc<ImportCredentials>,
    /// 이 노드의 업데이트 확인. 없으면(업데이트가 꺼진 노드) 업데이트 확인 Task는 실패한다.
    pub updates: Option<Arc<Updates>>,
}
```

`crates/opl-api/src/tasks.rs`에서

```rust
            import_credentials: Arc::default(),
        }
```

을 다음으로 바꾼다:

```rust
            import_credentials: Arc::default(),
            updates: None,
        }
```

`crates/opl-api/src/tasks.rs`에서

```rust
        TaskOp::VmImportDelete => import::delete(ctx, id).await,
    }
```

을 다음으로 바꾼다:

```rust
        TaskOp::VmImportDelete => import::delete(ctx, id).await,
        TaskOp::NodeUpdateCheck => match &ctx.config.updates {
            Some(updates) => updates.refresh_waiting(&ctx.cluster, ctx.config.node).await,
            None => Err("updates are not enabled on this node".into()),
        },
    }
```

`crates/opl-api/src/updates.rs`를 만든다:

```rust
//! 노드의 APT 업데이트: opLite 저장소 소스 파일 관리, 업데이트 확인(`apt-get -s dist-upgrade`),
//! 주기 확인 에이전트, 다시 확인 API(`POST /nodes/{key}/updates/refresh`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec, NodeStatus, NodeUpdates, PackageUpdate};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::settings::{Channel, SettingsSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::version::{BUILD, SUITE};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, now};
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::create_task;

/// opld가 쓴 소스 파일의 첫 줄. 이 줄이 없는 파일은 관리자가 직접 관리하는 것으로 보고 건드리지 않는다.
pub const SOURCES_MARK: &str = "# managed by opld; edits are overwritten";
/// 패키지가 싣는 저장소 공개 키(`UpdatesConfig.keyring`의 기본값)
pub const KEYRING: &str = "/usr/share/keyrings/oplite-archive-keyring.gpg";
/// `NodeStatus.updates`에 담는 패키지 수의 상한
pub const MAX_PACKAGES: usize = 500;
/// 다른 확인이 도는 중일 때의 오류
const CHECK_BUSY: &str = "another update check is running";
/// 확인 명령이 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 60초까지 기다리게 하는 apt 설정
const CHECK_LOCK_TIMEOUT: &str = "DPkg::Lock::Timeout=60";
/// 오류에 담는 명령 stderr의 끝 줄 수
const ERROR_LINES: usize = 5;
/// 설정 구독을 하지 못했을 때 다시 해 보는 간격
const RESUBSCRIBE: Duration = Duration::from_secs(10);

/// 노드 업데이트 설정(opld `[updates]`를 푼 값). 업데이트가 꺼진 노드에는 없다.
#[derive(Debug, Clone)]
pub struct UpdatesConfig {
    /// 주기 확인 간격
    pub check_interval: Duration,
    /// 시작한 뒤 첫 확인까지 기다리는 시간
    pub first_check: Duration,
    /// opLite APT 저장소 주소. 비면 소스 파일을 쓰지 않는다.
    pub apt_url: String,
    /// opld가 관리하는 deb822 소스 파일
    pub sources_file: PathBuf,
    /// 저장소 공개 키. 없으면 소스 파일을 쓰지 않는다(apt가 서명을 확인하지 못한다).
    pub keyring: PathBuf,
    pub apt_get: PathBuf,
    pub systemd_run: PathBuf,
    pub systemctl: PathBuf,
    pub dpkg: PathBuf,
    /// 업그레이드 로그를 두는 디렉터리
    pub log_dir: PathBuf,
    /// 업그레이드 유닛 상태를 다시 보는 간격
    pub poll: Duration,
}

/// 이 노드의 업데이트 확인. 확인은 한 번에 하나만 돈다.
#[derive(Debug)]
pub struct Updates {
    pub config: UpdatesConfig,
    lock: tokio::sync::Mutex<()>,
}

impl Updates {
    pub fn new(config: UpdatesConfig) -> Self {
        Self {
            config,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// `apt-get update -q` 뒤 `apt-get -s -q dist-upgrade`의 `Inst` 줄로 올릴 패키지를 찾는다. 명령이
    /// 실패하거나 다른 확인이 도는 중이면(기다리지 않는다) `error`에 이유를 담는다.
    pub async fn check(&self) -> NodeUpdates {
        let Ok(_guard) = self.lock.try_lock() else {
            return failed(CHECK_BUSY.into());
        };
        self.check_locked().await
    }

    /// 다른 확인이 끝나기를 기다렸다가 확인한다. 잠금 줄에 서지 않고 `poll`마다 다시 해 본다.
    async fn check_waiting(&self) -> NodeUpdates {
        loop {
            if let Ok(_guard) = self.lock.try_lock() {
                return self.check_locked().await;
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    async fn check_locked(&self) -> NodeUpdates {
        if let Err(e) = self.apt(&["update", "-q", "-o", CHECK_LOCK_TIMEOUT]).await {
            return failed(e);
        }
        match self
            .apt(&["-s", "-q", "-o", CHECK_LOCK_TIMEOUT, "dist-upgrade"])
            .await
        {
            Ok(out) => found(parse_simulate(&out), now()),
            Err(e) => failed(e),
        }
    }

    /// 확인해 결과를 노드 `node`의 `status.updates`에 쓴다. 확인이 실패했으면(결과를 쓴 뒤) 그 이유가
    /// 오류다. 다른 확인이 도는 중이면 쓰지 않는다(그 확인이 쓴다).
    pub(crate) async fn refresh(&self, cluster: &Cluster, node: Uuid) -> Result<(), String> {
        let updates = self.check().await;
        if updates.error.as_deref() == Some(CHECK_BUSY) {
            return Err(CHECK_BUSY.into());
        }
        self.record_check(cluster, node, updates).await
    }

    /// [`Self::refresh`]와 같지만 다른 확인이 도는 중이면 끝나기를 기다린다(사용자가 요청한 확인).
    pub(crate) async fn refresh_waiting(
        &self,
        cluster: &Cluster,
        node: Uuid,
    ) -> Result<(), String> {
        let updates = self.check_waiting().await;
        self.record_check(cluster, node, updates).await
    }

    async fn record_check(
        &self,
        cluster: &Cluster,
        node: Uuid,
        updates: NodeUpdates,
    ) -> Result<(), String> {
        record(cluster, node, &updates)
            .await
            .map_err(|e| format!("cannot record the update check: {e}"))?;
        match updates.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// `apt-get <args>`(LANG=C)의 표준 출력. 실패하면 명령과 stderr 끝 줄이 오류다.
    async fn apt(&self, args: &[&str]) -> Result<String, String> {
        let apt_get = &self.config.apt_get;
        let output = tokio::process::Command::new(apt_get)
            .args(args)
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            // 에이전트가 멈추면(opld 종료) 확인도 끝낸다.
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", apt_get.display()))?;
        if !output.status.success() {
            return Err(format!(
                "apt-get {} failed ({}): {}",
                args.join(" "),
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// 소스 파일을 지금 설정의 채널로 맞춘다(`apt_url`이 있을 때만).
    /// 파일을 새로 썼으면 true다.
    fn sync_sources(&self, cluster: &Cluster) -> bool {
        if self.config.apt_url.is_empty() {
            return false;
        }
        let channel = match crate::settings::current(cluster) {
            Ok(settings) => settings.spec.updates.channel,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the settings for the APT sources");
                return false;
            }
        };
        let c = &self.config;
        let path = &c.sources_file;
        match write_sources(path, &c.apt_url, channel, &c.keyring) {
            Ok(SourcesOutcome::Written) => {
                tracing::info!(file = %path.display(), channel = channel.as_str(), "wrote the APT sources");
                true
            }
            Ok(
                SourcesOutcome::Unchanged | SourcesOutcome::Unmanaged | SourcesOutcome::NoKeyring,
            ) => false,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot write the APT sources");
                false
            }
        }
    }
}

fn failed(error: String) -> NodeUpdates {
    NodeUpdates {
        checked_at: Some(now()),
        error: Some(error),
        ..NodeUpdates::default()
    }
}

/// 찾은 패키지의 확인 결과. [`MAX_PACKAGES`]가 넘으면 앞의 것만 담는다.
fn found(mut packages: Vec<PackageUpdate>, checked_at: u64) -> NodeUpdates {
    let truncated = packages.len() > MAX_PACKAGES;
    packages.truncate(MAX_PACKAGES);
    NodeUpdates {
        checked_at: Some(checked_at),
        packages,
        truncated,
        error: None,
    }
}

/// 비지 않은 마지막 `n`줄
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// `apt-get -s dist-upgrade` 출력의 `Inst <pkg> [<cur>] (<new> <suite…> [<arch>])` 줄. `[<cur>]`가
/// 없으면 새로 설치할 패키지다. `Conf`·`Remv`와 모양이 다른 줄은 건너뛴다.
pub(crate) fn parse_simulate(text: &str) -> Vec<PackageUpdate> {
    text.lines().filter_map(parse_inst).collect()
}

fn parse_inst(line: &str) -> Option<PackageUpdate> {
    let rest = line.strip_prefix("Inst ")?;
    let (name, rest) = rest.split_once(' ')?;
    let mut rest = rest.trim_start();
    let mut current = None;
    if let Some(r) = rest.strip_prefix('[') {
        let (cur, r) = r.split_once(']')?;
        current = Some(cur.to_string());
        rest = r.trim_start();
    }
    let candidate = rest.strip_prefix('(')?.split_whitespace().next()?;
    Some(PackageUpdate {
        name: name.to_string(),
        current,
        candidate: candidate.trim_end_matches(')').to_string(),
    })
}

/// opLite 저장소의 deb822 소스 파일 내용
pub(crate) fn sources_contents(url: &str, channel: Channel, keyring: &Path) -> String {
    format!(
        "{SOURCES_MARK}\nTypes: deb\nURIs: {url}\nSuites: {SUITE}\nComponents: {}\nSigned-By: {}\n",
        channel.as_str(),
        keyring.display()
    )
}

/// [`write_sources`]의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourcesOutcome {
    Written,
    /// 이미 같은 내용이다.
    Unchanged,
    /// 관리 표시가 없는 파일이다(관리자가 직접 관리한다). 건드리지 않았다.
    Unmanaged,
    /// 저장소 공개 키가 없다. 쓰지 않았고 opld가 쓴 파일이 있으면 지웠다.
    NoKeyring,
}

/// 소스 파일을 쓴다. 파일이 있는데 첫 줄이 [`SOURCES_MARK`]가 아니면(비었거나 공백뿐이면 opld 것으로
/// 본다) 건드리지 않고 경고한다. 공개 키 `keyring`이 없으면 apt가 서명을 확인하지 못해 `apt-get
/// update`가 실패하므로 쓰지 않고, opld가 쓴 파일은 지운다. 임시 파일(apt가 무시하는 `.dpkg-tmp`)에
/// 쓰고 디스크에 내린 뒤 바꿔 넣어 apt가 반쯤 쓴 파일을 읽지 않게 한다.
pub(crate) fn write_sources(
    path: &Path,
    url: &str,
    channel: Channel,
    keyring: &Path,
) -> std::io::Result<SourcesOutcome> {
    let old = match std::fs::read_to_string(path) {
        Ok(old) => Some(old),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &old
        && !old.trim().is_empty()
        && old.lines().next() != Some(SOURCES_MARK)
    {
        tracing::warn!(
            file = %path.display(),
            "the APT sources file has no opld mark; leaving it as it is"
        );
        return Ok(SourcesOutcome::Unmanaged);
    }
    if !keyring.exists() {
        tracing::warn!(
            keyring = %keyring.display(),
            "the opLite archive key is missing; not writing the APT sources"
        );
        if old.is_some() {
            std::fs::remove_file(path)?;
        }
        return Ok(SourcesOutcome::NoKeyring);
    }
    let contents = sources_contents(url, channel, keyring);
    if old.as_deref() == Some(contents.as_str()) {
        return Ok(SourcesOutcome::Unchanged);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".dpkg-tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        // 남은 임시 파일이나 umask와 상관없이 누구나 읽게 한다.
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // 바꿔 넣은 것도 디스크에 내린다. 실패해도 파일은 이미 바뀌었다.
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(SourcesOutcome::Written)
}

/// 노드 `node`의 `status.updates`와 이 노드의 버전을 쓴다.
async fn record(cluster: &Cluster, node: Uuid, updates: &NodeUpdates) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        status.updates = Some(updates.clone());
        status.version = Some(BUILD.into());
        true
    })
    .await
}

/// 이 노드 status의 `version`과 `updates`가 비었으면(이 필드를 모르는 옛 opld가 리더일 때 status를
/// 다시 쓰며 지웠다) 다시 쓴다. 확인 결과는 남긴다.
async fn restore(cluster: &Cluster, node: Uuid) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        let stale = status.version.as_deref() != Some(BUILD) || status.updates.is_none();
        status.version = Some(BUILD.into());
        status.updates.get_or_insert_with(NodeUpdates::default);
        stale
    })
    .await
}

/// 노드 `node`의 status를 `change`로 고쳐 쓴다(false면 쓰지 않는다). status의 다른 필드는 opld도
/// 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn change_status(
    cluster: &Cluster,
    node: Uuid,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<NodeSpec>(node)? else {
            return Ok(());
        };
        let mut status = current.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(node, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 소스 파일을 시작할 때와 설정이 바뀔 때 맞추고, `first_check` 뒤 처음, 그 뒤 `check_interval`마다
/// 업데이트를 확인하는 에이전트. 업데이트가 켜진 노드에서 돈다.
pub struct UpdateAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl UpdateAgent {
    pub fn spawn(cluster: Arc<Cluster>, node: Uuid, updates: Arc<Updates>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_agent(cluster, node, updates, rx)),
        }
    }

    /// 멈춘다. 도는 확인은 기다리지 않는다(apt 명령은 혼자 끝난다).
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_agent(
    cluster: Arc<Cluster>,
    node: Uuid,
    updates: Arc<Updates>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut next = Instant::now() + updates.config.first_check;
    let mut watch = None;
    let mut sync = true;
    let mut started = false;
    if let Err(e) = restore(&cluster, node).await {
        tracing::warn!(error = %e, "cannot restore the node status");
    }
    loop {
        if sync {
            // 이번에 읽을 설정 뒤의 변경을 놓치지 않게 먼저 구독한다.
            if watch.is_none() {
                watch = cluster
                    .list::<SettingsSpec>()
                    .ok()
                    .and_then(|(revision, _)| cluster.watch(revision).ok());
            }
            // 채널을 바꿔 소스 파일을 새로 썼으면 곧 확인해 새 채널의 업데이트를 보인다.
            if updates.sync_sources(&cluster) && started {
                next = Instant::now();
            }
            sync = false;
            started = true;
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = tokio::time::sleep_until(next) => {
                tokio::select! {
                    _ = &mut shutdown => return,
                    result = updates.refresh(&cluster, node) => {
                        if let Err(e) = result {
                            tracing::warn!(error = %e, "update check failed");
                        }
                    }
                }
                next = Instant::now() + updates.config.check_interval;
            }
            () = settings_changed(&mut watch) => sync = true,
        }
    }
}

/// 설정이 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고 돌아가 다시
/// 맞추게 한다. 구독이 없으면 잠시 뒤 돌아가 다시 구독하게 한다.
async fn settings_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return tokio::time::sleep(RESUBSCRIBE).await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Settings,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Settings,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

/// 업데이트를 확인하지 않는 노드(위트니스, 업데이트가 꺼진 노드, 옛 노드)면 400 `updates_unsupported`
pub(crate) fn check_updates_supported(node: &Node) -> Result<(), ApiError> {
    if node.spec.witness || node.status.updates.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "updates_unsupported",
            format!("node {:?} does not check for updates", node.name),
        ));
    }
    Ok(())
}

/// 노드의 업데이트를 다시 확인할 Task(`node-update-check`, 그 노드에서 돈다)를 돌려준다(202). 결과는
/// Node `status.updates`에 쓴다. 그 노드에 끝나지 않은 확인 Task가 있으면 새로 만들지 않고 그것을
/// 돌려준다. 업데이트를 확인하지 않는 노드면 400 `updates_unsupported`, 오프라인이면 409 `node_offline`,
/// 옛 노드가 남았으면 409 `cluster_upgrading`(옛 노드는 이 Task 종류를 풀지 못한다).
#[utoipa::path(post, path = "/nodes/{key}/updates/refresh", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_updates(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if let Some(task) = tasks.into_iter().find(|t| {
        t.spec.op == TaskOp::NodeUpdateCheck
            && t.spec.node == node.id
            && !t.status.state.is_finished()
    }) {
        return Ok((StatusCode::ACCEPTED, Json(task)));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpdateCheck, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debian 13에서 `LANG=C apt-get -s -q dist-upgrade`가 내는 모양(보안 업데이트, 새 커널 의존성,
    /// 지울 패키지, opLite 저장소 패키지가 섞임)
    const SIMULATE: &str = "\
Reading package lists...
Building dependency tree...
Reading state information...
Calculating upgrade...
The following packages will be REMOVED:
  libfoo1
The following NEW packages will be installed:
  linux-image-6.12.48+deb13-amd64
The following packages will be upgraded:
  base-files libc-bin libc6 linux-image-amd64 oplite
5 upgraded, 1 newly installed, 1 to remove and 0 not upgraded.
Remv libfoo1 [1.2-3]
Inst base-files [13.8+deb13u1] (13.8+deb13u2 Debian:13.2/stable [amd64])
Conf base-files (13.8+deb13u2 Debian:13.2/stable [amd64])
Inst libc6 [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64]) [libc-bin:amd64 ]
Inst libc-bin [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc6 (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc-bin (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Inst linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Inst linux-image-amd64 [6.12.41-1] (6.12.48-1 Debian:13.2/stable [amd64])
Inst oplite [0.2.0] (1:0.2.1~rc1 opLite:trixie [amd64])
Conf linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf linux-image-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf oplite (1:0.2.1~rc1 opLite:trixie [amd64])
";

    fn pkg(name: &str, current: Option<&str>, candidate: &str) -> PackageUpdate {
        PackageUpdate {
            name: name.into(),
            current: current.map(Into::into),
            candidate: candidate.into(),
        }
    }

    #[test]
    fn parse_simulate_reads_inst_lines() {
        assert_eq!(
            parse_simulate(SIMULATE),
            [
                pkg("base-files", Some("13.8+deb13u1"), "13.8+deb13u2"),
                pkg("libc6", Some("2.41-12"), "2.41-12+deb13u1"),
                pkg("libc-bin", Some("2.41-12"), "2.41-12+deb13u1"),
                // 새 커널 의존성: 설치된 버전이 없다.
                pkg("linux-image-6.12.48+deb13-amd64", None, "6.12.48-1"),
                pkg("linux-image-amd64", Some("6.12.41-1"), "6.12.48-1"),
                pkg("oplite", Some("0.2.0"), "1:0.2.1~rc1"),
            ]
        );
        assert!(parse_simulate("").is_empty());
        // 모양이 다른 줄은 건너뛴다.
        assert!(parse_simulate("Inst\nInst x [1.0\nInst y [1.0]\nInst z").is_empty());
    }

    #[test]
    fn found_keeps_at_most_max_packages() {
        let many: String = (0..600)
            .map(|i| format!("Inst p{i} [1.0] (1.1 Debian:13.2/stable [amd64])\n"))
            .collect();
        let packages = parse_simulate(&many);
        assert_eq!(packages.len(), 600);
        let updates = found(packages, 7);
        assert_eq!(updates.checked_at, Some(7));
        assert_eq!(updates.packages.len(), MAX_PACKAGES);
        assert_eq!(updates.packages[0].name, "p0");
        assert!(updates.truncated);
        assert_eq!(updates.error, None);
        let few = found(parse_simulate(SIMULATE), 7);
        assert_eq!(few.packages.len(), 6);
        assert!(!few.truncated);
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\n\nc\nd\ne\nf\n", 5), "b\nc\nd\ne\nf");
        assert_eq!(tail("only\n", 5), "only");
        assert_eq!(tail("", 5), "");
    }

    fn missing_tools() -> Updates {
        let dir = PathBuf::from("/nonexistent/opl-updates");
        Updates::new(UpdatesConfig {
            check_interval: Duration::from_secs(3600),
            first_check: Duration::from_secs(3600),
            apt_url: String::new(),
            sources_file: dir.join("oplite.sources"),
            keyring: dir.join("keyring.gpg"),
            apt_get: dir.join("apt-get"),
            systemd_run: dir.join("systemd-run"),
            systemctl: dir.join("systemctl"),
            dpkg: dir.join("dpkg"),
            log_dir: dir.join("log"),
            poll: Duration::from_millis(20),
        })
    }

    #[tokio::test]
    async fn check_reports_a_missing_apt_get_and_does_not_wait_for_another_check() {
        let updates = missing_tools();
        let got = updates.check().await;
        assert_eq!(
            got.error.as_deref(),
            Some(
                "cannot run /nonexistent/opl-updates/apt-get: No such file or directory (os error 2)"
            )
        );
        assert!(got.checked_at.is_some());
        let _running = updates.lock.try_lock().unwrap();
        let got = updates.check().await;
        assert_eq!(got.error.as_deref(), Some(CHECK_BUSY));
        assert!(got.packages.is_empty());
    }

    /// 사용자가 요청한 확인은 도는 확인을 기다렸다가 확인한다.
    #[tokio::test]
    async fn a_waiting_check_waits_for_the_running_one() {
        let updates = missing_tools();
        let running = updates.lock.try_lock().unwrap();
        let waiting = updates.check_waiting();
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        drop(running);
        let got = waiting.await;
        assert!(got.error.unwrap().starts_with("cannot run "));
    }

    #[test]
    fn sources_are_deb822_with_the_mark() {
        assert_eq!(
            sources_contents(
                "https://apt.example.com/oplite",
                Channel::Testing,
                Path::new(KEYRING)
            ),
            "# managed by opld; edits are overwritten\n\
             Types: deb\n\
             URIs: https://apt.example.com/oplite\n\
             Suites: trixie\n\
             Components: testing\n\
             Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg\n"
        );
    }

    #[test]
    fn write_sources_writes_replaces_and_leaves_unmanaged_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        std::fs::write(&keyring, "").unwrap();
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            sources_contents(url, Channel::Stable, &keyring)
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unchanged
        );
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Components: testing\n")
        );
        // 관리 표시가 없는 파일(관리자가 직접 쓴 것)은 그대로 둔다.
        let own =
            "Types: deb\nURIs: http://mirror.lan/oplite\nSuites: trixie\nComponents: stable\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
        // 비었거나 공백뿐인 파일은 opld 것으로 본다.
        for empty in ["", " \n\n"] {
            std::fs::write(&path, empty).unwrap();
            assert_eq!(
                write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
                SourcesOutcome::Written
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                sources_contents(url, Channel::Stable, &keyring)
            );
        }
        assert!(!dir.path().join("oplite.sources.dpkg-tmp").exists());
    }

    /// 공개 키가 없으면 쓰지 않고, opld가 쓴 파일은 지우고, 관리자의 파일은 그대로 둔다.
    #[test]
    fn write_sources_needs_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        std::fs::write(&path, sources_contents(url, Channel::Stable, &keyring)).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        let own = "Types: deb\nURIs: http://mirror.lan/oplite\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
    }
}
```

`crates/opl-api/tests/common/fake_apt.rs`를 만든다:

````rust
//! 시험용 가짜 `apt-get`. 임시 디렉터리에 셸 스크립트를 두고 상태를 파일로 흉내 낸다:
//!
//! - 부를 때마다 `LANG=<값> <인자…>` 한 줄을 `apt-get.calls`에 더한다.
//! - 인자에 `-s`가 있으면 `simulate.txt`를 출력한다.
//! - `update`면 `update.stderr`(있으면)를 표준 오류로 내고 `update.exit`(없으면 0)로 끝난다.
//!
//! ```ignore
//! let apt = FakeApt::new();
//! apt.set("simulate.txt", "Inst oplite [0.2.0] (0.2.1 opLite:trixie [amd64])\n");
//! let updates = Arc::new(Updates::new(apt.config()));
//! ```
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_api::UpdatesConfig;
use tempfile::TempDir;

const APT_GET: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "LANG=$LANG $*" >> "$d/apt-get.calls"
for a; do
    if [ "$a" = -s ]; then
        cat "$d/simulate.txt"
        exit 0
    fi
done
if [ "$1" = update ]; then
    if [ -f "$d/update.stderr" ]; then cat "$d/update.stderr" >&2; fi
    if [ -f "$d/update.exit" ]; then exit "$(cat "$d/update.exit")"; fi
fi
exit 0
"#;

pub struct FakeApt {
    pub dir: TempDir,
}

impl FakeApt {
    pub fn new() -> Self {
        let apt = Self {
            dir: TempDir::new().unwrap(),
        };
        write_script(&apt.path("apt-get"), APT_GET);
        apt.set("simulate.txt", "");
        apt.set("keyring.gpg", "");
        apt
    }

    /// 디렉터리 안의 파일 경로
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// 상태 파일을 쓴다.
    pub fn set(&self, name: &str, contents: &str) {
        std::fs::write(self.path(name), contents).unwrap();
    }

    /// 가짜 `apt-get`이 받은 호출(`LANG=<값> <인자…>`)
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("apt-get.calls"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// 가짜 명령을 쓰는 업데이트 설정. 주기 확인은 시험 중에 돌지 않을 만큼 길다. 소스 파일은
    /// 쓰지 않는다(`apt_url`이 비었다). 저장소 공개 키는 빈 `keyring.gpg`다.
    pub fn config(&self) -> UpdatesConfig {
        UpdatesConfig {
            check_interval: Duration::from_secs(3600),
            first_check: Duration::from_secs(3600),
            apt_url: String::new(),
            sources_file: self.path("oplite.sources"),
            keyring: self.path("keyring.gpg"),
            apt_get: self.path("apt-get"),
            systemd_run: self.path("systemd-run"),
            systemctl: self.path("systemctl"),
            dpkg: self.path("dpkg"),
            log_dir: self.path("log"),
            poll: Duration::from_millis(20),
        }
    }
}

impl Default for FakeApt {
    fn default() -> Self {
        Self::new()
    }
}

/// 실행 파일 셸 스크립트를 쓴다. 이 프로세스가 연 쓰기 핸들이 다른 스레드의 fork에 새어 실행이
/// `ETXTBSY`로 실패하지 않도록 `sh`가 쓴다.
fn write_script(path: &Path, text: &str) {
    use std::io::Write;
    let mut sh = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    sh.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    assert!(sh.wait().unwrap().success(), "cannot write {path:?}");
}
````

`crates/opl-api/tests/common/mod.rs`에서

```rust
pub mod esxi;
pub mod ustar;
```

을 다음으로 바꾼다:

```rust
pub mod esxi;
pub mod fake_apt;
pub mod ustar;
```

`crates/opl-api/tests/common/mod.rs`에서

```rust
            version: None,
        };
```

을 다음으로 바꾼다:

```rust
            version: None,
            updates: None,
        };
```

`crates/opl-api/tests/ha.rs`에서

```rust
        version: None,
    };
```

을 다음으로 바꾼다:

```rust
        version: None,
        updates: None,
    };
```

`crates/opl-api/tests/migrate.rs`에서

```rust
        version: None,
    };
```

을 다음으로 바꾼다:

```rust
        version: None,
        updates: None,
    };
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/settings",
        "/vms",
```

을 다음으로 바꾼다:

```rust
        "/settings",
        "/nodes/{key}/updates/refresh",
        "/vms",
```

`crates/opl-api/tests/two_nodes.rs`에서

```rust
            version: None,
        };
```

을 다음으로 바꾼다:

```rust
            version: None,
            updates: None,
        };
```

`crates/opl-api/tests/updates.rs`를 만든다:

```rust
//! 노드 업데이트 확인: 다시 확인 Task(가짜 apt), 요청 검사, 소스 파일과 주기 확인 에이전트.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::fake_apt::FakeApt;
use common::{Env, MOUNT_ROOT, WAIT};
use opl_api::{UpdateAgent, Updates};
use opl_driver::DirStorage;
use opl_types::node::{NodeSpec, NodeUpdates};
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

/// 노드의 status를 `change`로 고쳐 쓴다.
async fn set_node_status(env: &Env, change: impl FnOnce(&mut opl_types::node::NodeStatus)) {
    let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
    let mut status = node.status;
    change(&mut status);
    env.cluster
        .update_status::<NodeSpec>(env.node, &status)
        .await
        .unwrap();
}

/// 업데이트가 켜진 노드(등록 때처럼 빈 `updates`)와 가짜 apt를 쓰는 Task 실행기
async fn env_with_updates(apt: &FakeApt) -> Env {
    let mut env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    env.stop_runner().await;
    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    env
}

async fn eventually<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = probe() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn refresh_runs_a_task_that_records_the_updates() {
    let apt = FakeApt::new();
    apt.set(
        "simulate.txt",
        "Reading package lists...\n\
         Inst oplite [0.2.0] (0.2.1 opLite:trixie [amd64])\n\
         Conf oplite (0.2.1 opLite:trixie [amd64])\n\
         Inst libnew1 (1.0-1 Debian:13.2/stable [amd64])\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "node-update-check" }));
    assert_eq!(task["spec"]["target"], env.node.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");

    assert_eq!(
        apt.calls(),
        [
            "LANG=C update -q -o DPkg::Lock::Timeout=60",
            "LANG=C -s -q -o DPkg::Lock::Timeout=60 dist-upgrade"
        ]
    );
    let (_, node) = admin.get("/nodes/node1").await;
    let updates = &node["status"]["updates"];
    assert!(updates["checked_at"].as_u64().is_some(), "{updates}");
    assert_eq!(
        updates["packages"],
        json!([
            { "name": "oplite", "current": "0.2.0", "candidate": "0.2.1" },
            { "name": "libnew1", "current": null, "candidate": "1.0-1" }
        ])
    );
    assert_eq!(updates["error"], Value::Null);
    assert!(updates.get("truncated").is_none(), "{updates}");
}

#[tokio::test]
async fn a_failed_apt_command_fails_the_task_and_records_the_error() {
    let apt = FakeApt::new();
    apt.set("update.exit", "100");
    apt.set(
        "update.stderr",
        "W: line 1\nW: line 2\nE: line 3\nE: line 4\nE: line 5\nE: line 6\n\
         E: Could not get lock /var/lib/apt/lists/lock. It is held by process 4242 (apt-get)\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    let error = done["status"]["error"].as_str().unwrap();
    assert!(
        error.starts_with("apt-get update -q -o DPkg::Lock::Timeout=60 failed ("),
        "{error}"
    );
    // stderr의 끝 5줄만 담는다.
    assert!(
        error.ends_with(
            "): E: line 3\nE: line 4\nE: line 5\nE: line 6\n\
             E: Could not get lock /var/lib/apt/lists/lock. It is held by process 4242 (apt-get)"
        ),
        "{error}"
    );
    // 시뮬레이션까지 가지 않는다.
    assert_eq!(apt.calls(), ["LANG=C update -q -o DPkg::Lock::Timeout=60"]);

    let (_, node) = admin.get("/nodes/node1").await;
    let updates = &node["status"]["updates"];
    assert_eq!(updates["error"], error, "{updates}");
    assert_eq!(updates["packages"], json!([]));
    assert!(updates["checked_at"].as_u64().is_some(), "{updates}");
}

#[tokio::test]
async fn refresh_checks_the_node_and_the_caller() {
    let apt = FakeApt::new();
    let env = Env::start().await;
    let admin = env.admin().await;

    // 등록 때 업데이트가 꺼졌던 노드(위트니스, macOS 등)
    assert_error(
        &admin.post("/nodes/node1/updates/refresh", json!({})).await,
        StatusCode::BAD_REQUEST,
        "updates_unsupported",
    );
    assert_error(
        &admin.post("/nodes/nope/updates/refresh", json!({})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    for role in ["operator", "viewer"] {
        let user = env.user(role, role).await;
        assert_error(
            &user.post("/nodes/node1/updates/refresh", json!({})).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    set_node_status(&env, |s| s.online = false).await;
    assert_error(
        &admin.post("/nodes/node1/updates/refresh", json!({})).await,
        StatusCode::CONFLICT,
        "node_offline",
    );
    // 실행기에 업데이트가 없으면(설정과 등록이 어긋남) Task가 실패한다.
    set_node_status(&env, |s| s.online = true).await;
    let (status, task) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "updates are not enabled on this node"
    );
    assert!(apt.calls().is_empty());
}

#[tokio::test]
async fn refresh_returns_the_unfinished_check() {
    let apt = FakeApt::new();
    let mut env = env_with_updates(&apt).await;
    let admin = env.admin().await;
    // 실행기가 멈춰 있으면 Task는 pending으로 남는다.
    env.stop_runner().await;
    let (status, first) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let (status, again) = admin
        .post(&format!("/nodes/{}/updates/refresh", env.node), json!({}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{again}");
    assert_eq!(again["id"], first["id"]);

    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    let done = admin.wait_task(&first).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    // 끝났으면 새로 만든다.
    let (_, next) = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_ne!(next["id"], first["id"]);
    assert!(Uuid::try_parse(next["id"].as_str().unwrap()).is_ok());
}

/// 버전을 적지 않은 옛 노드가 있으면 옛 노드가 풀지 못하는 확인 Task를 만들지 않는다.
#[tokio::test]
async fn refresh_waits_until_every_node_is_upgraded() {
    let apt = FakeApt::new();
    let env = env_with_updates(&apt).await;
    let old = opl_types::resource::Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    env.cluster.create(&old).await.unwrap();
    assert_error(
        &env.admin()
            .await
            .post("/nodes/node1/updates/refresh", json!({}))
            .await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    let (_, tasks) = env.cluster.list::<opl_types::task::TaskSpec>().unwrap();
    assert!(tasks.is_empty());
}

/// 이 필드를 모르는 옛 리더가 지운 버전과 `updates`를 에이전트가 시작할 때와 확인할 때 되살린다.
#[tokio::test]
async fn the_agent_restores_the_version_and_updates() {
    let apt = FakeApt::new();
    let env = Env::start().await;
    set_node_status(&env, |s| {
        s.version = None;
        s.updates = None;
    })
    .await;
    let mut config = apt.config();
    config.first_check = Duration::from_millis(300);
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );
    let status = || {
        env.cluster
            .get::<NodeSpec>(env.node)
            .unwrap()
            .unwrap()
            .status
    };
    eventually("the restored status", || {
        let s = status();
        (s.version.as_deref() == Some(opl_types::version::BUILD)
            && s.updates == Some(NodeUpdates::default()))
        .then_some(())
    })
    .await;
    // 확인할 때도 되살린다.
    set_node_status(&env, |s| {
        s.version = None;
        s.updates = None;
    })
    .await;
    eventually("the first check", || {
        let s = status();
        (s.version.as_deref() == Some(opl_types::version::BUILD)
            && s.updates.is_some_and(|u| u.checked_at.is_some()))
        .then_some(())
    })
    .await;
    agent.shutdown().await;
}

#[tokio::test]
async fn the_agent_writes_the_sources_for_the_channel_and_checks() {
    let apt = FakeApt::new();
    apt.set(
        "simulate.txt",
        "Inst oplite [0.2.0] (0.2.1 opLite:trixie [amd64])\n",
    );
    let env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    let admin = env.admin().await;
    let mut config = apt.config();
    config.apt_url = "https://apt.example.com/oplite".into();
    config.first_check = Duration::from_millis(50);
    let sources = config.sources_file.clone();
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );

    let read = || std::fs::read_to_string(&sources).ok();
    let text = eventually("the sources file", read).await;
    assert!(
        text.starts_with("# managed by opld; edits are overwritten\n"),
        "{text}"
    );
    assert!(
        text.contains("URIs: https://apt.example.com/oplite\n"),
        "{text}"
    );
    assert!(text.contains("Components: stable\n"), "{text}");

    // 첫 확인
    eventually("the first check", || {
        let node = env.cluster.get::<NodeSpec>(env.node).unwrap().unwrap();
        node.status
            .updates
            .filter(|u| u.checked_at.is_some())
            .map(|u| assert_eq!(u.packages[0].candidate, "0.2.1"))
    })
    .await;

    // 채널을 바꾸면 소스 파일이 따라간다.
    let (status, body) = admin
        .patch(
            "/settings",
            json!({ "updates": { "channel": "testing" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    eventually("the testing channel", || {
        read().filter(|t| t.contains("Components: testing\n"))
    })
    .await;
    // 새 채널은 다음 주기(한 시간 뒤)를 기다리지 않고 곧 확인한다.
    eventually("a check of the new channel", || {
        (apt.calls().len() == 4).then_some(())
    })
    .await;
    agent.shutdown().await;
}
```

`crates/opl-types/src/node.rs`에서

```rust
    pub version: Option<String>,
}
```

을 다음으로 바꾼다:

```rust
    pub version: Option<String>,
    /// 이 노드의 APT 업데이트 확인 결과. 업데이트가 켜진 노드는 등록할 때 `Some`(아직 확인 전이면
    /// 빈 값)이고, 꺼진 노드(위트니스, macOS 등)와 옛 노드는 None이다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub updates: Option<NodeUpdates>,
}

/// 노드의 APT 업데이트 확인 결과(`apt-get -s dist-upgrade`가 올릴 패키지)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeUpdates {
    /// 마지막으로 확인한 시각(유닉스 초). 아직 확인하지 않았으면 None
    pub checked_at: Option<u64>,
    /// 올릴 수 있는 패키지. 너무 많으면 앞의 것만 담고 `truncated`가 true다.
    pub packages: Vec<PackageUpdate>,
    #[serde(skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub truncated: bool,
    /// 확인이 실패한 이유(명령과 오류 출력 끝). 실패하면 `packages`는 비어 있다.
    pub error: Option<String>,
}

/// 올릴 수 있는 패키지 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackageUpdate {
    pub name: String,
    /// 설치된 버전. 새로 설치할 패키지(새 의존성)는 None이다.
    pub current: Option<String>,
    /// 올릴 버전
    pub candidate: String,
}
```

`crates/opl-types/src/node.rs`에서

```rust
    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn status_updates_round_trip_and_are_omitted_when_none() {
        let none = serde_json::to_value(NodeStatus::default()).unwrap();
        assert!(none.get("updates").is_none(), "{none}");
        // 켜졌지만 아직 확인하지 않은 노드
        let status = NodeStatus {
            updates: Some(NodeUpdates::default()),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["updates"],
            serde_json::json!({ "checked_at": null, "packages": [], "error": null })
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
        let status = NodeStatus {
            updates: Some(NodeUpdates {
                checked_at: Some(1_793_000_000),
                packages: vec![
                    PackageUpdate {
                        name: "oplite".into(),
                        current: Some("0.2.0".into()),
                        candidate: "0.2.1".into(),
                    },
                    PackageUpdate {
                        name: "libnew1".into(),
                        current: None,
                        candidate: "1.0-1".into(),
                    },
                ],
                truncated: true,
                error: None,
            }),
            ..Default::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["updates"]["truncated"], true);
        assert_eq!(
            json["updates"]["packages"][1],
            serde_json::json!({ "name": "libnew1", "current": null, "candidate": "1.0-1" })
        );
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);
    }

    #[test]
    fn ha_capable_needs_fencing_and_no_witness() {
```

`crates/opl-types/src/task.rs`에서

```rust
    VmImportDelete,
}
```

을 다음으로 바꾼다:

```rust
    VmImportDelete,
    /// 대상 노드(Task의 `node`와 같다)의 APT 업데이트를 확인해 Node `status.updates`에 쓴다.
    NodeUpdateCheck,
}
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn node_update_check_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "node-update-check" });
        assert_eq!(serde_json::to_value(TaskOp::NodeUpdateCheck).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::NodeUpdateCheck
        );
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/oplctl/src/cli.rs`에서

```rust
    List,
    Get { node: String },
}
```

을 다음으로 바꾼다:

```rust
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
```

`crates/oplctl/src/cli.rs`에서

```rust
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
```

을 다음으로 바꾼다:

```rust
                print_json(out, &client.get(&format!("/nodes/{node}")).await?)?
            }
            NodeCmd::Updates { node, refresh } => {
                node_updates(&client, &node, refresh, json_out, out).await?
            }
```

`crates/oplctl/src/cli.rs`에서

```rust

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
```

을 다음으로 바꾼다:

```rust

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

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
```

`crates/opld/src/bootstrap.rs`에서

```rust
/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

을 다음으로 바꾼다:

```rust
/// 이 노드의 Node 리소스를 만들거나 주소를 맞추고, 온라인 상태와 용량, 브리지, 아키텍처, HA 펜싱,
/// underlay, 방화벽 여부, 버전, 업데이트 확인 여부를 기록한다.
pub async fn register_node(cluster: &Cluster, config: &Config, host: &Host) -> anyhow::Result<()> {
```

`crates/opld/src/bootstrap.rs`에서

```rust
            version: Some(BUILD.to_string()),
        };
```

을 다음으로 바꾼다:

```rust
            version: Some(BUILD.to_string()),
            // 업데이트 확인이 쓴 결과는 지우지 않는다. 꺼진 노드는 None이다(다시 확인을 받지 않는다).
            updates: config
                .updates
                .enabled()
                .then(|| status.updates.take().unwrap_or_default()),
        };
```

`crates/opld/src/bootstrap.rs`에서

```rust
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, PoolState};
```

을 다음으로 바꾼다:

```rust
    use opl_driver::{DriverError, Firewall, HostNet, Overlay, OverlayChanges, Tap};
    use opl_types::node::{BridgeInfo, Fencing, NodeHa, NodeUpdates, PoolState};
```

`crates/opld/src/bootstrap.rs`에서

```rust

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
```

을 다음으로 바꾼다:

```rust

    /// 업데이트가 켜진 노드는 `updates`를 기록하고(처음엔 빈 값) 확인 결과를 다시 등록해도 지키며,
    /// 꺼지면 지운다.
    #[tokio::test]
    async fn registration_records_whether_updates_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (cluster, id) = single_node(dir.path()).await;
        let mut config = test_config();
        let mut host = Host::from_config(&config, "t").unwrap();
        host.net = None;
        let updates = || {
            cluster
                .get::<NodeSpec>(id.0)
                .unwrap()
                .unwrap()
                .status
                .updates
        };
        config.updates.enabled = Some(true);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(NodeUpdates::default()));
        let checked = NodeUpdates {
            checked_at: Some(1_793_000_000),
            error: Some("apt-get update -q failed".into()),
            ..NodeUpdates::default()
        };
        update_node_status(&cluster, |status| {
            status.updates = Some(checked.clone());
            true
        })
        .await
        .unwrap();
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), Some(checked));
        config.updates.enabled = Some(false);
        register_node(&cluster, &config, &host).await.unwrap();
        assert_eq!(updates(), None);
        cluster.shutdown().await.unwrap();
    }

    /// 위트니스는 역할과 온라인, 아키텍처만 기록하고 VM을 돌리는 데 쓰는 값은 비운다.
```

`crates/opld/src/bootstrap.rs`에서

```rust
        let (cluster, id) = single_node(dir.path()).await;
        // underlay를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"",
        )
```

을 다음으로 바꾼다:

```rust
        let (cluster, id) = single_node(dir.path()).await;
        // underlay와 업데이트를 설정해도 기록하지 않는다.
        let config: Config = toml::from_str(
            "name = \"w\"\naddress = \"10.0.0.9\"\nwitness = true\n[network]\nunderlay = \"10.1.0.9\"\n\
             [updates]\nenabled = true",
        )
```

`crates/opld/src/config.rs`에서

```rust

use opl_api::{BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig};
use opl_qemu::{Accel, HostProfile, kvm_available};
```

을 다음으로 바꾼다:

```rust

use opl_api::{
    BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig, UpdatesConfig,
};
use opl_qemu::{Accel, HostProfile, kvm_available};
```

`crates/opld/src/config.rs`에서

```rust
    pub network: NetworkSection,
}
```

을 다음으로 바꾼다:

```rust
    pub network: NetworkSection,
    #[serde(default)]
    pub updates: UpdatesSection,
}
```

`crates/opld/src/config.rs`에서

```rust
    pub underlay: Option<IpAddr>,
}
```

을 다음으로 바꾼다:

```rust
    pub underlay: Option<IpAddr>,
}

/// 노드의 APT 업데이트(소스 파일, 업데이트 확인). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesSection {
    /// 업데이트를 확인한다. 없으면 `apt_get`이 있을 때(Debian) 켜진다.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 업데이트를 확인하는 간격
    #[serde(
        default = "default_check_interval",
        deserialize_with = "deserialize_duration"
    )]
    pub check_interval: Duration,
    /// 시작한 뒤 첫 확인까지 기다리는 시간
    #[serde(
        default = "default_first_check",
        deserialize_with = "deserialize_duration"
    )]
    pub first_check: Duration,
    /// opLite APT 저장소 주소. 비면 빌드 값(`OPL_APT_URL`), 그것도 비면 소스 파일을 쓰지 않는다.
    #[serde(default)]
    pub apt_url: String,
    /// opld가 관리하는 deb822 소스 파일
    #[serde(default = "default_sources_file")]
    pub sources_file: PathBuf,
    #[serde(default = "default_apt_get")]
    pub apt_get: PathBuf,
    #[serde(default = "default_systemd_run")]
    pub systemd_run: PathBuf,
    #[serde(default = "default_systemctl")]
    pub systemctl: PathBuf,
    #[serde(default = "default_dpkg")]
    pub dpkg: PathBuf,
    /// 업그레이드 로그를 두는 디렉터리
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    /// 업그레이드 유닛 상태를 다시 보는 간격
    #[serde(
        default = "default_upgrade_poll",
        deserialize_with = "deserialize_duration"
    )]
    pub poll: Duration,
}

impl Default for UpdatesSection {
    fn default() -> Self {
        Self {
            enabled: None,
            check_interval: default_check_interval(),
            first_check: default_first_check(),
            apt_url: String::new(),
            sources_file: default_sources_file(),
            apt_get: default_apt_get(),
            systemd_run: default_systemd_run(),
            systemctl: default_systemctl(),
            dpkg: default_dpkg(),
            log_dir: default_log_dir(),
            poll: default_upgrade_poll(),
        }
    }
}

impl UpdatesSection {
    /// 업데이트를 확인하는지: `enabled`, 없으면 `apt_get`이 있는지
    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or_else(|| self.apt_get.exists())
    }

    /// API의 업데이트 설정. 꺼졌으면 None이다.
    pub fn updates_config(&self) -> Option<UpdatesConfig> {
        if !self.enabled() {
            return None;
        }
        let apt_url = if self.apt_url.is_empty() {
            opl_types::version::APT_URL.to_string()
        } else {
            self.apt_url.clone()
        };
        Some(UpdatesConfig {
            check_interval: self.check_interval,
            first_check: self.first_check,
            apt_url,
            sources_file: self.sources_file.clone(),
            keyring: opl_api::KEYRING.into(),
            apt_get: self.apt_get.clone(),
            systemd_run: self.systemd_run.clone(),
            systemctl: self.systemctl.clone(),
            dpkg: self.dpkg.clone(),
            log_dir: self.log_dir.clone(),
            poll: self.poll,
        })
    }
}

fn default_check_interval() -> Duration {
    Duration::from_secs(24 * 3600)
}
fn default_first_check() -> Duration {
    Duration::from_secs(60)
}
fn default_sources_file() -> PathBuf {
    "/etc/apt/sources.list.d/oplite.sources".into()
}
fn default_apt_get() -> PathBuf {
    "/usr/bin/apt-get".into()
}
fn default_systemd_run() -> PathBuf {
    "/usr/bin/systemd-run".into()
}
fn default_systemctl() -> PathBuf {
    "/usr/bin/systemctl".into()
}
fn default_dpkg() -> PathBuf {
    "/usr/bin/dpkg".into()
}
fn default_log_dir() -> PathBuf {
    "/var/log/opl".into()
}
fn default_upgrade_poll() -> Duration {
    Duration::from_secs(2)
}
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn updates_section_defaults_and_overrides() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        let u = &c.updates;
        assert_eq!(u.enabled, None);
        assert_eq!(u.check_interval, Duration::from_secs(24 * 3600));
        assert_eq!(u.first_check, Duration::from_secs(60));
        assert_eq!(u.apt_url, "");
        assert_eq!(
            u.sources_file,
            Path::new("/etc/apt/sources.list.d/oplite.sources")
        );
        assert_eq!(u.apt_get, Path::new("/usr/bin/apt-get"));
        assert_eq!(u.systemd_run, Path::new("/usr/bin/systemd-run"));
        assert_eq!(u.systemctl, Path::new("/usr/bin/systemctl"));
        assert_eq!(u.dpkg, Path::new("/usr/bin/dpkg"));
        assert_eq!(u.log_dir, Path::new("/var/log/opl"));
        assert_eq!(u.poll, Duration::from_secs(2));

        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[updates]\nenabled = true\ncheck_interval = \"6h\"\n\
             first_check = \"1s\"\napt_url = \"https://apt.example.com/oplite\"\n\
             sources_file = \"/tmp/x.sources\"\napt_get = \"/opt/apt-get\"\npoll = \"1m30s\"",
        );
        let config = c.updates.updates_config().unwrap();
        assert_eq!(config.check_interval, Duration::from_secs(6 * 3600));
        assert_eq!(config.first_check, Duration::from_secs(1));
        assert_eq!(config.apt_url, "https://apt.example.com/oplite");
        assert_eq!(config.sources_file, Path::new("/tmp/x.sources"));
        assert_eq!(config.apt_get, Path::new("/opt/apt-get"));
        assert_eq!(config.poll, Duration::from_secs(90));
        // 비어 있으면 빌드 값을 쓴다.
        let c = parse("name = \"n\"\naddress = \"h\"\n[updates]\nenabled = true");
        assert_eq!(
            c.updates.updates_config().unwrap().apt_url,
            opl_types::version::APT_URL
        );

        for bad in [
            "check_interval = \"0s\"",
            "first_check = \"soon\"",
            "poll = 2",
            "interval = \"1h\"",
        ] {
            assert!(
                toml::from_str::<Config>(&format!(
                    "name = \"n\"\naddress = \"h\"\n[updates]\n{bad}"
                ))
                .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn updates_are_enabled_when_apt_get_exists_unless_set() {
        let dir = tempfile::tempdir().unwrap();
        let apt_get = dir.path().join("apt-get");
        let with = |extra: &str| {
            parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[updates]\napt_get = \"{}\"\n{extra}",
                apt_get.display()
            ))
        };
        assert!(!with("").updates.enabled());
        assert!(with("").updates.updates_config().is_none());
        assert!(with("enabled = true").updates.enabled());
        std::fs::write(&apt_get, "").unwrap();
        assert!(with("").updates.enabled());
        assert!(with("").updates.updates_config().is_some());
        assert!(!with("enabled = false").updates.enabled());
        assert!(with("enabled = false").updates.updates_config().is_none());
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig,
    IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig, UefiVarsFetcher,
    UpdateAgent, Updates,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Typed};
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::firewall::{FirewallAgent, FirewallRules};
use crate::host::Host;
use crate::overlay::{OverlayAgent, cluster_tag};
use crate::pools::PoolMounter;

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록을 다시 확인하는 간격. 관리자가 브리지를 만들면 이만큼 뒤에 스케줄러가 안다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    /// 리더가 백업 작업을 실행한다.
    backups: BackupJobScheduler,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    pools: PoolMounter,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
    overlay: Option<OverlayAgent>,
    /// VM 방화벽 규칙을 맞춘다(방화벽이 없으면 없다).
    firewall: Option<FirewallAgent>,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
    /// APT 소스 파일과 주기 업데이트 확인(업데이트가 꺼졌으면 없다)
    updates: Option<UpdateAgent>,
}

impl Services {
    /// `left`면 클러스터에서 빠진 것이다(VXLAN 장치와 방화벽 테이블을 지운다).
    async fn shutdown(mut self, left: bool) {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.backups.shutdown().await;
        self.recovery.shutdown().await;
        self.pools.shutdown().await;
        if let Some(updates) = self.updates {
            updates.shutdown().await;
        }
        if let Some(overlay) = self.overlay {
            overlay.shutdown(left).await;
        }
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 멈춰야 테이블을 지운 뒤 시작 직전 확인이 다시 넣지 않는다.
        if let Some(firewall) = self.firewall {
            firewall.shutdown(left).await;
        }
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let cluster = Arc::new(
            Cluster::open(cluster_config, GrpcNetwork::new(peers.clone()))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap).await?;
            return Ok((member, None));
        }
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 이 클러스터가 만든 VXLAN 장치의 표시. 남의 관리 이름 브리지는 브리지 목록에서 뺀다.
        let host = cluster_tag(&tls.ca_pem).and_then(|tag| {
            let host = Host::from_config(config, &tag)?;
            Ok((host, tag))
        });
        let (mut host, tag) = match host {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 등록 때 방화벽 여부를 기록하므로 등록 전에 찾는다.
        host.detect_firewall(&tag).await;
        // 워커와 마이그레이션 받기가 VM을 켜기 직전에 규칙을 넣는다. 에이전트는 아래에서 띄운다.
        let firewall = FirewallRules::new(cluster.clone(), config, &host, tag.clone());
        let gate = firewall.clone().map(|f| f as Arc<dyn FirewallGate>);
        let hv: Arc<dyn Hypervisor> = match QemuHypervisor::new(host.qemu_config(config)).await {
            Ok(hv) => Arc::new(hv),
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e.into());
            }
        };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        let (exited_tx, exited) = mpsc::channel(2);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
            },
        );
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone());
        let peer = serve_peer(peer_listener, &tls, service, exited_tx.clone());

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            let api_tls = api_server_config(&tls)?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = spawn_promotion(first_time, &cluster, &peers);
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        // 장치를 만들거나 지우면 곧바로 브리지 목록을 기록한다(아래 주기 갱신을 기다리지 않는다).
        let overlay = OverlayAgent::spawn(cluster.clone(), config, &host, tag);
        let firewall = firewall.map(FirewallAgent::spawn);
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| HaGuard::spawn(cluster.clone(), ha));
        reconciler_config.ha = ha.clone();
        reconciler_config.firewall = gate;
        let reconciler = Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config);
        let pools = PoolMounter::spawn(
            cluster.clone(),
            mount_root.clone(),
            qemu_binary,
            config.pools.refresh(),
        );
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        runner_config.mount_root = mount_root.clone();
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        // ESXi 가져오기의 자격 증명은 API가 받아 두고 가져오기 Task가 쓴다.
        runner_config.import_credentials = state.import_credentials.clone();
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
        let updates = config
            .updates
            .updates_config()
            .map(|c| Arc::new(Updates::new(c)));
        runner_config.updates = updates.clone();
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv,
            reconciler.handle(),
            runner_config,
        )
        .await;
        let monitor = NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default());
        let updates = updates.map(|u| UpdateAgent::spawn(cluster.clone(), node, u));

        let listener = TlsListener::new(api_listener, api_tls)?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "opld is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let state = Arc::new(state);
        let schedule = SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config());
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let backups = BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
        let app = opl_api::router(state).merge(crate::web::router());
        let task = tokio::spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            services: Some(Services {
                api: Server { stop, task },
                reconciler,
                runner,
                monitor,
                schedule,
                janitor,
                backups,
                recovery,
                pools,
                overlay,
                firewall,
                ha,
                updates,
            }),
            register,
            promotion,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = serve_peer(peer_listener, &tls, service, exited_tx);
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = spawn_promotion(first_time, &cluster, &peers);
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            tokio::spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "opld is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            exited,
        })
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        if let Some(services) = self.services.take() {
            services.shutdown(left).await;
        }
        self.register.abort();
        self.promotion.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop opld and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다. 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(first_time: bool, cluster: &Arc<Cluster>, peers: &Arc<Peers>) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opl_types::resource::Resource;

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
```

`crates/opld/tests/common/mod.rs`에서

```rust
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\n{hypervisor}\n",
            sock = dir.path().join("opld.sock").display(),
```

을 다음으로 바꾼다:

```rust
             data_dir = \"{data}\"\nrun_dir = \"{run}\"\nqemu_share_dir = \"{share}\"\n{extra_config}\n\
             [hypervisor]\n{hypervisor}\n[updates]\nenabled = false\n",
            sock = dir.path().join("opld.sock").display(),
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1175개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: 노드 업데이트 확인 — opld [updates](enabled 없으면 apt_get이 있을 때 켜짐, check_interval 24h, first_check 1m, apt_url 비면 빌드 값 OPL_APT_URL, sources_file, apt_get·systemd_run·systemctl·dpkg·log_dir·poll)를 opl_api::UpdatesConfig로 풀어 TaskRunnerConfig.updates와 UpdateAgent에 넘김, opl-api updates.rs(Updates::check: apt-get update -q → apt-get -s -q dist-upgrade(LANG=C)의 Inst 줄 해석, 500개 넘으면 truncated, 실패는 명령과 stderr 끝 5줄, 다른 확인 중이면 기다리지 않고 오류; deb822 소스 파일 — 관리 표시 첫 줄, 표시 없는 파일은 건드리지 않음, 같으면 쓰지 않음; UpdateAgent가 시작·Settings 변경 때 소스 파일을 채널에 맞추고 주기 확인), NodeStatus.updates(NodeUpdates{checked_at 유닉스 초, packages[PackageUpdate{name,current,candidate}], truncated, error} — 켜진 노드는 등록 때 Some, record_node가 결과를 지킴, 꺼진 노드·위트니스는 None), TaskOp::NodeUpdateCheck와 POST /nodes/{key}/updates/refresh(admin, 400 updates_unsupported, 409 node_offline, 끝나지 않은 확인이 있으면 그 Task, OpenAPI·schema.d.ts), 시험용 가짜 apt(tests/common/fake_apt.rs), opld 시험 데몬은 업데이트를 끔, oplctl node updates <node> [--refresh](확인 시각·오류·PACKAGE CURRENT CANDIDATE 표·잘렸음, -o json이면 status.updates)"
```

---

### Task 4: 노드 업그레이드 Task

`POST /api/v1/nodes/{key}/upgrade`(admin) → Task `node-upgrade`: 그 노드가 `systemd-run`으로 별도 유닛에서 `apt-get dist-upgrade`를 돌리고(opld가 패키지 업그레이드로 다시 시작해도 계속) `systemctl show`로 지켜본다. 클러스터에서 한 번에 한 노드, 시간 제한·취소 없음, opld가 다시 시작하면 같은 유닛을 이어서 본다(로그의 시작·종료 줄과 `dpkg --audit`로 판단), 업그레이드 동안 업데이트 확인과 apt 잠금을 다투지 않는다. oplctl `node upgrade [--wait]`. 시험은 가짜 `systemd-run`·`systemctl`·`dpkg` 스크립트로 한다.

**Files:**
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/tasks.rs`
- Modify: `crates/opl-api/src/updates.rs`
- Modify: `crates/opl-api/tests/common/fake_apt.rs`
- Modify: `crates/opl-api/tests/openapi.rs`
- Modify: `crates/opl-api/tests/updates.rs`
- Modify: `crates/opl-types/src/task.rs`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `crates/oplctl/src/client.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(updates::refresh_updates))
        .routes(routes!(cluster::get_cluster))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(updates::refresh_updates))
        .routes(routes!(updates::upgrade_node))
        .routes(routes!(cluster::get_cluster))
```

`crates/opl-api/src/tasks.rs` 전체:

```rust
//! Task 실행기. 이 노드에 배정된 Task를 저장소에서 받아 실행하고 결과를 기록한다.
//!
//! API는 Task를 저장소에 만들기만 한다. 실행기는 reconciler처럼 목록을 읽고 그 revision부터
//! 구독하므로, 어느 노드의 API가 만든 Task든 대상 노드에서 실행된다.

mod backup;
mod clone;
mod import;
mod migrate;
mod snapshot;

pub(crate) use clone::volume_name;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_cluster::types::Rejection;
use opl_cluster::watch::WatchEvent;
use opl_cluster::{Cluster, ClusterError};
use opl_driver::{DriverError, Hypervisor, Storage};
use opl_peer::Peers;
use opl_reconciler::{FirmwareFetcher, ReconcilerHandle};
use opl_types::backup::{BackupPhase, BackupSpec};
use opl_types::resource::{Kind, Resource};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::{SnapshotPhase, SnapshotSpec};
use opl_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, VolumeStatus,
    is_shared, pool_listed_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::{PowerState, Vm, VmPhase, VmSpec};
use tokio::sync::{oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use uuid::Uuid;

use crate::error::ApiError;
use crate::schedule::task_failed;
use crate::snapshots::{uses_block_snapshots, vm_placement};
use crate::updates::Updates;
use crate::vm_imports::ImportCredentials;
use crate::vms::is_stopped;
use crate::volumes::{block_pool_kind, usable_node};

/// 새 Task를 저장소에 만든다. 이름은 id 문자열이다.
pub(crate) async fn create_task(
    cluster: &Cluster,
    op: TaskOp,
    target: Uuid,
    node: Uuid,
) -> Result<Task, ApiError> {
    let mut task: Task = Resource::new("", TaskSpec { op, target, node });
    task.name = task.id.to_string();
    task.revision = cluster.create(&task).await?;
    Ok(task)
}

#[derive(Clone)]
pub struct TaskRunnerConfig {
    /// 이 노드의 id. `spec.node`가 이 값인 Task만 실행한다.
    pub node: Uuid,
    /// Task 하나가 이보다 오래 걸리면 실패로 끝낸다.
    pub timeout: Duration,
    /// 저장소 쓰기가 실패했을 때 다시 시도하는 간격, VM 상태를 다시 확인하는 간격
    pub poll_interval: Duration,
    /// API가 올린 이미지를 받아 두는 디렉터리. 시작할 때 Task가 없는 파일을 지운다.
    pub upload_dir: Option<PathBuf>,
    /// VM별 상태 디렉터리의 뿌리(`QemuConfig::state_dir`). 스냅샷의 UEFI 변수 사본을 둔다.
    /// 없으면 변수를 복사하거나 되돌리지 않는다.
    pub state_dir: Option<PathBuf>,
    /// NFS 풀을 마운트하는 디렉터리(opld의 `{data_dir}/pools`). 백업 Task가 백업 풀의 파일 경로를 정한다.
    pub mount_root: PathBuf,
    /// 풀이 마운트되지 않아 시작할 때 지우지 못한 고아 볼륨, 시간을 넘긴 블록 스냅샷이 남긴 태그를
    /// 다시 지워 보는 간격
    pub orphan_retry: Duration,
    /// 블록 스냅샷이 남긴 태그, 이 노드에서 켜진 채 남은 볼륨을 다시 찾아 정리하는 간격(시작할 때
    /// 한 번 찾고 그 뒤로). 확정되지 않았거나 확정 뒤 남은 마이그레이션 QEMU도 이 간격으로 정리한다.
    pub tag_sweep: Duration,
    /// 라이브 마이그레이션을 보낼 때 대상 노드를 부르는 노드 간 클라이언트. 없으면 마이그레이션 Task는
    /// 실패한다.
    pub peers: Option<Arc<Peers>>,
    /// 마이그레이션을 보내는 대역폭 상한(바이트/초). `None`이면 QEMU 기본값
    pub migration_bandwidth: Option<u64>,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다. 마이그레이션 Task는 `timeout` 대신 이것을 쓴다.
    pub migration_timeout: Duration,
    /// 스냅샷·복제가 UEFI 변수를 복사하기 전에, 변수 파일이 다른 노드에 있으면 받아 온다. 없으면
    /// 이 노드의 파일을 그대로 쓴다.
    pub firmware: Option<Arc<dyn FirmwareFetcher>>,
    /// 이 노드가 받은 ESXi 가져오기의 자격 증명. 가져오기 Task가 쓰고, 끝나거나 지우면 지운다. API
    /// (`AppState::import_credentials`)와 같은 것을 넘긴다.
    pub import_credentials: Arc<ImportCredentials>,
    /// 이 노드의 업데이트 확인. 없으면(업데이트가 꺼진 노드) 업데이트 확인 Task는 실패한다.
    pub updates: Option<Arc<Updates>>,
}

impl TaskRunnerConfig {
    pub fn new(node: Uuid) -> Self {
        Self {
            node,
            timeout: Duration::from_secs(600),
            poll_interval: Duration::from_millis(200),
            upload_dir: None,
            state_dir: None,
            mount_root: PathBuf::new(),
            orphan_retry: Duration::from_secs(60),
            tag_sweep: Duration::from_secs(30 * 60),
            peers: None,
            migration_bandwidth: None,
            migration_timeout: Duration::from_secs(30 * 60),
            firmware: None,
            import_credentials: Arc::default(),
            updates: None,
        }
    }
}

pub struct TaskRunner {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Ctx {
    cluster: Arc<Cluster>,
    storage: Arc<dyn Storage>,
    hv: Arc<dyn Hypervisor>,
    reconciler: ReconcilerHandle,
    config: TaskRunnerConfig,
    /// 실행 중인 복제 Task가 만든 것(Task id별). 실패하면 정리가 지운다.
    cloned: std::sync::Mutex<HashMap<Uuid, clone::Made>>,
    /// 실행기가 멈췄는지. 뒤에서 도는 일(남은 태그 지우기)이 보고 함께 멈춘다.
    stopped: watch::Sender<bool>,
}

impl TaskRunner {
    /// 고아(Task 없는 Pending 볼륨·스냅샷, 올린 파일)를 지운 뒤 실행 루프를 띄우고 돌아온다.
    /// API가 막 만든 볼륨이나 올린 파일을 고아로 지우지 않도록 API 서버는 이것을 기다린 뒤 띄운다.
    /// `hv`는 실행 중인 VM의 스냅샷을 다룬다(이 노드의 reconciler와 같은 하이퍼바이저).
    pub async fn spawn(
        cluster: Arc<Cluster>,
        storage: Arc<dyn Storage>,
        hv: Arc<dyn Hypervisor>,
        reconciler: ReconcilerHandle,
        config: TaskRunnerConfig,
    ) -> Self {
        let ctx = Arc::new(Ctx {
            cluster,
            storage,
            hv,
            reconciler,
            config,
            cloned: Default::default(),
            stopped: watch::channel(false).0,
        });
        let (revision, tasks) = loop {
            match ctx.cluster.list::<TaskSpec>() {
                Ok(list) => break list,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot list tasks; will retry");
                    tokio::time::sleep(ctx.config.poll_interval).await;
                }
            }
        };
        let unmounted = sweep_orphans(&ctx, revision, &tasks).await;
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(ctx, rx, unmounted)),
        }
    }

    /// 실행 중인 Task를 멈추고 돌아온다. 멈춘 Task는 다음에 시작할 때 실패로 기록한다.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

/// `unmounted`는 풀이 마운트되지 않아 지우지 못한 고아 볼륨이다. 따로 다시 지워 본다.
async fn run(ctx: Arc<Ctx>, mut shutdown: oneshot::Receiver<()>, unmounted: Vec<Uuid>) {
    let retry = tokio::spawn(retry_orphans(ctx.clone(), unmounted));
    let tags = tokio::spawn(sweep_block_tags_every(ctx.clone()));
    let idle = tokio::spawn(deactivate_idle_volumes_every(ctx.clone()));
    let migrations = tokio::spawn(migrate::sweep_every(ctx.clone()));
    // 실행을 시작한 Task. 목록을 다시 읽을 때 같은 Task를 두 번 시작하지 않게 한다.
    let mut started: HashSet<Uuid> = HashSet::new();
    // 실행 중인 Task. 실행기가 멈출 때 함께 멈춘다.
    let mut running: HashMap<Uuid, AbortHandle> = HashMap::new();
    // 줄(볼륨, VM)별 마지막 Task. 같은 줄의 Task는 앞의 것이 끝난 뒤에 시작한다(`queue`).
    let mut by_target: HashMap<Uuid, JoinHandle<()>> = HashMap::new();
    let mut first = true;
    'sync: loop {
        let (revision, tasks) = match ctx.cluster.list::<TaskSpec>() {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(error = %e, "cannot list tasks; will retry");
                tokio::select! {
                    _ = &mut shutdown => break,
                    () = tokio::time::sleep(ctx.config.poll_interval) => continue,
                }
            }
        };
        running.retain(|_, h| !h.is_finished());
        by_target.retain(|_, h| !h.is_finished());
        started.retain(|id| {
            tasks
                .iter()
                .any(|t| t.id == *id && !t.status.state.is_finished())
        });
        for task in tasks {
            if task.spec.node != ctx.config.node || started.contains(&task.id) {
                continue;
            }
            match task.status.state {
                TaskState::Pending => start(
                    &ctx,
                    &mut started,
                    &mut running,
                    &mut by_target,
                    task,
                    false,
                ),
                // 이전 opld가 실행하다 내려갔다.
                TaskState::Running if first => {
                    start(&ctx, &mut started, &mut running, &mut by_target, task, true)
                }
                _ => {}
            }
        }
        first = false;
        let mut watch = match ctx.cluster.watch(revision) {
            Ok(w) => w,
            Err(_) => continue,
        };
        loop {
            tokio::select! {
                _ = &mut shutdown => break 'sync,
                ev = watch.next() => match ev {
                    Ok(WatchEvent::Put { kind: Kind::Task, resource }) => {
                        let Ok(task) = serde_json::to_value(resource).and_then(serde_json::from_value::<Task>)
                        else {
                            continue;
                        };
                        if task.spec.node == ctx.config.node
                            && task.status.state == TaskState::Pending
                            && !started.contains(&task.id)
                        {
                            start(&ctx, &mut started, &mut running, &mut by_target, task, false);
                        }
                    }
                    Ok(WatchEvent::Reset { .. }) | Err(_) => continue 'sync,
                    Ok(_) => {}
                },
            }
        }
    }
    for handle in running.values() {
        handle.abort();
    }
    retry.abort();
    tags.abort();
    idle.abort();
    migrations.abort();
    ctx.stopped.send_replace(true);
}

/// 공유 풀에서 남은 태그로 보고 지우려면 스냅샷이 만들어진 뒤 지나야 하는 시간. 다른 노드의 스냅샷
/// Task가 막 찍은 태그를 그 리소스보다 먼저 보고 지우지 않게 한다.
const SHARED_TAG_MIN_AGE: Duration = Duration::from_secs(30 * 60);

/// 남은 블록 스냅샷 태그를 시작할 때와 그 뒤 `tag_sweep`마다 지운다([`sweep_block_tags`]). 실행기가
/// 멈추면 함께 멈춘다.
async fn sweep_block_tags_every(ctx: Arc<Ctx>) {
    loop {
        sweep_block_tags(ctx.clone()).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 시간을 넘긴 블록 스냅샷이 남긴 태그를 지운다: 이 노드가 맡는 블록 풀(LVM-thin, ZFS, RBD) 볼륨의
/// 태그 중 UUID 모양인데 그 id의 스냅샷 리소스가 없는 것. 스냅샷 Task는 리소스를 만든 뒤 태그를
/// 찍고 태그를 지운 뒤 리소스를 지우므로 그런 태그는 남은 것이다. 백업의 임시 스냅샷(백업 id의 simple
/// 문자열)도 같은 모양이므로, 백업 Task가 끝나지 않은 `pending` 백업의 태그는 둔다. opLite 밖에서 만든 태그는 두고,
/// 스토리지 명령이 느려도 실행기 시작을 막지 않게 뒤에서 돈다.
///
/// 노드 로컬 풀의 볼륨은 그 볼륨을 만든 노드가 맡는다. 공유 풀(RBD)의 볼륨은 어느 노드에서나
/// 보이므로 노드마다 같은 답이 나오게 풀을 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드가
/// 맡는다(그런 노드가 없으면 만든 노드). 공유 풀에서는 다른 노드의 Task가 막 찍은 태그일 수 있으므로
/// 만든 지 [`SHARED_TAG_MIN_AGE`]가 지난 태그만 지운다.
async fn sweep_block_tags(ctx: Arc<Ctx>) {
    let volumes = match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => volumes,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list volumes");
            return;
        }
    };
    for volume in volumes {
        if volume.status.phase != VolumePhase::Ready {
            continue;
        }
        let Ok(Some(pool)) = ctx
            .cluster
            .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        else {
            continue;
        };
        if !uses_block_snapshots(&pool) {
            continue;
        }
        let shared = is_shared(&pool);
        let owner = if shared {
            usable_node(&ctx.cluster, &pool, None)
                .ok()
                .flatten()
                .or(volume.status.node)
        } else {
            volume.status.node
        };
        if owner.is_some_and(|n| n != ctx.config.node) {
            continue;
        }
        let format = volume.spec.format;
        let tags = if shared {
            ctx.storage
                .snapshot_tags_older_than(&pool, volume.id, format, SHARED_TAG_MIN_AGE)
                .await
        } else {
            ctx.storage.snapshot_tags(&pool, volume.id, format).await
        };
        let tags = match tags {
            Ok(tags) => tags,
            Err(e) => {
                tracing::debug!(volume = %volume.name, error = %e, "cannot read block snapshots");
                continue;
            }
        };
        for tag in tags {
            let Ok(id) = tag.parse::<Uuid>() else {
                continue;
            };
            if !matches!(ctx.cluster.get::<SnapshotSpec>(id), Ok(None)) {
                continue;
            }
            match ctx.cluster.get::<BackupSpec>(id) {
                Ok(Some(b)) if b.status.phase != BackupPhase::Pending => {}
                // 채울 Task가 없는 pending 백업(Task가 끝났거나 지워졌다)의 태그도 남은 것이다.
                Ok(Some(_)) if !backing_up(&ctx, id) => {}
                Ok(None) => {}
                _ => continue,
            }
            tracing::warn!(volume = %volume.name, tag, "removing a block snapshot left without its snapshot");
            if let Err(e) = ctx
                .storage
                .snapshot_delete(&pool, volume.id, format, &tag)
                .await
            {
                tracing::warn!(volume = %volume.name, tag, error = %e, "cannot remove block snapshot");
            }
        }
    }
}

/// 백업 `id`를 채우는 백업 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn backing_up(ctx: &Ctx, id: Uuid) -> bool {
    let Ok((_, tasks)) = ctx.cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == id)
    })
}

/// 이 노드에서 켜진 채 남은 볼륨을 시작할 때와 그 뒤 `tag_sweep`마다 끈다
/// ([`deactivate_idle_volumes`]). 실행기가 멈추면 함께 멈춘다.
async fn deactivate_idle_volumes_every(ctx: Arc<Ctx>) {
    loop {
        deactivate_idle_volumes(&ctx).await;
        tokio::time::sleep(ctx.config.tag_sweep).await;
    }
}

/// 이 노드에서 켜진 채 남은 볼륨(공유 LVM의 배타 활성화)을 끈다. VM이 멈춘 뒤 reconciler가 끄지
/// 못했거나 opld가 그 전에 내려갔을 때 남는다. 이 노드에 놓여 실행 중이거나 켜려는 VM(하이퍼바이저에서
/// 도는 VM 포함)의 디스크, 이 노드에서 끝나지 않은 Task의 볼륨(VM Task면 그 VM의 디스크), 아직
/// 만드는 중인(pending) 볼륨은 그대로 둔다. 리소스가 없는 볼륨은 끈다.
///
/// 켜진 볼륨을 먼저 읽고 그 뒤에 VM·Task를 읽는다: 볼륨을 켜는 VM·Task는 켜기 전에 이미 저장소에
/// 있으므로 쓰는 중인 볼륨을 끄지 않는다(그 사이 켠 볼륨은 이번에 보지 않는다). 이미 켜져 있던 볼륨을
/// 그 사이 시작한 VM이 그대로 쓸 수도 있으므로 VM·Task는 볼륨마다 끄기 직전에 다시 읽는다.
async fn deactivate_idle_volumes(ctx: &Ctx) {
    let me = ctx.config.node;
    let pools = match ctx.cluster.list::<StoragePoolSpec>() {
        Ok((_, pools)) => pools,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list storage pools");
            return;
        }
    };
    let mut active = Vec::new();
    for pool in pools.into_iter().filter(|p| pool_listed_on(p, me)) {
        match ctx.storage.active_volumes(&pool).await {
            Ok(ids) if !ids.is_empty() => active.push((pool, ids)),
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(pool = %pool.name, error = %e, "cannot list active volumes")
            }
        }
    }
    for (pool, ids) in active {
        for id in ids {
            let (in_use, formats) = match volumes_in_use(ctx).await {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read which volumes are in use");
                    return;
                }
            };
            if in_use.contains(&id) {
                continue;
            }
            let format = formats.get(&id).copied().unwrap_or(ImageFormat::Raw);
            tracing::info!(pool = %pool.name, volume = %id, "deactivating a volume left active");
            if let Err(e) = ctx.storage.deactivate_volume(&pool, id, format).await {
                tracing::warn!(pool = %pool.name, volume = %id, error = %e, "cannot deactivate volume");
            }
        }
    }
}

/// 이 노드가 쓰는 볼륨([`deactivate_idle_volumes`]이 그대로 두는 볼륨)과 볼륨별 형식. 이 노드로
/// 마이그레이션하는 중인 VM의 디스크도 쓰는 중이다(받는 QEMU를 띄우기 전에 켠다). 이 노드에서 보내는
/// 중인 VM은 원하는 상태가 running이고 마이그레이션 Task가 있다.
async fn volumes_in_use(ctx: &Ctx) -> Result<(HashSet<Uuid>, HashMap<Uuid, ImageFormat>), String> {
    let me = ctx.config.node;
    let running = ctx
        .hv
        .list()
        .await
        .map_err(|e| format!("cannot list running VMs: {e}"))?;
    let (volumes, tasks, vms) = ctx
        .cluster
        .list::<VolumeSpec>()
        .and_then(|(_, volumes)| {
            Ok((
                volumes,
                ctx.cluster.list::<TaskSpec>()?.1,
                ctx.cluster.list::<VmSpec>()?.1,
            ))
        })
        .map_err(|e| format!("cannot list volumes, tasks or VMs: {e}"))?;
    let mut in_use: HashSet<Uuid> = volumes
        .iter()
        .filter(|v| v.status.phase != VolumePhase::Ready)
        .map(|v| v.id)
        .collect();
    let mut task_vms = HashSet::new();
    for task in tasks
        .iter()
        .filter(|t| t.spec.node == me && !t.status.state.is_finished())
    {
        in_use.insert(task.spec.target);
        task_vms.insert(task.spec.target);
    }
    let by_name: HashMap<&str, Uuid> = volumes.iter().map(|v| (v.name.as_str(), v.id)).collect();
    for vm in &vms {
        let used = task_vms.contains(&vm.id)
            || running.iter().any(|r| r.id == vm.id)
            || (vm.spec.node == Some(me) && vm.spec.power == PowerState::Running)
            || vm.status.migration.is_some_and(|m| m.node == me);
        if used {
            in_use.extend(
                vm.spec
                    .disks
                    .iter()
                    .filter_map(|d| by_name.get(d.volume.as_str())),
            );
        }
    }
    let formats = volumes.iter().map(|v| (v.id, v.spec.format)).collect();
    Ok((in_use, formats))
}

/// 풀이 마운트되지 않았거나 응답하지 않아 지우지 못한 고아 볼륨을 `orphan_retry`마다 다시 지워 본다. 지웠거나 볼륨이
/// 없어졌으면 목록에서 뺀다. 마운트 말고 다른 이유로 실패하면 시작할 때처럼 기록만 하고 뺀다.
async fn retry_orphans(ctx: Arc<Ctx>, mut volumes: Vec<Uuid>) {
    while !volumes.is_empty() {
        tokio::time::sleep(ctx.config.orphan_retry).await;
        let mut left = Vec::new();
        for id in volumes {
            match remove_volume(&ctx, id).await {
                Ok(()) => tracing::info!(volume = %id, "removed a volume left unfinished"),
                Err(RemoveError::Storage(
                    DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                )) => left.push(id),
                Err(e) => tracing::warn!(volume = %id, error = %e, "cannot remove volume"),
            }
        }
        volumes = left;
    }
}

fn is_volume_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. } | TaskOp::VolumeDelete
    )
}

fn is_snapshot_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
    )
}

/// Task가 설 줄. 볼륨 Task는 그 볼륨의 줄에 선다(`Storage`의 요구). 스냅샷 Task는 VM의 줄에
/// 선다(부모·현재 위치가 차례로 바뀐다). 복제 Task도 원본 VM의 줄에 선다. 복사하는 동안 그 스냅샷을
/// 지우거나 디스크를 되돌리면 안 된다. 볼륨 크기 늘리기는 그 볼륨을 쓰는 VM의 줄에 선다. 스냅샷
/// 만들기·되돌리기가 같은 디스크를 다루고 되돌리기는 크기도 되돌리므로 섞이면 안 된다. 지금 쓰는
/// VM이 없으면 그 볼륨을 담은 스냅샷의 VM(되돌리면 다시 붙는다), 그것도 없으면 볼륨의 줄이다.
/// VM 전원 Task는 기다리기만 하므로 줄을 두지 않는다. 강제 종료가 앞의 reboot 뒤에 묶이면 안 된다.
/// 마이그레이션 Task는 VM의 줄에 선다(앞의 스냅샷·복제가 끝난 뒤 옮긴다). 백업 Task도 VM의 줄에
/// 선다(임시 스냅샷을 찍고 디스크를 읽는다). 백업 지우기와 복원은 그 백업의 줄에 서서 겹치지 않는다
/// (복원 Task의 대상은 새 VM이다). VM 가져오기와 가져오기 지우기도 그 가져오기의 줄에 선다(가져오기
/// Task의 대상은 새 VM이다).
fn queue(ctx: &Ctx, task: &Task) -> Option<Uuid> {
    let target = task.spec.target;
    match task.spec.op {
        TaskOp::BackupRestore { backup, .. } => return Some(backup),
        TaskOp::VmImport { import, .. } => return Some(import),
        _ => {}
    }
    if !matches!(task.spec.op, TaskOp::VolumeResize { .. }) {
        let vm_queue = matches!(
            task.spec.op,
            TaskOp::VmClone { .. } | TaskOp::VmMigrate { .. } | TaskOp::VmBackup { .. }
        );
        let backup_queue = matches!(task.spec.op, TaskOp::BackupDelete | TaskOp::VmImportDelete);
        return (is_volume_op(&task.spec.op)
            || is_snapshot_op(&task.spec.op)
            || vm_queue
            || backup_queue)
            .then_some(target);
    }
    let Ok(Some(volume)) = ctx.cluster.get::<VolumeSpec>(target) else {
        return Some(target);
    };
    let uses = |spec: &VmSpec| spec.disks.iter().any(|d| d.volume == volume.name);
    if let Ok((_, vms)) = ctx.cluster.list::<VmSpec>()
        && let Some(vm) = vms.iter().find(|vm| uses(&vm.spec))
    {
        return Some(vm.id);
    }
    if let Ok((_, snapshots)) = ctx.cluster.list::<SnapshotSpec>()
        && let Some(s) = snapshots.iter().find(|s| uses(&s.spec.vm_spec))
    {
        return Some(s.spec.vm);
    }
    Some(target)
}

/// Task를 실행한다. 줄(`queue`)이 있으면 그 줄의 앞 Task가 끝난 뒤에 시작한다.
/// `interrupted`면 이전 opld가 실행하다 멈춘 Task이므로 실패로 기록한다. 업그레이드 Task는 opld 밖의
/// 유닛에서 돌고 opld를 다시 시작하게 하므로 실패로 기록하지 않고 그 유닛을 이어서 지켜본다.
fn start(
    ctx: &Arc<Ctx>,
    started: &mut HashSet<Uuid>,
    running: &mut HashMap<Uuid, AbortHandle>,
    by_target: &mut HashMap<Uuid, JoinHandle<()>>,
    task: Task,
    interrupted: bool,
) {
    started.insert(task.id);
    let id = task.id;
    let queue = queue(ctx, &task);
    let previous = queue.and_then(|q| by_target.remove(&q));
    let ctx = ctx.clone();
    let handle = tokio::spawn(async move {
        if let Some(previous) = previous {
            let _ = previous.await;
        }
        if interrupted && task.spec.op != TaskOp::NodeUpgrade {
            let error = "interrupted: opld restarted while the task was running".to_string();
            fail(&ctx, &task, error, true).await;
        } else {
            execute(&ctx, task, interrupted).await;
        }
    });
    running.insert(id, handle.abort_handle());
    if let Some(q) = queue {
        by_target.insert(q, handle);
    }
}

/// `resume`이면 이전 opld가 실행하던 Task를 이어서 한다(업그레이드만).
async fn execute(ctx: &Ctx, task: Task, resume: bool) {
    if !begin(ctx, task.id).await {
        return;
    }
    // 시간을 넘겨 버린 작업은 끝나지 않았다(명령이 아직 돌 수 있다). 마이그레이션은 단계마다
    // 시간 제한이 있고(`migration_timeout`, RPC 기한), 확정 쓰기 도중에 버리면 안 되므로 감싸지 않는다.
    // 백업과 복원, VM 가져오기는 디스크 크기만큼 오래 걸릴 수 있으므로 시간 제한을 두지 않는다(설계 §2.3).
    // 업그레이드도 오래 걸릴 수 있고 apt를 중간에 끊으면 더 나쁘다.
    let unlimited = matches!(
        task.spec.op,
        TaskOp::VmMigrate { .. }
            | TaskOp::VmBackup { .. }
            | TaskOp::BackupRestore { .. }
            | TaskOp::VmImport { .. }
            | TaskOp::NodeUpgrade
    );
    let (result, unfinished) = if unlimited {
        (perform(ctx, &task, resume).await, false)
    } else {
        match tokio::time::timeout(ctx.config.timeout, perform(ctx, &task, resume)).await {
            Ok(r) => (r, false),
            Err(_) => (
                Err(format!("timed out after {:?}", ctx.config.timeout)),
                true,
            ),
        }
    };
    match result {
        Ok(()) => {
            // 예약 오류는 만들기가 성공해야 지운다(보관 삭제의 성공은 만들기 실패를 풀지 않는다).
            let create = matches!(task.spec.op, TaskOp::SnapshotCreate { .. });
            if let Some(vm) = snapshot::scheduled_vm(ctx, &task).filter(|_| create) {
                snapshot::record_schedule(ctx, vm, None).await;
            }
            set_status(ctx, task.id, TaskState::Succeeded, None).await
        }
        Err(e) => fail(ctx, &task, e, unfinished).await,
    }
}

/// 실패로 기록한다. 만들다 만 볼륨·스냅샷은 지운다. 예약 스냅샷의 만들기나 예약 정리의 삭제였으면
/// VM에 이유를 남긴다. `unfinished`면 작업이 끝나지 않았다(시간 초과, 재시작으로 중단).
async fn fail(ctx: &Ctx, task: &Task, error: String, unfinished: bool) {
    tracing::warn!(task = %task.id, error = %error, "task failed");
    // 정리가 스냅샷 리소스를 지우므로 먼저 본다.
    let scheduled = snapshot::scheduled_vm(ctx, task);
    cleanup(ctx, task, &error, unfinished).await;
    if let Some(vm) = scheduled {
        snapshot::record_schedule(ctx, vm, Some(task_failed(error.clone()))).await;
    }
    set_status(ctx, task.id, TaskState::Failed, Some(error)).await;
}

/// 실패한 볼륨 생성·가져오기, 스냅샷 만들기, 복제·복원·VM 가져오기의 흔적을 지워 이름을 다시 쓸 수 있게
/// 한다. 실패한 백업은 디렉터리를 지우고 `failed`와 `error`를 남긴다. 실패한 VM 가져오기는 `ready`로
/// 돌아가 `error`를 남긴다.
/// 스냅샷의 디스크 태그는 만들기가 실패를 확인한 경우에 스스로 지운다. 여기서는(시간 초과,
/// 재시작으로 중단) 작업이 아직 돌고 있을 수 있어 태그를 두고 리소스만 지운다(설계 §6 "태그만 남은 경우").
/// 블록 디스크의 태그는 뒤에서 다시 지운다([`snapshot::remove_unfinished_tags`]).
async fn cleanup(ctx: &Ctx, task: &Task, error: &str, unfinished: bool) {
    if let TaskOp::SnapshotCreate { snapshot } = task.spec.op {
        if unfinished {
            snapshot::remove_unfinished_tags(ctx, snapshot);
        }
        if let Err(e) = snapshot::discard_pending(ctx, snapshot).await {
            tracing::warn!(snapshot = %snapshot, error = %e, "cannot clean up failed snapshot");
        }
    }
    if let TaskOp::VolumeImport { source, .. } = &task.spec.op {
        let _ = tokio::fs::remove_file(source).await;
    }
    if let TaskOp::VmClone { .. } | TaskOp::BackupRestore { .. } = task.spec.op {
        clone::discard(ctx, task.id).await;
    }
    if let TaskOp::VmBackup { backup } = task.spec.op {
        backup::discard(ctx, task, backup, error, unfinished).await;
    }
    if let TaskOp::VmImport { import, .. } = task.spec.op {
        import::discard(ctx, task, import, error).await;
    }
    if matches!(
        task.spec.op,
        TaskOp::VolumeCreate | TaskOp::VolumeImport { .. }
    ) && let Err(e) = remove_volume(ctx, task.spec.target).await
    {
        tracing::warn!(volume = %task.spec.target, error = %e, "cannot clean up failed volume");
    }
}

/// `pending`을 `running`으로 읽은 revision에서만 바꾼다. 그사이 다른 쪽(리더의 정리 작업)이
/// 실패로 끝냈거나 Task가 지워졌으면 `false`로, 실행하지 않는다. `running`으로 읽히면 응답을 잃은
/// 앞의 쓰기가 적용된 것이다(`running`은 이 노드만 쓴다). 기록될 때까지 다시 시도한다.
async fn begin(ctx: &Ctx, id: Uuid) -> bool {
    let running = TaskStatus {
        state: TaskState::Running,
        error: None,
    };
    loop {
        let current = match ctx.cluster.get::<TaskSpec>(id) {
            Ok(Some(task)) => task,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot read task; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        match current.status.state {
            TaskState::Pending => {}
            TaskState::Running => return true,
            TaskState::Succeeded | TaskState::Failed => return false,
        }
        match ctx
            .cluster
            .update_status_checked::<TaskSpec>(id, current.revision, &running)
            .await
        {
            Ok(_) => return true,
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return false,
            // 바뀐 Task가 이 노드에 적용되면 다시 읽어 판단한다.
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task start; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 저장소에 기록될 때까지 다시 시도한다. 실행기가 멈추면 이 future도 함께 멈춘다.
async fn set_status(ctx: &Ctx, id: Uuid, state: TaskState, error: Option<String>) {
    let status = TaskStatus { state, error };
    loop {
        match ctx.cluster.update_status::<TaskSpec>(id, &status).await {
            Ok(_) => return,
            // 정리 작업이 지운 Task는 끝난 것이다(`begin`과 같다).
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(e) => {
                tracing::warn!(task = %id, error = %e, "cannot record task status; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

async fn perform(ctx: &Ctx, task: &Task, resume: bool) -> Result<(), String> {
    let id = task.spec.target;
    match &task.spec.op {
        TaskOp::VmStart => wait_vm(ctx, id, started).await,
        TaskOp::VmStop => wait_vm(ctx, id, stopped).await,
        TaskOp::VmForceStop => {
            ctx.reconciler.force_stop(id);
            wait_vm(ctx, id, stopped).await
        }
        TaskOp::VmReboot => reboot(ctx, task).await,
        TaskOp::VolumeCreate => create_volume(ctx, id).await,
        TaskOp::VolumeImport {
            source,
            source_format,
        } => import_volume(ctx, id, source, *source_format).await,
        TaskOp::VolumeDelete => delete_volume(ctx, id).await,
        TaskOp::SnapshotCreate { snapshot } => snapshot::create(ctx, id, *snapshot).await,
        TaskOp::SnapshotRevert { snapshot } => snapshot::revert(ctx, task, *snapshot).await,
        TaskOp::SnapshotDelete { snapshot, .. } => snapshot::delete(ctx, id, *snapshot).await,
        TaskOp::VolumeResize { size_bytes } => resize_volume(ctx, id, *size_bytes).await,
        TaskOp::VmClone {
            name,
            snapshot,
            pool,
        } => clone::clone(ctx, task, name, *snapshot, pool.as_deref()).await,
        TaskOp::VmMigrate { node } => migrate::migrate(ctx, task, *node).await,
        // 기록 Task는 끝난 상태로 만들어지므로 실행되지 않는다.
        TaskOp::VmHaRecover { .. } => Err("an HA recovery task is a record and is not run".into()),
        TaskOp::VmBackup { backup } => backup::backup(ctx, task, *backup).await,
        TaskOp::BackupDelete => backup::delete(ctx, id).await,
        TaskOp::BackupRestore { backup, name, pool } => {
            backup::restore(ctx, task, *backup, name, pool).await
        }
        TaskOp::VmImport { import, run } => import::import(ctx, task, *import, run).await,
        TaskOp::VmImportDelete => import::delete(ctx, id).await,
        TaskOp::NodeUpdateCheck => match &ctx.config.updates {
            Some(updates) => updates.refresh_waiting(&ctx.cluster, ctx.config.node).await,
            None => Err("updates are not enabled on this node".into()),
        },
        TaskOp::NodeUpgrade => match &ctx.config.updates {
            Some(updates) => {
                updates
                    .upgrade(&ctx.cluster, ctx.config.node, task.id, resume)
                    .await
            }
            None => Err("updates are not enabled on this node".into()),
        },
    }
}

/// VM을 켰다: 원하는 상태와 실제 상태가 모두 running. reconciler가 포기해 원하는 상태를
/// stopped로 바꿨으면 실패다. 이전 실행의 Failed status는 원하는 상태가 running인 동안 무시한다.
fn started(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, VmPhase::Running) => Some(Ok(())),
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Err(vm
            .status
            .last_error
            .clone()
            .unwrap_or_else(|| "VM was stopped".into()))),
        _ => None,
    }
}

fn stopped(vm: &Vm) -> Option<Result<(), String>> {
    match (vm.spec.power, vm.status.phase) {
        (PowerState::Stopped, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        (PowerState::Running, _) => Some(Err("VM was started again".into())),
        _ => None,
    }
}

/// VM이 조건을 만족할 때까지 로컬 저장소를 다시 읽는다.
async fn wait_vm(
    ctx: &Ctx,
    id: Uuid,
    done: impl Fn(&Vm) -> Option<Result<(), String>>,
) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if let Some(result) = done(&vm) {
            return result;
        }
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

/// 원하는 상태를 바꾼다. reconciler의 status 쓰기와 부딪치면 다시 읽어 쓴다.
async fn set_power(ctx: &Ctx, id: Uuid, power: PowerState) -> Result<(), String> {
    loop {
        let vm = ctx
            .cluster
            .get::<VmSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("VM was deleted")?;
        if vm.spec.power == power {
            return Ok(());
        }
        let mut spec = vm.spec.clone();
        spec.power = power;
        match ctx
            .cluster
            .update_spec(id, Some(vm.revision), vm.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// ACPI로 끈 뒤 다시 켠다. 게스트가 ACPI를 무시하면 reconciler가 `acpi_timeout` 뒤 강제로 끈다.
/// 그 사이 같은 VM에 대한 요청(시작, 정지, 강제 종료)이 새로 들어오면 그 요청을 따르고 다시 켜지 않는다.
async fn reboot(ctx: &Ctx, task: &Task) -> Result<(), String> {
    let id = task.spec.target;
    // 뒤의 요청이 이미 끝났을 수 있다(켜진 VM의 start는 곧바로 끝난다). 그 뒤에 끄면 안 된다.
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Stopped).await?;
    wait_vm(ctx, id, |vm| match (vm.spec.power, vm.status.phase) {
        (PowerState::Running, _) | (_, VmPhase::Stopped | VmPhase::Failed) => Some(Ok(())),
        _ => None,
    })
    .await?;
    if superseded(ctx, task)? {
        return Err("superseded by a later request for this VM".into());
    }
    set_power(ctx, id, PowerState::Running).await?;
    wait_vm(ctx, id, started).await
}

/// 이 Task 뒤에 같은 VM의 전원을 바꾸는 Task(동작, 스냅샷 되돌리기)가 만들어졌는지.
/// 스냅샷 만들기·삭제는 전원을 바꾸지 않으므로 세지 않는다. Task id는 UUIDv7이라 만든 순서대로 커진다.
fn superseded(ctx: &Ctx, task: &Task) -> Result<bool, String> {
    let (_, tasks) = ctx.cluster.list::<TaskSpec>().map_err(|e| e.to_string())?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == task.spec.target
            && t.id > task.id
            && matches!(
                t.spec.op,
                TaskOp::VmStart
                    | TaskOp::VmStop
                    | TaskOp::VmForceStop
                    | TaskOp::VmReboot
                    | TaskOp::SnapshotRevert { .. }
            )
    }))
}

fn volume_and_pool(ctx: &Ctx, id: Uuid) -> Result<(Volume, StoragePool), String> {
    let volume = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
        .ok_or("volume was deleted")?;
    let pool = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("storage pool {:?} not found", volume.spec.pool))?;
    Ok((volume, pool))
}

/// 볼륨 스펙의 크기를 바꾼다. 다른 쓰기(라벨 등)를 덮지 않게 다시 읽은 revision으로 쓰고,
/// 그 사이 바뀌었으면 다시 읽어 쓴다.
async fn set_volume_size(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    loop {
        let volume = ctx
            .cluster
            .get::<VolumeSpec>(id)
            .map_err(|e| e.to_string())?
            .ok_or("volume was deleted")?;
        if volume.spec.size_bytes == size_bytes {
            return Ok(());
        }
        let spec = VolumeSpec {
            size_bytes,
            ..volume.spec.clone()
        };
        match ctx
            .cluster
            .update_spec(id, Some(volume.revision), volume.labels.clone(), &spec)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// 파일이 준비된 볼륨을 Ready로 기록한다. 실제 크기(qemu-img가 512바이트 단위로 올림하고, 가져온
/// 이미지는 원본 크기다)와 할당 크기도 남기는데, 이것은 실패해도 넘어간다. Ready는 기록될 때까지
/// 다시 시도한다. 실패로 끝내면 정리하면서 다 만든 파일을 지우게 된다.
async fn mark_ready(ctx: &Ctx, volume: &Volume, pool: &StoragePool) {
    let info = match ctx
        .storage
        .volume_info(pool, volume.id, volume.spec.format)
        .await
    {
        Ok(info) => Some(info),
        Err(e) => {
            tracing::warn!(volume = %volume.id, error = %e, "cannot read volume size");
            None
        }
    };
    if let Some(info) = info
        && volume.spec.size_bytes != info.virtual_bytes
    {
        let mut spec = volume.spec.clone();
        spec.size_bytes = info.virtual_bytes;
        if let Err(e) = ctx
            .cluster
            .update_spec(volume.id, None, volume.labels.clone(), &spec)
            .await
        {
            tracing::warn!(volume = %volume.id, error = %e, "cannot record volume size");
        }
    }
    let status = VolumeStatus {
        phase: VolumePhase::Ready,
        allocated_bytes: info.map(|i| i.allocated_bytes),
        ..volume.status.clone()
    };
    while let Err(e) = ctx
        .cluster
        .update_status::<VolumeSpec>(volume.id, &status)
        .await
    {
        tracing::warn!(volume = %volume.id, error = %e, "cannot mark volume ready; will retry");
        tokio::time::sleep(ctx.config.poll_interval).await;
    }
}

async fn create_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    match ctx
        .storage
        .create_volume(&pool, id, volume.spec.size_bytes, volume.spec.format)
        .await
    {
        // 볼륨 id는 유일하므로 이미 있는 파일은 이 Task의 이전 시도가 만든 것이다.
        Ok(_) | Err(DriverError::VolumeExists(_)) => {}
        Err(e) => return Err(e.to_string()),
    }
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn import_volume(
    ctx: &Ctx,
    id: Uuid,
    source: &Path,
    source_format: SourceFormat,
) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    ctx.storage
        .check_import_source(source, source_format)
        .await
        .map_err(|e| e.to_string())?;
    ctx.storage
        .import_volume(&pool, id, source, source_format, volume.spec.format)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(source).await;
    mark_ready(ctx, &volume, &pool).await;
    Ok(())
}

async fn delete_volume(ctx: &Ctx, id: Uuid) -> Result<(), String> {
    remove_volume(ctx, id).await.map_err(|e| e.to_string())
}

/// 볼륨 크기를 늘린다(설계 §5). 실행 중인 VM이 쓰면 그 QEMU로(`block_resize`, 게스트가 바로
/// 본다. 블록 볼륨(LVM-thin, ZFS, RBD, 공유 LVM)은 스토리지로 먼저 늘린다), 아니면 스토리지로 늘린 뒤 실제 크기를
/// 스펙에 기록한다. API가 확인한 뒤 VM이 켜지거나 꺼졌을 수 있고 앞의 Task가 크기를 바꿨을 수 있어 여기서
/// 다시 본다.
async fn resize_volume(ctx: &Ctx, id: Uuid, size_bytes: u64) -> Result<(), String> {
    let (volume, pool) = volume_and_pool(ctx, id)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err("volume is not ready".into());
    }
    if size_bytes <= volume.spec.size_bytes {
        return Err(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        ));
    }
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    let user = vms.iter().find_map(|vm| {
        let disk = vm.spec.disks.iter().position(|d| d.volume == volume.name)?;
        Some((vm, disk))
    });
    match user {
        // 꺼진 VM의 경로(저장소로 늘린다)는 원하는 상태도 꺼짐일 때만 탄다. 켜는 중에 늘리면 그동안
        // 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다.
        Some((vm, _)) if vm.status.phase != VmPhase::Running && !is_stopped(vm) => {
            return Err(format!(
                "VM {:?} is starting or stopping; try again when it has settled",
                vm.name
            ));
        }
        // 실행 중에는 디스크를 바꿀 수 없으므로 스펙의 순서가 QEMU의 디스크 번호다. 블록 볼륨은
        // QEMU가 늘릴 수 없으므로 저장소로 먼저 늘린 뒤 QEMU에 볼륨의 실제 크기(LVM은 extent,
        // ZFS는 1 MiB 단위로 올린다)를 알린다. 그래야 게스트가 보는 크기가 아래에서 기록하는
        // 크기와 같다.
        Some((vm, disk)) if vm.status.phase == VmPhase::Running => {
            let mut size_bytes = size_bytes;
            if block_pool_kind(&pool).is_some() {
                ctx.storage
                    .resize_volume(&pool, id, volume.spec.format, size_bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                size_bytes = ctx
                    .storage
                    .volume_info(&pool, id, volume.spec.format)
                    .await
                    .map_err(|e| format!("cannot read the new size: {e}"))?
                    .virtual_bytes;
            }
            ctx.hv
                .block_resize(vm.id, disk, size_bytes)
                .await
                .map_err(|e| e.to_string())?
        }
        _ => ctx
            .storage
            .resize_volume(&pool, id, volume.spec.format, size_bytes)
            .await
            .map_err(|e| e.to_string())?,
    }
    let info = ctx
        .storage
        .volume_info(&pool, id, volume.spec.format)
        .await
        .map_err(|e| format!("cannot read the new size: {e}"))?;
    set_volume_size(ctx, id, info.virtual_bytes).await
}

/// 볼륨을 지우지 못한 이유
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error(transparent)]
    Storage(DriverError),
    #[error("{0}")]
    Other(String),
}

impl From<String> for RemoveError {
    fn from(e: String) -> Self {
        RemoveError::Other(e)
    }
}

/// VM이 쓰지 않는 볼륨의 파일(있으면)과 리소스를 지운다. 실패한 생성·가져오기를 정리할 때도
/// 쓴다. 준비되지 않은 볼륨은 API가 붙이지 못하게 하지만, 확인과 생성 사이에 붙었을 수도 있어
/// 여기서 다시 본다.
async fn remove_volume(ctx: &Ctx, id: Uuid) -> Result<(), RemoveError> {
    let Some(volume) = ctx
        .cluster
        .get::<VolumeSpec>(id)
        .map_err(|e| e.to_string())?
    else {
        return Ok(());
    };
    let (_, vms) = ctx.cluster.list::<VmSpec>().map_err(|e| e.to_string())?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(format!("volume is attached to VM {:?}", vm.name).into());
    }
    if let Some(pool) = ctx
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)
        .map_err(|e| e.to_string())?
    {
        match ctx
            .storage
            .delete_volume(&pool, id, volume.spec.format)
            .await
        {
            Ok(()) | Err(DriverError::VolumeNotFound(_)) => {}
            Err(e) => return Err(RemoveError::Storage(e)),
        }
    }
    match ctx.cluster.delete::<VolumeSpec>(id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => Ok(()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// 이전 opld가 남긴 고아를 지운다: 이 노드가 만들던, 실행할 Task가 없는 Pending 볼륨(볼륨을 만든
/// 뒤 Task를 만들기 전에, 또는 업로드 도중 opld가 죽었다)과 Pending 스냅샷, 가져오기 Task가 없는
/// 올린 파일, VM 가져오기의 남은 파일([`crate::vm_imports::sweep`]).
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 다른 노드가 만드는 볼륨은 그 노드가 정리한다.
/// 풀이 아직 마운트되지 않아(opld가 막 시작했다) 지우지 못한 볼륨을 돌려준다.
async fn sweep_orphans(ctx: &Ctx, revision: u64, tasks: &[Task]) -> Vec<Uuid> {
    let mut unmounted = Vec::new();
    let unfinished: Vec<&Task> = tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .collect();
    match ctx.cluster.list::<VolumeSpec>() {
        Ok((_, volumes)) => {
            for volume in volumes {
                // Task 목록보다 나중에 만든 볼륨은 그 목록으로 판단할 수 없다(다른 노드의 API가
                // 방금 만들었을 수 있다).
                if volume.revision > revision
                    || volume.status.node.is_some_and(|n| n != ctx.config.node)
                {
                    continue;
                }
                let has_task = unfinished.iter().any(|t| t.spec.target == volume.id);
                if volume.status.phase == VolumePhase::Pending && !has_task {
                    tracing::warn!(volume = %volume.name, "removing a volume left unfinished");
                    match remove_volume(ctx, volume.id).await {
                        Ok(()) => {}
                        Err(
                            e @ RemoveError::Storage(
                                DriverError::PoolNotMounted(_) | DriverError::PoolNotResponding(_),
                            ),
                        ) => {
                            tracing::info!(volume = %volume.name, error = %e, "will remove the volume when its pool is mounted");
                            unmounted.push(volume.id);
                        }
                        Err(e) => {
                            tracing::warn!(volume = %volume.name, error = %e, "cannot remove volume")
                        }
                    }
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list volumes"),
    }
    match ctx.cluster.list::<SnapshotSpec>() {
        Ok((_, snapshots)) => {
            for s in snapshots {
                if s.revision > revision || s.status.phase != SnapshotPhase::Pending {
                    continue;
                }
                let creates = |t: &&Task| matches!(t.spec.op, TaskOp::SnapshotCreate { snapshot } if snapshot == s.id);
                if unfinished.iter().any(creates) {
                    continue;
                }
                // 만들던 노드: 끝난 Task가 있으면 그 노드, 없으면(Task를 만들기 전에 API가 죽었다)
                // API가 Task를 배정했을 노드(`vm_placement`, 공유 풀이면 노드마다 같은 답이 나오게
                // 이 노드를 앞세우지 않는다). 그것도 없으면 디스크가 없는 VM이라 태그가 없으니 어느
                // 노드가 지워도 된다.
                let vm = ctx.cluster.get::<VmSpec>(s.spec.vm).ok().flatten();
                let owner = tasks
                    .iter()
                    .filter(creates)
                    .map(|t| t.spec.node)
                    .next_back()
                    .or_else(|| vm.and_then(|vm| vm_placement(&ctx.cluster, None, &vm)));
                if owner.is_some_and(|n| n != ctx.config.node) {
                    continue;
                }
                tracing::warn!(snapshot = %s.id, vm = %s.spec.vm, "removing a snapshot left unfinished");
                if let Err(e) = snapshot::discard(ctx, &s).await {
                    tracing::warn!(snapshot = %s.id, error = %e, "cannot remove snapshot");
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot list snapshots"),
    }
    let Some(dir) = &ctx.config.upload_dir else {
        return unmounted;
    };
    crate::vm_imports::sweep(&ctx.cluster, ctx.config.node, dir, revision, tasks).await;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return unmounted;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // 하위 디렉터리(`imports`)는 가져오기의 정리가 맡는다.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let wanted = unfinished
            .iter()
            .any(|t| matches!(&t.spec.op, TaskOp::VolumeImport { source, .. } if *source == path));
        if !wanted {
            tracing::warn!(path = %path.display(), "removing an upload without a task");
            let _ = std::fs::remove_file(&path);
        }
    }
    unmounted
}
```

`crates/opl-api/src/updates.rs` 전체:

```rust
//! 노드의 APT 업데이트: opLite 저장소 소스 파일 관리, 업데이트 확인(`apt-get -s dist-upgrade`),
//! 주기 확인 에이전트, 다시 확인 API(`POST /nodes/{key}/updates/refresh`), 업그레이드(`POST
//! /nodes/{key}/upgrade`, `systemd-run`의 일시 유닛에서 `apt-get dist-upgrade`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use opl_cluster::types::Rejection;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::{Node, NodeSpec, NodeStatus, NodeUpdates, PackageUpdate};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::settings::{Channel, SettingsSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::version::{BUILD, SUITE};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, node_name, now};
use crate::error::{ApiError, Json};
use crate::resources::lookup;
use crate::state::AppState;
use crate::tasks::create_task;

/// opld가 쓴 소스 파일의 첫 줄. 이 줄이 없는 파일은 관리자가 직접 관리하는 것으로 보고 건드리지 않는다.
pub const SOURCES_MARK: &str = "# managed by opld; edits are overwritten";
/// 패키지가 싣는 저장소 공개 키(`UpdatesConfig.keyring`의 기본값)
pub const KEYRING: &str = "/usr/share/keyrings/oplite-archive-keyring.gpg";
/// `NodeStatus.updates`에 담는 패키지 수의 상한
pub const MAX_PACKAGES: usize = 500;
/// 다른 확인이 도는 중일 때의 오류
const CHECK_BUSY: &str = "another update check is running";
/// 확인 명령이 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 60초까지 기다리게 하는 apt 설정
const CHECK_LOCK_TIMEOUT: &str = "DPkg::Lock::Timeout=60";
/// 오류에 담는 명령 stderr의 끝 줄 수
const ERROR_LINES: usize = 5;
/// 설정 구독을 하지 못했을 때 다시 해 보는 간격
const RESUBSCRIBE: Duration = Duration::from_secs(10);
/// 업그레이드 셸이 로그 끝에 남기는 종료 상태 줄의 앞부분. `--collect`로 유닛이 사라진 뒤에도 결과를 안다.
const EXIT_MARK: &str = "opl-upgrade: exit ";
/// 업그레이드 셸이 맨 처음 로그에 남기는 줄. 없으면 유닛이 셸을 시작하지 못했다.
const START_MARK: &str = "opl-upgrade: start";
/// apt가 다른 apt(무인 업그레이드 등)의 dpkg 잠금을 기다리는 시간(초)
const LOCK_TIMEOUT: u32 = 600;
/// 실패한 업그레이드의 오류에 담는 로그 끝 줄 수
const LOG_LINES: usize = 20;

/// 노드 업데이트 설정(opld `[updates]`를 푼 값). 업데이트가 꺼진 노드에는 없다.
#[derive(Debug, Clone)]
pub struct UpdatesConfig {
    /// 주기 확인 간격
    pub check_interval: Duration,
    /// 시작한 뒤 첫 확인까지 기다리는 시간
    pub first_check: Duration,
    /// opLite APT 저장소 주소. 비면 소스 파일을 쓰지 않는다.
    pub apt_url: String,
    /// opld가 관리하는 deb822 소스 파일
    pub sources_file: PathBuf,
    /// 저장소 공개 키. 없으면 소스 파일을 쓰지 않는다(apt가 서명을 확인하지 못한다).
    pub keyring: PathBuf,
    pub apt_get: PathBuf,
    pub systemd_run: PathBuf,
    pub systemctl: PathBuf,
    pub dpkg: PathBuf,
    /// 업그레이드 로그를 두는 디렉터리
    pub log_dir: PathBuf,
    /// 업그레이드 유닛 상태를 다시 보는 간격
    pub poll: Duration,
}

/// 이 노드의 업데이트 확인. 확인은 한 번에 하나만 돈다.
#[derive(Debug)]
pub struct Updates {
    pub config: UpdatesConfig,
    lock: tokio::sync::Mutex<()>,
}

impl Updates {
    pub fn new(config: UpdatesConfig) -> Self {
        Self {
            config,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// `apt-get update -q` 뒤 `apt-get -s -q dist-upgrade`의 `Inst` 줄로 올릴 패키지를 찾는다. 명령이
    /// 실패하거나 다른 확인이 도는 중이면(기다리지 않는다) `error`에 이유를 담는다.
    pub async fn check(&self) -> NodeUpdates {
        let Ok(_guard) = self.lock.try_lock() else {
            return failed(CHECK_BUSY.into());
        };
        self.check_locked().await
    }

    /// 다른 확인이 끝나기를 기다렸다가 확인한다. 잠금 줄에 서지 않고 `poll`마다 다시 해 본다. 잠금이
    /// 잡혀 있고 `give_up()`이면(업그레이드가 잠금을 잡았다) 확인하지 않고 `None`이다.
    async fn check_waiting(&self, give_up: impl Fn() -> bool) -> Option<NodeUpdates> {
        loop {
            if let Ok(_guard) = self.lock.try_lock() {
                return Some(self.check_locked().await);
            }
            if give_up() {
                return None;
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    async fn check_locked(&self) -> NodeUpdates {
        if let Err(e) = self.apt(&["update", "-q", "-o", CHECK_LOCK_TIMEOUT]).await {
            return failed(e);
        }
        match self
            .apt(&["-s", "-q", "-o", CHECK_LOCK_TIMEOUT, "dist-upgrade"])
            .await
        {
            Ok(out) => found(parse_simulate(&out), now()),
            Err(e) => failed(e),
        }
    }

    /// 확인해 결과를 노드 `node`의 `status.updates`에 쓴다. 확인이 실패했으면(결과를 쓴 뒤) 그 이유가
    /// 오류다. 다른 확인이 도는 중이면 쓰지 않는다(그 확인이 쓴다).
    pub(crate) async fn refresh(&self, cluster: &Cluster, node: Uuid) -> Result<(), String> {
        let updates = self.check().await;
        if updates.error.as_deref() == Some(CHECK_BUSY) {
            return Err(CHECK_BUSY.into());
        }
        self.record_check(cluster, node, updates).await
    }

    /// [`Self::refresh`]와 같지만 다른 확인이 도는 중이면 끝나기를 기다린다(사용자가 요청한 확인). 이
    /// 노드를 업그레이드하는 중이면 확인하지 않고 성공이다(업그레이드가 끝나면 확인한다).
    pub(crate) async fn refresh_waiting(
        &self,
        cluster: &Cluster,
        node: Uuid,
    ) -> Result<(), String> {
        match self.check_waiting(|| upgrading(cluster, node)).await {
            Some(updates) => self.record_check(cluster, node, updates).await,
            None => Ok(()),
        }
    }

    async fn record_check(
        &self,
        cluster: &Cluster,
        node: Uuid,
        updates: NodeUpdates,
    ) -> Result<(), String> {
        record(cluster, node, &updates)
            .await
            .map_err(|e| format!("cannot record the update check: {e}"))?;
        match updates.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// `apt-get <args>`(LANG=C)의 표준 출력. 실패하면 명령과 stderr 끝 줄이 오류다.
    async fn apt(&self, args: &[&str]) -> Result<String, String> {
        let apt_get = &self.config.apt_get;
        let output = tokio::process::Command::new(apt_get)
            .args(args)
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            // 에이전트가 멈추면(opld 종료) 확인도 끝낸다.
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", apt_get.display()))?;
        if !output.status.success() {
            return Err(format!(
                "apt-get {} failed ({}): {}",
                args.join(" "),
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// 업그레이드 Task `task`를 실행한다: 일시 유닛에서 apt를 돌리고(`resume`이면 이전 opld가 띄운
    /// 유닛을 이어서 본다) 유닛이 끝날 때까지 지켜본다. 성공이든 실패든 끝나면 업데이트를 다시 확인한다.
    /// 클러스터 어디든 다른 업그레이드가 돌고 있으면(두 요청이 함께 지나갔다) 시작하지 않는다.
    /// 업그레이드하는 동안 확인 잠금을 잡아 apt 잠금을 다투지 않고, 잠금을 잡은 채로 마지막 확인을 한다.
    pub(crate) async fn upgrade(
        &self,
        cluster: &Cluster,
        node: Uuid,
        task: Uuid,
        resume: bool,
    ) -> Result<(), String> {
        if !resume && let Some(other) = other_upgrade(cluster, task) {
            return Err(format!(
                "upgrade_in_progress: node {other:?} is being upgraded"
            ));
        }
        let (result, updates) = {
            let _guard = self.lock.lock().await;
            let result = self.run_upgrade(task, resume).await;
            (result, self.check_locked().await)
        };
        if let Err(e) = self.record_check(cluster, node, updates).await {
            tracing::warn!(error = %e, "update check after the upgrade failed");
        }
        result
    }

    async fn run_upgrade(&self, task: Uuid, resume: bool) -> Result<(), String> {
        let unit = upgrade_unit(task);
        let log = self.config.log_dir.join(format!("upgrade-{task}.log"));
        // 이 opld가 유닛을 띄웠는지. 이어 볼 때는 이전 opld가 띄우지 못했을 수 있다.
        let mut started = !resume;
        if started {
            self.start_unit(&unit, &log).await?;
        }
        loop {
            match self.unit_state(&unit).await? {
                Some(UnitState::Running) | None => {}
                Some(UnitState::Succeeded) => return Ok(()),
                Some(UnitState::Failed) => return Err(upgrade_failed(&log).await),
                Some(UnitState::Gone) => {
                    let text = read_log(&log).await;
                    if text.lines().any(|l| l == START_MARK) {
                        return self.judge_gone(&log, &text).await;
                    }
                    if started {
                        return Err("the upgrade did not start; try again".into());
                    }
                    // 이전 opld가 유닛을 띄우기 전에 멈췄다. apt는 다시 돌려도 된다.
                    self.start_unit(&unit, &log).await?;
                    started = true;
                    continue;
                }
            }
            tokio::time::sleep(self.config.poll).await;
        }
    }

    /// `systemd-run`으로 apt를 opld 밖의 일시 유닛에서 띄운다. 패키지의 postinst가 opld를 다시
    /// 시작해도 apt는 끊기지 않는다. 출력은 로그 파일에 더한다.
    async fn start_unit(&self, unit: &str, log: &Path) -> Result<(), String> {
        let c = &self.config;
        tokio::fs::create_dir_all(&c.log_dir)
            .await
            .map_err(|e| format!("cannot create {}: {e}", c.log_dir.display()))?;
        let output = tokio::process::Command::new(&c.systemd_run)
            .arg(format!("--unit={unit}"))
            .arg("--collect")
            .arg("--property=Type=exec")
            .arg(format!(
                "--property=StandardOutput=append:{}",
                log.display()
            ))
            .arg(format!("--property=StandardError=append:{}", log.display()))
            .arg("--setenv=DEBIAN_FRONTEND=noninteractive")
            .arg("--setenv=LANG=C")
            .args(["/bin/sh", "-c", &upgrade_script(&c.apt_get)])
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", c.systemd_run.display()))?;
        if !output.status.success() {
            return Err(format!(
                "systemd-run failed ({}): {}",
                output.status,
                tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES)
            ));
        }
        Ok(())
    }

    /// `systemctl show`로 읽은 유닛 상태. `systemctl`이 실패하면(systemd 자신을 올리는 중 등) `None`이다
    /// (다음에 다시 본다).
    async fn unit_state(&self, unit: &str) -> Result<Option<UnitState>, String> {
        let systemctl = &self.config.systemctl;
        let output = tokio::process::Command::new(systemctl)
            .args([
                "show",
                "-p",
                "ActiveState",
                "-p",
                "Result",
                "-p",
                "LoadState",
            ])
            .arg(format!("{unit}.service"))
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", systemctl.display()))?;
        if !output.status.success() {
            tracing::warn!(
                unit,
                status = %output.status,
                stderr = %tail(&String::from_utf8_lossy(&output.stderr), ERROR_LINES),
                "cannot read the upgrade unit; will retry"
            );
            return Ok(None);
        }
        Ok(Some(unit_state(&String::from_utf8_lossy(&output.stdout))))
    }

    /// 셸을 시작한 유닛이 사라졌다(`--collect`가 치웠다). 로그 `text`의 종료 상태 줄이 있으면 그것으로
    /// 판단한다. 없으면 apt가 도중에 끊겼다(전원이 꺼진 경우 등): `dpkg --audit`가 무언가 알리면 dpkg를
    /// 마저 설정하라고, 아니면 다시 시작하라고 실패한다.
    async fn judge_gone(&self, log: &Path, text: &str) -> Result<(), String> {
        match exit_status(text) {
            Some(0) => return Ok(()),
            Some(_) => return Err(upgrade_failed(log).await),
            None => {}
        }
        let dpkg = &self.config.dpkg;
        let output = tokio::process::Command::new(dpkg)
            .arg("--audit")
            .env("LANG", "C")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("cannot run {}: {e}", dpkg.display()))?;
        if output.status.success() && output.stdout.iter().all(u8::is_ascii_whitespace) {
            Err("the upgrade was interrupted; start it again".into())
        } else {
            Err("the upgrade was interrupted; run \"dpkg --configure -a\" on the node".into())
        }
    }

    /// 소스 파일을 지금 설정의 채널로 맞춘다(`apt_url`이 있을 때만).
    /// 파일을 새로 썼으면 true다.
    fn sync_sources(&self, cluster: &Cluster) -> bool {
        if self.config.apt_url.is_empty() {
            return false;
        }
        let channel = match crate::settings::current(cluster) {
            Ok(settings) => settings.spec.updates.channel,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the settings for the APT sources");
                return false;
            }
        };
        let c = &self.config;
        let path = &c.sources_file;
        match write_sources(path, &c.apt_url, channel, &c.keyring) {
            Ok(SourcesOutcome::Written) => {
                tracing::info!(file = %path.display(), channel = channel.as_str(), "wrote the APT sources");
                true
            }
            Ok(
                SourcesOutcome::Unchanged | SourcesOutcome::Unmanaged | SourcesOutcome::NoKeyring,
            ) => false,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "cannot write the APT sources");
                false
            }
        }
    }
}

fn failed(error: String) -> NodeUpdates {
    NodeUpdates {
        checked_at: Some(now()),
        error: Some(error),
        ..NodeUpdates::default()
    }
}

/// 찾은 패키지의 확인 결과. [`MAX_PACKAGES`]가 넘으면 앞의 것만 담는다.
fn found(mut packages: Vec<PackageUpdate>, checked_at: u64) -> NodeUpdates {
    let truncated = packages.len() > MAX_PACKAGES;
    packages.truncate(MAX_PACKAGES);
    NodeUpdates {
        checked_at: Some(checked_at),
        packages,
        truncated,
        error: None,
    }
}

/// 비지 않은 마지막 `n`줄
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// 업그레이드 Task `task`의 일시 유닛 이름(Task id 전체). opld가 다시 시작해도 Task id로 다시 만든다.
fn upgrade_unit(task: Uuid) -> String {
    format!("opl-upgrade-{}", task.simple())
}

/// 업그레이드 유닛이 돌리는 셸 명령. 처음에 시작 줄([`START_MARK`]), 끝에 종료 상태 줄([`EXIT_MARK`])을
/// 로그에 남긴다. apt는 다른 apt의 잠금을 [`LOCK_TIMEOUT`]초까지 기다린다.
fn upgrade_script(apt_get: &Path) -> String {
    let apt = sh_quote(&apt_get.display().to_string());
    let lock = format!("-o DPkg::Lock::Timeout={LOCK_TIMEOUT}");
    format!(
        "echo \"{START_MARK}\"; {apt} update -q {lock} && {apt} -y -q {lock} \
         -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
         s=$?; echo \"{EXIT_MARK}$s\"; exit $s"
    )
}

/// 셸의 작은따옴표 인용
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 업그레이드 유닛의 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitState {
    Running,
    Succeeded,
    Failed,
    /// 유닛이 없다(끝나고 치워졌거나 노드가 다시 켜졌다).
    Gone,
}

/// `systemctl show -p ActiveState -p Result -p LoadState`의 `KEY=VALUE` 줄
fn unit_state(show: &str) -> UnitState {
    let value = |key: &str| {
        show.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("")
    };
    let result = value("Result");
    match value("ActiveState") {
        _ if value("LoadState") == "not-found" => UnitState::Gone,
        "failed" => UnitState::Failed,
        "inactive" if result.is_empty() => UnitState::Gone,
        "inactive" if result == "success" => UnitState::Succeeded,
        "inactive" => UnitState::Failed,
        _ => UnitState::Running,
    }
}

/// 로그 끝의 종료 상태 줄([`EXIT_MARK`])에 적힌 값
fn exit_status(log: &str) -> Option<i32> {
    log.lines()
        .rev()
        .find_map(|l| l.strip_prefix(EXIT_MARK))?
        .trim()
        .parse()
        .ok()
}

/// 업그레이드 로그(없으면 빈 문자열)
async fn read_log(log: &Path) -> String {
    tokio::fs::read(log)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// 실패한 업그레이드의 오류: 로그 끝 줄
async fn upgrade_failed(log: &Path) -> String {
    format!(
        "the upgrade failed; last log lines:\n{}",
        tail(&read_log(log).await, LOG_LINES)
    )
}

/// `apt-get -s dist-upgrade` 출력의 `Inst <pkg> [<cur>] (<new> <suite…> [<arch>])` 줄. `[<cur>]`가
/// 없으면 새로 설치할 패키지다. `Conf`·`Remv`와 모양이 다른 줄은 건너뛴다.
pub(crate) fn parse_simulate(text: &str) -> Vec<PackageUpdate> {
    text.lines().filter_map(parse_inst).collect()
}

fn parse_inst(line: &str) -> Option<PackageUpdate> {
    let rest = line.strip_prefix("Inst ")?;
    let (name, rest) = rest.split_once(' ')?;
    let mut rest = rest.trim_start();
    let mut current = None;
    if let Some(r) = rest.strip_prefix('[') {
        let (cur, r) = r.split_once(']')?;
        current = Some(cur.to_string());
        rest = r.trim_start();
    }
    let candidate = rest.strip_prefix('(')?.split_whitespace().next()?;
    Some(PackageUpdate {
        name: name.to_string(),
        current,
        candidate: candidate.trim_end_matches(')').to_string(),
    })
}

/// opLite 저장소의 deb822 소스 파일 내용
pub(crate) fn sources_contents(url: &str, channel: Channel, keyring: &Path) -> String {
    format!(
        "{SOURCES_MARK}\nTypes: deb\nURIs: {url}\nSuites: {SUITE}\nComponents: {}\nSigned-By: {}\n",
        channel.as_str(),
        keyring.display()
    )
}

/// [`write_sources`]의 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourcesOutcome {
    Written,
    /// 이미 같은 내용이다.
    Unchanged,
    /// 관리 표시가 없는 파일이다(관리자가 직접 관리한다). 건드리지 않았다.
    Unmanaged,
    /// 저장소 공개 키가 없다. 쓰지 않았고 opld가 쓴 파일이 있으면 지웠다.
    NoKeyring,
}

/// 소스 파일을 쓴다. 파일이 있는데 첫 줄이 [`SOURCES_MARK`]가 아니면(비었거나 공백뿐이면 opld 것으로
/// 본다) 건드리지 않고 경고한다. 공개 키 `keyring`이 없으면 apt가 서명을 확인하지 못해 `apt-get
/// update`가 실패하므로 쓰지 않고, opld가 쓴 파일은 지운다. 임시 파일(apt가 무시하는 `.dpkg-tmp`)에
/// 쓰고 디스크에 내린 뒤 바꿔 넣어 apt가 반쯤 쓴 파일을 읽지 않게 한다.
pub(crate) fn write_sources(
    path: &Path,
    url: &str,
    channel: Channel,
    keyring: &Path,
) -> std::io::Result<SourcesOutcome> {
    let old = match std::fs::read_to_string(path) {
        Ok(old) => Some(old),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &old
        && !old.trim().is_empty()
        && old.lines().next() != Some(SOURCES_MARK)
    {
        tracing::warn!(
            file = %path.display(),
            "the APT sources file has no opld mark; leaving it as it is"
        );
        return Ok(SourcesOutcome::Unmanaged);
    }
    if !keyring.exists() {
        tracing::warn!(
            keyring = %keyring.display(),
            "the opLite archive key is missing; not writing the APT sources"
        );
        if old.is_some() {
            std::fs::remove_file(path)?;
        }
        return Ok(SourcesOutcome::NoKeyring);
    }
    let contents = sources_contents(url, channel, keyring);
    if old.as_deref() == Some(contents.as_str()) {
        return Ok(SourcesOutcome::Unchanged);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".dpkg-tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&tmp)?;
        // 남은 임시 파일이나 umask와 상관없이 누구나 읽게 한다.
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    // 바꿔 넣은 것도 디스크에 내린다. 실패해도 파일은 이미 바뀌었다.
    if let Some(dir) = path.parent()
        && let Ok(dir) = std::fs::File::open(dir)
    {
        let _ = dir.sync_all();
    }
    Ok(SourcesOutcome::Written)
}

/// 노드 `node`의 `status.updates`와 이 노드의 버전을 쓴다.
async fn record(cluster: &Cluster, node: Uuid, updates: &NodeUpdates) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        status.updates = Some(updates.clone());
        status.version = Some(BUILD.into());
        true
    })
    .await
}

/// 이 노드 status의 `version`과 `updates`가 비었으면(이 필드를 모르는 옛 opld가 리더일 때 status를
/// 다시 쓰며 지웠다) 다시 쓴다. 확인 결과는 남긴다.
async fn restore(cluster: &Cluster, node: Uuid) -> Result<(), ClusterError> {
    change_status(cluster, node, |status| {
        let stale = status.version.as_deref() != Some(BUILD) || status.updates.is_none();
        status.version = Some(BUILD.into());
        status.updates.get_or_insert_with(NodeUpdates::default);
        stale
    })
    .await
}

/// 노드 `node`의 status를 `change`로 고쳐 쓴다(false면 쓰지 않는다). status의 다른 필드는 opld도
/// 쓰므로 revision을 확인하며 쓰고, 그 사이 누가 썼으면 다시 읽어 쓴다.
async fn change_status(
    cluster: &Cluster,
    node: Uuid,
    mut change: impl FnMut(&mut NodeStatus) -> bool,
) -> Result<(), ClusterError> {
    loop {
        let Some(current) = cluster.get::<NodeSpec>(node)? else {
            return Ok(());
        };
        let mut status = current.status;
        if !change(&mut status) {
            return Ok(());
        }
        match cluster
            .update_status_checked::<NodeSpec>(node, current.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 소스 파일을 시작할 때와 설정이 바뀔 때 맞추고, `first_check` 뒤 처음, 그 뒤 `check_interval`마다
/// 업데이트를 확인하는 에이전트. 업데이트가 켜진 노드에서 돈다.
pub struct UpdateAgent {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl UpdateAgent {
    pub fn spawn(cluster: Arc<Cluster>, node: Uuid, updates: Arc<Updates>) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run_agent(cluster, node, updates, rx)),
        }
    }

    /// 멈춘다. 도는 확인은 기다리지 않는다(apt 명령은 혼자 끝난다).
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run_agent(
    cluster: Arc<Cluster>,
    node: Uuid,
    updates: Arc<Updates>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut next = Instant::now() + updates.config.first_check;
    let mut watch = None;
    let mut sync = true;
    let mut started = false;
    if let Err(e) = restore(&cluster, node).await {
        tracing::warn!(error = %e, "cannot restore the node status");
    }
    loop {
        if sync {
            // 이번에 읽을 설정 뒤의 변경을 놓치지 않게 먼저 구독한다.
            if watch.is_none() {
                watch = cluster
                    .list::<SettingsSpec>()
                    .ok()
                    .and_then(|(revision, _)| cluster.watch(revision).ok());
            }
            // 채널을 바꿔 소스 파일을 새로 썼으면 곧 확인해 새 채널의 업데이트를 보인다.
            if updates.sync_sources(&cluster) && started {
                next = Instant::now();
            }
            sync = false;
            started = true;
        }
        tokio::select! {
            _ = &mut shutdown => return,
            () = tokio::time::sleep_until(next) => {
                // 업그레이드 중에는 apt 잠금을 다투지 않는다. 업그레이드가 끝나면 스스로 확인한다.
                if upgrading(&cluster, node) {
                    tracing::debug!("skipping the update check during an upgrade");
                } else {
                    tokio::select! {
                        _ = &mut shutdown => return,
                        result = updates.refresh(&cluster, node) => {
                            if let Err(e) = result {
                                tracing::warn!(error = %e, "update check failed");
                            }
                        }
                    }
                }
                next = Instant::now() + updates.config.check_interval;
            }
            () = settings_changed(&mut watch) => sync = true,
        }
    }
}

/// 노드 `node`에서 업그레이드 Task가 끝나지 않았는지. 목록을 읽지 못하면 그렇다고 본다.
fn upgrading(cluster: &Cluster, node: Uuid) -> bool {
    let Ok((_, tasks)) = cluster.list::<TaskSpec>() else {
        return true;
    };
    tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeUpgrade && t.spec.node == node && !t.status.state.is_finished()
    })
}

/// 업그레이드 Task `task` 말고 running인 업그레이드가 있으면 그 노드 이름(없는 노드면 id). 두 요청이
/// 함께 지나가 두 Task가 생겨도 running 쓰기는 Raft 순서대로 적용되므로 나중 것이 앞의 것을 본다.
fn other_upgrade(cluster: &Cluster, task: Uuid) -> Option<String> {
    let (_, tasks) = cluster.list::<TaskSpec>().ok()?;
    let other = tasks.into_iter().find(|t| {
        t.id != task && t.spec.op == TaskOp::NodeUpgrade && t.status.state == TaskState::Running
    })?;
    Some(match cluster.get::<NodeSpec>(other.spec.node) {
        Ok(Some(node)) => node.name,
        _ => other.spec.node.to_string(),
    })
}

/// 설정이 바뀔 때까지 기다린다. 구독이 끊기면(뒤처짐, 스냅샷 설치) 구독을 버리고 돌아가 다시
/// 맞추게 한다. 구독이 없으면 잠시 뒤 돌아가 다시 구독하게 한다.
async fn settings_changed(watch: &mut Option<Watch>) {
    let Some(w) = watch else {
        return tokio::time::sleep(RESUBSCRIBE).await;
    };
    loop {
        match w.next().await {
            Ok(
                WatchEvent::Put {
                    kind: Kind::Settings,
                    ..
                }
                | WatchEvent::Deleted {
                    kind: Kind::Settings,
                    ..
                },
            ) => return,
            Ok(WatchEvent::Reset { .. }) | Err(_) => {
                *watch = None;
                return;
            }
            Ok(_) => {}
        }
    }
}

/// 업데이트를 확인하지 않는 노드(위트니스, 업데이트가 꺼진 노드, 옛 노드)면 400 `updates_unsupported`
pub(crate) fn check_updates_supported(node: &Node) -> Result<(), ApiError> {
    if node.spec.witness || node.status.updates.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "updates_unsupported",
            format!("node {:?} does not check for updates", node.name),
        ));
    }
    Ok(())
}

/// 노드의 업데이트를 다시 확인할 Task(`node-update-check`, 그 노드에서 돈다)를 돌려준다(202). 결과는
/// Node `status.updates`에 쓴다. 그 노드에 끝나지 않은 확인 Task가 있으면 새로 만들지 않고 그것을
/// 돌려준다. 업데이트를 확인하지 않는 노드면 400 `updates_unsupported`, 오프라인이면 409 `node_offline`,
/// 옛 노드가 남았으면 409 `cluster_upgrading`(옛 노드는 이 Task 종류를 풀지 못한다), 그 노드를
/// 업그레이드하는 중이면 409 `upgrade_in_progress`.
#[utoipa::path(post, path = "/nodes/{key}/updates/refresh", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_updates(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    // 업그레이드가 apt 잠금을 잡고 있다. 끝나면 실행기가 스스로 확인한다.
    if tasks.iter().any(|t| {
        t.spec.op == TaskOp::NodeUpgrade && t.spec.node == node.id && !t.status.state.is_finished()
    }) {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            format!(
                "node {:?} is being upgraded; it checks for updates when the upgrade ends",
                node.name
            ),
        ));
    }
    if let Some(task) = tasks.into_iter().find(|t| {
        t.spec.op == TaskOp::NodeUpdateCheck
            && t.spec.node == node.id
            && !t.status.state.is_finished()
    }) {
        return Ok((StatusCode::ACCEPTED, Json(task)));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpdateCheck, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 노드의 패키지를 올릴 Task(`node-upgrade`, 그 노드에서 돈다)를 돌려준다(202). apt는 opld 밖의 일시
/// 유닛에서 돌므로 VM은 계속 돌고 opld만 잠깐 다시 시작한다. 업데이트를 확인하지 않는 노드면 400
/// `updates_unsupported`, 오프라인이면 409 `node_offline`, 클러스터 어디든 끝나지 않은 업그레이드가
/// 있으면 409 `upgrade_in_progress`(한 번에 한 노드 — 나머지 노드가 과반수와 서비스를 지킨다), 옛 노드가
/// 남았으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/nodes/{key}/upgrade", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn upgrade_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    UrlPath(key): UrlPath<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    check_updates_supported(&node)?;
    check_online(&state, node.id)?;
    crate::cluster::require_upgraded(&state.cluster)?;
    // 이 프로세스에 함께 온 요청이 둘 다 검사를 지나 업그레이드 둘을 만들지 않게 한다. 다른 노드로 온
    // 요청과의 경합은 실행기가 막는다(`other_upgrade`).
    static REQUESTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REQUESTS.lock().await;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if let Some(task) = tasks
        .iter()
        .find(|t| t.spec.op == TaskOp::NodeUpgrade && !t.status.state.is_finished())
    {
        return Err(ApiError::conflict(
            "upgrade_in_progress",
            format!(
                "node {:?} is still being upgraded; upgrade one node at a time",
                node_name(&state, task.spec.node)?
            ),
        ));
    }
    let task = create_task(&state.cluster, TaskOp::NodeUpgrade, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debian 13에서 `LANG=C apt-get -s -q dist-upgrade`가 내는 모양(보안 업데이트, 새 커널 의존성,
    /// 지울 패키지, opLite 저장소 패키지가 섞임)
    const SIMULATE: &str = "\
Reading package lists...
Building dependency tree...
Reading state information...
Calculating upgrade...
The following packages will be REMOVED:
  libfoo1
The following NEW packages will be installed:
  linux-image-6.12.48+deb13-amd64
The following packages will be upgraded:
  base-files libc-bin libc6 linux-image-amd64 oplite
5 upgraded, 1 newly installed, 1 to remove and 0 not upgraded.
Remv libfoo1 [1.2-3]
Inst base-files [13.8+deb13u1] (13.8+deb13u2 Debian:13.2/stable [amd64])
Conf base-files (13.8+deb13u2 Debian:13.2/stable [amd64])
Inst libc6 [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64]) [libc-bin:amd64 ]
Inst libc-bin [2.41-12] (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc6 (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Conf libc-bin (2.41-12+deb13u1 Debian:13.2/stable, Debian-Security:13/stable-security [amd64])
Inst linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Inst linux-image-amd64 [6.12.41-1] (6.12.48-1 Debian:13.2/stable [amd64])
Inst oplite [0.2.0] (1:0.2.1~rc1 opLite:trixie [amd64])
Conf linux-image-6.12.48+deb13-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf linux-image-amd64 (6.12.48-1 Debian:13.2/stable [amd64])
Conf oplite (1:0.2.1~rc1 opLite:trixie [amd64])
";

    fn pkg(name: &str, current: Option<&str>, candidate: &str) -> PackageUpdate {
        PackageUpdate {
            name: name.into(),
            current: current.map(Into::into),
            candidate: candidate.into(),
        }
    }

    #[test]
    fn parse_simulate_reads_inst_lines() {
        assert_eq!(
            parse_simulate(SIMULATE),
            [
                pkg("base-files", Some("13.8+deb13u1"), "13.8+deb13u2"),
                pkg("libc6", Some("2.41-12"), "2.41-12+deb13u1"),
                pkg("libc-bin", Some("2.41-12"), "2.41-12+deb13u1"),
                // 새 커널 의존성: 설치된 버전이 없다.
                pkg("linux-image-6.12.48+deb13-amd64", None, "6.12.48-1"),
                pkg("linux-image-amd64", Some("6.12.41-1"), "6.12.48-1"),
                pkg("oplite", Some("0.2.0"), "1:0.2.1~rc1"),
            ]
        );
        assert!(parse_simulate("").is_empty());
        // 모양이 다른 줄은 건너뛴다.
        assert!(parse_simulate("Inst\nInst x [1.0\nInst y [1.0]\nInst z").is_empty());
    }

    #[test]
    fn found_keeps_at_most_max_packages() {
        let many: String = (0..600)
            .map(|i| format!("Inst p{i} [1.0] (1.1 Debian:13.2/stable [amd64])\n"))
            .collect();
        let packages = parse_simulate(&many);
        assert_eq!(packages.len(), 600);
        let updates = found(packages, 7);
        assert_eq!(updates.checked_at, Some(7));
        assert_eq!(updates.packages.len(), MAX_PACKAGES);
        assert_eq!(updates.packages[0].name, "p0");
        assert!(updates.truncated);
        assert_eq!(updates.error, None);
        let few = found(parse_simulate(SIMULATE), 7);
        assert_eq!(few.packages.len(), 6);
        assert!(!few.truncated);
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\n\nc\nd\ne\nf\n", 5), "b\nc\nd\ne\nf");
        assert_eq!(tail("only\n", 5), "only");
        assert_eq!(tail("", 5), "");
    }

    fn missing_tools() -> Updates {
        let dir = PathBuf::from("/nonexistent/opl-updates");
        Updates::new(UpdatesConfig {
            check_interval: Duration::from_secs(3600),
            first_check: Duration::from_secs(3600),
            apt_url: String::new(),
            sources_file: dir.join("oplite.sources"),
            keyring: dir.join("keyring.gpg"),
            apt_get: dir.join("apt-get"),
            systemd_run: dir.join("systemd-run"),
            systemctl: dir.join("systemctl"),
            dpkg: dir.join("dpkg"),
            log_dir: dir.join("log"),
            poll: Duration::from_millis(20),
        })
    }

    #[tokio::test]
    async fn check_reports_a_missing_apt_get_and_does_not_wait_for_another_check() {
        let updates = missing_tools();
        let got = updates.check().await;
        assert_eq!(
            got.error.as_deref(),
            Some(
                "cannot run /nonexistent/opl-updates/apt-get: No such file or directory (os error 2)"
            )
        );
        assert!(got.checked_at.is_some());
        let _running = updates.lock.try_lock().unwrap();
        let got = updates.check().await;
        assert_eq!(got.error.as_deref(), Some(CHECK_BUSY));
        assert!(got.packages.is_empty());
    }

    /// 사용자가 요청한 확인은 도는 확인을 기다렸다가 확인한다.
    #[tokio::test]
    async fn a_waiting_check_waits_for_the_running_one() {
        let updates = missing_tools();
        let running = updates.lock.try_lock().unwrap();
        let waiting = updates.check_waiting(|| false);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        drop(running);
        let got = waiting.await.unwrap();
        assert!(got.error.unwrap().starts_with("cannot run "));
    }

    /// 업그레이드가 잠금을 잡았으면 기다리지 않고 확인하지 않는다.
    #[tokio::test]
    async fn a_waiting_check_gives_up_during_an_upgrade() {
        let updates = missing_tools();
        let _upgrading = updates.lock.try_lock().unwrap();
        assert!(updates.check_waiting(|| true).await.is_none());
    }

    #[test]
    fn unit_state_reads_systemctl_show() {
        let show = |active: &str, result: &str, load: &str| {
            unit_state(&format!(
                "ActiveState={active}\nResult={result}\nLoadState={load}\n"
            ))
        };
        assert_eq!(show("active", "success", "loaded"), UnitState::Running);
        assert_eq!(show("activating", "success", "loaded"), UnitState::Running);
        assert_eq!(
            show("deactivating", "success", "loaded"),
            UnitState::Running
        );
        assert_eq!(show("inactive", "success", "loaded"), UnitState::Succeeded);
        assert_eq!(show("failed", "exit-code", "loaded"), UnitState::Failed);
        assert_eq!(show("inactive", "exit-code", "loaded"), UnitState::Failed);
        assert_eq!(show("inactive", "", "loaded"), UnitState::Gone);
        assert_eq!(show("inactive", "success", "not-found"), UnitState::Gone);
        // 줄 순서는 상관없다(systemctl은 자기 순서로 낸다).
        assert_eq!(
            unit_state("LoadState=loaded\nResult=success\nActiveState=inactive\n"),
            UnitState::Succeeded
        );
        // 알 수 없는 상태는 계속 지켜본다.
        assert_eq!(unit_state(""), UnitState::Running);
        assert_eq!(show("reloading", "success", "loaded"), UnitState::Running);
    }

    #[test]
    fn exit_status_reads_the_last_mark() {
        assert_eq!(
            exit_status("Setting up oplite (0.2.1) ...\nopl-upgrade: exit 0\n"),
            Some(0)
        );
        assert_eq!(
            exit_status("opl-upgrade: exit 0\nopl-upgrade: exit 100\n"),
            Some(100)
        );
        assert_eq!(exit_status("Unpacking oplite (0.2.1) ...\n"), None);
        assert_eq!(exit_status(""), None);
    }

    #[test]
    fn upgrade_unit_and_script() {
        let id: Uuid = "0193a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b".parse().unwrap();
        assert_eq!(
            upgrade_unit(id),
            "opl-upgrade-0193a1b2c3d47e5f8a9b0c1d2e3f4a5b"
        );
        assert_eq!(
            upgrade_script(Path::new("/usr/bin/apt-get")),
            "echo \"opl-upgrade: start\"; \
             '/usr/bin/apt-get' update -q -o DPkg::Lock::Timeout=600 && \
             '/usr/bin/apt-get' -y -q -o DPkg::Lock::Timeout=600 \
             -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
             s=$?; echo \"opl-upgrade: exit $s\"; exit $s"
        );
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    /// 셸이 정말 돌면 로그는 시작 줄로 시작해 apt의 종료 상태 줄로 끝난다(따옴표가 든 경로도).
    #[test]
    fn upgrade_script_runs_in_sh() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("it's tools");
        std::fs::create_dir(&tools).unwrap();
        let apt_get = tools.join("apt-get");
        let run = |exit: u8| {
            std::fs::write(
                &apt_get,
                format!("#!/bin/sh\necho \"apt-get $*\"\nexit {exit}\n"),
            )
            .unwrap();
            let output = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    &format!("chmod 755 \"$1\" && {}", upgrade_script(&apt_get)),
                    "sh",
                ])
                .arg(&apt_get)
                .output()
                .unwrap();
            (
                output.status.code(),
                String::from_utf8(output.stdout).unwrap(),
            )
        };
        let (code, log) = run(0);
        assert_eq!(code, Some(0));
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines[0], START_MARK);
        assert_eq!(lines[1], "apt-get update -q -o DPkg::Lock::Timeout=600");
        assert!(lines[2].ends_with(" dist-upgrade"), "{log}");
        assert_eq!(lines[3], "opl-upgrade: exit 0");
        assert_eq!(exit_status(&log), Some(0));
        let (code, log) = run(100);
        assert_eq!(code, Some(100));
        assert_eq!(log.lines().last(), Some("opl-upgrade: exit 100"));
        assert_eq!(exit_status(&log), Some(100));
    }

    #[test]
    fn sources_are_deb822_with_the_mark() {
        assert_eq!(
            sources_contents(
                "https://apt.example.com/oplite",
                Channel::Testing,
                Path::new(KEYRING)
            ),
            "# managed by opld; edits are overwritten\n\
             Types: deb\n\
             URIs: https://apt.example.com/oplite\n\
             Suites: trixie\n\
             Components: testing\n\
             Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg\n"
        );
    }

    #[test]
    fn write_sources_writes_replaces_and_leaves_unmanaged_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        std::fs::write(&keyring, "").unwrap();
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            sources_contents(url, Channel::Stable, &keyring)
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unchanged
        );
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Written
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Components: testing\n")
        );
        // 관리 표시가 없는 파일(관리자가 직접 쓴 것)은 그대로 둔다.
        let own =
            "Types: deb\nURIs: http://mirror.lan/oplite\nSuites: trixie\nComponents: stable\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Testing, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
        // 비었거나 공백뿐인 파일은 opld 것으로 본다.
        for empty in ["", " \n\n"] {
            std::fs::write(&path, empty).unwrap();
            assert_eq!(
                write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
                SourcesOutcome::Written
            );
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                sources_contents(url, Channel::Stable, &keyring)
            );
        }
        assert!(!dir.path().join("oplite.sources.dpkg-tmp").exists());
    }

    /// 공개 키가 없으면 쓰지 않고, opld가 쓴 파일은 지우고, 관리자의 파일은 그대로 둔다.
    #[test]
    fn write_sources_needs_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplite.sources");
        let keyring = dir.path().join("keyring.gpg");
        let url = "https://apt.example.com/oplite";
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        std::fs::write(&path, sources_contents(url, Channel::Stable, &keyring)).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::NoKeyring
        );
        assert!(!path.exists());
        let own = "Types: deb\nURIs: http://mirror.lan/oplite\n";
        std::fs::write(&path, own).unwrap();
        assert_eq!(
            write_sources(&path, url, Channel::Stable, &keyring).unwrap(),
            SourcesOutcome::Unmanaged
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), own);
    }
}
```

`crates/opl-api/tests/common/fake_apt.rs`에서

```rust
//! 시험용 가짜 `apt-get`. 임시 디렉터리에 셸 스크립트를 두고 상태를 파일로 흉내 낸다:
//!
//! - 부를 때마다 `LANG=<값> <인자…>` 한 줄을 `apt-get.calls`에 더한다.
//! - 인자에 `-s`가 있으면 `simulate.txt`를 출력한다.
//! - `update`면 `update.stderr`(있으면)를 표준 오류로 내고 `update.exit`(없으면 0)로 끝난다.
//!
```

을 다음으로 바꾼다:

```rust
//! 시험용 가짜 `apt-get`·`systemd-run`·`systemctl`·`dpkg`. 임시 디렉터리에 셸 스크립트를 두고 상태를
//! 파일로 흉내 낸다:
//!
//! - `apt-get`: 부를 때마다 `LANG=<값> <인자…>` 한 줄을 `apt-get.calls`에 더한다. 인자에 `-s`가
//!   있으면 `simulate.txt`를 출력한다. `update`면 `update.stderr`(있으면)를 표준 오류로 내고
//!   `update.exit`(없으면 0)로 끝난다.
//! - `systemd-run`: 인자를 한 줄에 하나씩 `systemd-run.args`에 쓴다. `systemd-run.exit`가 있으면
//!   `systemd-run.stderr`를 표준 오류로 내고 그 코드로 끝나고, 없으면 `unit.state`에 `active`를 쓴다.
//!   명령은 돌리지 않는다.
//! - `systemctl`: 인자를 `systemctl.calls`에 더하고 `unit.state`(`active`·`success`·`failed`, 없거나
//!   `gone`이면 사라진 유닛)에 맞는 `ActiveState=`·`Result=`·`LoadState=` 줄을 출력한다.
//! - `dpkg`: 인자를 `dpkg.calls`에 더하고 `audit.txt`(있으면)를 출력한다.
//!
```

`crates/opl-api/tests/common/fake_apt.rs`에서

```rust

pub struct FakeApt {
```

을 다음으로 바꾼다:

```rust

const SYSTEMD_RUN: &str = r#"#!/bin/sh
d=$(dirname "$0")
: > "$d/systemd-run.args"
for a; do printf '%s\n' "$a" >> "$d/systemd-run.args"; done
if [ -f "$d/systemd-run.exit" ]; then
    if [ -f "$d/systemd-run.stderr" ]; then cat "$d/systemd-run.stderr" >&2; fi
    exit "$(cat "$d/systemd-run.exit")"
fi
echo active > "$d/unit.state"
"#;

const SYSTEMCTL: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/systemctl.calls"
state=gone
if [ -f "$d/unit.state" ]; then state=$(cat "$d/unit.state"); fi
case $state in
active) printf 'ActiveState=active\nResult=success\nLoadState=loaded\n' ;;
success) printf 'ActiveState=inactive\nResult=success\nLoadState=loaded\n' ;;
failed) printf 'ActiveState=failed\nResult=exit-code\nLoadState=loaded\n' ;;
*) printf 'ActiveState=inactive\nResult=\nLoadState=not-found\n' ;;
esac
"#;

const DPKG: &str = r#"#!/bin/sh
d=$(dirname "$0")
echo "$*" >> "$d/dpkg.calls"
if [ -f "$d/audit.txt" ]; then cat "$d/audit.txt"; fi
"#;

pub struct FakeApt {
```

`crates/opl-api/tests/common/fake_apt.rs`에서

```rust
        write_script(&apt.path("apt-get"), APT_GET);
        apt.set("simulate.txt", "");
```

을 다음으로 바꾼다:

```rust
        write_script(&apt.path("apt-get"), APT_GET);
        write_script(&apt.path("systemd-run"), SYSTEMD_RUN);
        write_script(&apt.path("systemctl"), SYSTEMCTL);
        write_script(&apt.path("dpkg"), DPKG);
        apt.set("simulate.txt", "");
```

`crates/opl-api/tests/common/fake_apt.rs`에서

```rust
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("apt-get.calls"))
            .unwrap_or_default()
```

을 다음으로 바꾼다:

```rust
    pub fn calls(&self) -> Vec<String> {
        self.lines("apt-get.calls")
    }

    /// 파일의 줄(없으면 빈 목록). `systemd-run.args`, `systemctl.calls`, `dpkg.calls` 등
    pub fn lines(&self, name: &str) -> Vec<String> {
        std::fs::read_to_string(self.path(name))
            .unwrap_or_default()
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/nodes/{key}/updates/refresh",
        "/vms",
```

을 다음으로 바꾼다:

```rust
        "/nodes/{key}/updates/refresh",
        "/nodes/{key}/upgrade",
        "/vms",
```

`crates/opl-api/tests/updates.rs`에서

```rust
//! 노드 업데이트 확인: 다시 확인 Task(가짜 apt), 요청 검사, 소스 파일과 주기 확인 에이전트.
```

을 다음으로 바꾼다:

```rust
//! 노드 업데이트 확인과 업그레이드: 다시 확인 Task(가짜 apt), 요청 검사, 소스 파일과 주기 확인 에이전트,
//! 업그레이드 Task(가짜 systemd-run·systemctl·dpkg).
```

`crates/opl-api/tests/updates.rs`에서

```rust
use opl_types::node::{NodeSpec, NodeUpdates};
use reqwest::StatusCode;
```

을 다음으로 바꾼다:

```rust
use opl_types::node::{NodeSpec, NodeUpdates};
use opl_types::resource::Resource;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskState, TaskStatus};
use reqwest::StatusCode;
```

`crates/opl-api/tests/updates.rs`에서

```rust
        (apt.calls().len() == 4).then_some(())
    })
    .await;
    agent.shutdown().await;
}
```

을 다음으로 바꾼다:

```rust
        (apt.calls().len() == 4).then_some(())
    })
    .await;
    agent.shutdown().await;
}

/// 실행기가 업그레이드 유닛을 지켜보기 시작할 때까지(가짜 `systemctl`이 불릴 때까지) 기다린다.
async fn wait_for_polling(apt: &FakeApt) {
    eventually("the unit to be polled", || {
        (!apt.lines("systemctl.calls").is_empty()).then_some(())
    })
    .await;
}

/// 업그레이드 Task의 유닛 이름과 로그 파일
fn unit_and_log(apt: &FakeApt, task: &Value) -> (String, std::path::PathBuf) {
    let id = task["id"].as_str().unwrap();
    (
        format!("opl-upgrade-{}", id.replace('-', "")),
        apt.path("log").join(format!("upgrade-{id}.log")),
    )
}

/// 업그레이드 Task `id`의 로그를 쓴다(이전 opld 때 유닛이 남긴 것처럼).
fn write_log(apt: &FakeApt, id: Uuid, text: &str) {
    std::fs::create_dir_all(apt.path("log")).unwrap();
    std::fs::write(apt.path("log").join(format!("upgrade-{id}.log")), text).unwrap();
}

#[tokio::test]
async fn upgrade_runs_apt_in_a_transient_unit_and_checks_afterwards() {
    let apt = FakeApt::new();
    apt.set(
        "simulate.txt",
        "Inst libnew1 (1.0-1 Debian:13.2/stable [amd64])\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    assert_eq!(task["spec"]["op"], json!({ "kind": "node-upgrade" }));
    assert_eq!(task["spec"]["target"], env.node.to_string());
    assert_eq!(task["spec"]["node"], env.node.to_string());
    wait_for_polling(&apt).await;

    let (unit, log) = unit_and_log(&apt, &task);
    let apt_get = apt.path("apt-get").display().to_string();
    assert_eq!(
        apt.lines("systemd-run.args"),
        [
            format!("--unit={unit}"),
            "--collect".into(),
            "--property=Type=exec".into(),
            format!("--property=StandardOutput=append:{}", log.display()),
            format!("--property=StandardError=append:{}", log.display()),
            "--setenv=DEBIAN_FRONTEND=noninteractive".into(),
            "--setenv=LANG=C".into(),
            "/bin/sh".into(),
            "-c".into(),
            format!(
                "echo \"opl-upgrade: start\"; \
                 '{apt_get}' update -q -o DPkg::Lock::Timeout=600 && \
                 '{apt_get}' -y -q -o DPkg::Lock::Timeout=600 \
                 -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold dist-upgrade; \
                 s=$?; echo \"opl-upgrade: exit $s\"; exit $s"
            ),
        ]
    );
    // 로그 디렉터리를 만든다(systemd가 로그 파일을 그 안에 만든다).
    assert!(apt.path("log").is_dir());
    assert_eq!(
        apt.lines("systemctl.calls")[0],
        format!("show -p ActiveState -p Result -p LoadState {unit}.service")
    );
    // 유닛이 도는 동안은 끝나지 않는다. 업데이트 확인도 아직 하지 않는다.
    let (_, now) = admin
        .get(&format!("/tasks/{}", task["id"].as_str().unwrap()))
        .await;
    assert_eq!(now["status"]["state"], "running", "{now}");
    assert!(apt.calls().is_empty());
    // 업그레이드가 apt 잠금을 잡고 있는 동안은 다시 확인하지 않는다.
    let got = admin.post("/nodes/node1/updates/refresh", json!({})).await;
    assert_error(&got, StatusCode::CONFLICT, "upgrade_in_progress");
    assert_eq!(
        got.1["message"],
        "node \"node1\" is being upgraded; it checks for updates when the upgrade ends"
    );

    apt.set("unit.state", "success");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    // 끝나면 업데이트를 다시 확인한다.
    assert_eq!(
        apt.calls(),
        [
            "LANG=C update -q -o DPkg::Lock::Timeout=60",
            "LANG=C -s -q -o DPkg::Lock::Timeout=60 dist-upgrade"
        ]
    );
    let (_, node) = admin.get("/nodes/node1").await;
    assert_eq!(
        node["status"]["updates"]["packages"],
        json!([{ "name": "libnew1", "current": null, "candidate": "1.0-1" }])
    );
    assert!(apt.lines("dpkg.calls").is_empty());
}

/// 업그레이드가 확인 잠금을 잡은 동안 (막히기 전에 만들어진) 다시 확인 Task는 기다리지 않고 성공으로
/// 끝난다. 확인은 업그레이드가 끝날 때 한 번 한다.
#[tokio::test]
async fn a_check_task_during_an_upgrade_succeeds_without_checking() {
    let apt = FakeApt::new();
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;
    let (status, upgrade) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{upgrade}");
    wait_for_polling(&apt).await;

    let mut check: Task = Resource::new(
        "check",
        TaskSpec {
            op: TaskOp::NodeUpdateCheck,
            target: env.node,
            node: env.node,
        },
    );
    check.status.state = TaskState::Pending;
    env.cluster.create(&check).await.unwrap();
    let done = admin
        .wait_task(&json!({ "id": check.id.to_string() }))
        .await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert!(apt.calls().is_empty(), "{:?}", apt.calls());

    apt.set("unit.state", "success");
    let done = admin.wait_task(&upgrade).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert_eq!(apt.calls().len(), 2);
}

#[tokio::test]
async fn a_failed_upgrade_fails_the_task_with_the_last_log_lines() {
    let apt = FakeApt::new();
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (status, task) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    wait_for_polling(&apt).await;
    let (_, log) = unit_and_log(&apt, &task);
    let lines: Vec<String> = (1..=25).map(|i| format!("line {i}")).collect();
    std::fs::write(&log, lines.join("\n") + "\n").unwrap();
    apt.set("unit.state", "failed");

    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        format!(
            "the upgrade failed; last log lines:\n{}",
            lines[5..].join("\n")
        )
    );
    // 실패해도 업데이트를 다시 확인한다.
    assert_eq!(
        apt.calls(),
        [
            "LANG=C update -q -o DPkg::Lock::Timeout=60",
            "LANG=C -s -q -o DPkg::Lock::Timeout=60 dist-upgrade"
        ]
    );
}

#[tokio::test]
async fn upgrade_fails_when_systemd_run_fails() {
    let apt = FakeApt::new();
    apt.set("systemd-run.exit", "1");
    apt.set(
        "systemd-run.stderr",
        "Failed to start transient service unit: Access denied\n",
    );
    let env = env_with_updates(&apt).await;
    let admin = env.admin().await;

    let (_, task) = admin.post("/nodes/node1/upgrade", json!({})).await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    let error = done["status"]["error"].as_str().unwrap();
    assert!(error.starts_with("systemd-run failed ("), "{error}");
    assert!(
        error.ends_with("): Failed to start transient service unit: Access denied"),
        "{error}"
    );
    assert!(apt.lines("systemctl.calls").is_empty());
}

#[tokio::test]
async fn upgrade_checks_the_node_the_caller_and_other_upgrades() {
    let apt = FakeApt::new();
    let mut env = Env::start().await;
    let admin = env.admin().await;

    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::BAD_REQUEST,
        "updates_unsupported",
    );
    assert_error(
        &admin.post("/nodes/nope/upgrade", json!({})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    for role in ["operator", "viewer"] {
        let user = env.user(role, role).await;
        assert_error(
            &user.post("/nodes/node1/upgrade", json!({})).await,
            StatusCode::FORBIDDEN,
            "forbidden",
        );
    }
    set_node_status(&env, |s| s.online = false).await;
    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::CONFLICT,
        "node_offline",
    );
    set_node_status(&env, |s| s.online = true).await;

    // 한 번에 한 노드: 이 노드의 업그레이드가 끝나지 않았으면 409
    env.stop_runner().await;
    let (status, first) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    let (status, body) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_error(
        &(status, body.clone()),
        StatusCode::CONFLICT,
        "upgrade_in_progress",
    );
    assert_eq!(
        body["message"],
        "node \"node1\" is still being upgraded; upgrade one node at a time"
    );
    // 다른 노드의 업그레이드도 막는다.
    let id = first["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    env.cluster.delete::<TaskSpec>(id, None).await.unwrap();
    let other = Uuid::now_v7();
    let mut task: Task = Resource::new(
        "other",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: other,
            node: other,
        },
    );
    task.status.state = TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::CONFLICT,
        "upgrade_in_progress",
    );
    // 끝난 업그레이드는 막지 않는다.
    env.cluster
        .update_status::<TaskSpec>(
            task.id,
            &TaskStatus {
                state: TaskState::Succeeded,
                error: None,
            },
        )
        .await
        .unwrap();
    // 버전을 적지 않은 옛 노드가 있으면 옛 노드가 풀지 못하는 Task를 만들지 않는다.
    let old = Resource::new(
        "old",
        NodeSpec {
            address: "10.0.0.9:8006".into(),
            maintenance: false,
            witness: true,
        },
    );
    env.cluster.create(&old).await.unwrap();
    assert_error(
        &admin.post("/nodes/node1/upgrade", json!({})).await,
        StatusCode::CONFLICT,
        "cluster_upgrading",
    );
    env.cluster.delete::<NodeSpec>(old.id, None).await.unwrap();
    let (status, body) = admin.post("/nodes/node1/upgrade", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(apt.lines("systemd-run.args").is_empty());
}

/// 두 요청이 함께 지나가 업그레이드 Task가 둘 생겨도, 다른 업그레이드가 running이면 실행기가 새
/// 업그레이드를 시작하지 않는다.
#[tokio::test]
async fn the_runner_refuses_an_upgrade_while_another_runs() {
    let apt = FakeApt::new();
    let mut env = env_with_updates(&apt).await;
    env.stop_runner().await;
    let other = Uuid::now_v7();
    let mut running: Task = Resource::new(
        "other",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: other,
            node: other,
        },
    );
    running.status.state = TaskState::Running;
    env.cluster.create(&running).await.unwrap();
    let mut task: Task = Resource::new(
        "mine",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: env.node,
            node: env.node,
        },
    );
    task.status.state = TaskState::Pending;
    env.cluster.create(&task).await.unwrap();
    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    let done = env
        .admin()
        .await
        .wait_task(&json!({ "id": task.id.to_string() }))
        .await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        format!("upgrade_in_progress: node \"{other}\" is being upgraded")
    );
    assert!(apt.lines("systemd-run.args").is_empty());
}

/// 이전 opld가 지켜보던(running) 업그레이드 Task를 만들고, `prepare(Task id)` 뒤 가짜 apt를 쓰는
/// 실행기를 띄운다.
async fn resume_upgrade(apt: &FakeApt, prepare: impl FnOnce(Uuid)) -> (Env, Value) {
    let mut env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    env.stop_runner().await;
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: env.node,
            node: env.node,
        },
    );
    task.name = task.id.to_string();
    task.status.state = TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    prepare(task.id);
    let updates = Arc::new(Updates::new(apt.config()));
    env.start_runner_config(Arc::new(DirStorage::new("qemu-img", MOUNT_ROOT)), |c| {
        c.updates = Some(updates)
    })
    .await;
    let task = json!({ "id": task.id.to_string() });
    (env, task)
}

#[tokio::test]
async fn a_restarted_opld_keeps_watching_the_upgrade_unit() {
    let apt = FakeApt::new();
    apt.set("unit.state", "active");
    let (env, task) = resume_upgrade(&apt, |_| {}).await;
    let admin = env.admin().await;
    wait_for_polling(&apt).await;
    let (unit, _) = unit_and_log(&apt, &task);
    assert_eq!(
        apt.lines("systemctl.calls")[0],
        format!("show -p ActiveState -p Result -p LoadState {unit}.service")
    );
    // 실패로 정리하지 않고, 유닛을 다시 띄우지도 않는다.
    let (_, now) = admin
        .get(&format!("/tasks/{}", task["id"].as_str().unwrap()))
        .await;
    assert_eq!(now["status"]["state"], "running", "{now}");
    assert!(!apt.path("systemd-run.args").exists());

    apt.set("unit.state", "success");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert_eq!(apt.calls().len(), 2);
}

/// 셸이 시작했는데 종료 상태 줄 없이 사라졌으면(apt가 도중에 끊겼다) dpkg가 깨끗해도 실패다.
#[tokio::test]
async fn a_vanished_unit_without_an_exit_status_was_interrupted() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |id| {
        write_log(
            &apt,
            id,
            "opl-upgrade: start\nUnpacking oplite (0.2.1) ...\n",
        )
    })
    .await;
    let admin = env.admin().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "the upgrade was interrupted; start it again"
    );
    assert_eq!(apt.lines("dpkg.calls"), ["--audit"]);
    let (_, node) = admin.get("/nodes/node1").await;
    assert!(
        node["status"]["updates"]["checked_at"].as_u64().is_some(),
        "{node}"
    );
}

/// 셸이 끝까지 돌고 유닛이 치워졌으면 로그의 종료 상태로 성공이다.
#[tokio::test]
async fn a_vanished_unit_that_exited_zero_succeeded() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |id| {
        write_log(&apt, id, "opl-upgrade: start\nopl-upgrade: exit 0\n")
    })
    .await;
    let done = env.admin().await.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert!(apt.lines("dpkg.calls").is_empty());
    assert!(!apt.path("systemd-run.args").exists());
}

/// 이전 opld가 유닛을 띄우기 전에 멈췄으면(로그가 없다) 이어 볼 때 유닛을 띄운다.
#[tokio::test]
async fn resuming_an_upgrade_that_never_started_starts_it() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |_| {}).await;
    let admin = env.admin().await;
    // 가짜 systemd-run은 인자를 쓴 뒤 `unit.state`에 active를 쓴다.
    eventually("the unit to be started", || {
        let state = std::fs::read_to_string(apt.path("unit.state")).ok()?;
        (state.trim() == "active").then_some(())
    })
    .await;
    let (unit, _) = unit_and_log(&apt, &task);
    assert_eq!(apt.lines("systemd-run.args")[0], format!("--unit={unit}"));
    apt.set("unit.state", "success");
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "succeeded", "{done}");
    assert!(apt.lines("dpkg.calls").is_empty());
}

#[tokio::test]
async fn a_vanished_unit_with_a_dpkg_audit_report_was_interrupted() {
    let apt = FakeApt::new();
    apt.set(
        "audit.txt",
        "The following packages are only half configured, probably due to problems\n\
         configuring them the first time.  The configuration should be retried using\n\
         dpkg --configure <package> or the configure menu option in dselect:\n\
         oplite               opLite virtualization platform\n",
    );
    let (env, task) = resume_upgrade(&apt, |id| write_log(&apt, id, "opl-upgrade: start\n")).await;
    let admin = env.admin().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "the upgrade was interrupted; run \"dpkg --configure -a\" on the node"
    );
}

#[tokio::test]
async fn a_vanished_unit_uses_the_exit_status_in_the_log() {
    let apt = FakeApt::new();
    let (env, task) = resume_upgrade(&apt, |id| {
        write_log(
            &apt,
            id,
            "opl-upgrade: start\nE: Unable to fetch some archives\nopl-upgrade: exit 100\n",
        )
    })
    .await;
    let admin = env.admin().await;
    let done = admin.wait_task(&task).await;
    assert_eq!(done["status"]["state"], "failed", "{done}");
    assert_eq!(
        done["status"]["error"],
        "the upgrade failed; last log lines:\nopl-upgrade: start\n\
         E: Unable to fetch some archives\nopl-upgrade: exit 100"
    );
    // 종료 상태가 있으면 dpkg로 짐작하지 않는다.
    assert!(apt.lines("dpkg.calls").is_empty());
}

#[tokio::test]
async fn the_agent_skips_checks_while_the_node_is_upgrading() {
    let apt = FakeApt::new();
    let mut env = Env::start().await;
    set_node_status(&env, |s| s.updates = Some(NodeUpdates::default())).await;
    // 실행기가 없으면 업그레이드 Task는 running으로 남는다.
    env.stop_runner().await;
    let mut task: Task = Resource::new(
        "up",
        TaskSpec {
            op: TaskOp::NodeUpgrade,
            target: env.node,
            node: env.node,
        },
    );
    task.status.state = TaskState::Running;
    env.cluster.create(&task).await.unwrap();
    let mut config = apt.config();
    config.first_check = Duration::from_millis(20);
    config.check_interval = Duration::from_millis(50);
    let agent = UpdateAgent::spawn(
        env.cluster.clone(),
        env.node,
        Arc::new(Updates::new(config)),
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(apt.calls().is_empty(), "{:?}", apt.calls());

    env.cluster
        .update_status::<TaskSpec>(
            task.id,
            &TaskStatus {
                state: TaskState::Succeeded,
                error: None,
            },
        )
        .await
        .unwrap();
    eventually("a check after the upgrade", || {
        (!apt.calls().is_empty()).then_some(())
    })
    .await;
    agent.shutdown().await;
}
```

`crates/opl-types/src/task.rs`에서

```rust
    NodeUpdateCheck,
}
```

을 다음으로 바꾼다:

```rust
    NodeUpdateCheck,
    /// 대상 노드(Task의 `node`와 같다)의 패키지를 `apt-get dist-upgrade`로 올린다. opld 밖의 일시
    /// systemd 유닛에서 돌므로 opld가 다시 시작해도 이어서 지켜본다.
    NodeUpgrade,
}
```

`crates/opl-types/src/task.rs`에서

```rust
    #[test]
    fn new_task_is_pending() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn node_upgrade_op_is_tagged_by_kind() {
        let json = serde_json::json!({ "kind": "node-upgrade" });
        assert_eq!(serde_json::to_value(TaskOp::NodeUpgrade).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<TaskOp>(json).unwrap(),
            TaskOp::NodeUpgrade
        );
    }

    #[test]
    fn new_task_is_pending() {
```

`crates/oplctl/src/cli.rs`에서

```rust
use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, fetch_ca, fingerprint, local,
};
```

을 다음으로 바꾼다:

```rust
use crate::client::{
    ApiError, BACKUP_WAIT, Client, IMPORT_WAIT, MIGRATE_WAIT, UPGRADE_WAIT, fetch_ca, fingerprint,
    local,
};
```

`crates/oplctl/src/cli.rs`에서

```rust
        refresh: bool,
    },
}
```

을 다음으로 바꾼다:

```rust
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
```

`crates/oplctl/src/cli.rs`에서

```rust
                node_updates(&client, &node, refresh, json_out, out).await?
            }
        },
        Cmd::Pool { cmd } => match cmd {
```

을 다음으로 바꾼다:

```rust
                node_updates(&client, &node, refresh, json_out, out).await?
            }
            NodeCmd::Upgrade { node, wait } => node_upgrade(&client, &node, wait, out).await?,
        },
        Cmd::Pool { cmd } => match cmd {
```

`crates/oplctl/src/cli.rs`에서

```rust

    /// `settings set`은 KEY=VALUE를 PATCH 본문으로 바꾼다. 모르는 키와 `=` 없는 인자는 거절한다.
```

을 다음으로 바꾼다:

```rust

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
```

`crates/oplctl/src/client.rs`에서

```rust
pub const IMPORT_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

을 다음으로 바꾼다:

```rust
pub const IMPORT_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// 노드 업그레이드 Task를 기다리는 시간. 큰 업그레이드는 오래 걸리므로 Task 실행기는 시간 제한을 두지 않는다.
pub const UPGRADE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1195개, 실패 0

Run: `pnpm -C web typecheck`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add crates web/src/api/schema.d.ts
git commit -m "feat: 노드 업그레이드 Task — TaskOp::NodeUpgrade(\"node-upgrade\", 필드 없음, 대상이 노드)와 POST /nodes/{key}/upgrade(admin, 400 updates_unsupported, 409 node_offline, 클러스터 어디든 끝나지 않은 업그레이드가 있으면 409 upgrade_in_progress, OpenAPI·schema.d.ts), 실행기가 log_dir를 만들고 systemd-run --unit=opl-upgrade-<task id 앞 8자> --collect(Type=exec, 출력은 <log_dir>/upgrade-<task id>.log에 더함, DEBIAN_FRONTEND=noninteractive, LANG=C)으로 apt-get update·dist-upgrade(confdef·confold)를 돌리고 로그 끝에 종료 상태 줄을 남김, poll마다 systemctl show(ActiveState·Result·LoadState)로 지켜봄 — 끝나면 성공, 실패면 로그 끝 20줄, 유닛이 사라졌으면 로그의 종료 상태 줄, 없으면 dpkg --audit가 비었는지(아니면 \"dpkg --configure -a\" 안내), systemctl 실패는 다시 봄, 성공·실패 모두 끝나면 업데이트를 다시 확인, 시간 제한 없음, 실행기 첫 패스에서 running인 업그레이드는 실패로 정리하지 않고 같은 유닛을 이어 봄, UpdateAgent는 이 노드의 업그레이드가 끝나지 않았으면 확인을 건너뜀, 시험용 가짜 systemd-run·systemctl·dpkg, oplctl node upgrade <node> [--wait](시작 안내 또는 24시간까지 기다림)"
```

---

### Task 5: APT 저장소 스크립트와 패키지 키링

`packaging/repo/build-repo.sh <repo dir> <channel> <deb…>`: `pool/<channel>/`에 패키지를 넣고 `apt-ftparchive`로 색인을 만들어 `gpg`로 서명한다(다른 채널은 그대로, 같은 이름 다른 내용은 거절, 서명이 다 성공한 뒤에만 바꿔 넣음). `build-deb.sh`는 `OPL_APT_KEY`(바이너리 키링)를 받으면 `/usr/share/keyrings/oplite-archive-keyring.gpg`로 싣는다. 패키지를 지우면 opld가 관리하는 소스 파일도 지운다. 시험: `build-repo-test.sh`(가짜 도구), CI `package` 작업의 `repo-test.sh`(`debian:13` 컨테이너에서 실제 저장소로 설치하고 `testing`의 새 버전이 보이는지).

**Files:**
- Create: `packaging/repo/build-repo.sh`
- Create: `packaging/tests/build-repo-test.sh`
- Create: `packaging/tests/repo-test.sh`
- Modify: `.github/workflows/ci.yml`
- Modify: `packaging/deb/build-deb.sh`
- Modify: `packaging/deb/postrm`
- Modify: `packaging/tests/build-deb-test.sh`
- Modify: `packaging/tests/deb-test.sh`
- Modify: `packaging/tests/maint-test.sh`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml

  # 패키징 셸 스크립트: shellcheck, build-deb.sh 버전 확인 시험, maintainer 스크립트 시험(가짜 systemctl)
  shell:
```

을 다음으로 바꾼다:

```yaml

  # 패키징 셸 스크립트: shellcheck, build-deb.sh 버전 확인·키링 시험, build-repo.sh 시험(가짜 apt-ftparchive·gpg),
  # maintainer 스크립트 시험(가짜 systemctl)
  shell:
```

`.github/workflows/ci.yml`에서

```yaml
      - run: bash packaging/tests/build-deb-test.sh
      - run: bash packaging/tests/maint-test.sh
```

을 다음으로 바꾼다:

```yaml
      - run: bash packaging/tests/build-deb-test.sh
      - run: bash packaging/tests/build-repo-test.sh
      - run: bash packaging/tests/maint-test.sh
```

`.github/workflows/ci.yml`에서

```yaml

  # Debian 패키지 oplite: 웹 UI를 넣은 릴리스 빌드 → build-deb.sh(.github/actions/build-deb) → Debian 13 컨테이너에서 설치 시험
  package:
```

을 다음으로 바꾼다:

```yaml

  # Debian 패키지 oplite: 웹 UI를 넣은 릴리스 빌드 → build-deb.sh(.github/actions/build-deb) → Debian 13 컨테이너에서 설치 시험,
  # build-repo.sh로 만든 APT 저장소에서 설치하고 testing 채널의 새 버전을 보는 시험
  package:
```

`.github/workflows/ci.yml`에서

```yaml
            bash /work/packaging/tests/deb-test.sh "/work/$DEB"
      # 빌드 버전 시험을 OPL_VERSION을 준 빌드로 다시 돌린다(clap 기본 version으로 되돌리면 실패한다).
```

을 다음으로 바꾼다:

```yaml
            bash /work/packaging/tests/deb-test.sh "/work/$DEB"
      - name: APT repository test in Debian 13
        env:
          DEB: ${{ steps.build.outputs.deb }}
        run: |
          docker run --rm -v "$PWD:/work" debian:13 \
            bash /work/packaging/tests/repo-test.sh "/work/$DEB"
      # 빌드 버전 시험을 OPL_VERSION을 준 빌드로 다시 돌린다(clap 기본 version으로 되돌리면 실패한다).
```

`packaging/deb/build-deb.sh`에서

```sh
# 바이너리 위치는 기본 target/release이고 OPL_BIN_DIR로 바꾼다.
set -eu
```

을 다음으로 바꾼다:

```sh
# 바이너리 위치는 기본 target/release이고 OPL_BIN_DIR로 바꾼다.
# OPL_APT_KEY(APT 저장소 공개 키 파일, armor가 아닌 gpg --export)가 있으면
# /usr/share/keyrings/oplite-archive-keyring.gpg로 싣는다.
set -eu
```

`packaging/deb/build-deb.sh`에서

```sh

STAGE=$(mktemp -d)
```

을 다음으로 바꾼다:

```sh

APT_KEY=${OPL_APT_KEY:-}
if [ -n "$APT_KEY" ] && [ ! -f "$APT_KEY" ]; then
	echo "build-deb: OPL_APT_KEY의 $APT_KEY 이 없다" >&2
	exit 1
fi
# apt의 Signed-By는 .gpg 확장자의 armor 키를 읽지 못한다.
if [ -n "$APT_KEY" ] && head -n 1 "$APT_KEY" | grep -q '^-----BEGIN PGP'; then
	echo "build-deb: OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)" >&2
	exit 1
fi

STAGE=$(mktemp -d)
```

`packaging/deb/build-deb.sh`에서

```sh
install -m 0644 "$DEB_DIR/copyright" "$STAGE/usr/share/doc/oplite/copyright"
```

을 다음으로 바꾼다:

```sh
install -m 0644 "$DEB_DIR/copyright" "$STAGE/usr/share/doc/oplite/copyright"
# 저장소 공개 키: opld가 쓰는 소스 파일의 Signed-By
if [ -n "$APT_KEY" ]; then
	mkdir -p "$STAGE/usr/share/keyrings"
	install -m 0644 "$APT_KEY" "$STAGE/usr/share/keyrings/oplite-archive-keyring.gpg"
fi
```

`packaging/deb/postrm`에서

```
# oplite postrm. /var/lib/opl(Raft 데이터, 풀, 볼륨)과 /etc/opl은 purge여도 지우지 않는다.
set -eu
```

을 다음으로 바꾼다:

```
# oplite postrm. /var/lib/opl(Raft 데이터, 풀, 볼륨)과 /etc/opl은 purge여도 지우지 않는다.
# opld가 쓴 APT 소스 파일(첫 줄이 관리 표시)은 지운다: 함께 지워진 저장소 공개 키를 가리켜 apt-get
# update가 실패한다. 관리자가 직접 쓴 파일은 남긴다.
set -eu
```

`packaging/deb/postrm`에서

```
SYSTEMD_ETC="${OPL_SYSTEMD_ETC:-/etc/systemd/system}"
```

을 다음으로 바꾼다:

```
SYSTEMD_ETC="${OPL_SYSTEMD_ETC:-/etc/systemd/system}"
APT_SOURCES="${OPL_APT_SOURCES:-/etc/apt/sources.list.d/oplite.sources}"
```

`packaging/deb/postrm`에서

```
	fi
	if [ -d "$SYSTEMD_DIR" ]; then
```

을 다음으로 바꾼다:

```
	fi
	if [ -f "$APT_SOURCES" ] && [ "$(head -n 1 "$APT_SOURCES")" = "# managed by opld; edits are overwritten" ]; then
		rm -f "$APT_SOURCES"
	fi
	if [ -d "$SYSTEMD_DIR" ]; then
```

`packaging/repo/build-repo.sh`를 만든다:

```sh
#!/bin/sh
# opLite APT 저장소의 한 채널에 패키지를 더하고 색인을 다시 만들어 서명한다.
# 쓰는 법: OPL_REPO_GPG_KEY_ID=<키> packaging/repo/build-repo.sh <repo dir> <channel> <deb…>
# 채널은 stable|testing|enterprise. 레이아웃:
#   pool/<channel>/*.deb
#   dists/trixie/<channel>/binary-amd64/Packages(.gz)
#   dists/trixie/Release, InRelease, Release.gpg  (Release는 있는 모든 채널을 묶는다)
# 그 채널의 Packages만 다시 만들고 다른 채널의 Packages는 그대로 둔다. pool의 옛 패키지는 지우지 않는다.
# 이미 올린 이름에 내용이 다른 deb는 거절한다. 색인은 두 서명이 다 끝난 뒤에 바꾼다(InRelease가 마지막).
# 서명 키는 OPL_REPO_GPG_KEY_ID(필수), GPG 홈은 호출하는 쪽(GNUPGHOME). 도구 경로는 APT_FTPARCHIVE·GPG로 바꾼다.
set -eu
umask 022

SUITE=trixie
ARCH=amd64
# Release의 Components 순서
CHANNELS="stable testing enterprise"

usage() {
	echo "usage: $0 <repo dir> <stable|testing|enterprise> <deb…>" >&2
	exit 2
}

[ $# -ge 3 ] || usage
REPO=$1
CHANNEL=$2
shift 2
case $CHANNEL in
stable | testing | enterprise) ;;
*)
	echo "build-repo: 모르는 채널 '$CHANNEL'(stable|testing|enterprise)" >&2
	usage
	;;
esac
for deb; do
	if [ ! -f "$deb" ]; then
		echo "build-repo: $deb 이 없다" >&2
		usage
	fi
done
KEY=${OPL_REPO_GPG_KEY_ID:-}
if [ -z "$KEY" ]; then
	echo "build-repo: OPL_REPO_GPG_KEY_ID(서명 키)가 필요하다" >&2
	exit 1
fi
APT_FTPARCHIVE=${APT_FTPARCHIVE:-apt-ftparchive}
GPG=${GPG:-gpg}

POOL="$REPO/pool/$CHANNEL"
DIST="$REPO/dists/$SUITE"
BIN="$DIST/$CHANNEL/binary-$ARCH"
# 이미 올린 이름에 다른 파일은 받지 않는다(받아 둔 apt의 해시가 맞지 않게 된다). 같은 파일은 넘어간다.
for deb; do
	old="$POOL/$(basename "$deb")"
	if [ -e "$old" ] && ! cmp -s "$deb" "$old"; then
		echo "build-repo: version already published: $old differs from $deb" >&2
		exit 1
	fi
done
mkdir -p "$POOL"
cp "$@" "$POOL/"

# 새 색인과 Release는 dists/.trixie.new에서 만들어 서명까지 끝낸 뒤 옮긴다. 도중에 실패해도 저장소는
# 옛 색인과 서명 그대로다.
STAGE="$REPO/dists/.$SUITE.new"
trap 'rm -rf "$STAGE" "$REPO/dists/.Release.new"' EXIT
rm -rf "$STAGE"
mkdir -p "$STAGE"
for c in $CHANNELS; do
	if [ -f "$DIST/$c/binary-$ARCH/Packages" ]; then
		mkdir -p "$STAGE/$c/binary-$ARCH"
		cp "$DIST/$c/binary-$ARCH/"Packages* "$STAGE/$c/binary-$ARCH/"
	fi
done

# 이 채널의 Packages. Filename이 저장소 루트 기준이 되도록 루트에서 돌린다.
NEW="$STAGE/$CHANNEL/binary-$ARCH"
mkdir -p "$NEW"
(cd "$REPO" && "$APT_FTPARCHIVE" packages "pool/$CHANNEL") >"$NEW/Packages"
gzip -9 -n -c "$NEW/Packages" >"$NEW/Packages.gz"

# Packages가 있는 채널들
COMPONENTS=
for c in $CHANNELS; do
	[ -f "$STAGE/$c/binary-$ARCH/Packages" ] && COMPONENTS="${COMPONENTS:+$COMPONENTS }$c"
done

# Release: 색인에 섞이지 않게 STAGE 밖에 만든 뒤 옮긴다.
(cd "$STAGE" && "$APT_FTPARCHIVE" \
	-o APT::FTPArchive::Release::Origin=opLite \
	-o APT::FTPArchive::Release::Label=opLite \
	-o APT::FTPArchive::Release::Suite="$SUITE" \
	-o APT::FTPArchive::Release::Codename="$SUITE" \
	-o APT::FTPArchive::Release::Components="$COMPONENTS" \
	-o APT::FTPArchive::Release::Architectures="$ARCH" \
	release .) >"$REPO/dists/.Release.new"
mv "$REPO/dists/.Release.new" "$STAGE/Release"

# 서명: InRelease(본문 안 서명)와 Release.gpg(따로 둔 서명)
"$GPG" --batch --yes --local-user "$KEY" --clearsign --output "$STAGE/InRelease" "$STAGE/Release"
"$GPG" --batch --yes --local-user "$KEY" --armor --detach-sign --output "$STAGE/Release.gpg" "$STAGE/Release"

# 옮긴다: 색인 먼저, apt가 먼저 읽는 InRelease는 마지막에.
mkdir -p "$BIN"
mv "$NEW/Packages" "$BIN/Packages"
mv "$NEW/Packages.gz" "$BIN/Packages.gz"
mv "$STAGE/Release" "$DIST/Release"
mv "$STAGE/Release.gpg" "$DIST/Release.gpg"
mv "$STAGE/InRelease" "$DIST/InRelease"

echo "$REPO"
```

`packaging/tests/build-deb-test.sh`에서

```sh
#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
```

을 다음으로 바꾼다:

```sh
#!/bin/bash
# build-deb.sh가 바이너리의 --version이 패키지 버전과 같은지 확인하는지, OPL_APT_KEY의 키링을 싣는지 본다.
# 가짜 opld·oplctl(버전을 찍는 스크립트)과 가짜 dpkg-deb(만들 파일과 패키지 트리 사본만 만듦)를 쓴다. bash만 있으면 되고 macOS에서도 돈다.
# 쓰는 법: bash packaging/tests/build-deb-test.sh
```

`packaging/tests/build-deb-test.sh`에서

```sh
mkdir -p "$WORK/fakebin"
# 가짜 dpkg-deb: 마지막 인자(만들 .deb)를 만든다.
cat >"$WORK/fakebin/dpkg-deb" <<'EOF'
#!/bin/sh
for a; do out=$a; done
echo deb >"$out"
EOF
```

을 다음으로 바꾼다:

```sh
mkdir -p "$WORK/fakebin"
# 가짜 dpkg-deb: 마지막 인자(만들 .deb)를 만들고, 그 앞 인자(패키지 트리)를 $WORK/tree로 복사한다.
cat >"$WORK/fakebin/dpkg-deb" <<'EOF'
#!/bin/sh
for a; do
	root=${out-}
	out=$a
done
echo deb >"$out"
rm -rf "$(dirname "$out")/../tree"
cp -Rp "$root" "$(dirname "$out")/../tree"
EOF
```

`packaging/tests/build-deb-test.sh`에서

```sh

if [ "$FAILED" -ne 0 ]; then
```

을 다음으로 바꾼다:

```sh

# OPL_APT_KEY: 저장소 공개 키를 /usr/share/keyrings/oplite-archive-keyring.gpg(0644)로 싣고 md5sums에 넣는다.
KEYRING=usr/share/keyrings/oplite-archive-keyring.gpg
fake_bins 0.1.0 0.1.0
run 0.1.0
if [ "$code" -eq 0 ] && [ ! -e "$WORK/tree/usr/share/keyrings" ]; then
	ok "OPL_APT_KEY가 없으면 키링을 싣지 않는다"
else
	fail "키링 없음: 종료 코드 $code"
fi
printf 'public key' >"$WORK/archive.gpg"
chmod 0600 "$WORK/archive.gpg"
OPL_APT_KEY="$WORK/archive.gpg" run 0.1.0
if [ "$code" -eq 0 ] && cmp -s "$WORK/archive.gpg" "$WORK/tree/$KEYRING" &&
	[ -n "$(find "$WORK/tree/$KEYRING" -perm 0644)" ] &&
	grep -q " $KEYRING\$" "$WORK/tree/DEBIAN/md5sums"; then
	ok "OPL_APT_KEY의 키링을 싣는다"
else
	fail "키링: 종료 코드 $code"
	cat "$WORK/stderr"
fi
OPL_APT_KEY="$WORK/missing.gpg" run 0.1.0
if [ "$code" -eq 1 ] && grep -q OPL_APT_KEY "$WORK/stderr" && [ ! -e "$WORK/out" ]; then
	ok "OPL_APT_KEY 파일이 없으면 실패"
else
	fail "없는 OPL_APT_KEY: 종료 코드 $code"
fi
printf -- '-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQINBG\n-----END PGP PUBLIC KEY BLOCK-----\n' >"$WORK/archive.asc"
OPL_APT_KEY="$WORK/archive.asc" run 0.1.0
if [ "$code" -eq 1 ] && grep -q 'OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)' "$WORK/stderr" &&
	[ ! -e "$WORK/out" ]; then
	ok "armor 키는 거절"
else
	fail "armor OPL_APT_KEY: 종료 코드 $code"
fi

if [ "$FAILED" -ne 0 ]; then
```

`packaging/tests/build-repo-test.sh`를 만든다:

```sh
#!/bin/bash
# build-repo.sh의 인자 검사, 저장소 레이아웃, apt-ftparchive·gpg 인자, 다른 채널 보존, 잘못된 채널 거절을 본다.
# 가짜 apt-ftparchive(인자를 남기고 Packages·Release 모양을 찍음)와 가짜 gpg(인자를 남기고 --output 파일을 만듦)를
# 쓴다. bash만 있으면 되고 macOS에서도 돈다. 진짜 도구로 apt가 읽는지는 repo-test.sh(CI 컨테이너)가 본다.
# 쓰는 법: bash packaging/tests/build-repo-test.sh
set -eu

REPO_SCRIPT="$(cd "$(dirname "$0")/../repo" && pwd)/build-repo.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/fakebin"
# 가짜 apt-ftparchive: 인자 한 줄을 calls에 남긴다.
# packages <dir>: <dir> 안의 .deb마다 Filename 줄. release <dir>: -o 값과 <dir> 아래 Packages 파일 목록.
cat >"$WORK/fakebin/apt-ftparchive" <<'EOF'
#!/bin/sh
echo "apt-ftparchive $*" >>"$FAKE_LOG"
cmd=
dir=
for a; do
	case $a in
	packages | release) cmd=$a ;;
	*) [ -n "$cmd" ] && dir=$a ;;
	esac
done
case $cmd in
packages)
	for f in "$dir"/*.deb; do
		printf 'Package: oplite\nFilename: %s\n\n' "$f"
	done
	;;
release)
	for a; do
		case $a in APT::FTPArchive::Release::*) echo "${a#APT::FTPArchive::Release::}" ;; esac
	done
	(cd "$dir" && find . -name 'Packages*' | LC_ALL=C sort)
	;;
*) exit 1 ;;
esac
EOF
# 가짜 gpg: 인자 한 줄을 calls에 남기고 --output 파일에 서명 대신 표시를 쓴다.
cat >"$WORK/fakebin/gpg" <<'EOF'
#!/bin/sh
echo "gpg $*" >>"$FAKE_LOG"
out=
next=
for a; do
	if [ -n "$next" ]; then
		out=$a
		next=
	fi
	[ "$a" = --output ] && next=1
done
echo signed >"$out"
EOF
chmod 0755 "$WORK/fakebin/apt-ftparchive" "$WORK/fakebin/gpg"

printf 'deb1' >"$WORK/oplite_1.0_amd64.deb"
printf 'deb2' >"$WORK/oplite_1.1_amd64.deb"
R="$WORK/repo"
D="$R/dists/trixie"

FAILED=0

ok() { echo "ok   $1"; }
fail() {
	echo "FAIL $1"
	FAILED=1
}

# run <args…>: 가짜 도구로 build-repo.sh를 돌려 종료 코드(code)·stdout·stderr·calls를 남긴다.
run() {
	code=0
	: >"$WORK/calls"
	FAKE_LOG="$WORK/calls" APT_FTPARCHIVE="$WORK/fakebin/apt-ftparchive" GPG="$WORK/fakebin/gpg" \
		OPL_REPO_GPG_KEY_ID=${KEY-ABCD1234} \
		sh "$REPO_SCRIPT" "$@" >"$WORK/stdout" 2>"$WORK/stderr" || code=$?
}

# 인자 검사: 종료 코드 2, 아무것도 만들지 않음
expect_usage() {
	if [ "$code" -eq 2 ] && [ ! -e "$R" ] && [ ! -s "$WORK/calls" ]; then
		ok "$1"
	else
		fail "$1: 종료 코드 $code"
		cat "$WORK/stderr"
	fi
}
run "$R" stable
expect_usage "deb가 없으면 사용법"
run "$R" unstable "$WORK/oplite_1.0_amd64.deb"
expect_usage "모르는 채널은 거절"
grep -q 'stable|testing|enterprise' "$WORK/stderr" || fail "채널 거절 문구: $(cat "$WORK/stderr")"
run "$R" stable "$WORK/missing.deb"
expect_usage "없는 deb는 거절"
KEY='' run "$R" stable "$WORK/oplite_1.0_amd64.deb"
if [ "$code" -eq 1 ] && [ ! -e "$R" ] && grep -q OPL_REPO_GPG_KEY_ID "$WORK/stderr"; then
	ok "OPL_REPO_GPG_KEY_ID가 없으면 실패"
else
	fail "OPL_REPO_GPG_KEY_ID 없음: 종료 코드 $code"
fi

# 첫 채널 stable
run "$R" stable "$WORK/oplite_1.0_amd64.deb"
if [ "$code" -eq 0 ] && [ "$(cat "$R/pool/stable/oplite_1.0_amd64.deb")" = deb1 ] &&
	[ -f "$D/stable/binary-amd64/Packages" ] && [ -f "$D/Release" ] &&
	[ -f "$D/InRelease" ] && [ -f "$D/Release.gpg" ]; then
	ok "stable 레이아웃"
else
	fail "stable 레이아웃: 종료 코드 $code"
	cat "$WORK/stderr"
	find "$R" | sort
fi
# Packages의 Filename은 저장소 루트 기준(apt-ftparchive를 저장소 루트에서 돌림)
if grep -qx 'Filename: pool/stable/oplite_1.0_amd64.deb' "$D/stable/binary-amd64/Packages" &&
	gunzip -c "$D/stable/binary-amd64/Packages.gz" | cmp -s - "$D/stable/binary-amd64/Packages"; then
	ok "Packages와 Packages.gz"
else
	fail "Packages: $(cat "$D/stable/binary-amd64/Packages" 2>&1)"
fi
EXPECTED_RELEASE='Origin=opLite
Label=opLite
Suite=trixie
Codename=trixie
Components=stable
Architectures=amd64
./stable/binary-amd64/Packages
./stable/binary-amd64/Packages.gz'
if [ "$(cat "$D/Release")" = "$EXPECTED_RELEASE" ]; then
	ok "Release 값"
else
	fail "Release: $(cat "$D/Release")"
fi
# 서명은 옮기기 전의 새 Release(dists/.trixie.new)에 한다.
S="$R/dists/.trixie.new"
if grep -q -- "--local-user ABCD1234 .*--clearsign --output $S/InRelease $S/Release" "$WORK/calls" &&
	grep -q -- "--local-user ABCD1234 .*--detach-sign --output $S/Release.gpg $S/Release" "$WORK/calls" &&
	grep -q -- '--batch' "$WORK/calls"; then
	ok "gpg 서명 인자"
else
	fail "gpg 인자: $(cat "$WORK/calls")"
fi
# 저장소에 임시 파일을 남기지 않는다.
if [ -z "$(find "$R" -name '*.new' -o -name '.*' -type f)" ]; then
	ok "임시 파일 없음"
else
	fail "임시 파일: $(find "$R" -name '*.new' -o -name '.*' -type f)"
fi

# 둘째 채널 testing: stable은 그대로, Release는 두 채널을 다시 묶는다.
cp "$D/stable/binary-amd64/Packages" "$WORK/stable.Packages"
run "$R" testing "$WORK/oplite_1.1_amd64.deb"
if [ "$code" -eq 0 ] && cmp -s "$WORK/stable.Packages" "$D/stable/binary-amd64/Packages" &&
	grep -qx 'Filename: pool/testing/oplite_1.1_amd64.deb' "$D/testing/binary-amd64/Packages" &&
	! grep -q 'pool/stable' "$D/testing/binary-amd64/Packages" &&
	! grep -q 'packages pool/stable' "$WORK/calls"; then
	ok "다른 채널의 Packages는 남긴다"
else
	fail "testing 추가: 종료 코드 $code"
	cat "$WORK/stderr" "$WORK/calls"
fi
if grep -qx 'Components=stable testing' "$D/Release" &&
	grep -qx './stable/binary-amd64/Packages' "$D/Release" &&
	grep -qx './testing/binary-amd64/Packages' "$D/Release"; then
	ok "Release가 모든 채널을 묶는다"
else
	fail "Release: $(cat "$D/Release")"
fi

# 같은 채널에 새 버전: 옛 패키지도 pool에 남고 Packages에 둘 다 있다.
run "$R" stable "$WORK/oplite_1.1_amd64.deb"
if [ "$code" -eq 0 ] && [ "$(grep -c '^Filename: pool/stable/' "$D/stable/binary-amd64/Packages")" -eq 2 ]; then
	ok "같은 채널에 더하면 옛 버전도 남는다"
else
	fail "stable 다시: $(cat "$D/stable/binary-amd64/Packages")"
fi

# enterprise도 채널이다. Components는 stable testing enterprise 순서.
run "$R" enterprise "$WORK/oplite_1.1_amd64.deb"
if [ "$code" -eq 0 ] && grep -qx 'Components=stable testing enterprise' "$D/Release" &&
	[ "$(tail -n1 "$WORK/stdout")" = "$R" ]; then
	ok "enterprise 채널"
else
	fail "enterprise: 종료 코드 $code, $(cat "$D/Release")"
fi

# 같은 이름에 내용이 다른 deb는 거절하고, 같은 deb는 넘어간다.
mkdir -p "$WORK/other"
printf 'rebuilt' >"$WORK/other/oplite_1.0_amd64.deb"
run "$R" stable "$WORK/other/oplite_1.0_amd64.deb"
if [ "$code" -eq 1 ] && grep -q 'version already published: ' "$WORK/stderr" &&
	[ "$(cat "$R/pool/stable/oplite_1.0_amd64.deb")" = deb1 ] && [ ! -s "$WORK/calls" ]; then
	ok "같은 이름의 다른 deb는 거절"
else
	fail "다시 올린 deb: 종료 코드 $code, $(cat "$WORK/stderr")"
fi
run "$R" stable "$WORK/oplite_1.0_amd64.deb"
if [ "$code" -eq 0 ]; then
	ok "같은 deb는 넘어간다"
else
	fail "같은 deb: 종료 코드 $code, $(cat "$WORK/stderr")"
fi

# 서명이 실패하면 실패하고, 색인과 서명은 옛 것 그대로다.
INDEX="Release InRelease Release.gpg stable/binary-amd64/Packages stable/binary-amd64/Packages.gz"
for f in $INDEX; do
	mkdir -p "$WORK/before/$(dirname "$f")"
	cp "$D/$f" "$WORK/before/$f"
done
unchanged() {
	for f in $INDEX; do
		cmp -s "$WORK/before/$f" "$D/$f" || return 1
	done
	[ ! -e "$R/dists/.trixie.new" ] && [ ! -e "$R/dists/.Release.new" ]
}
printf 'deb3' >"$WORK/oplite_1.2_amd64.deb"
cp "$WORK/fakebin/gpg" "$WORK/fakebin/gpg.ok"
printf '#!/bin/sh\nexit 3\n' >"$WORK/fakebin/gpg"
run "$R" stable "$WORK/oplite_1.2_amd64.deb"
if [ "$code" -ne 0 ] && unchanged; then
	ok "서명이 실패하면 실패하고 옛 색인과 서명이 남는다"
else
	fail "서명 실패: 종료 코드 $code, $(find "$R/dists" | sort)"
fi
# 둘째 서명(Release.gpg)만 실패해도 같다.
cat >"$WORK/fakebin/gpg" <<'EOF'
#!/bin/sh
for a; do [ "$a" = --detach-sign ] && exit 3; done
exec sh "$(dirname "$0")/gpg.ok" "$@"
EOF
run "$R" stable "$WORK/oplite_1.2_amd64.deb"
if [ "$code" -ne 0 ] && unchanged; then
	ok "Release.gpg 서명만 실패해도 옛 InRelease가 남는다"
else
	fail "Release.gpg 실패: 종료 코드 $code, $(find "$R/dists" | sort)"
fi

if [ "$FAILED" -ne 0 ]; then
	echo "build-repo-test: 실패"
	exit 1
fi
echo "build-repo-test: 모두 통과"
```

`packaging/tests/deb-test.sh`에서

```sh

# purge: enable 링크는 지우고, 데이터(/var/lib/opl)와 설정(/etc/opl)은 남긴다.
mkdir -p /var/lib/opl /etc/opl
echo keep >/var/lib/opl/deb-test.marker
echo keep >/etc/opl/deb-test.marker
dpkg --purge oplite
LINK=/etc/systemd/system/multi-user.target.wants/opld.service
```

을 다음으로 바꾼다:

```sh

# purge: enable 링크와 opld가 쓴 APT 소스 파일은 지우고, 데이터(/var/lib/opl)와 설정(/etc/opl)은 남긴다.
mkdir -p /var/lib/opl /etc/opl
echo keep >/var/lib/opl/deb-test.marker
echo keep >/etc/opl/deb-test.marker
SOURCES=/etc/apt/sources.list.d/oplite.sources
printf '# managed by opld; edits are overwritten\nTypes: deb\n' >"$SOURCES"
dpkg --purge oplite
[ ! -e "$SOURCES" ] || fail "purge 뒤 opld가 쓴 소스 파일이 남음"
LINK=/etc/systemd/system/multi-user.target.wants/opld.service
```

`packaging/tests/maint-test.sh`에서

```sh
	fi
	# OPL_SYSTEMD_ETC: postrm purge가 지우는 enable 링크가 시험 디렉터리 안에 있게 한다.
	if ! PATH="$WORK/bin:$PATH" OPL_FAKE_LOG="$log" OPL_SYSTEMD_DIR="$systemd_dir" \
		OPL_SYSTEMD_ETC="$WORK/etc-systemd" "$DEB_DIR/$1" "${@:2}"; then
		echo "FAIL $name: 스크립트가 0이 아닌 코드로 끝났다"
```

을 다음으로 바꾼다:

```sh
	fi
	# OPL_SYSTEMD_ETC·OPL_APT_SOURCES: postrm이 지우는 enable 링크와 소스 파일이 시험 디렉터리 안에 있게 한다.
	if ! PATH="$WORK/bin:$PATH" OPL_FAKE_LOG="$log" OPL_SYSTEMD_DIR="$systemd_dir" \
		OPL_SYSTEMD_ETC="$WORK/etc-systemd" OPL_APT_SOURCES="$WORK/oplite.sources" "$DEB_DIR/$1" "${@:2}"; then
		echo "FAIL $name: 스크립트가 0이 아닌 코드로 끝났다"
```

`packaging/tests/maint-test.sh`에서

```sh

# 가짜 systemctl이 실패해도 maintainer 스크립트는 성공해야 한다(|| true).
```

을 다음으로 바꾼다:

```sh

# remove·purge는 opld가 쓴 소스 파일(관리 표시)을 지우고, 관리자가 쓴 파일과 업그레이드 때는 남긴다.
SOURCES="$WORK/oplite.sources"
managed() { printf '# managed by opld; edits are overwritten\nTypes: deb\n' >"$SOURCES"; }
own() { printf 'Types: deb\nURIs: http://mirror.lan/oplite\n' >"$SOURCES"; }
for action in remove purge; do
	managed
	check "postrm $action(관리 소스 파일)" no "$([ "$action" = purge ] && echo "disable opld.service")" postrm "$action"
	if [ ! -e "$SOURCES" ]; then echo "ok   $action 뒤 관리 소스 파일 없음"; else
		echo "FAIL $action 뒤 관리 소스 파일이 남았다"
		FAILED=1
	fi
	own
	check "postrm $action(관리자 소스 파일)" no "$([ "$action" = purge ] && echo "disable opld.service")" postrm "$action"
	if grep -q mirror.lan "$SOURCES"; then echo "ok   $action 뒤 관리자 소스 파일 남음"; else
		echo "FAIL $action가 관리자 소스 파일을 지웠다"
		FAILED=1
	fi
done
managed
check "postrm upgrade(소스 파일 둠)" no "" postrm upgrade 0.2.0
if [ -e "$SOURCES" ]; then echo "ok   upgrade 뒤 소스 파일 남음"; else
	echo "FAIL upgrade가 소스 파일을 지웠다"
	FAILED=1
fi
rm -f "$SOURCES"

# 가짜 systemctl이 실패해도 maintainer 스크립트는 성공해야 한다(|| true).
```

`packaging/tests/repo-test.sh`를 만든다:

```sh
#!/bin/bash
# build-repo.sh로 만든 저장소를 진짜 apt가 읽는지 Debian 13 컨테이너 안에서 본다(root).
# 임시 GPG 키로 받은 deb를 stable에, 버전을 <v>+1로 고친 사본을 testing에 넣고, file:// 소스와 Signed-By로
# stable에서 oplite를 설치한 뒤 소스를 testing으로 바꾸면 apt-get -s dist-upgrade가 새 버전을 보이는지 본다.
# 쓰는 법(CI): docker run --rm -v "$PWD:/work" debian:13 bash /work/packaging/tests/repo-test.sh /work/<deb>
set -eu

[ $# -eq 1 ] || {
	echo "usage: $0 <oplite deb>" >&2
	exit 2
}
DEB=$1
REPO_SCRIPT="$(cd "$(dirname "$0")/../repo" && pwd)/build-repo.sh"

fail() {
	echo "repo-test: FAIL: $*" >&2
	exit 1
}

export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends apt-utils gnupg gpgv

# apt의 _apt 사용자도 저장소와 키링을 읽도록 0755 디렉터리에 둔다.
WORK=$(mktemp -d)
chmod 0755 "$WORK"
trap 'rm -rf "$WORK"' EXIT
REPO="$WORK/repo"
KEYRING="$WORK/oplite-archive-keyring.gpg"

# 임시 서명 키(암호 없음)와 apt가 읽을 키링
export GNUPGHOME="$WORK/gnupg"
mkdir -m 0700 "$GNUPGHOME"
gpg --batch --passphrase '' --quick-generate-key 'opLite repo test <repo-test@oplite.invalid>' ed25519 sign never
KEY_ID=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
[ -n "$KEY_ID" ] || fail "임시 키 지문을 못 찾음"
gpg --batch --export "$KEY_ID" >"$KEYRING"
chmod 0644 "$KEYRING"

# stable: 받은 deb
VERSION=$(dpkg-deb -f "$DEB" Version)
OPL_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" stable "$DEB"

# testing: 버전만 <v>+1로 고친 사본
NEW_VERSION="$VERSION+1"
dpkg-deb -R "$DEB" "$WORK/pkg"
sed -i "s/^Version: .*/Version: $NEW_VERSION/" "$WORK/pkg/DEBIAN/control"
NEW_DEB="$WORK/oplite_${NEW_VERSION}_amd64.deb"
dpkg-deb --build --root-owner-group "$WORK/pkg" "$NEW_DEB"
OPL_REPO_GPG_KEY_ID=$KEY_ID sh "$REPO_SCRIPT" "$REPO" testing "$NEW_DEB"

# 서명과 Release 값
gpgv --keyring "$KEYRING" "$REPO/dists/trixie/InRelease" || fail "InRelease 서명"
gpgv --keyring "$KEYRING" "$REPO/dists/trixie/Release.gpg" "$REPO/dists/trixie/Release" || fail "Release.gpg 서명"
grep -qx 'Suite: trixie' "$REPO/dists/trixie/Release" || fail "Release Suite"
grep -qx 'Codename: trixie' "$REPO/dists/trixie/Release" || fail "Release Codename"
grep -qx 'Components: stable testing' "$REPO/dists/trixie/Release" || fail "Release Components"
grep -qx 'Architectures: amd64' "$REPO/dists/trixie/Release" || fail "Release Architectures"

# write_sources <channel>: opld가 쓰는 것과 같은 모양의 deb822 소스 파일(URIs만 file://)
SOURCES=/etc/apt/sources.list.d/oplite.sources
write_sources() {
	cat >"$SOURCES" <<EOF
# managed by opld; edits are overwritten
Types: deb
URIs: file://$REPO
Suites: trixie
Components: $1
Signed-By: $KEYRING
EOF
}

# stable에서 설치
write_sources stable
apt-get update
apt-get install -y --no-install-recommends oplite
[ "$(dpkg-query -W -f '${Version}' oplite)" = "$VERSION" ] || fail "설치된 버전: $(dpkg-query -W -f '${Version}' oplite)"

# testing으로 바꾸면 새 버전이 보인다(opld의 parse_simulate가 읽는 줄 모양).
write_sources testing
apt-get update
LANG=C apt-get -s -q dist-upgrade | tee "$WORK/simulate.txt"
grep -qF "Inst oplite [$VERSION] ($NEW_VERSION " "$WORK/simulate.txt" || fail "dist-upgrade에 oplite $NEW_VERSION 없음"

echo "repo-test: 모두 통과"
```

- [ ] **Step 2: 실행 권한**

Run: `chmod 0755 packaging/repo/build-repo.sh packaging/tests/build-repo-test.sh packaging/tests/repo-test.sh`
Expected: 출력 없음

- [ ] **Step 3: 확인**

Run: `bash packaging/tests/maint-test.sh && bash packaging/tests/build-deb-test.sh && bash packaging/tests/build-repo-test.sh && bash packaging/tests/setup-test.sh && bash packaging/tests/iso-test.sh`
Expected: 모두 통과

Run: `shellcheck packaging/**/*.sh packaging/deb/postinst packaging/deb/prerm packaging/deb/postrm`
Expected: 경고 없음 (bash는 `shopt -s globstar` 뒤에, shellcheck가 없으면 CI `shell` 작업이 본다)

Run: `ruby -ryaml -e 'ARGV.each { |f| YAML.load_file(f) }' .github/workflows/*.yml .github/actions/build-deb/action.yml`
Expected: 오류 없음

- [ ] **Step 4: 커밋**

```bash
git add .github packaging
git commit -m "feat(packaging): APT 저장소 스크립트와 패키지 키링 — packaging/repo/build-repo.sh <repo dir> <stable|testing|enterprise> <deb…>(POSIX sh, OPL_REPO_GPG_KEY_ID 필수, APT_FTPARCHIVE·GPG로 도구 경로를 바꿈)가 pool/<channel>/에 패키지를 더하고 그 채널의 dists/trixie/<channel>/binary-amd64/Packages(.gz)만 저장소 루트에서 apt-ftparchive packages로 다시 만들고(다른 채널은 그대로, pool의 옛 버전도 남김), 있는 모든 채널을 묶은 dists/trixie/Release(Origin·Label opLite, Suite·Codename trixie, Components는 stable testing enterprise 순서, Architectures amd64)를 만들어 gpg --clearsign(InRelease)·--armor --detach-sign(Release.gpg)으로 서명, build-deb.sh는 OPL_APT_KEY 파일이 있으면 /usr/share/keyrings/oplite-archive-keyring.gpg(0644)로 싣고 없는 파일이면 실패, 시험 build-repo-test.sh(가짜 apt-ftparchive·gpg로 인자 검사·레이아웃·Release 값·서명 인자·채널 보존·enterprise·서명 실패)와 build-deb-test.sh의 키링 경우, repo-test.sh(Debian 13 컨테이너: 임시 GPG 키, 받은 deb를 stable에, 버전을 <v>+1로 고친 사본을 testing에, gpgv로 서명 확인, file:// 소스와 Signed-By로 설치, testing으로 바꾸면 apt-get -s dist-upgrade에 Inst oplite [<v>] (<v>+1 줄), ci.yml shell 작업에 build-repo-test, package 작업에 저장소 시험"
```

---

### Task 6: 웹: 노드 버전·업데이트와 설정 화면

노드 표의 버전 열과 업데이트 배지, 노드 업데이트 대화상자(패키지 표, 다시 확인, 확인을 거치는 업그레이드 — 노드 상세 페이지가 없으므로 대화상자로), 대시보드의 혼합 버전 경고(Debian 규칙 비교 `compareVersions`), 새 페이지 `/settings`(업데이트 채널), 로그인 화면의 서버 버전, Task 라벨, 오류 문구(ko·en).

**Files:**
- Create: `web/src/lib/version.test.ts`
- Create: `web/src/lib/version.ts`
- Create: `web/src/nodes/NodeUpdatesDialog.tsx`
- Create: `web/src/settings/SettingsPage.test.tsx`
- Create: `web/src/settings/SettingsPage.tsx`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/LiveProvider.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/nodes/NodesPage.test.tsx`
- Modify: `web/src/nodes/NodesPage.tsx`
- Modify: `web/src/pages/DashboardPage.test.tsx`
- Modify: `web/src/pages/DashboardPage.tsx`
- Modify: `web/src/pages/LoginPage.tsx`
- Modify: `web/src/tasks/TasksPage.test.tsx`
- Modify: `web/src/tasks/TasksPage.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`web/src/App.test.tsx`에서

```tsx
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
    http.get(apiUrl("/cluster"), () =>
```

을 다음으로 바꾼다:

```tsx
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
    http.get(apiUrl("/version"), () => HttpResponse.json({ version: "0.9.0~ci7" })),
    http.get(apiUrl("/cluster"), () =>
```

`web/src/App.test.tsx`에서

```tsx
  expect(screen.getByRole("heading", { level: 1, name: "opLite" })).toBeInTheDocument();
  await user.type(screen.getByLabelText("Username"), "admin");
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("heading", { level: 1, name: "opLite" })).toBeInTheDocument();
  // 로그인하기 전에도 서버 버전이 보인다.
  expect(await screen.findByText("opLite 0.9.0~ci7")).toBeInTheDocument();
  await user.type(screen.getByLabelText("Username"), "admin");
```

`web/src/App.tsx`에서

```tsx
import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { PoolsPage } from "@/storage/PoolsPage";
```

을 다음으로 바꾼다:

```tsx
import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { SettingsPage } from "@/settings/SettingsPage";
import { PoolsPage } from "@/storage/PoolsPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "cluster", element: <ClusterPage /> },
            { path: "*", element: <NotFoundPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "cluster", element: <ClusterPage /> },
            { path: "settings", element: <SettingsPage /> },
            { path: "*", element: <NotFoundPage /> },
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx

import { listKey, useList } from "@/api/live";
import { LiveProvider, useLiveStatus } from "@/api/LiveProvider";
```

을 다음으로 바꾼다:

```tsx

import { listKey, useList, useSettings } from "@/api/live";
import { LiveProvider, useLiveStatus } from "@/api/LiveProvider";
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup,backup-job,vm-import&since=7",
  );
});
```

을 다음으로 바꾼다:

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup,backup-job,vm-import,settings&since=7",
  );
});

function settings(revision: number, channel: string) {
  return {
    id: "s1",
    name: "cluster",
    revision,
    spec: { updates: { channel } },
    status: {},
  };
}

function SettingsProbe() {
  const status = useLiveStatus();
  const current = useSettings();
  const vms = useList("vm");
  return (
    <p>
      {status}: {current.data?.spec.updates?.channel} {vms.data?.map((v) => v.status.phase)}
    </p>
  );
}

it("reads the settings again on every connection and applies newer ones", async () => {
  let reads = 0;
  server.use(
    ...lists([], 7),
    http.get(apiUrl("/settings"), () => {
      reads += 1;
      return HttpResponse.json(settings(5, "stable"));
    }),
    watch.addEventListener("connection", ({ client }) => {
      client.send(
        JSON.stringify({ type: "put", kind: "settings", resource: settings(9, "testing") }),
      );
      // 가진 것보다 오래된 설정은 버린다.
      client.send(JSON.stringify({ type: "put", kind: "settings", resource: settings(4, "x") }));
      client.send(JSON.stringify({ type: "put", kind: "vm", resource: vm("web", 9, "running") }));
    }),
  );
  render(
    <QueryClientProvider client={createQueryClient()}>
      <LiveProvider>
        <SettingsProbe />
      </LiveProvider>
    </QueryClientProvider>,
  );
  // 마지막 이벤트(VM)까지 반영한 뒤에도 새 설정이 남는다.
  expect(await screen.findByText("live: testing running")).toBeInTheDocument();
  // 연결하면서 다시 읽었다.
  expect(reads).toBeGreaterThanOrEqual(2);
});
```

`web/src/api/LiveProvider.tsx`에서

```tsx

import { applyEvent, fetchList, KINDS, type List, listKey, type WatchEvent } from "@/api/live";
```

을 다음으로 바꾼다:

```tsx

import {
  applyEvent,
  fetchList,
  KINDS,
  type List,
  listKey,
  SETTINGS_KEY,
  SETTINGS_KIND,
  type WatchEvent,
} from "@/api/live";
import type { Settings } from "@/api/types";
```

`web/src/api/LiveProvider.tsx`에서

```tsx
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/v1/watch?kinds=${KINDS.join(",")}&since=${since}`;
}
```

을 다음으로 바꾼다:

```tsx
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/v1/watch?kinds=${[...KINDS, SETTINGS_KIND].join(",")}&since=${since}`;
}
```

`web/src/api/LiveProvider.tsx`에서

```tsx
        since = Math.min(...lists.map((list) => list.revision));
      } catch {
```

을 다음으로 바꾼다:

```tsx
        since = Math.min(...lists.map((list) => list.revision));
        // 설정은 목록이 없으므로 watch를 열기 전에 다시 읽는다(보는 화면이 있을 때만).
        await queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
      } catch {
```

`web/src/api/LiveProvider.tsx`에서

```tsx
          case "deleted":
            queryClient.setQueryData<List<{ id: string; revision: number }>>(
```

을 다음으로 바꾼다:

```tsx
          case "deleted":
            // 설정은 지우지 않는다. 읽은 것보다 새것일 때만 바꾼다.
            if (event.kind === SETTINGS_KIND) {
              const resource = event.resource as Settings;
              if (event.type === "put") {
                queryClient.setQueryData<Settings>(SETTINGS_KEY, (current) =>
                  current && current.revision >= resource.revision ? current : resource,
                );
              }
              break;
            }
            queryClient.setQueryData<List<{ id: string; revision: number }>>(
```

`web/src/api/live.ts`에서

```ts

/** 목록 응답. `revision`은 목록을 읽은 시점의 클러스터 revision이다. */
```

을 다음으로 바꾼다:

```ts

/**
 * 목록 없이 watch로만 받는 클러스터 설정(`Settings`, 늘 하나). 캐시 키는 `SETTINGS_KEY`이고, 연결할 때마다
 * `LiveProvider`가 다시 읽는다.
 */
export const SETTINGS_KIND = "settings";
export const SETTINGS_KEY = ["settings"] as const;

/** 목록 응답. `revision`은 목록을 읽은 시점의 클러스터 revision이다. */
```

`web/src/api/live.ts`에서

```ts
export type WatchEvent =
  | { type: "put"; kind: WatchKind; resource: Versioned }
  | { type: "deleted"; revision: number; kind: WatchKind; resource: Versioned }
  | { type: "reset"; revision: number }
```

을 다음으로 바꾼다:

```ts
export type WatchEvent =
  | { type: "put"; kind: WatchKind | typeof SETTINGS_KIND; resource: Versioned }
  | {
      type: "deleted";
      revision: number;
      kind: WatchKind | typeof SETTINGS_KIND;
      resource: Versioned;
    }
  | { type: "reset"; revision: number }
```

`web/src/api/live.ts`에서

```ts
  });
}
```

을 다음으로 바꾼다:

```ts
  });
}

/** 클러스터 설정. 아직 저장한 적이 없으면 서버가 기본값(revision 0)을 돌려준다. */
export function useSettings() {
  return useQuery({
    queryKey: SETTINGS_KEY,
    queryFn: () => call(api.GET("/settings")),
    staleTime: Infinity,
  });
}
```

`web/src/api/types.ts`에서

```ts
export type Role = Schemas["Role"];
```

을 다음으로 바꾼다:

```ts
export type Role = Schemas["Role"];
export type Settings = Schemas["Resource_SettingsSpec_SettingsStatus"];
export type Channel = Schemas["Channel"];
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("operator", "write", "vm-import")).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("operator", "write", "vm-import")).toBe(true);
  // 클러스터 설정은 누구나 읽고 admin만 바꾼다.
  expect(can("viewer", "read", "settings")).toBe(true);
  expect(can("operator", "write", "settings")).toBe(false);
  expect(can("admin", "write", "settings")).toBe(true);
});
```

`web/src/auth/permissions.ts`에서

```ts
/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind = WatchKind | "user" | "api-token" | "join-token";
```

을 다음으로 바꾼다:

```ts
/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind = WatchKind | "settings" | "user" | "api-token" | "join-token";
```

`web/src/i18n/en.json`에서

```json
    "esxi_auth": "ESXi rejected the user name or password. Check them and try again.",
    "esxi_unreachable": "Cannot reach the ESXi host ({{message}})."
  },
```

을 다음으로 바꾼다:

```json
    "esxi_auth": "ESXi rejected the user name or password. Check them and try again.",
    "esxi_unreachable": "Cannot reach the ESXi host ({{message}}).",
    "version_too_old": "The node runs an older opLite than the cluster. Upgrade it first: {{message}}",
    "updates_unsupported": "The node does not check for updates.",
    "upgrade_in_progress": "An upgrade is in progress: {{message}}",
    "cluster_upgrading": "The cluster is being upgraded: {{message}}"
  },
```

`web/src/i18n/en.json`에서

```json
    "tokens": "API tokens",
    "cluster": "Cluster"
  },
```

을 다음으로 바꾼다:

```json
    "tokens": "API tokens",
    "cluster": "Cluster",
    "settings": "Settings"
  },
```

`web/src/i18n/en.json`에서

```json
    "password": "Password",
    "submit": "Sign in"
  },
```

을 다음으로 바꾼다:

```json
    "password": "Password",
    "submit": "Sign in",
    "version": "opLite {{version}}"
  },
```

`web/src/i18n/en.json`에서

```json
      "vm-import": "Import VM {{target}}",
      "vm-import-delete": "Delete import {{target}}"
    }
```

을 다음으로 바꾼다:

```json
      "vm-import": "Import VM {{target}}",
      "vm-import-delete": "Delete import {{target}}",
      "node-update-check": "Check updates on {{target}}",
      "node-upgrade": "Upgrade {{target}}"
    }
```

`web/src/i18n/en.json`에서

```json
      "process": "Process"
    }
```

을 다음으로 바꾼다:

```json
      "process": "Process"
    },
    "version": "Version",
    "updates": {
      "column": "Updates",
      "open": "Updates for {{name}}: {{state}}",
      "stateCount": "{{n}} available",
      "stateFailed": "check failed",
      "stateNotChecked": "not checked yet",
      "menu": "Updates…",
      "title": "Updates for {{name}}",
      "checked": "Last checked {{time}}.",
      "notChecked": "Not checked yet.",
      "none": "No updates available.",
      "failed": "The last check failed:",
      "failedBadge": "Check failed",
      "package": "Package",
      "current": "Installed",
      "candidate": "Candidate",
      "newPackage": "(new)",
      "truncated": "Only the first {{count}} packages are shown.",
      "refresh": "Check again",
      "checking": "Checking…",
      "upgrade": "Upgrade",
      "upgrading": "Upgrading…",
      "upgradeTitle": "Upgrade {{name}}?",
      "upgradeDescription": "VMs keep running, but opld restarts briefly. Upgrade other nodes after this upgrade finishes.",
      "errors": {
        "node_offline": "The node is offline."
      }
    }
```

`web/src/i18n/en.json`에서

```json
    "noLeader": "No leader",
    "noNodes": "No nodes."
  }
```

을 다음으로 바꾼다:

```json
    "noLeader": "No leader",
    "noNodes": "No nodes.",
    "mixedVersions": "Nodes run different opLite versions ({{versions}}). Finish the upgrade on every node."
  },
  "settings": {
    "updates": "Updates",
    "channel": "Update channel",
    "channelHint": "Every node gets opLite packages from this channel. Testing gets new versions before stable.",
    "channels": {
      "stable": "Stable",
      "testing": "Testing"
    },
    "saved": "Settings saved.",
    "adminOnly": "Only administrators can change the settings."
  }
```

`web/src/i18n/ko.json`에서

```json
    "esxi_auth": "ESXi가 사용자 이름이나 비밀번호를 거절했습니다. 확인하고 다시 시도하세요.",
    "esxi_unreachable": "ESXi 호스트에 연결할 수 없습니다({{message}})."
  },
```

을 다음으로 바꾼다:

```json
    "esxi_auth": "ESXi가 사용자 이름이나 비밀번호를 거절했습니다. 확인하고 다시 시도하세요.",
    "esxi_unreachable": "ESXi 호스트에 연결할 수 없습니다({{message}}).",
    "version_too_old": "노드의 opLite가 클러스터보다 오래됐습니다. 먼저 업그레이드하세요: {{message}}",
    "updates_unsupported": "이 노드는 업데이트를 확인하지 않습니다.",
    "upgrade_in_progress": "업그레이드가 진행 중입니다: {{message}}",
    "cluster_upgrading": "클러스터를 업그레이드하는 중입니다: {{message}}"
  },
```

`web/src/i18n/ko.json`에서

```json
    "tokens": "API 토큰",
    "cluster": "클러스터"
  },
```

을 다음으로 바꾼다:

```json
    "tokens": "API 토큰",
    "cluster": "클러스터",
    "settings": "설정"
  },
```

`web/src/i18n/ko.json`에서

```json
    "password": "비밀번호",
    "submit": "로그인"
  },
```

을 다음으로 바꾼다:

```json
    "password": "비밀번호",
    "submit": "로그인",
    "version": "opLite {{version}}"
  },
```

`web/src/i18n/ko.json`에서

```json
      "vm-import": "VM {{target}} 가져오기",
      "vm-import-delete": "가져오기 {{target}} 삭제"
    }
```

을 다음으로 바꾼다:

```json
      "vm-import": "VM {{target}} 가져오기",
      "vm-import-delete": "가져오기 {{target}} 삭제",
      "node-update-check": "{{target}} 업데이트 확인",
      "node-upgrade": "{{target}} 업그레이드"
    }
```

`web/src/i18n/ko.json`에서

```json
      "process": "프로세스"
    }
```

을 다음으로 바꾼다:

```json
      "process": "프로세스"
    },
    "version": "버전",
    "updates": {
      "column": "업데이트",
      "open": "{{name}}의 업데이트: {{state}}",
      "stateCount": "{{n}}개",
      "stateFailed": "확인 실패",
      "stateNotChecked": "아직 확인하지 않음",
      "menu": "업데이트…",
      "title": "{{name}}의 업데이트",
      "checked": "마지막 확인: {{time}}",
      "notChecked": "아직 확인하지 않았습니다.",
      "none": "올릴 업데이트가 없습니다.",
      "failed": "마지막 확인이 실패했습니다:",
      "failedBadge": "확인 실패",
      "package": "패키지",
      "current": "설치됨",
      "candidate": "새 버전",
      "newPackage": "(새 패키지)",
      "truncated": "앞의 {{count}}개만 보입니다.",
      "refresh": "다시 확인",
      "checking": "확인 중…",
      "upgrade": "업그레이드",
      "upgrading": "업그레이드 중…",
      "upgradeTitle": "{{name}}을(를) 업그레이드할까요?",
      "upgradeDescription": "VM은 계속 돌지만 opld가 잠깐 다시 시작합니다. 다른 노드는 이 업그레이드가 끝난 뒤에 하세요.",
      "errors": {
        "node_offline": "노드가 오프라인입니다."
      }
    }
```

`web/src/i18n/ko.json`에서

```json
    "noLeader": "리더 없음",
    "noNodes": "노드가 없습니다."
  }
```

을 다음으로 바꾼다:

```json
    "noLeader": "리더 없음",
    "noNodes": "노드가 없습니다.",
    "mixedVersions": "노드들이 서로 다른 opLite 버전({{versions}})을 돌립니다. 모든 노드의 업그레이드를 끝내세요."
  },
  "settings": {
    "updates": "업데이트",
    "channel": "업데이트 채널",
    "channelHint": "모든 노드가 이 채널에서 opLite 패키지를 받습니다. testing은 stable보다 새 버전을 먼저 받습니다.",
    "channels": {
      "stable": "stable(안정)",
      "testing": "testing(시험)"
    },
    "saved": "설정을 저장했습니다.",
    "adminOnly": "설정은 관리자만 바꿀 수 있습니다."
  }
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.getByRole("link", { name: "Imports" })).toHaveAttribute("href", "/imports");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("link", { name: "Imports" })).toHaveAttribute("href", "/imports");
  // 설정도 누구나 읽는다(바꾸기는 admin만).
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute("href", "/settings");
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Server,
  Shield,
```

을 다음으로 바꾼다:

```tsx
  Server,
  Settings,
  Shield,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/cluster", key: "nav.cluster", icon: Boxes, end: false },
];
```

을 다음으로 바꾼다:

```tsx
  { to: "/cluster", key: "nav.cluster", icon: Boxes, end: false },
  { to: "/settings", key: "nav.settings", icon: Settings, end: false },
];
```

`web/src/lib/version.test.ts`를 만든다:

```ts
import { compareVersions } from "@/lib/version";

// opl_types::version의 비교 표와 같다.
it.each([
  ["1.2.3", "1.2.3", 0],
  ["1.01", "1.1", 0],
  ["1.0", "1.0-0", 0],
  ["0:1.0", "1.0", 0],
  ["0.10.0", "0.9.0", 1],
  ["1.0~ci12", "1.0~ci5", 1],
  ["1.2", "1.2.1", -1],
  ["1.0~rc1", "1.0", -1],
  ["1.0~~", "1.0~", -1],
  ["1.0", "1.0a", -1],
  ["1.0a", "1.0+", -1],
  ["1.0", "1.0+1", -1],
  ["1.0b", "1.0a", 1],
  ["1:0.1", "2.0", 1],
  ["2:1.0", "10:0.1", -1],
  ["1.0-1", "1.0-2", -1],
  ["1.0-10", "1.0-9", 1],
  ["1.0-rc-2", "1.0-rc-10", -1],
  ["1.0-1", "1.0", 1],
  ["99999999999999999999999", "99999999999999999999998", 1],
])("compares %s and %s", (a, b, want) => {
  expect(compareVersions(a, b)).toBe(want);
  expect(compareVersions(b, a)).toBe(-want || 0);
});

it("compares odd strings without throwing", () => {
  for (const a of ["", ":", "a:b", "-", "1:", "~", "1.0-", ":-:", "é1", "1:2:3"]) {
    for (const b of ["", "1.0", "~", a]) {
      expect(Math.abs(compareVersions(a, b))).toBeLessThanOrEqual(1);
    }
    expect(compareVersions(a, a)).toBe(0);
  }
  expect(compareVersions("", "1")).toBe(-1);
  expect(compareVersions("~", "")).toBe(-1);
});
```

`web/src/lib/version.ts`를 만든다:

```ts
/** 비숫자 부분 문자의 순서: `~` < 끝·숫자 < 문자 < 그 밖 */
function order(c: number | undefined): number {
  if (c === undefined) return 0;
  if (c === 0x7e) return -1; // ~
  if (isDigit(c)) return 0;
  if ((c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a)) return c;
  return c + 256;
}

function isDigit(c: number | undefined): boolean {
  return c !== undefined && c >= 0x30 && c <= 0x39;
}

/** 숫자 문자열을 수로 비교한다(앞 0 무시, 길이에 제한 없음). */
function compareNumber(a: string, b: string): number {
  const x = a.replace(/^0+/, "");
  const y = b.replace(/^0+/, "");
  if (x.length !== y.length) return x.length < y.length ? -1 : 1;
  return x < y ? -1 : x > y ? 1 : 0;
}

/** 업스트림·리비전을 비숫자 부분과 숫자 부분을 번갈아 비교한다(dpkg `verrevcmp`). */
function comparePart(a: Uint8Array, b: Uint8Array): number {
  let i = 0;
  let j = 0;
  const digits = (s: Uint8Array, from: number): [string, number] => {
    let end = from;
    while (isDigit(s[end])) end++;
    return [String.fromCharCode(...s.subarray(from, end)), end];
  };
  while (i < a.length || j < b.length) {
    while ((i < a.length && !isDigit(a[i])) || (j < b.length && !isDigit(b[j]))) {
      const x = order(a[i]);
      const y = order(b[j]);
      if (x !== y) return x < y ? -1 : 1;
      i++;
      j++;
    }
    const [x, ni] = digits(a, i);
    const [y, nj] = digits(b, j);
    const ord = compareNumber(x, y);
    if (ord !== 0) return ord;
    i = ni;
    j = nj;
  }
  return 0;
}

/** 에포크(숫자가 아니면 없는 것으로 본다), 업스트림, 리비전(마지막 `-` 뒤) */
function split(v: string): [string, string, string] {
  let epoch = "0";
  let rest = v;
  const colon = v.indexOf(":");
  if (colon > 0 && /^[0-9]+$/.test(v.slice(0, colon))) {
    epoch = v.slice(0, colon);
    rest = v.slice(colon + 1);
  }
  const dash = rest.lastIndexOf("-");
  return dash < 0 ? [epoch, rest, ""] : [epoch, rest.slice(0, dash), rest.slice(dash + 1)];
}

/**
 * 두 버전을 Debian 규칙으로 비교한다(`opl_types::version::compare`와 같다): `[에포크:]업스트림[-리비전]`,
 * `~`는 끝보다도 낮다. 형식에 맞지 않는 문자열도 같은 규칙으로 비교한다.
 */
export function compareVersions(a: string, b: string): number {
  const bytes = (s: string) => new TextEncoder().encode(s);
  const [ae, au, ar] = split(a);
  const [be, bu, br] = split(b);
  return (
    compareNumber(ae, be) || comparePart(bytes(au), bytes(bu)) || comparePart(bytes(ar), bytes(br))
  );
}
```

`web/src/nodes/NodeUpdatesDialog.tsx`를 만든다:

```tsx
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { ApiError, api, call } from "@/api/client";
import { useList } from "@/api/live";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { focusBack } from "@/components/focusBack";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { codeMessage, errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

/** 노드의 오류 문구(`node_offline`은 VM 문구가 아니라 노드 문구로) */
function nodeError(t: Parameters<typeof errorMessage>[0], err: unknown): string {
  return err instanceof ApiError
    ? codeMessage(t, err.code, err.message, "nodes.updates.errors")
    : errorMessage(t, err);
}

/**
 * 노드의 업데이트 확인 결과. admin은 다시 확인하고 업그레이드한다. 결과는 watch로 바뀐 Node를 따라간다.
 * 연 쪽은 열 때만 그린다.
 */
export function NodeUpdatesDialog({
  node: opened,
  onClose,
  returnFocus,
}: {
  node: Node;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const nodes = useList("node");
  const node = nodes.data?.find((n) => n.id === opened.id) ?? opened;
  const tasks = useTaskIndex().get(node.id) ?? [];
  const checking = tasks.some((task) => task.spec.op.kind === "node-update-check");
  // 업그레이드가 apt를 쓰는 동안은 다시 확인도 새 업그레이드도 하지 않는다(서버도 409).
  const upgradeRunning = tasks.some((task) => task.spec.op.kind === "node-upgrade");
  const write = can(me.role, "write", "node");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [upgrading, setUpgrading] = useState(false);
  const upgradeButton = useRef<HTMLButtonElement>(null);
  const updates = node.status.updates;

  async function refresh() {
    setBusy(true);
    setError(null);
    try {
      const task = await call(
        api.POST("/nodes/{key}/updates/refresh", { params: { path: { key: node.id } } }),
      );
      trackTask(task, t("tasks.op.node-update-check", { target: node.name }));
    } catch (err) {
      setError(nodeError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
        <DialogContent
          aria-describedby={undefined}
          className="sm:max-w-2xl"
          onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
        >
          <DialogHeader>
            <DialogTitle>{t("nodes.updates.title", { name: node.name })}</DialogTitle>
          </DialogHeader>
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <p className="text-sm text-muted-foreground">
            {updates?.checked_at
              ? t("nodes.updates.checked", {
                  time: formatTime(new Date(updates.checked_at * 1000), i18n.language),
                })
              : t("nodes.updates.notChecked")}
          </p>
          {updates?.error ? (
            <div className="flex flex-col gap-1 text-sm">
              <p className="text-destructive">{t("nodes.updates.failed")}</p>
              <pre className="rounded-md bg-muted p-2 text-xs whitespace-pre-wrap">
                {updates.error}
              </pre>
            </div>
          ) : !updates?.checked_at ? null : updates.packages.length === 0 ? (
            <p className="text-sm">{t("nodes.updates.none")}</p>
          ) : (
            <div className="max-h-96 overflow-y-auto">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("nodes.updates.package")}</TableHead>
                    <TableHead>{t("nodes.updates.current")}</TableHead>
                    <TableHead>{t("nodes.updates.candidate")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {updates.packages.map((p) => (
                    <TableRow key={p.name}>
                      <TableCell className="font-medium">{p.name}</TableCell>
                      {/* 새 의존성은 설치된 버전이 없다. */}
                      <TableCell className="font-mono text-xs">
                        {p.current ?? t("nodes.updates.newPackage")}
                      </TableCell>
                      <TableCell className="font-mono text-xs">{p.candidate}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
          {updates?.truncated && (
            <p className="text-sm text-muted-foreground">
              {t("nodes.updates.truncated", { count: updates.packages.length })}
            </p>
          )}
          <DialogFooter>
            {write && (
              <>
                <Button
                  variant="outline"
                  disabled={busy || checking || upgradeRunning}
                  onClick={() => void refresh()}
                >
                  {t(checking ? "nodes.updates.checking" : "nodes.updates.refresh")}
                </Button>
                {/* 확인이 apt를 쓰는 동안도 업그레이드하지 않는다. */}
                <Button
                  ref={upgradeButton}
                  disabled={busy || checking || upgradeRunning}
                  onClick={() => setUpgrading(true)}
                >
                  {t(upgradeRunning ? "nodes.updates.upgrading" : "nodes.updates.upgrade")}
                </Button>
              </>
            )}
            <Button variant="outline" disabled={busy} onClick={onClose}>
              {t("common.done")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={upgrading}
        onOpenChange={setUpgrading}
        title={t("nodes.updates.upgradeTitle", { name: node.name })}
        description={t("nodes.updates.upgradeDescription")}
        confirmLabel={t("nodes.updates.upgrade")}
        returnFocus={upgradeButton}
        onConfirm={async () => {
          try {
            const task = await call(
              api.POST("/nodes/{key}/upgrade", { params: { path: { key: node.id } } }),
            );
            trackTask(task, t("tasks.op.node-upgrade", { target: node.name }));
          } catch (err) {
            throw new Error(nodeError(t, err), { cause: err });
          }
        }}
      />
    </>
  );
}
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
import { NodesPage } from "@/nodes/NodesPage";
import { NODE, poolState, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { NodesPage } from "@/nodes/NodesPage";
import { NODE, poolState, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/nodes/NodesPage.test.tsx`에서

```tsx
  await vi.waitFor(() => expect(within(a).getAllByRole("cell")[vmsColumn]).toHaveTextContent("—"));
});
```

을 다음으로 바꾼다:

```tsx
  await vi.waitFor(() => expect(within(a).getAllByRole("cell")[vmsColumn]).toHaveTextContent("—"));
});

/** 업데이트를 확인하는 노드. 기본은 2026-11-05 확인, 패키지 둘(하나는 새 의존성). */
function updatable(extra: Partial<(typeof NODE)["status"]["updates"] & object> = {}) {
  return {
    ...NODE,
    status: {
      ...NODE.status,
      version: "0.9.0",
      updates: {
        checked_at: 1_793_000_000,
        packages: [
          { name: "oplite", current: "0.9.0", candidate: "0.10.0" },
          { name: "libfoo1", current: null, candidate: "1.2-3" },
        ],
        error: null,
        ...extra,
      },
    },
  };
}

it("shows each node's version and how many updates it has", async () => {
  cluster();
  const failing = {
    ...DOWN,
    status: { ...DOWN.status, updates: { checked_at: null, packages: [], error: "apt-get: boom" } },
  };
  const witness = { ...NODE, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  mockLists({ node: [updatable(), failing, witness] });
  renderPage(<NodesPage />);
  const table = await screen.findByRole("table", { name: "Nodes" });
  const headers = within(table).getAllByRole("columnheader");
  const version = headers.findIndex((h) => h.textContent === "Version");
  const updates = headers.findIndex((h) => h.textContent === "Updates");
  // 버전은 주소 다음이다.
  expect(headers[version - 1]).toHaveTextContent("API address");
  const a = (await within(table).findByText("node-a")).closest("tr")!;
  expect(within(a).getAllByRole("cell")[version]).toHaveTextContent("0.9.0");
  expect(within(a).getAllByRole("cell")[updates]).toHaveTextContent("2");
  const b = within(table).getByText("node-b").closest("tr")!;
  expect(within(b).getAllByRole("cell")[version]).toHaveTextContent("—");
  expect(within(b).getAllByRole("cell")[updates]).toHaveTextContent("Check failed");
  // 업데이트를 확인하지 않는 노드(위트니스 등)
  const w = within(table).getByText("node-w").closest("tr")!;
  expect(within(w).getAllByRole("cell")[updates]).toHaveTextContent("—");
  expect(within(w).queryByRole("button", { name: /^Updates for node-w/ })).not.toBeInTheDocument();
  // 배지 버튼의 이름에 확인 상태가 들어간다.
  expect(within(a).getByRole("button", { name: "Updates for node-a: 2 available" })).toBeVisible();
  expect(within(b).getByRole("button", { name: "Updates for node-b: check failed" })).toBeVisible();
});

it("names the badge of a node that has not checked yet or has more updates", async () => {
  cluster();
  const more = {
    ...updatable({ truncated: true }),
    id: "n2",
    name: "node-b",
  };
  mockLists({ node: [updatable({ checked_at: null, packages: [] }), more] });
  renderPage(<NodesPage />);
  expect(
    await screen.findByRole("button", { name: "Updates for node-a: not checked yet" }),
  ).toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Updates for node-b: 2+ available" }),
  ).toBeInTheDocument();
});

it("lists the updates of a node", async () => {
  cluster();
  mockLists({ node: [updatable({ truncated: true })] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(dialog).toHaveTextContent("Last checked");
  const rows = within(within(dialog).getByRole("table")).getAllByRole("row");
  expect(rows.map((r) => r.textContent)).toEqual([
    "PackageInstalledCandidate",
    "oplite0.9.00.10.0",
    "libfoo1(new)1.2-3",
  ]);
  expect(dialog).toHaveTextContent("Only the first 2 packages are shown.");
});

it("shows why the last check failed", async () => {
  cluster();
  mockLists({ node: [updatable({ packages: [], error: "apt-get update: E: no network" })] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(dialog).toHaveTextContent("The last check failed:apt-get update: E: no network");
  expect(within(dialog).queryByRole("table")).not.toBeInTheDocument();
});

it("says when a node has not checked yet or has nothing to update", async () => {
  cluster();
  mockLists({ node: [updatable({ checked_at: null, packages: [] })] });
  const { unmount } = renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  expect(await screen.findByRole("dialog")).toHaveTextContent("Not checked yet.");
  unmount();

  mockLists({ node: [updatable({ packages: [] })] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  expect(await screen.findByRole("dialog")).toHaveTextContent("No updates available.");
});

it("checks again and upgrades from the row menu", async () => {
  cluster();
  mockLists({ node: [updatable()] });
  const posted: string[] = [];
  server.use(
    http.post(apiUrl("/nodes/n1/updates/refresh"), () => {
      posted.push("refresh");
      return HttpResponse.json(task("t1", "node-update-check", "n1"), { status: 202 });
    }),
    http.post(apiUrl("/nodes/n1/upgrade"), () => {
      posted.push("upgrade");
      return HttpResponse.json(task("t2", "node-upgrade", "n1"), { status: 202 });
    }),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Updates…" }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Check again" }));
  expect(await screen.findByText("Check updates on node-a")).toBeInTheDocument();

  await userEvent.click(within(dialog).getByRole("button", { name: "Upgrade" }));
  const confirm = await screen.findByRole("alertdialog", { name: "Upgrade node-a?" });
  expect(confirm).toHaveTextContent(
    "VMs keep running, but opld restarts briefly. Upgrade other nodes after this upgrade finishes.",
  );
  await userEvent.click(within(confirm).getByRole("button", { name: "Upgrade" }));
  expect(await screen.findByText("Upgrade node-a")).toBeInTheDocument();
  expect(posted).toEqual(["refresh", "upgrade"]);
});

it("says why the upgrade did not start", async () => {
  cluster();
  mockLists({ node: [updatable()] });
  server.use(
    http.post(apiUrl("/nodes/n1/upgrade"), () =>
      HttpResponse.json(
        { code: "upgrade_in_progress", message: 'node "node-b" is still being upgraded' },
        { status: 409 },
      ),
    ),
    http.post(apiUrl("/nodes/n1/updates/refresh"), () =>
      HttpResponse.json({ code: "node_offline", message: "offline" }, { status: 409 }),
    ),
  );
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Check again" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("The node is offline.");

  await userEvent.click(within(dialog).getByRole("button", { name: "Upgrade" }));
  const confirm = await screen.findByRole("alertdialog");
  await userEvent.click(within(confirm).getByRole("button", { name: "Upgrade" }));
  expect(await within(confirm).findByRole("alert")).toHaveTextContent(
    'An upgrade is in progress: node "node-b" is still being upgraded',
  );
});

it("waits for a running upgrade before checking or upgrading again", async () => {
  cluster();
  const running = {
    ...task("t2", "node-upgrade", "n1"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [updatable()], task: [running] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(await within(dialog).findByRole("button", { name: "Upgrading…" })).toBeDisabled();
  expect(within(dialog).getByRole("button", { name: "Check again" })).toBeDisabled();
  expect(within(dialog).queryByRole("button", { name: "Upgrade" })).not.toBeInTheDocument();
});

it("waits for a running check before checking or upgrading", async () => {
  cluster();
  const running = {
    ...task("t1", "node-update-check", "n1"),
    status: { state: "running" as const, error: null },
  };
  mockLists({ node: [updatable()], task: [running] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(await within(dialog).findByRole("button", { name: "Checking…" })).toBeDisabled();
  // 확인이 apt를 쓰는 동안은 업그레이드도 하지 않는다.
  expect(within(dialog).getByRole("button", { name: "Upgrade" })).toBeDisabled();
});

it("has no update menu for nodes that do not check for updates", async () => {
  cluster();
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-a" }));
  expect(await screen.findByRole("menuitem", { name: "Enter maintenance" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Updates…" })).not.toBeInTheDocument();
});

it("lets viewers read the updates but not act on them", async () => {
  cluster();
  mockLists({ node: [updatable()] });
  renderPage(<NodesPage />, { as: { ...ADMIN, role: "viewer" } });
  await userEvent.click(await screen.findByRole("button", { name: /^Updates for node-a:/ }));
  const dialog = await screen.findByRole("dialog", { name: "Updates for node-a" });
  expect(within(dialog).getByText("oplite")).toBeInTheDocument();
  expect(within(dialog).queryByRole("button", { name: "Check again" })).not.toBeInTheDocument();
  expect(within(dialog).queryByRole("button", { name: "Upgrade" })).not.toBeInTheDocument();
});
```

`web/src/nodes/NodesPage.tsx` 전체:

```tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { applyEvent, listKey, useList, type List } from "@/api/live";
import type { Node, Vm } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ReasonMenuItem } from "@/components/Reason";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatCapacity } from "@/lib/format";
import { NodeUpdatesDialog } from "@/nodes/NodeUpdatesDialog";
import { PoolStatus } from "@/storage/PoolStatus";

/** 그 노드에 고정했거나 그 노드에서 마지막으로 돈 VM의 수 */
function vmCount(node: Node, vms: Vm[]): number {
  return vms.filter((vm) => (vm.spec.node ?? vm.status.node) === node.id).length;
}

/** 배지 버튼의 접근 이름에 넣는 확인 상태 */
function updatesState(
  t: ReturnType<typeof useTranslation>["t"],
  updates: NonNullable<Node["status"]["updates"]>,
): string {
  if (updates.error) {
    return t("nodes.updates.stateFailed");
  }
  if (!updates.checked_at) {
    return t("nodes.updates.stateNotChecked");
  }
  return t("nodes.updates.stateCount", {
    n: `${updates.packages.length}${updates.truncated ? "+" : ""}`,
  });
}

/** 올릴 수 있는 패키지 수. 확인이 실패했으면 경고, 아직 확인하지 않았으면 비어 있다. */
function UpdatesBadge({ updates }: { updates: NonNullable<Node["status"]["updates"]> }) {
  const { t } = useTranslation();
  if (updates.error) {
    return <Badge variant="destructive">{t("nodes.updates.failedBadge")}</Badge>;
  }
  if (!updates.checked_at) {
    return <span className="text-muted-foreground">—</span>;
  }
  const count = `${updates.packages.length}${updates.truncated ? "+" : ""}`;
  return <Badge variant={updates.packages.length > 0 ? "default" : "outline"}>{count}</Badge>;
}

export function NodesPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const vms = useList("vm");
  const cluster = useQuery({ queryKey: ["cluster"], queryFn: () => call(api.GET("/cluster")) });
  const [removing, setRemoving] = useState<Node | null>(null);
  const [showingUpdates, setShowingUpdates] = useState<Node | null>(null);
  // 메뉴에서 연 대화상자를 닫으면 메뉴 항목이 사라지므로, 메뉴를 연 버튼으로 포커스를 돌린다.
  const trigger = useRef<HTMLButtonElement | null>(null);
  const rememberTrigger = (e: React.SyntheticEvent<HTMLButtonElement>) => {
    trigger.current = e.currentTarget;
  };
  const write = can(me.role, "write", "node");

  async function setMaintenance(node: Node, maintenance: boolean) {
    try {
      const updated = await call(
        api.PUT("/nodes/{key}", {
          params: { path: { key: node.id }, header: { "If-Match": node.revision } },
          body: { labels: node.labels, spec: { ...node.spec, maintenance } },
        }),
      );
      // watch 이벤트보다 먼저 반영한다(곧바로 다시 바꿔도 새 revision으로 보낸다).
      queryClient.setQueryData<List<Node>>(
        listKey("node"),
        (list) => list && applyEvent(list, { type: "put", kind: "node", resource: updated }),
      );
      toast.success(
        t(maintenance ? "nodes.maintenanceOn" : "nodes.maintenanceOff", { name: node.name }),
      );
    } catch (err) {
      toast.error(t("vms.actionFailed", { name: node.name, error: errorMessage(t, err) }));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 id="nodes-title" className="text-2xl font-semibold">
        {t("nav.nodes")}
      </h1>
      <p className="text-sm text-muted-foreground">{t("nodes.joinNote")}</p>
      <Table aria-labelledby="nodes-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("nodes.address")}</TableHead>
            <TableHead>{t("nodes.version")}</TableHead>
            <TableHead>{t("nodes.updates.column")}</TableHead>
            <TableHead>{t("nodes.underlay")}</TableHead>
            <TableHead>{t("nodes.hypervisor")}</TableHead>
            <TableHead>{t("nodes.arch")}</TableHead>
            <TableHead>{t("nodes.fencing")}</TableHead>
            <TableHead>{t("nodes.cpus")}</TableHead>
            <TableHead>{t("nodes.memory")}</TableHead>
            <TableHead>{t("nodes.bridges")}</TableHead>
            <TableHead>{t("nodes.pools")}</TableHead>
            <TableHead>{t("nodes.vms")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {(nodes.data ?? []).length === 0 && (
            <TableMessage colSpan={write ? 15 : 14} query={nodes} empty={t("dashboard.noNodes")} />
          )}
          {(nodes.data ?? []).map((node) => (
            <TableRow key={node.id}>
              <TableCell className="font-medium">
                {node.name}
                {node.id === cluster.data?.node && (
                  <span className="ml-2 text-xs text-muted-foreground">{t("nodes.thisNode")}</span>
                )}
              </TableCell>
              <TableCell>
                <div className="flex flex-wrap gap-1">
                  <Badge variant={node.status.online ? "default" : "destructive"}>
                    {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                  </Badge>
                  {node.spec.maintenance && (
                    <Badge variant="outline">{t("dashboard.maintenance")}</Badge>
                  )}
                  {node.spec.witness && <Badge variant="outline">{t("dashboard.witness")}</Badge>}
                </div>
              </TableCell>
              <TableCell className="font-mono text-xs">{node.spec.address}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell className="font-mono text-xs">{node.status.version ?? "—"}</TableCell>
              {/* 업데이트를 확인하지 않는 노드(위트니스, 꺼 둔 노드)는 `updates`가 없다. */}
              <TableCell>
                {node.status.updates ? (
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={t("nodes.updates.open", {
                      name: node.name,
                      state: updatesState(t, node.status.updates),
                    })}
                    onPointerDown={rememberTrigger}
                    onKeyDown={rememberTrigger}
                    onClick={() => setShowingUpdates(node)}
                  >
                    <UpdatesBadge updates={node.status.updates} />
                  </Button>
                ) : (
                  "—"
                )}
              </TableCell>
              {/* VXLAN 패킷을 주고받는 주소. 위트니스와 정하지 못한 노드는 없다. */}
              <TableCell className="font-mono text-xs">{node.status.underlay ?? "—"}</TableCell>
              <TableCell>{node.status.hypervisor}</TableCell>
              {/* 옛 버전으로 등록한 노드는 모른다. */}
              <TableCell>{node.status.arch ?? "—"}</TableCell>
              {/* 펜싱이 꺼진 노드(위트니스 포함)는 HA VM을 돌리지 않는다. */}
              <TableCell>
                {node.status.ha ? t(`nodes.fencingMode.${node.status.ha.fencing}`) : "—"}
              </TableCell>
              <TableCell>{node.status.cpus}</TableCell>
              <TableCell>{formatCapacity(node.status.memory_mib)}</TableCell>
              <TableCell className="font-mono text-xs">
                {node.status.bridges.map((b) => b.name).join(", ") || "—"}
              </TableCell>
              <TableCell>
                {Object.keys(node.status.pools).length === 0
                  ? "—"
                  : Object.entries(node.status.pools)
                      .sort(([a], [b]) => a.localeCompare(b))
                      .map(([name, state]) => (
                        <PoolStatus
                          key={name}
                          name={name}
                          state={state}
                          offline={!node.status.online}
                        />
                      ))}
              </TableCell>
              {/* VM 목록을 다 읽기 전에는 모른다. */}
              <TableCell>{vms.isSuccess ? vmCount(node, vms.data) : "—"}</TableCell>
              {write && (
                <TableCell className="text-right">
                  <DropdownMenu modal={false}>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={t("vms.actionsFor", { name: node.name })}
                        onPointerDown={rememberTrigger}
                        onKeyDown={rememberTrigger}
                      >
                        <MoreHorizontal />
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end">
                      <DropdownMenuItem
                        onSelect={() => void setMaintenance(node, !node.spec.maintenance)}
                      >
                        {t(
                          node.spec.maintenance ? "nodes.endMaintenance" : "nodes.startMaintenance",
                        )}
                      </DropdownMenuItem>
                      {node.status.updates && (
                        <DropdownMenuItem onSelect={() => setShowingUpdates(node)}>
                          {t("nodes.updates.menu")}
                        </DropdownMenuItem>
                      )}
                      <DropdownMenuSeparator />
                      <ReasonMenuItem
                        variant="destructive"
                        // 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다(서버도 막는다).
                        reason={
                          node.id === cluster.data?.node
                            ? "nodes.reason.thisNode"
                            : node.status.online
                              ? "nodes.reason.online"
                              : null
                        }
                        onSelect={() => setRemoving(node)}
                      >
                        {t("nodes.remove")}
                      </ReasonMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {write && (
        <>
          <p className="text-sm text-muted-foreground">{t("nodes.maintenanceNote")}</p>
          <p className="text-sm text-muted-foreground">{t("nodes.removeNote")}</p>
        </>
      )}
      {showingUpdates && (
        <NodeUpdatesDialog
          node={showingUpdates}
          returnFocus={trigger}
          onClose={() => setShowingUpdates(null)}
        />
      )}
      <ConfirmDialog
        open={removing !== null}
        onOpenChange={(open) => !open && setRemoving(null)}
        title={t("nodes.removeTitle", { name: removing?.name })}
        description={t("nodes.removeDescription")}
        confirmLabel={t("nodes.remove")}
        typeToConfirm={removing?.name}
        returnFocus={trigger}
        onConfirm={async () => {
          const node = removing!;
          await call(api.DELETE("/nodes/{key}", { params: { path: { key: node.id } } }));
          toast.success(t("nodes.removed", { name: node.name }));
        }}
      />
    </div>
  );
}
```

`web/src/pages/DashboardPage.test.tsx`에서

```tsx
  expect(await screen.findByRole("table", { name: "Recent tasks" })).toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(await screen.findByRole("table", { name: "Recent tasks" })).toBeInTheDocument();
});

it("warns while nodes run different versions", async () => {
  const at = (id: string, version?: string) => ({
    ...NODE,
    id,
    name: id,
    status: { ...NODE.status, version },
  });
  // 버전을 모르는 노드(옛 노드)는 세지 않고, 같은 버전은 한 번만 보인다. Debian 순서로 보인다(~는
  // 정식 버전보다 낮다).
  mockLists({
    node: [at("a", "0.10.0"), at("b", "0.9.0"), at("c", "0.9.0"), at("d"), at("e", "0.10.0~rc1")],
    vm: [],
  });
  server.use(cluster("a"));
  const { unmount } = renderPage(<DashboardPage />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Nodes run different opLite versions (0.9.0, 0.10.0~rc1, 0.10.0). Finish the upgrade on every node.",
  );
  unmount();

  mockLists({ node: [at("a", "0.10.0"), at("b", "0.10.0"), at("d")], vm: [] });
  renderPage(<DashboardPage />);
  expect(await screen.findByText("a", { selector: "td" })).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
import { formatCapacity } from "@/lib/format";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
```

을 다음으로 바꾼다:

```tsx
import { formatCapacity } from "@/lib/format";
import { compareVersions } from "@/lib/version";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
  };
}
```

을 다음으로 바꾼다:

```tsx
  };
}

/** 노드들이 돌리는 서로 다른 버전(낮은 것부터). 버전을 모르는 노드(옛 노드)는 뺀다. */
function nodeVersions(nodes: Node[]): string[] {
  const versions = new Set(nodes.flatMap((n) => (n.status.version ? [n.status.version] : [])));
  return [...versions].sort(compareVersions);
}
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
  const failed = allVms.filter((vm) => vm.status.phase === "failed");
```

을 다음으로 바꾼다:

```tsx
  const failed = allVms.filter((vm) => vm.status.phase === "failed");
  const versions = nodeVersions(allNodes);
```

`web/src/pages/DashboardPage.tsx`에서

```tsx
      <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
```

을 다음으로 바꾼다:

```tsx
      <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>
      {/* 롤링 업그레이드 중이거나 멈춘 업그레이드가 있다. */}
      {versions.length > 1 && (
        <p
          role="alert"
          className="rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm"
        >
          {t("dashboard.mixedVersions", { versions: versions.join(", ") })}
        </p>
      )}
      <div className="grid grid-cols-1 gap-4 sm:grid-cols-3">
```

`web/src/pages/LoginPage.tsx`에서

```tsx
import { useState } from "react";
```

을 다음으로 바꾼다:

```tsx
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
```

`web/src/pages/LoginPage.tsx`에서

```tsx

import { useAuth } from "@/auth/AuthProvider";
```

을 다음으로 바꾼다:

```tsx

import { api, call } from "@/api/client";
import { useAuth } from "@/auth/AuthProvider";
```

`web/src/pages/LoginPage.tsx`에서

```tsx
  const [busy, setBusy] = useState(false);
```

을 다음으로 바꾼다:

```tsx
  const [busy, setBusy] = useState(false);
  // 서버 버전은 로그인 없이 읽는다. 읽지 못하면(옛 서버) 보이지 않는다.
  const version = useQuery({
    queryKey: ["version"],
    queryFn: () => call(api.GET("/version")),
    staleTime: Infinity,
    retry: false,
  });
```

`web/src/pages/LoginPage.tsx`에서

```tsx
  return (
    <div className="flex min-h-svh items-center justify-center p-4">
      <Card className="w-full max-w-sm">
```

을 다음으로 바꾼다:

```tsx
  return (
    <div className="flex min-h-svh flex-col items-center justify-center gap-4 p-4">
      <Card className="w-full max-w-sm">
```

`web/src/pages/LoginPage.tsx`에서

```tsx
      </Card>
    </div>
```

을 다음으로 바꾼다:

```tsx
      </Card>
      {version.data && (
        <p className="text-xs text-muted-foreground">
          {t("login.version", { version: version.data.version })}
        </p>
      )}
    </div>
```

`web/src/settings/SettingsPage.test.tsx`를 만든다:

```tsx
import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { QueryClient } from "@tanstack/react-query";
import { http, HttpResponse } from "msw";

import { SETTINGS_KEY } from "@/api/live";

import { SettingsPage } from "@/settings/SettingsPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

// 아직 저장한 적 없는 설정: 서버가 기본값을 revision 0으로 돌려준다.
const DEFAULT = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 0,
  labels: {},
  spec: { updates: { channel: "stable" } },
  status: {},
};

it("changes the update channel with the revision it read", async () => {
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { updates: { channel: string } };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...DEFAULT, revision: 12, spec: { updates: body.updates } });
    }),
  );
  renderPage(<SettingsPage />);
  const channel = await screen.findByLabelText("Update channel");
  expect(channel).toHaveValue("stable");
  const save = screen.getByRole("button", { name: "Save" });
  // 바뀐 것이 없으면 보낼 것도 없다.
  expect(save).toBeDisabled();
  await userEvent.selectOptions(channel, "testing");
  await userEvent.click(save);
  expect(await screen.findByText("Settings saved.")).toBeInTheDocument();
  expect(sent).toEqual([{ ifMatch: "0", body: { updates: { channel: "testing" } } }]);
  // 응답을 바로 보여 준다.
  expect(screen.getByLabelText("Update channel")).toHaveValue("testing");
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
});

it("keeps newer settings that arrived while saving", async () => {
  const page: { queryClient?: QueryClient } = {};
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () => {
      // 응답 전에 watch가 더 새 설정(다른 admin이 stable로 되돌림)을 넣었다.
      page.queryClient!.setQueryData(SETTINGS_KEY, { ...DEFAULT, revision: 20 });
      return HttpResponse.json({
        ...DEFAULT,
        revision: 12,
        spec: { updates: { channel: "testing" } },
      });
    }),
  );
  page.queryClient = renderPage(<SettingsPage />).queryClient;
  const channel = await screen.findByLabelText("Update channel");
  await userEvent.selectOptions(channel, "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(channel).toHaveValue("stable"));
  expect(page.queryClient.getQueryData(SETTINGS_KEY)).toMatchObject({ revision: 20 });
});

it("says why the settings could not be saved", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json({ code: "revision_mismatch", message: "stale" }, { status: 412 }),
    ),
  );
  renderPage(<SettingsPage />);
  await userEvent.selectOptions(await screen.findByLabelText("Update channel"), "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Someone else changed it first. Reload and try again.",
  );
});

it("shows the settings read-only to everyone but admins", async () => {
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({ ...DEFAULT, revision: 3, spec: { updates: { channel: "testing" } } }),
    ),
  );
  renderPage(<SettingsPage />, { as: { ...ADMIN, role: "viewer" } });
  const channel = await screen.findByLabelText("Update channel");
  expect(channel).toHaveValue("testing");
  expect(channel).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
  expect(screen.getByText("Only administrators can change the settings.")).toBeInTheDocument();
});
```

`web/src/settings/SettingsPage.tsx`를 만든다:

```tsx
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { SETTINGS_KEY, useSettings } from "@/api/live";
import type { Channel, Settings } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";

const CHANNELS: Channel[] = ["stable", "testing"];

/** 클러스터 설정. 누구나 읽고 admin만 바꾼다. */
export function SettingsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const settings = useSettings();
  const write = can(me.role, "write", "settings");
  // 고르기 전에는 서버의 값을 보인다(watch로 바뀌어도 따라간다).
  const [chosen, setChosen] = useState<Channel | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!settings.data) {
    return <ListPending error={settings.error} />;
  }
  const current = settings.data.spec.updates?.channel ?? "stable";
  const channel = chosen ?? current;
  const revision = settings.data.revision;

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const updated = await call(
        api.PATCH("/settings", {
          params: { header: { "If-Match": revision } },
          body: { updates: { channel } },
        }),
      );
      // 응답을 기다리는 사이 watch가 더 새 설정을 넣었으면 그것을 남긴다.
      queryClient.setQueryData<Settings>(SETTINGS_KEY, (old) =>
        old && old.revision > updated.revision ? old : updated,
      );
      setChosen(null);
      toast.success(t("settings.saved"));
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex max-w-xl flex-col gap-6">
      <h1 className="text-2xl font-semibold">{t("nav.settings")}</h1>
      <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
        <h2 className="font-medium">{t("settings.updates")}</h2>
        {error && (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        )}
        <Field id="settings-channel" label={t("settings.channel")} hint={t("settings.channelHint")}>
          {(a11y) => (
            <NativeSelect
              id="settings-channel"
              value={channel}
              disabled={!write || busy}
              onChange={(e) => setChosen(e.target.value as Channel)}
              {...a11y}
            >
              {CHANNELS.map((c) => (
                <NativeSelectOption key={c} value={c}>
                  {t(`settings.channels.${c}`)}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
        {write ? (
          <div>
            <Button type="submit" disabled={busy || channel === current}>
              {t("common.save")}
            </Button>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">{t("settings.adminOnly")}</p>
        )}
      </form>
    </div>
  );
}
```

`web/src/tasks/TasksPage.test.tsx`에서

```tsx

function many(n: number, state: "failed" | "succeeded" = "succeeded") {
```

을 다음으로 바꾼다:

```tsx

it("names node update tasks by the node", async () => {
  mockLists({
    node: [{ id: "n2", name: "node-b" }],
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000005",
        spec: { op: { kind: "node-update-check" }, target: "n2", node: "n2" },
        status: { state: "succeeded", error: null },
      },
      {
        id: "019a0f3c-8a10-7000-8000-000000000006",
        spec: { op: { kind: "node-upgrade" }, target: "n2", node: "n2" },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  expect(await screen.findByText("Check updates on node-b")).toBeInTheDocument();
  expect(screen.getByText("Upgrade node-b")).toBeInTheDocument();
});

function many(n: number, state: "failed" | "succeeded" = "succeeded") {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
  const vms = useList("vm");
  const volumes = useList("volume");
```

을 다음으로 바꾼다:

```tsx
  const vms = useList("vm");
  const nodes = useList("node");
  const volumes = useList("volume");
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
    const importById = new Map(imports.data?.map((i) => [i.id, i]));
    return (task: Task) => {
```

을 다음으로 바꾼다:

```tsx
    const importById = new Map(imports.data?.map((i) => [i.id, i]));
    const nodeById = new Map(nodes.data?.map((n) => [n.id, n]));
    return (task: Task) => {
```

`web/src/tasks/TasksPage.tsx`에서

```tsx
      }
      const name = volumeById.get(task.spec.target)?.name ?? backupById.get(task.spec.target)?.name;
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data, backups.data, imports.data]);
}
```

을 다음으로 바꾼다:

```tsx
      }
      // 노드 업데이트 확인·업그레이드의 대상은 노드다.
      const name =
        volumeById.get(task.spec.target)?.name ??
        backupById.get(task.spec.target)?.name ??
        nodeById.get(task.spec.target)?.name;
      // 지워진 대상은 id의 끝(UUIDv7의 무작위 부분)으로 구분한다. 앞부분은 시각이라 서로 비슷하다.
      return { name: name ?? task.spec.target.slice(-8), href: undefined };
    };
  }, [vms.data, volumes.data, backups.data, imports.data, nodes.data]);
}
```

- [ ] **Step 2: 확인**

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test && pnpm -C web build`
Expected: 모두 통과

- [ ] **Step 3: 커밋**

```bash
git add web
git commit -m "feat(web): 노드 버전·업데이트와 설정 화면 — 노드 표에 버전 열(API 주소 다음)과 업데이트 열(패키지 수 배지, 잘렸으면 +, 확인 실패면 경고 배지, 확인하지 않는 노드는 —), 배지를 누르거나 행 메뉴 \"업데이트…\"(업데이트를 확인하는 노드만)로 여는 대화상자(마지막 확인 시각·패키지 표 PACKAGE CURRENT CANDIDATE·새 의존성 표시·잘림 안내·실패 이유, admin만 \"다시 확인\"(POST /nodes/{key}/updates/refresh, 확인 Task가 돌고 있으면 막음)과 \"업그레이드\"(확인 대화상자 뒤 POST /nodes/{key}/upgrade), node_offline은 노드 문구로), 대시보드의 혼합 버전 경고(버전을 아는 노드가 둘 이상의 버전을 돌릴 때), 새 화면 /settings와 내비게이션 \"설정\"(업데이트 채널 stable·testing, admin만 PATCH /settings에 If-Match로 저장, 그 밖은 읽기만), 로그인 화면 아래 서버 버전(GET /version), watch 종류에 settings(목록 없이 SETTINGS_KEY 캐시를 연결 때마다 다시 읽고 더 새 revision의 이벤트만 반영), Task 라벨 node-update-check·node-upgrade와 노드 대상 이름, 오류 문구 version_too_old·updates_unsupported·upgrade_in_progress, ko·en"
```

---

### Task 7: 문서와 ISO E2E 버전 확인

README 새 절 "업데이트"(채널, 소스 파일, 확인·업그레이드, 롤링 순서, `[updates]`, 저장소 만들기), 설계 §3.8 "구현(계획 32)"(설계와 다른 점과 알려진 틈)과 §5 오류, 처음 분해 문서의 하위 프로젝트 9 메모, `iso.yml`이 설치한 노드의 버전이 빌드 버전인지 확인.

**Files:**
- Modify: `.github/workflows/iso.yml`
- Modify: `README.md`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/iso.yml`에서

```yaml
# 설치 ISO E2E: 패키지 → netinst(debian.lock) → build-iso.sh --unattended → KVM으로 무인 설치 →
# 디스크로 부팅 → 첫 부팅 cluster init 출력(직렬 콘솔)으로 oplctl login·node list·node get.
# BIOS(isolinux 메뉴)와 UEFI(OVMF, grub 메뉴) 두 경로를 따로 돌린다.
```

을 다음으로 바꾼다:

```yaml
# 설치 ISO E2E: 패키지 → netinst(debian.lock) → build-iso.sh --unattended → KVM으로 무인 설치 →
# 디스크로 부팅 → 첫 부팅 cluster init 출력(직렬 콘솔)으로 oplctl login·node list(VERSION이 빌드 버전)·node get.
# BIOS(isolinux 메뉴)와 UEFI(OVMF, grub 메뉴) 두 경로를 따로 돌린다.
```

`.github/workflows/iso.yml`에서

```yaml
          $oplctl node list
          $oplctl node get opl-ci | jq -e '.status.bridges[] | select(.name == "vmbr0")'
```

을 다음으로 바꾼다:

```yaml
          $oplctl node list
          # 설치한 노드의 VERSION 열이 이 빌드의 버전(build-deb가 남긴 OPL_VERSION)인지 본다.
          $oplctl node list | awk -v want="$OPL_VERSION" '
            NR == 1 { for (i = 1; i <= NF; i++) if ($i == "VERSION") col = i; next }
            $1 == "opl-ci" { found = 1; got = col ? $col : "" }
            END {
              if (!found || got != want) { print "opl-ci VERSION: \"" got "\", 기대: \"" want "\"" > "/dev/stderr"; exit 1 }
            }'
          $oplctl node get opl-ci | jq -e --arg v "$OPL_VERSION" '.status.version == $v'
          $oplctl node get opl-ci | jq -e '.status.bridges[] | select(.name == "vmbr0")'
```

`README.md`에서

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO)
```

을 다음으로 바꾼다:

```markdown
- 라이선스: Apache-2.0
- 상태: 개발 초기 (노드 코어 M7, 웹 UI, VM 스냅샷·복제·예약 스냅샷, NFS 공유 스토리지, LVM-thin·ZFS·Ceph RBD·공유 LVM(iSCSI) 풀, 라이브 마이그레이션, 위트니스 노드, Debian 패키지·설치 ISO, 버전과 업데이트)
```

`README.md`에서

```markdown
- 설치 화면에서 클러스터를 만들거나 합류하지 않는다(설치한 뒤 위 명령으로).
```

을 다음으로 바꾼다:

````markdown
- 설치 화면에서 클러스터를 만들거나 합류하지 않는다(설치한 뒤 위 명령으로).

## 업데이트

패키지로 설치한 노드는 opLite APT 저장소에서 업데이트를 확인하고, 관리자가 노드를 **하나씩** 업그레이드한다.
클러스터 설정의 **채널**(`stable`·`testing`)이 어느 저장소 구성 요소를 따를지 정한다.

```bash
oplctl settings get                              # updates.channel: stable
oplctl settings set updates.channel=testing      # admin. 모든 노드의 소스 파일이 바뀐다
oplctl node list                                 # VERSION 열(ADDRESS 다음)
oplctl node updates node1                        # 마지막 확인 시각, PACKAGE CURRENT CANDIDATE 표, 확인 실패 이유
oplctl node updates node1 --refresh              # 지금 다시 확인하고(작업을 기다린다) 결과를 보인다
oplctl node upgrade node1                        # upgrade of node1 started (task …); follow it with `oplctl task get …`
oplctl node upgrade node1 --wait                 # 끝날 때까지 기다린다: "node node1 upgraded"
oplctl cluster status                            # 버전이 섞여 있으면 경고
```

- **채널.** 설정은 클러스터에 하나뿐인 `Settings` 리소스(이름 `cluster`)다. `GET /api/v1/settings`는 로그인한 누구나,
  `PATCH /api/v1/settings`(`{"updates":{"channel":"testing"}}`, 다른 패치 API처럼 `If-Match`를 받는다)는 admin만
  한다. 한 번도 바꾸지 않았으면 기본값(`stable`)이다. 웹은 "설정" 화면(admin만 저장, 그 밖은 읽기만).
- **소스 파일.** 저장소 주소는 opld 설정 `[updates] apt_url`이고, 비면 빌드할 때 넣은 `OPL_APT_URL`이다. 둘 다 비면
  opld는 소스 파일을 쓰지 않는다(관리자가 apt 소스를 직접 둔다). 주소가 있으면 opld가 시작할 때와 채널이 바뀔 때
  `/etc/apt/sources.list.d/oplite.sources`를 쓴다:

  ```
  # managed by opld; edits are overwritten
  Types: deb
  URIs: https://apt.example.com/oplite
  Suites: trixie
  Components: stable
  Signed-By: /usr/share/keyrings/oplite-archive-keyring.gpg
  ```

  첫 줄의 표시가 없는 파일은 opld가 건드리지 않고 로그에 경고만 남긴다(비었거나 공백뿐인 파일은 opld 것으로 본다).
  소스를 직접 관리하려면 첫 줄을 지운다. 저장소 공개 키(`Signed-By`의 키링)가 없으면 apt가 저장소를 거절하므로
  opld는 소스 파일을 쓰지 않고(경고) 자기가 쓴 파일은 지운다. 패키지를 지우면(`remove`·`purge`) postrm이 표시가
  있는 소스 파일을 지운다.
- **확인.** 노드마다 opld가 시작하고 1분 뒤, 그 뒤 24시간마다 `apt-get update -q`와 `apt-get -s -q dist-upgrade`
  (설치하지 않고 흉내만)로 올릴 수 있는 패키지를 찾아 노드 상태(`node get`의 `status.updates`)에 쓴다. opLite만이
  아니라 Debian 패키지(보안 업데이트 등)도 보인다. 500개까지 담고 넘으면 잘렸다고 표시한다. 다른 apt(무인 업그레이드
  등)의 잠금은 `DPkg::Lock::Timeout=60`초까지 기다린다. 실패하면 이유(명령과 stderr 끝 5줄)를 남긴다. `--refresh`와
  웹의 "다시 확인"은 그 노드에서 도는 작업 `node-update-check`다(admin. 끝나지 않은 확인이 있으면 새로 만들지 않고 그
  작업을 돌려준다. 주기 확인이 도는 중이면 끝나기를 기다렸다가 확인하고, 그사이 업그레이드가 시작되면 확인하지 않고
  끝난다). 채널을 바꿔 소스 파일이 바뀌면 다음 주기를 기다리지 않고 곧 확인한다. 업그레이드 중에는 주기 확인을
  건너뛰고 다시 확인은 **409** `upgrade_in_progress`다(업그레이드가 apt 잠금을 잡고 있다).
- **업그레이드.** `node upgrade`(admin)는 그 노드에서 작업 `node-upgrade`를 돌린다. 작업은 opld 밖의 systemd 유닛
  `opl-upgrade-<작업 ID 32자(하이픈 없이)>`(`systemd-run`)에서 `apt-get update`와 `apt-get dist-upgrade`(바뀐 설정
  파일은 지금 것을 둔다. 무인 업그레이드 같은 다른 apt의 잠금은 `DPkg::Lock::Timeout=600`초까지 기다린다)를 돌리고
  유닛을 지켜본다. 로그는 `/var/log/opl/upgrade-<작업 ID>.log`이고 처음에 `opl-upgrade: start`, 끝에
  `opl-upgrade: exit <코드>` 줄이 있다. opLite 패키지가 바뀌면 postinst가 opld를 다시 시작하고, 다시 시작한 opld가
  같은 유닛을 이어 지켜본다. VM은 자기 scope에 있어 계속 돈다. 실패하면 작업 오류에 로그 끝 20줄이 있다. 유닛이
  끝나고 치워졌으면 로그로 판단한다: 종료 줄이 있으면 그 코드, 시작 줄만 있으면(도중에 노드가 꺼짐) dpkg가 반쯤
  설정된 채 남았을 때 "the upgrade was interrupted; run "dpkg --configure -a" on the node", 아니면 "the upgrade was
  interrupted; start it again", 시작 줄도 없으면 이어 볼 때는 유닛을 지금 띄우고(apt는 다시 돌려도 된다) 아니면
  "the upgrade did not start; try again"으로 실패한다. 업그레이드하는 동안 그 노드의 업데이트 확인은 쉰다. 시간 제한과
  취소는 없다(apt를 중간에 끊으면 더 나쁘다). 끝나면(성공·실패 모두) 업데이트를 다시 확인하고, 새 버전은 다시 시작한
  opld가 노드 상태에 쓴다.
- **거절.** 클러스터 어디든 끝나지 않은 업그레이드가 있으면 **409** `upgrade_in_progress`(한 번에 한 노드. 다른
  노드로 함께 온 두 요청이 모두 지나가도 나중에 시작한 작업이 "upgrade_in_progress: node "X" is being upgraded"로
  실패한다), 오프라인 노드는 **409** `node_offline`, 업데이트를 확인하지 않는 노드(macOS, apt가 없는 노드, 위트니스)는
  **400** `updates_unsupported`다. 버전을 적지 않은 옛 노드(계획 32 전의 opld)가 남아 있으면 설정 바꾸기·다시
  확인·업그레이드는 **409** `cluster_upgrading`("some nodes run an older opLite (node2, node3); …")이다: 옛 노드는
  모르는 리소스 종류와 작업 종류가 든 Raft 항목을 풀지 못한다. 그래서 계획 32 전 클러스터의 첫 업그레이드는 노드마다
  손으로(`apt-get update && apt-get dist-upgrade`) 하고, 모든 노드가 버전을 적은 뒤부터 `node upgrade`를 쓴다.
- **`--wait`.** 업그레이드하는 노드의 API에 로그인한 oplctl은 opld가 다시 시작할 때 연결 오류로 끝날 수 있다.
  업그레이드는 계속된다. 다른 노드에 로그인해 두거나 `oplctl task get <ID>`로 다시 본다.
- **합류.** 클러스터에서 가장 낮은 노드 버전보다 낮은 opld는 합류할 수 없다(**409** `version_too_old`: "this node runs
  X; the cluster runs Y or newer; upgrade it first"). 버전을 보내지 않는 옛 opld도 버전을 적은 노드가 있는 클러스터에는
  합류할 수 없다("this node runs an unknown version; …"). 새 노드는 먼저 클러스터와 같은 버전으로 올린다. 합류할 때
  보낸 버전은 그 노드가 등록하기 전부터 노드 상태에 적힌다.

**롤링 순서.** opLite는 노드를 자동으로 돌아가며 올리지 않고 노드를 다시 부팅하지도 않는다. 한 노드씩:

1. `oplctl node updates <노드> --refresh`로 무엇이 바뀌는지 본다.
2. 커널처럼 다시 부팅해야 하는 업데이트가 있으면 그 노드의 VM을 다른 노드로 옮기고(`vm migrate`) 웹에서 유지보수
   모드로 둔다.
3. `oplctl node upgrade <노드> --wait`(다른 노드에 로그인해서). 필요하면 노드를 다시 부팅하고 유지보수 모드를 끈다.
4. `oplctl node list`에서 그 노드의 VERSION과 ONLINE을 확인하고 다음 노드로 간다.

모든 노드가 같은 버전이 될 때까지 `cluster status`와 웹 대시보드에 "nodes run different opLite versions (…); finish
the upgrade on every node" 경고가 보인다. 웹은 노드 표의 업데이트 배지(올릴 패키지 수, 확인 실패면 경고)를 누르면
같은 정보와 "다시 확인"·"업그레이드" 버튼(admin)이 있는 대화상자를 연다. 로그인 화면 아래에 서버 버전
(`GET /api/v1/version`, 인증 없음)이 보인다.

설정(노드마다, 생략하면 아래 기본값):

```toml
[updates]
# enabled = true                 # 없으면 apt_get이 있을 때(Debian) 켜진다. macOS는 꺼진다
check_interval = "24h"           # 업데이트 확인 간격
first_check = "1m"               # opld가 시작하고 첫 확인까지
apt_url = ""                     # 비면 빌드 값 OPL_APT_URL. 그것도 비면 소스 파일을 쓰지 않는다
sources_file = "/etc/apt/sources.list.d/oplite.sources"
apt_get = "/usr/bin/apt-get"
systemd_run = "/usr/bin/systemd-run"
systemctl = "/usr/bin/systemctl"
dpkg = "/usr/bin/dpkg"
log_dir = "/var/log/opl"         # 업그레이드 로그
poll = "2s"                      # 업그레이드 유닛을 다시 보는 간격
```

**저장소 만들기.** 저장소는 서명한 정적 파일 트리이고, 올릴 웹 서버는 opLite 밖이다. 패키지에는 저장소 주소와 서명
공개 키를 빌드할 때 넣는다(키를 넣지 않은 패키지는 `Signed-By`의 키링이 없어 apt가 저장소를 거절한다).

```bash
# 서명 키(한 번). GPG 홈은 비밀 키를 둘 곳으로(GNUPGHOME).
gpg --quick-generate-key 'opLite archive <apt@example.com>' ed25519 sign never
KEY_ID=$(gpg --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
gpg --export "$KEY_ID" > oplite-archive-keyring.gpg
# 패키지: 저장소 주소(바이너리에)와 공개 키(/usr/share/keyrings/oplite-archive-keyring.gpg)
OPL_VERSION=0.2.0 OPL_APT_URL=https://apt.example.com/oplite \
    cargo build --release -p opld --features web-ui -p oplctl
OPL_APT_KEY=oplite-archive-keyring.gpg packaging/deb/build-deb.sh 0.2.0 dist
# 저장소: 채널(stable|testing|enterprise)에 패키지를 더하고 색인을 다시 만들어 서명한다(apt-ftparchive, apt-utils)
OPL_REPO_GPG_KEY_ID=$KEY_ID packaging/repo/build-repo.sh repo testing dist/oplite_0.2.0_amd64.deb
# testing에서 확인한 뒤 같은 패키지를 stable에도
OPL_REPO_GPG_KEY_ID=$KEY_ID packaging/repo/build-repo.sh repo stable dist/oplite_0.2.0_amd64.deb
```

레이아웃: `pool/<채널>/*.deb`, `dists/trixie/<채널>/binary-amd64/Packages(.gz)`, `dists/trixie/Release`
(있는 모든 채널을 묶는다)와 서명 `InRelease`·`Release.gpg`. `repo/`를 `apt_url`이 가리키는 곳에 그대로 올린다.
`build-repo.sh`는 고른 채널의 `Packages`만 다시 만들고 다른 채널은 그대로 둔다. pool의 옛 패키지는 지우지 않으므로
옛 버전도 색인에 남는다(apt는 가장 새 버전을 고른다). 이미 올린 이름에 내용이 다른 deb는 거절한다("version already
published") — 같은 버전을 다시 빌드했으면 버전을 올린다. 새 색인과 Release는 옆 디렉터리에서 만들어 두 서명이 다
끝난 뒤에 옮기므로(`InRelease`가 마지막) 서명이 실패해도 저장소는 옛 색인 그대로다. 웹 서버에 올릴 때도 같은 순서로
`pool/`과 `Packages`를 먼저, `Release`·`Release.gpg`를 다음, `InRelease`를 마지막에 올린다. `OPL_APT_KEY`는
`gpg --export`의 바이너리 키링이어야 한다(armor면 `build-deb.sh`가 거절한다). Release에 `Valid-Until`은 넣지 않는다:
넣으면 저장소를 주기적으로 다시 서명해야 하고, 그러지 못하면 모든 노드의 확인이 실패한다. `enterprise` 채널은
계획 33(구독)이 쓴다.
````

`README.md`에서

```markdown
- [VMware 가져오기 설계 (하위 프로젝트 8: 계획 29·30)](docs/superpowers/specs/2026-11-04-vmware-import-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [VMware 가져오기 설계 (하위 프로젝트 8: 계획 29·30)](docs/superpowers/specs/2026-11-04-vmware-import-design.md)
- [설치·업데이트·구독 설계 (하위 프로젝트 9: 계획 31·32·33)](docs/superpowers/specs/2026-11-05-install-update-subscription-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료)부터 했다(`2026-11-05-install-update-subscription-design.md`). 계획 32(버전과 업데이트)·33(구독과 텔레메트리)이 남았다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 9는 계획 31(Debian 패키지와 설치 ISO: 빌드 버전 `opl_types::version::BUILD`와 `--version`, 패키지 `oplite`(opld·oplctl·웹 UI·systemd 유닛·maintainer 스크립트), Debian 13 netinst에 preseed와 패키지를 더한 설치 ISO(설치가 기본 경로 인터페이스를 VLAN-aware `vmbr0`으로 옮기고 `/etc/opl/opld.toml`을 씀), CI `shell`·`package` 작업과 ISO 무인 설치 E2E 워크플로 `iso.yml`, 완료)과 계획 32(버전과 업데이트: Debian 규칙의 버전 비교, `GET /api/v1/version`, 노드 버전과 낮은 버전의 합류 거절, 클러스터 설정 `Settings`의 업데이트 채널 `stable`·`testing`, 노드마다 opld가 관리하는 APT 소스 파일과 주기적인 업데이트 확인(`NodeStatus.updates`), 다시 확인 Task와 한 번에 한 노드씩 `systemd-run`으로 도는 업그레이드 Task(opld가 다시 시작해도 이어 봄), 서명한 APT 저장소 스크립트 `build-repo.sh`와 패키지 키링, oplctl·웹, 완료)부터 했다(`2026-11-05-install-update-subscription-design.md`). 계획 33(구독과 텔레메트리)이 남았다.
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
- `oplctl node list`에 `VERSION`, 웹 노드 표에 버전 열. 노드 버전이 둘 이상이면 웹 대시보드와 `oplctl cluster status`에 "nodes run different opLite versions (…); finish the upgrade on every node".
- 합류: 합류 요청에 `version`을 넣는다. 받는 노드는 합류하는 노드의 버전이 클러스터에서 가장 오래된 노드보다 오래되었으면 409 `version_too_old`("this node runs X; the cluster runs Y or newer; upgrade it first"). 버전 비교는 Debian 버전 규칙(`~`가 가장 낮음)의 작은 구현(`opl_types::version::compare`)으로 한다. 버전이 없는 요청(옛 oplctl·opld)은 받는다.
```

을 다음으로 바꾼다:

```markdown
- `oplctl node list`에 `VERSION`, 웹 노드 표에 버전 열. 노드 버전이 둘 이상이면 웹 대시보드와 `oplctl cluster status`에 "nodes run different opLite versions (…); finish the upgrade on every node".
- 합류: 합류 요청에 `version`을 넣는다. 받는 노드는 합류하는 노드의 버전이 클러스터에서 가장 오래된 노드보다 오래되었으면 409 `version_too_old`("this node runs X; the cluster runs Y or newer; upgrade it first"). 버전 비교는 Debian 버전 규칙(`~`가 가장 낮음)의 작은 구현(`opl_types::version::compare`)으로 한다. 버전이 없는 요청(옛 opld)은 클러스터에 버전을 적은 노드가 하나도 없을 때만 받는다(구현에서 바꿈, §3.8).
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown

## 4. 구독과 텔레메트리 (계획 33)
```

을 다음으로 바꾼다:

```markdown

### 3.8 구현(계획 32)

- 버전: `opl_types::version::{compare, SUITE, APT_URL}`. `compare`는 Debian 규칙(에포크, 업스트림, 마지막 `-` 뒤 리비전, `~` < 끝 < 문자 < 그 밖, 숫자는 앞 0을 무시한 수 비교)이고 이상한 문자열에도 패닉하지 않는다. `SUITE = "trixie"`, `APT_URL`은 빌드 때 `OPL_APT_URL`(없으면 ""). `GET /api/v1/version`(`opl_api::cluster::get_version`, 인증 없음) → `VersionInfo { version }`. `NodeStatus.version`은 `register_node`·`register_witness`가 `BUILD`로 쓴다(등록 때, 그리고 업데이트가 켜진 노드는 확인·`UpdateAgent` 시작 때도 — opld가 다시 시작하면 새 값). 합류를 받는 쪽도 Node를 만들거나 다시 쓸 때 요청의 `version`을 적어, 등록 전에도 버전 검사와 옛 노드 게이트가 그 노드를 센다. `GET /cluster`의 `MemberView.version`도 더했다(웹 대시보드 경고가 쓴다).
- 합류: `JoinRequest.version`(`opld join::request`가 `BUILD`를 보냄). 토큰 검사 뒤(토큰을 쓰기 전), 이름 검사 앞에서 클러스터 노드(위트니스·오프라인 포함, 합류하는 노드 자신의 옛 기록은 뺌) 중 버전이 있는 가장 낮은 것과 비교한다. 설계와 다른 점: 버전을 보내지 않는 요청도 버전을 적은 노드가 있으면 409 `version_too_old`("this node runs an unknown version; the cluster runs Y or newer; upgrade it first")다. 옛 opld는 새 리소스·Task 종류가 든 Raft 항목을 풀지 못하기 때문이다(노드 코어 설계의 호환 규칙). 버전을 적은 노드가 없는 옛 클러스터는 받는다.
- 옛 노드 게이트: 같은 이유로 버전을 적지 않은 노드가 하나라도 있으면 새 종류를 쓰는 `PATCH /settings`·`POST /nodes/{key}/updates/refresh`·`POST /nodes/{key}/upgrade`가 409 `cluster_upgrading`("some nodes run an older opLite (a, b); finish the upgrade on every node first" — 버전 없는 노드 이름을 이름 순으로)이다(`opl_api::cluster::require_upgraded`). 계획 32 전 클러스터의 첫 업그레이드는 노드마다 손으로 apt를 돌린다.
- `Settings`(`opl_types::settings`, 이름은 언제나 `SETTINGS_NAME = "cluster"`): 리소스가 없으면 `GET`이 nil id·revision 0·기본 spec의 리소스 모양을 돌려준다(`opl_api::settings::current`, `UpdateAgent`도 이것으로 읽는다). 설계와 다른 점:
  - `PATCH`는 JSON merge patch가 아니라 기존 Patch 관례다: `SettingsPatch { updates: Option<{ channel: Option<Channel> }> }`(`deny_unknown_fields`). `{}`는 400, `{"updates":{}}`는 쓰지 않고 200. 없으면 `create`, 있으면 `update_spec`. `If-Match`가 있으면 그 revision(없는 설정은 0)이 아니면 412(`If-Match: 0`으로 함께 만들다 진 요청도 409 `name_taken`이 아니라 412), 없으면 `RevisionMismatch`·`NameTaken`에 10번 다시 한 뒤 409 `busy`. 같은 값이면 쓰지 않는다.
  - 감사 로그가 코드베이스에 없으므로 남기지 않는다.
- `[updates]`(opld `UpdatesSection`): 설계의 키에 `first_check`(기본 "1m"), `dpkg`(`/usr/bin/dpkg`), `poll`(기본 "2s")을 더했다. `enabled`가 없으면 `apt_get`이 있을 때 켜진다. 꺼지면 `opl_api::UpdatesConfig`·`Updates`가 아예 없다(`TaskRunnerConfig.updates: None`). 같은 `Arc<Updates>`를 Task 실행기와 `UpdateAgent`가 쓴다. opld 전체 프로세스 시험의 데몬은 `enabled = false`다(Ubuntu CI 러너에 진짜 `apt-get`이 있다).
- `NodeStatus.updates: Option<NodeUpdates { checked_at: Option<u64>(유닉스 초 — 다른 상태 시각과 같다), packages: [PackageUpdate { name, current: Option, candidate }], truncated, error }>`. 업데이트가 켜진 노드는 등록 때 `Some`(있던 결과는 `record_node`가 남김), 꺼진 노드·위트니스는 None이고 None이 `updates_unsupported`의 뜻이다. 쓰기는 노드 상태를 revision 확인하며 다시 쓴다.
- `opl_api::updates`: `Updates::check`(`apt-get update -q` → `LANG=C apt-get -s -q dist-upgrade`의 `Inst` 줄 — 둘 다 `-o DPkg::Lock::Timeout=60`으로 다른 apt의 잠금을 60초까지 기다림, `[<cur>]`이 없으면 새 의존성으로 current None, 500개 넘으면 truncated; 실패는 명령과 stderr 끝 5줄; 명령은 `kill_on_drop`; 주기 확인은 잠금이 잡혀 있으면 기다리지 않고 "another update check is running", 다시 확인 Task는 잠금 줄에 서지 않고 `poll`마다 다시 해 보며, 이 노드의 업그레이드가 잠금을 잡고 있으면 확인하지 않고 성공한다 — 업그레이드가 끝날 때 확인한다), `sources_contents`·`write_sources`(`<파일>.dpkg-tmp`에 0644로 써서 `fsync` 뒤 바꿔 넣고 디렉터리도 `fsync`, 같으면 `Unchanged`, 관리 표시 없는 파일은 `Unmanaged`와 `warn!` — 비었거나 공백뿐인 파일은 관리하는 것으로 봄, 저장소 공개 키(`UpdatesConfig.keyring`, 기본 `/usr/share/keyrings/oplite-archive-keyring.gpg`)가 없으면 쓰지 않고 관리하던 파일은 지움), `UpdateAgent`(시작과 `Settings` watch 이벤트 때 소스 파일, `first_check` 뒤 처음·그 뒤 `check_interval`마다 확인, 채널이 바뀌어 소스 파일을 새로 썼으면 곧 확인, 이 노드의 `node-upgrade`가 끝나지 않았으면 건너뜀, 시작할 때와 확인할 때 이 노드 상태의 `version`·`updates`가 비었으면 되살림). 패키지의 postrm은 `remove`·`purge` 때 관리 표시가 있는 소스 파일을 지운다.
- 다시 확인: 설계는 "그 노드로 전달"이지만 노드 사이 HTTP 전달 도우미가 없으므로 Task다. `POST /api/v1/nodes/{key}/updates/refresh` → `TaskOp::NodeUpdateCheck`(`node-update-check`, 대상·노드 = 그 노드), 202 Task. 거절 순서 403(admin만 — `Kind::Node` Write) → 404 → 400 `updates_unsupported` → 409 `node_offline` → 409 `cluster_upgrading` → 그 노드를 업그레이드하는 중이면 409 `upgrade_in_progress`. 그 노드에 끝나지 않은 확인이 있으면 그 Task를 돌려준다. 확인이 실패하면(결과에 `error`) Task도 실패한다.
- 업그레이드: `TaskOp::NodeUpgrade`(`node-upgrade`)는 설계의 `{ node }` 필드 없이 Task의 대상·노드가 그 노드다. `POST /api/v1/nodes/{key}/upgrade`(`updates::upgrade_node`) 거절 순서 403 → 404 → 400 → 409 `node_offline` → 409 `cluster_upgrading` → 409 `upgrade_in_progress`("node "X" is still being upgraded; upgrade one node at a time"). 검사와 만들기는 프로세스 안 잠금으로 감싸고, 다른 노드로 함께 온 요청은 실행기가 막는다: running을 쓴 뒤 다른 running `node-upgrade`가 보이면 "upgrade_in_progress: node "X" is being upgraded"로 실패(running 쓰기는 Raft 순서로 적용되므로 둘 다 지나가지 않는다). 실행(`Updates::upgrade(cluster, node, task, resume)`)은 확인 잠금을 잡고(apt 잠금을 다투지 않게, 끝난 뒤의 확인까지 잡은 채) `log_dir`를 만들고 `systemd-run --unit=opl-upgrade-<task id 32자 simple> --collect --property=Type=exec --property=StandardOutput=append:<log> --property=StandardError=append:<log> --setenv=DEBIAN_FRONTEND=noninteractive --setenv=LANG=C /bin/sh -c 'echo "opl-upgrade: start"; … update -q -o DPkg::Lock::Timeout=600 && … -y -q -o DPkg::Lock::Timeout=600 … dist-upgrade; s=$?; echo "opl-upgrade: exit $s"; exit $s'`. 설계와 다른 점:
  - 명령 처음에 시작 줄, 끝에 종료 상태 줄을 남겨, 유닛이 사라졌을 때(`ActiveState=inactive`이고 `Result`가 비었거나 `LoadState=not-found`) 로그로 판단한다: 시작·종료 줄이 있으면 그 코드, 시작 줄만 있으면 `dpkg --audit`가 무언가 알리면 "the upgrade was interrupted; run "dpkg --configure -a" on the node", 비었으면 "the upgrade was interrupted; start it again"(설계의 "audit가 비면 성공"은 끊긴 apt를 성공으로 보므로 바꿈), 시작 줄이 없으면 이어 볼 때는 유닛을 지금 띄우고 아니면 "the upgrade did not start; try again".
  - `systemctl show`는 `-p LoadState`도 묻고, `systemctl` 자체가 실패하면 다음 `poll`에 다시 본다.
  - 실행기 첫 패스(`resume = true`)에서 running인 `node-upgrade`는 실패로 정리하지 않고 task id로 유닛 이름을 다시 만들어 이어 본다. 시간 제한 없음(`unlimited`), 취소 없음. 끝나면 성공·실패 모두 잠금을 잡은 채 확인을 한 번 한다.
- 저장소(`packaging/repo/build-repo.sh`): 설계에 더해 채널을 `stable|testing|enterprise`만 받고(그 밖은 종료 2), Release에 `Origin`·`Label: opLite`, `Components`는 `Packages`가 있는 채널만 고정 순서(stable testing enterprise), `Release.gpg`는 armor다. pool의 옛 패키지를 지우지 않으므로 옛 버전도 그 채널 `Packages`에 남는다. 이미 올린 이름에 내용이 다른 deb는 "version already published"로 거절(같으면 넘어감)하고, 새 색인·Release는 `dists/.trixie.new`에서 만들어 두 서명이 끝난 뒤 옮긴다(`InRelease`가 마지막). 도구 경로는 `APT_FTPARCHIVE`·`GPG`(시험용). `build-deb.sh`는 `OPL_APT_KEY` 파일을 `/usr/share/keyrings/oplite-archive-keyring.gpg`(0644)로 싣고, 없는 파일이면 1, armor 키면 "OPL_APT_KEY must be a binary keyring (gpg --export, not --armor)"로 1. CI의 패키지는 키링·`OPL_APT_URL` 없이 만든다.
- oplctl: `node list`의 `VERSION`(ADDRESS 다음), `cluster status` 끝의 혼합 버전 경고(버전 순, 중복 없이, 버전 모르는 멤버 제외), `settings get|set`(`-o json`이면 리소스, 키는 `updates.channel`만), `node updates <node> [--refresh]`(`checked:`·`error:`·표·`no updates available`·잘림 줄, 꺼진 노드는 "node X does not check for updates"), `node upgrade <node> [--wait]`(기다림은 24시간까지).
- 웹: 설계의 노드 상세 "업데이트" 탭 대신(노드 상세 페이지가 없다) 노드 표의 버전 열·업데이트 배지와 `NodeUpdatesDialog`(배지는 누구나 읽기 전용으로 열고, 행 메뉴 "업데이트…"·다시 확인·업그레이드는 admin). `/settings`(`SettingsPage`, `If-Match` PATCH), 로그인 화면 아래 서버 버전, 대시보드 혼합 버전 경고, watch 종류 `settings`(목록 캐시가 아니라 `useSettings()` 캐시), Task 라벨, 오류 문구.
- 시험: opl-types 단위(비교 표, 설정 직렬화, 노드 상태), opl-api `tests/cluster.rs`(버전 API, 합류 거절), `tests/settings.rs`, `tests/updates.rs`(가짜 `apt-get`·`systemd-run`·`systemctl`·`dpkg` — `tests/common/fake_apt.rs` `FakeApt`, `unit.state`가 active·success·failed·gone; 확인·실패·꺼진 노드·같은 Task, 채널을 바꾸면 소스 파일, 업그레이드 인자·성공·실패·한 번에 하나·이어 보기·사라짐과 audit), `parse_simulate`의 실제 Debian 13 출력 견본, oplctl `fake_api`, 웹 vitest, 셸 `build-repo-test.sh`(macOS, 가짜 도구)와 `repo-test.sh`(CI `package`의 Debian 13 컨테이너: 임시 키, stable·testing, `gpgv`, `file://`+`Signed-By` 설치, testing으로 바꾸면 `Inst oplite [<v>] (<v>+1 `). `iso.yml`은 설치한 노드의 `oplctl node list` VERSION과 `status.version`이 빌드 버전(`OPL_VERSION`)인지 본다.
- 알려진 틈:
  - 업그레이드는 opld를 다시 시작할 뿐 노드를 다시 부팅하지 않는다. 커널 업데이트 뒤 부팅과 VM 비우기는 관리자의 일이다.
  - 업그레이드하는 노드에 붙은 oplctl `--wait`·웹은 opld가 다시 시작할 때 연결이 끊길 수 있다(업그레이드는 계속).
  - 업데이트 확인은 Debian 패키지 전체를 보이고, `testing`에서 `stable`로 돌아가도 이미 올린 버전을 내리지 않는다.
  - 업그레이드 로그(`/var/log/opl/upgrade-*.log`)는 지우지 않는다.
  - 이 필드를 모르는 옛 opld가 리더이면 노드 상태를 다시 쓰며 `version`·`updates`를 지울 수 있다. 업데이트가 켜진 노드는 `UpdateAgent`가 시작·확인 때 되살리지만, 꺼진 노드(macOS, 위트니스)는 opld가 다시 시작할 때까지 버전이 비어 보일 수 있다. 그동안 버전이 빈 노드 때문에 `cluster_upgrading` 게이트가 닫힌다(설정 바꾸기·다시 확인·업그레이드가 409).
  - 저장소 Release에 `Valid-Until`을 넣지 않는다(의도). 넣으면 운영자가 주기적으로 다시 서명해야 하고, 놓치면 모든 노드의 확인이 실패한다. 대신 오래된 서명 색인을 다시 내미는 공격(freeze)은 막지 못한다.
  - 저장소를 웹 서버에 올릴 때는 `pool/`·`Packages`를 먼저, `Release`·`Release.gpg` 다음, `InRelease`를 마지막에 올려야 apt가 반쯤 올린 저장소를 읽지 않는다(`build-repo.sh`는 로컬 디렉터리에서 이 순서로 바꿔 넣는다).

## 4. 구독과 텔레메트리 (계획 33)
```

`docs/superpowers/specs/2026-11-05-install-update-subscription-design.md`에서

```markdown
| `updates_unsupported` | 400 | 업데이트가 꺼진 노드(macOS, apt 없음)의 확인·업그레이드 |
| `upgrade_in_progress` | 409 | 다른 노드 업그레이드가 끝나지 않음 |
| `subscription_required` | 400 | 구독 없이 enterprise 채널 |
```

을 다음으로 바꾼다:

```markdown
| `updates_unsupported` | 400 | 업데이트가 꺼진 노드(macOS, apt 없음)의 확인·업그레이드 |
| `upgrade_in_progress` | 409 | 다른 노드 업그레이드가 끝나지 않음(그 노드를 업그레이드하는 중의 다시 확인도) |
| `cluster_upgrading` | 409 | 버전을 적지 않은 옛 노드가 남은 클러스터에서 설정 바꾸기·다시 확인·업그레이드(계획 32 구현에서 더함) |
| `subscription_required` | 400 | 구독 없이 enterprise 채널 |
```

- [ ] **Step 2: 확인**

Run: `ruby -ryaml -e 'ARGV.each { |f| YAML.load_file(f) }' .github/workflows/*.yml .github/actions/build-deb/action.yml`
Expected: 오류 없음

- [ ] **Step 3: 커밋**

```bash
git add .github README.md docs
git commit -m "docs: 버전과 업데이트 — README 새 절 \"업데이트\"(채널 stable·testing과 settings get|set, opld가 관리하는 deb822 소스 파일과 관리 표시, 주기 확인과 node updates [--refresh], node upgrade [--wait]와 systemd-run 유닛·로그·이어 보기·실패 안내, 한 번에 한 노드와 거절 코드, --wait 연결 끊김 주의, 합류 version_too_old, 롤링 순서, 혼합 버전 경고·웹 대화상자·로그인 화면 버전, [updates] 설정 기본값, 저장소 만들기: 서명 키·OPL_APT_URL·OPL_APT_KEY 빌드·build-repo.sh와 레이아웃), 상단 상태와 문서 목록에 설계 문서, 설계 §3.8 \"구현(계획 32)\"(설계와 다른 점: 감사 로그 없음, PATCH는 merge patch가 아니라 기존 Patch 관례, 다시 확인이 Task, NodeUpgrade 필드 없음, 종료 상태 줄과 LoadState, 노드 상세 탭 대신 대화상자, 더한 설정 키; 시험과 알려진 틈), 노드 코어 설계 §1.1에 계획 32 완료, iso.yml이 설치한 노드의 node list VERSION이 빌드 버전(OPL_VERSION)인지 확인"
```

---

## 완료 기준 (계획 32)

- `opld`·`oplctl`이 노드 버전을 보이고(`node list` VERSION, `GET /api/v1/version`, 웹), 클러스터보다 낮은 버전의 합류를 409 `version_too_old`로 거절한다.
- `oplctl settings set updates.channel=testing`으로 채널을 바꾸면 각 노드의 관리되는 소스 파일의 `Components`가 바뀌고 곧 업데이트를 다시 확인한다.
- `oplctl node updates <node> [--refresh]`가 업데이트할 패키지를 보이고, `oplctl node upgrade <node> --wait`가 한 번에 한 노드를 업그레이드한다(가짜 스크립트 시험: 성공·실패·재시작 뒤 이어 보기·시작하지 않음·중단).
- CI `package` 작업의 저장소 시험이 `debian:13`에서 `file://` 저장소로 설치하고 `testing`의 새 버전을 업데이트 후보로 본다.
- `iso.yml`의 `iso-e2e`(BIOS·UEFI)에서 설치한 노드의 버전이 빌드 버전이다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, 웹 `typecheck`·`lint`·`test`·`build`, 셸 시험과 shellcheck가 통과한다.

## 알고 받아들인 제한

- 처음 계획 32로 올릴 때(옛 노드에서)는 노드마다 손으로 패키지를 올린다. 모든 노드가 버전을 보고하기 전에는 설정·다시 확인·업그레이드가 409 `cluster_upgrading`이다.
- 업그레이드는 노드를 재부팅하지 않는다. 커널 업데이트는 VM을 옮기고 사람이 재부팅한다.
- 업그레이드 중인 노드에 붙은 oplctl `--wait`·웹은 opld가 다시 시작할 때 연결이 끊길 수 있다(업그레이드는 계속된다).
- `testing`에서 `stable`로 돌아가도 패키지는 내려가지 않는다. 업그레이드 로그는 지우지 않는다.
- 진짜 `systemd-run`·`systemctl`로 하는 업그레이드는 CI에서 시험하지 않는다(가짜 스크립트로 시험).
- 그 밖의 제한은 설계 문서 §3.8과 README "업데이트" 절에 있다.
