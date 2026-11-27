//! 감사 로그 항목. 각 노드가 바꾸는 API 요청·로그인·콘솔 열기를 한 줄 JSON으로 적는다.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Role;

/// 감사 로그 한 줄
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditEntry {
    /// UUID v7
    pub id: Uuid,
    /// 유닉스 밀리초
    pub time: u64,
    /// 적은 노드의 이름
    pub node: String,
    /// 사용자 이름. 인증 전이나 실패면 없다.
    pub user: Option<String>,
    pub user_id: Option<Uuid>,
    pub role: Option<Role>,
    pub auth: AuditAuth,
    /// 클라이언트 IP
    pub ip: String,
    pub method: String,
    /// 쿼리를 뺀 경로(`/api/v1` 아래)
    pub path: String,
    /// `"<METHOD> <라우트 템플릿>"`(예: `POST /vms/{key}/actions/start`)
    pub action: String,
    pub target: Option<AuditTarget>,
    /// 응답 상태
    pub status: u16,
    pub duration_ms: u64,
    /// 핸들러가 남긴 짧은 문자열(비밀은 넣지 않는다)
    pub detail: Option<String>,
    /// 응답 상태만으로는 알 수 없는 결과(계획 77-7 — 핸들러가 적는다). 없으면 상태대로다(옛 노드의 항목도).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub outcome: Option<AuditOutcome>,
    /// 이 요청이 시작한 Task(202 — 일의 결과는 Task에 남는다). 여럿이면 처음 것.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub task: Option<Uuid>,
}

/// 상태 번호와 다른 결과
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// 미리 보기(`dry_run`) — 계획·확인만 하고 아무것도 바꾸지 않았다(200)
    DryRun,
    /// 실패했지만 응답은 성공 꼴(로그인 화면으로 보내는 302 — OIDC 콜백의 실패)
    Failed,
}

/// 요청을 인증한 방식
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AuditAuth {
    Session,
    Token,
    None,
}

/// 요청의 대상(라우트 템플릿의 첫 경로 조각과 첫 매개변수 값)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditTarget {
    /// 예: `vms`
    pub kind: String,
    /// 예: `web-1`
    pub key: String,
}

/// 감사 로그 거르기
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditQuery {
    /// 이 시각(유닉스 밀리초)부터(포함)
    pub since: u64,
    /// 이 시각까지(포함)
    pub until: u64,
    /// 사용자 이름이 정확히 같은 것
    pub user: Option<String>,
    /// `action`이나 `path`에 이 문자열이 들어 있는 것(VM 동작은 템플릿이 같아 경로로 가른다)
    pub action: Option<String>,
    /// 돌려줄 최대 개수
    pub limit: u32,
}

impl AuditQuery {
    /// `limit`의 최댓값
    pub const MAX_LIMIT: u32 = 5000;

    pub fn matches(&self, entry: &AuditEntry) -> bool {
        (self.since..=self.until).contains(&entry.time)
            && self
                .user
                .as_deref()
                .is_none_or(|u| entry.user.as_deref() == Some(u))
            && self
                .action
                .as_deref()
                .is_none_or(|a| entry.action.contains(a) || entry.path.contains(a))
    }
}

/// `GET /audit`의 응답: 모든 노드의 항목을 합친 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AuditResponse {
    /// 시간 내림차순
    pub entries: Vec<AuditEntry>,
    /// 읽지 못한 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 감사 로그를 끈 노드의 이름(정렬)
    pub disabled: Vec<String>,
    /// 조건에 맞는 항목이 `limit`보다 많아 잘랐다.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(time: u64, user: Option<&str>, action: &str) -> AuditEntry {
        AuditEntry {
            id: Uuid::now_v7(),
            time,
            node: "n1".into(),
            user: user.map(Into::into),
            user_id: None,
            role: None,
            auth: AuditAuth::None,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms/web-1/actions/start".into(),
            action: action.into(),
            target: None,
            status: 200,
            duration_ms: 1,
            detail: None,
            outcome: None,
            task: None,
        }
    }

    fn query() -> AuditQuery {
        AuditQuery {
            since: 100,
            until: 200,
            user: None,
            action: None,
            limit: 10,
        }
    }

    #[test]
    fn time_range_is_inclusive() {
        let q = query();
        assert!(q.matches(&entry(100, None, "x")));
        assert!(q.matches(&entry(200, None, "x")));
        assert!(!q.matches(&entry(99, None, "x")));
        assert!(!q.matches(&entry(201, None, "x")));
    }

    #[test]
    fn user_must_match_exactly() {
        let q = AuditQuery {
            user: Some("alice".into()),
            ..query()
        };
        assert!(q.matches(&entry(150, Some("alice"), "x")));
        assert!(!q.matches(&entry(150, Some("alice2"), "x")));
        assert!(!q.matches(&entry(150, None, "x")));
    }

    #[test]
    fn action_matches_a_substring() {
        let q = AuditQuery {
            action: Some("/vms/".into()),
            ..query()
        };
        assert!(q.matches(&entry(150, None, "POST /vms/{key}/actions/start")));
        let login = AuditEntry {
            path: "/auth/login".into(),
            ..entry(150, None, "POST /auth/login")
        };
        assert!(!q.matches(&login));
    }

    #[test]
    fn action_also_matches_a_substring_of_the_path() {
        // VM 동작은 action 템플릿이 같아 경로(`/vms/web-1/actions/start`)로 가른다.
        let q = AuditQuery {
            action: Some("actions/start".into()),
            ..query()
        };
        assert!(q.matches(&entry(150, None, "POST /vms/{key}/actions/{action}")));
        let q = AuditQuery {
            action: Some("actions/stop".into()),
            ..query()
        };
        assert!(!q.matches(&entry(150, None, "POST /vms/{key}/actions/{action}")));
    }

    #[test]
    fn entry_serializes_auth_in_snake_case() {
        let json = serde_json::to_value(entry(1, None, "x")).unwrap();
        assert_eq!(json["auth"], "none");
        assert_eq!(json["user"], serde_json::Value::Null);
        let back: AuditEntry = serde_json::from_value(json).unwrap();
        assert_eq!(back.auth, AuditAuth::None);
    }
}
