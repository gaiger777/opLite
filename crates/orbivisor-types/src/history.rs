//! 설정 이력(계획 73 §6·§7): Raft 상태 머신이 설정 리소스의 spec 바뀜마다 남기는 기록과, 그 기록에 붙는 출처
//! (누가·언제·어디서·왜). 저장하는 쪽은 `orbivisor-cluster`의 상태 머신 표 `history`이고, 이 모듈은 꼴과 비밀 가림만 둔다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::image_catalog::redact_source_url;
use crate::resource::Kind;

/// 사유(`Orbivisor-Change-Reason`) 최대 글자 수
pub const MAX_REASON_CHARS: usize = 200;
/// 클라이언트 표시(`Orbivisor-Client`) 최대 길이
pub const MAX_CLIENT_LEN: usize = 64;
/// 비밀 칸 자리에 남기는 표시의 열쇠: `{"$secret": "same"}`
pub const SECRET_MARK: &str = "$secret";

/// 기본 보존 한도(설정 `history`가 없을 때 — 계획 73 §6)
pub const DEFAULT_MAX_PER_RESOURCE: u32 = 100;
pub const DEFAULT_MAX_AGE_DAYS: u32 = 365;
pub const DEFAULT_MAX_TOTAL_MIB: u32 = 64;

/// 바뀜이 어디서 왔나
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChangeSource {
    /// 그 밖의 API 손님(`Orbivisor-Client`가 없거나 모르는 값)
    Api,
    /// 웹(세션 쿠키 또는 `Orbivisor-Client: web`)
    Web,
    /// `orbivisorctl`
    Cli,
    /// 선언형 적용(`orbivisorctl apply` — Task `apply`)
    Apply,
    /// Terraform/OpenTofu 공급자
    Terraform,
    /// Task가 쓴 spec(마이그레이션의 노드 등)
    Task,
    /// 리더 루프·reconciler·출처 없는 쓰기
    #[default]
    System,
}

impl ChangeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::Web => "web",
            Self::Cli => "cli",
            Self::Apply => "apply",
            Self::Terraform => "terraform",
            Self::Task => "task",
            Self::System => "system",
        }
    }

    /// `Orbivisor-Client` 머리 값(`cli/0.1.0`·`terraform/0.1.0`·`web`)의 출처. 64자를 넘거나 영숫자·`/._-` 밖의 글자가
    /// 있거나 모르는 이름이면 `Api`다.
    pub fn from_client(header: &str) -> Self {
        let ok = !header.is_empty()
            && header.len() <= MAX_CLIENT_LEN
            && header
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/._-".contains(&c));
        if !ok {
            return Self::Api;
        }
        match header.split('/').next().unwrap_or_default() {
            "cli" => Self::Cli,
            "terraform" | "tofu" => Self::Terraform,
            "web" => Self::Web,
            _ => Self::Api,
        }
    }
}

/// 사람이 쓴 사유를 다듬는다: 제어 문자를 빼고 앞뒤 공백을 자른 뒤 200자까지. 비면 `None`.
pub fn clean_reason(raw: &str) -> Option<String> {
    let s: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_REASON_CHARS)
        .collect();
    (!s.is_empty()).then_some(s)
}

/// 어떻게 인증한 주체인가
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorAuth {
    /// 로그인 세션(웹 쿠키)
    Session,
    /// API 토큰(`name`은 토큰 리소스 이름, 없으면 id 문자열)
    Token { name: String },
    /// 사람이 아닌 쓰기(리더 루프·reconciler·시험)
    #[default]
    System,
}

/// 바꾼 주체
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Actor {
    /// 사용자 이름(그때의 이름)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub user_id: Option<Uuid>,
    #[serde(default)]
    pub auth: ActorAuth,
    /// 이 쓰기를 한 Task(Task 실행기가 쓴 spec)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub task: Option<Uuid>,
}

impl Actor {
    /// 사람이 아닌 쓰기
    pub fn system() -> Self {
        Self::default()
    }
}

/// 바꾸는 명령에 붙는 출처(`Command::Attributed`의 `meta`). `at_ms`는 리더가 제안 직전에 찍는다(따르미는 0을 보낸다) —
/// 그래서 상태 머신 적용(보존 한도의 "지금")이 노드마다 같다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeMeta {
    /// 유닉스 밀리초(리더 시계)
    #[serde(default)]
    pub at_ms: u64,
    #[serde(default)]
    pub actor: Actor,
    #[serde(default)]
    pub source: ChangeSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 감사 줄 id(같은 요청의 감사 항목과 잇는다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Uuid>,
    /// 선언형 적용 묶음(`apply-set` id)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_set: Option<Uuid>,
    /// 되돌리기면 되돌린 이력 줄의 revision
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restored_from: Option<u64>,
}

impl ChangeMeta {
    /// 출처 없는 쓰기(리더 루프·reconciler·시험)
    pub fn system() -> Self {
        Self::default()
    }
}

/// 이력 줄의 동작
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HistoryOp {
    Create,
    Update,
    Delete,
}

/// 이력 줄 하나(상태 머신 표 `history`의 값 — lz4로 줄여 둔다). `spec`은 그때의 spec이고 비밀 칸은
/// [`history_mask`]의 표시다. `Delete`는 지우기 직전 spec(살리기에 쓴다).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HistoryRecord {
    /// 이 바뀜을 적용한 Raft 로그 인덱스(리소스의 새 revision — 지우기는 지운 인덱스)
    pub revision: u64,
    pub kind: Kind,
    pub id: Uuid,
    pub name: String,
    pub op: HistoryOp,
    pub at_ms: u64,
    pub actor: Actor,
    pub source: ChangeSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub request: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub apply_set: Option<Uuid>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    pub spec: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub restored_from: Option<u64>,
}

impl HistoryRecord {
    /// 명령의 출처와 결과로 줄을 만든다(`spec`은 이미 가린 것).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        meta: &ChangeMeta,
        revision: u64,
        kind: Kind,
        id: Uuid,
        name: String,
        op: HistoryOp,
        labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Self {
        Self {
            revision,
            kind,
            id,
            name,
            op,
            at_ms: meta.at_ms,
            actor: meta.actor.clone(),
            source: meta.source,
            reason: meta.reason.clone(),
            request: meta.request,
            apply_set: meta.apply_set,
            labels,
            spec,
            restored_from: meta.restored_from,
        }
    }
}

/// 다른 기록이 가리키는 이력 줄(드리프트의 "누가 바꿨나")
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HistoryRef {
    pub revision: u64,
    pub at_ms: u64,
    pub actor: Actor,
    pub source: ChangeSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub reason: Option<String>,
}

/// 이력 보존 한도(설정 `history` — 계획 73 §7.3). 상태 머신이 적용 때 Raft의 설정에서 읽는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HistorySettings {
    /// 리소스 하나에 남기는 줄 수(10~1000)
    #[serde(default = "default_max_per_resource")]
    #[cfg_attr(
        feature = "openapi",
        schema(default = 100, minimum = 10, maximum = 1000)
    )]
    pub max_per_resource: u32,
    /// 이보다 오래된 줄은 지운다(일, 7~3650)
    #[serde(default = "default_max_age_days")]
    #[cfg_attr(
        feature = "openapi",
        schema(default = 365, minimum = 7, maximum = 3650)
    )]
    pub max_age_days: u32,
    /// 이력 전체(압축 뒤) 한도(MiB, 8~512). 스냅샷에 실리므로 크게 잡으면 새 노드 합류가 느려진다.
    #[serde(default = "default_max_total_mib")]
    #[cfg_attr(feature = "openapi", schema(default = 64, minimum = 8, maximum = 512))]
    pub max_total_mib: u32,
}

fn default_max_per_resource() -> u32 {
    DEFAULT_MAX_PER_RESOURCE
}
fn default_max_age_days() -> u32 {
    DEFAULT_MAX_AGE_DAYS
}
fn default_max_total_mib() -> u32 {
    DEFAULT_MAX_TOTAL_MIB
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            max_per_resource: DEFAULT_MAX_PER_RESOURCE,
            max_age_days: DEFAULT_MAX_AGE_DAYS,
            max_total_mib: DEFAULT_MAX_TOTAL_MIB,
        }
    }
}

impl HistorySettings {
    /// 범위 안인지
    pub fn valid(&self) -> bool {
        (10..=1000).contains(&self.max_per_resource)
            && (7..=3650).contains(&self.max_age_days)
            && (8..=512).contains(&self.max_total_mib)
    }

    pub fn max_age_ms(&self) -> u64 {
        u64::from(self.max_age_days) * 86_400_000
    }

    pub fn max_total_bytes(&self) -> u64 {
        u64::from(self.max_total_mib) * 1024 * 1024
    }
}

// ---- 비밀 가림 ----

/// 종류마다 비밀 칸이 있는 곳
enum SecretPath {
    /// spec 안 어느 깊이든 이 이름의 칸(태그 있는 enum 갈래가 여럿인 종류)
    Key(&'static str),
    /// spec 뿌리에서 이 경로의 칸
    Pointer(&'static [&'static str]),
    /// spec 뿌리에서 이 경로의 맵: 값마다 비밀(webhook 헤더 값 — 이름은 남긴다)
    Values(&'static [&'static str]),
    /// spec 뿌리에서 이 경로의 URL: 보기처럼 줄인 글로 남긴다(`*`는 목록의 모든 항목). 줄여 숨긴 부분만 바뀌면 이력의
    /// 바뀜에 보이지 않는다(되돌리기는 그 칸을 지금 값으로 — 줄인 글을 받는 처리기 규칙과 같다).
    Url(&'static [&'static str], fn(&str) -> String),
}

use SecretPath::{Key, Pointer, Url, Values};

/// 종류별 비밀 칸. 보기 가림 함수(`redact_*`)가 빼거나 가리는 칸을 모두 담는다(시험이 본다).
fn secret_paths(kind: Kind) -> &'static [SecretPath] {
    match kind {
        Kind::Vm => &[Pointer(&["cloud_init", "password"])],
        Kind::User => &[Key("password_hash")],
        Kind::Realm => &[Key("bind_password"), Key("client_secret")],
        Kind::NodeFence => &[Key("password")],
        Kind::Remote => &[Key("token")],
        Kind::NotificationTarget => &[
            Key("password"),
            Key("token"),
            Values(&["target", "headers"]),
            Url(&["target", "url"], crate::alert::redact_url),
        ],
        Kind::BackupStore => &[
            Pointer(&["encryption", "key"]),
            Pointer(&["backend", "s3", "secret_access_key"]),
        ],
        Kind::VmwareSource => &[Key("password")],
        Kind::Settings => &[
            Pointer(&["subscription"]),
            Url(&["image_catalog", "*", "url"], redact_source_url),
            Url(
                &["image_catalog", "*", "checksum", "url"],
                redact_source_url,
            ),
        ],
        Kind::AcmeAccount => &[Pointer(&["eab", "hmac_key"]), Pointer(&["key"])],
        Kind::AcmeDns => &[
            Key("api_token"),
            Key("secret_access_key"),
            Key("secret"),
            Key("api_key"),
        ],
        _ => &[],
    }
}

/// 비밀 칸이 어떻게 바뀌었나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretChange {
    /// 없던 것이 생김
    Set,
    Same,
    Changed,
    /// 있던 것이 없어짐
    Cleared,
}

impl SecretChange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Same => "same",
            Self::Changed => "changed",
            Self::Cleared => "cleared",
        }
    }

    fn mark(self) -> Value {
        Value::Object(Map::from_iter([(
            SECRET_MARK.to_string(),
            Value::String(self.as_str().into()),
        )]))
    }

    fn between(prev: Option<&Value>, next: Option<&Value>) -> Option<Self> {
        fn present(v: Option<&Value>) -> Option<&Value> {
            v.filter(|v| !v.is_null())
        }
        match (present(prev), present(next)) {
            (None, None) => None,
            (Some(_), None) => Some(Self::Cleared),
            (None, Some(_)) => Some(Self::Set),
            (Some(a), Some(b)) if a == b => Some(Self::Same),
            (Some(_), Some(_)) => Some(Self::Changed),
        }
    }
}

/// 값이 비밀 표시(`{"$secret": …}`)인지
pub fn secret_mark(value: &Value) -> Option<&str> {
    let obj = value.as_object()?;
    (obj.len() == 1).then(|| obj.get(SECRET_MARK)?.as_str())?
}

/// 이력에 남길 spec: 종류의 비밀 칸을 `{"$secret": "set|same|changed|cleared"}`로 바꾼다. 앞 spec(`prev` — 평문)과 지금
/// spec(`next` — 평문)을 견주므로 해시·지문조차 남지 않는다(낮은 엔트로피 비밀번호를 대입으로 풀지 못하게). 비밀 칸이
/// 없는 종류는 그대로다.
pub fn history_mask(kind: Kind, prev: Option<&Value>, next: &Value) -> Value {
    let mut out = next.clone();
    for path in secret_paths(kind) {
        match path {
            Pointer(segs) => mask_pointer(&mut out, prev, segs),
            Key(key) => mask_key(&mut out, prev, key),
            Values(segs) => mask_values(&mut out, prev, segs),
            Url(segs, redact) => redact_at(&mut out, segs, *redact),
        }
    }
    out
}

fn mask_pointer(out: &mut Value, prev: Option<&Value>, segs: &[&str]) {
    let Some((last, parents)) = segs.split_last() else {
        return;
    };
    let mut prev_v = prev;
    for s in segs {
        prev_v = prev_v.and_then(|p| p.get(*s));
    }
    let mut parent = Some(&mut *out);
    for s in parents {
        parent = parent.and_then(|p| p.get_mut(*s));
    }
    let Some(parent) = parent.and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(change) = SecretChange::between(prev_v, parent.get(*last)) {
        parent.insert(last.to_string(), change.mark());
    }
}

fn mask_values(out: &mut Value, prev: Option<&Value>, segs: &[&str]) {
    let mut prev_v = prev;
    let mut here = Some(out);
    for s in segs {
        prev_v = prev_v.and_then(|p| p.get(*s));
        here = here.and_then(|h| h.get_mut(*s));
    }
    let Some(map) = here.and_then(Value::as_object_mut) else {
        return;
    };
    // 앞 규칙(`Key`)이 이미 가린 값(이름이 `token`인 헤더)은 그대로 둔다.
    for (k, v) in map.iter_mut().filter(|(_, v)| secret_mark(v).is_none()) {
        if let Some(change) = SecretChange::between(prev_v.and_then(|p| p.get(k)), Some(&*v)) {
            *v = change.mark();
        }
    }
}

fn redact_at(out: &mut Value, segs: &[&str], redact: fn(&str) -> String) {
    match segs.split_first() {
        None => {
            if let Some(s) = out.as_str() {
                *out = Value::String(redact(s));
            }
        }
        Some((&"*", rest)) => {
            if let Some(items) = out.as_array_mut() {
                for item in items {
                    redact_at(item, rest, redact);
                }
            }
        }
        Some((s, rest)) => {
            if let Some(child) = out.get_mut(*s) {
                redact_at(child, rest, redact);
            }
        }
    }
}

fn mask_key(out: &mut Value, prev: Option<&Value>, key: &str) {
    match out {
        Value::Object(map) => {
            let prev_obj = prev.and_then(Value::as_object);
            if let Some(change) =
                SecretChange::between(prev_obj.and_then(|p| p.get(key)), map.get(key))
            {
                map.insert(key.to_string(), change.mark());
            }
            for (k, v) in map.iter_mut() {
                if k != key {
                    mask_key(v, prev_obj.and_then(|p| p.get(k)), key);
                }
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter_mut().enumerate() {
                mask_key(v, prev.and_then(|p| p.get(i)), key);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mark(s: &str) -> Value {
        json!({ "$secret": s })
    }

    #[test]
    fn sources_from_client_header() {
        for (h, want) in [
            ("cli/0.1.0", ChangeSource::Cli),
            ("terraform/0.1.0", ChangeSource::Terraform),
            ("tofu/1.0", ChangeSource::Terraform),
            ("web", ChangeSource::Web),
            ("curl/8", ChangeSource::Api),
            ("cli/0.1 0", ChangeSource::Api),
            ("", ChangeSource::Api),
        ] {
            assert_eq!(ChangeSource::from_client(h), want, "{h:?}");
        }
        assert_eq!(
            ChangeSource::from_client(&format!("cli/{}", "1".repeat(70))),
            ChangeSource::Api
        );
        assert_eq!(
            serde_json::to_value(ChangeSource::Terraform).unwrap(),
            "terraform"
        );
    }

    #[test]
    fn reasons_are_cleaned() {
        assert_eq!(clean_reason("  fix\u{7}  dns \n"), Some("fix  dns".into()));
        assert_eq!(clean_reason(" \t"), None);
        assert_eq!(
            clean_reason(&"가".repeat(300)).unwrap().chars().count(),
            200
        );
    }

    /// 출처는 빈 값을 쓰지 않는다(Raft 로그가 작게).
    #[test]
    fn meta_storage_format() {
        assert_eq!(
            serde_json::to_value(ChangeMeta::system()).unwrap(),
            json!({ "at_ms": 0, "actor": { "auth": { "kind": "system" } }, "source": "system" })
        );
        let meta = ChangeMeta {
            at_ms: 5,
            actor: Actor {
                user: Some("admin".into()),
                user_id: Some(Uuid::from_u128(1)),
                auth: ActorAuth::Token { name: "ci".into() },
                task: None,
            },
            source: ChangeSource::Cli,
            reason: Some("why".into()),
            request: Some(Uuid::from_u128(2)),
            apply_set: None,
            restored_from: Some(9),
        };
        let v = serde_json::to_value(&meta).unwrap();
        assert_eq!(
            v,
            json!({ "at_ms": 5, "actor": { "user": "admin", "user_id": Uuid::from_u128(1),
                    "auth": { "kind": "token", "name": "ci" } }, "source": "cli", "reason": "why",
                    "request": Uuid::from_u128(2), "restored_from": 9 })
        );
        assert_eq!(serde_json::from_value::<ChangeMeta>(v).unwrap(), meta);
        assert_eq!(
            serde_json::from_value::<ChangeMeta>(json!({})).unwrap(),
            ChangeMeta::system()
        );
    }

    #[test]
    fn masks_secret_changes_without_hashes() {
        let next = json!({ "host": "vc", "password": "hunter2" });
        assert_eq!(
            history_mask(Kind::VmwareSource, None, &next),
            json!({ "host": "vc", "password": mark("set") })
        );
        assert_eq!(
            history_mask(Kind::VmwareSource, Some(&next), &next),
            json!({ "host": "vc", "password": mark("same") })
        );
        let changed = json!({ "host": "vc", "password": "hunter3" });
        let masked = history_mask(Kind::VmwareSource, Some(&next), &changed);
        assert_eq!(masked, json!({ "host": "vc", "password": mark("changed") }));
        assert!(!masked.to_string().contains("hunter"));
        // 지운 비밀은 "cleared"로 자리를 남긴다.
        assert_eq!(
            history_mask(Kind::VmwareSource, Some(&next), &json!({ "host": "vc" })),
            json!({ "host": "vc", "password": mark("cleared") })
        );
        // 비밀 칸이 없는 종류는 그대로다.
        let net = json!({ "password": "not-a-secret-field" });
        assert_eq!(history_mask(Kind::Network, None, &net), net);
    }

    #[test]
    fn masks_nested_and_pointer_paths() {
        let vm =
            json!({ "cloud_init": { "user": "rocky", "password": "$6$abc" }, "password": "x" });
        assert_eq!(
            history_mask(Kind::Vm, None, &vm),
            json!({ "cloud_init": { "user": "rocky", "password": mark("set") }, "password": "x" })
        );
        let store = json!({ "backend": { "s3": { "bucket": "b", "secret_access_key": "S" } },
                            "encryption": { "key": "K" } });
        let masked = history_mask(Kind::BackupStore, None, &store);
        assert_eq!(masked["backend"]["s3"]["secret_access_key"], mark("set"));
        assert_eq!(masked["encryption"]["key"], mark("set"));
        assert_eq!(masked["backend"]["s3"]["bucket"], "b");
        // 태그 있는 갈래 안(realm backend)·목록 안도 찾는다.
        let realm = json!({ "backend": { "kind": "ldap", "bind_password": "p" } });
        let to_oidc = json!({ "backend": { "kind": "oidc", "client_secret": "c" } });
        assert_eq!(
            history_mask(Kind::Realm, Some(&realm), &to_oidc),
            json!({ "backend": { "kind": "oidc", "client_secret": mark("set"),
                                 "bind_password": mark("cleared") } })
        );
        let targets = json!([{ "token": "a" }, { "token": "b" }]);
        let prev = json!([{ "token": "a" }, { "token": "c" }]);
        assert_eq!(
            history_mask(Kind::NotificationTarget, Some(&prev), &targets),
            json!([{ "token": mark("same") }, { "token": mark("changed") }])
        );
        assert_eq!(secret_mark(&mark("same")), Some("same"));
        assert_eq!(secret_mark(&json!({ "$secret": "same", "x": 1 })), None);
    }

    /// 리뷰 1: webhook URL은 줄인 글, 헤더 값은 표시(이름은 남김), 카탈로그 URL은 질의·사용자 정보를 가린 글.
    #[test]
    fn masks_webhook_urls_headers_and_catalog_urls() {
        let prev = json!({ "target": { "kind": "webhook",
            "url": "https://hooks.slack.com/services/T0/B0/OLDPATH",
            "headers": { "Authorization": "Bearer OLD", "X-Same": "s", "token": "h" }, "token": "t" } });
        let next = json!({ "target": { "kind": "webhook",
            "url": "https://hooks.slack.com/services/T0/B0/SECRETPATH",
            "headers": { "Authorization": "Bearer SECRETHDR", "X-Same": "s", "X-New": "n", "token": "h" },
            "token": "t" } });
        let masked = history_mask(Kind::NotificationTarget, Some(&prev), &next);
        assert_eq!(
            masked,
            json!({ "target": { "kind": "webhook", "url": "https://hooks.slack.com/…",
                "headers": { "Authorization": mark("changed"), "X-Same": mark("same"),
                             "X-New": mark("set"), "token": mark("same") },
                "token": mark("same") } })
        );
        assert!(!masked.to_string().contains("SECRET"));
        // SMTP 대상에는 URL·헤더가 없다.
        let smtp = json!({ "target": { "kind": "smtp", "host": "mx", "password": "p" } });
        assert_eq!(
            history_mask(Kind::NotificationTarget, None, &smtp)["target"]["host"],
            "mx"
        );
        let settings = json!({ "image_catalog": [
            { "id": "a", "url": "https://u:pw@s3.example/a.qcow2?X-Amz-Signature=SIG",
              "checksum": { "kind": "sums", "url": "https://s3.example/SHA256SUMS?sig=SIG" } },
            { "id": "b", "url": "https://cloud.example/b.img", "checksum": { "kind": "sha256", "value": "00" } } ] });
        let masked = history_mask(Kind::Settings, None, &settings);
        assert_eq!(
            masked["image_catalog"][0]["url"],
            "https://***@s3.example/a.qcow2?***"
        );
        assert_eq!(
            masked["image_catalog"][0]["checksum"]["url"],
            "https://s3.example/SHA256SUMS?***"
        );
        assert_eq!(masked["image_catalog"][1], settings["image_catalog"][1]);
        assert!(!masked.to_string().contains("SIG") && !masked.to_string().contains("pw"));
    }

    /// 보기 가림 함수가 빼거나 가리는 칸은 모두 이력에서도 가린다(가림 ⊆ 이력 가림). orbivisor-api의 가림 함수는 그쪽 시험이 본다.
    #[test]
    fn view_redaction_is_covered_by_history_mask() {
        fn leaves(v: &Value, path: String, out: &mut Vec<(String, Value)>) {
            match v {
                Value::Object(m) if secret_mark(v).is_none() => {
                    for (k, c) in m {
                        leaves(c, format!("{path}/{k}"), out);
                    }
                }
                Value::Array(a) => {
                    for (i, c) in a.iter().enumerate() {
                        leaves(c, format!("{path}/{i}"), out);
                    }
                }
                _ => out.push((path, v.clone())),
            }
        }
        type Redact = fn(&mut Value);
        let cases: Vec<(Kind, Value, Redact)> = vec![
            (
                Kind::VmwareSource,
                json!({ "host": "h", "user": "u", "password": "p", "fingerprint": "f" }),
                crate::vmware::redact_source_json,
            ),
            (
                Kind::Vm,
                json!({ "spec": { "cloud_init": { "password": "$6$x" } } }),
                |v| crate::cloud_init::redact_vm_json(Kind::Vm, v),
            ),
            (
                Kind::AcmeAccount,
                json!({ "directory": "letsencrypt", "agree_tos": true, "key": "K",
                        "eab": { "kid": "k", "hmac_key": "H" } }),
                crate::acme::redact_account_json,
            ),
            (
                Kind::AcmeDns,
                json!({ "provider": { "kind": "route53", "access_key_id": "A",
                                      "secret_access_key": "S", "region": "us-east-1" } }),
                crate::acme::redact_dns_json,
            ),
            (
                Kind::NotificationTarget,
                json!({ "target": { "kind": "webhook",
                    "url": "https://hooks.slack.com/services/T0/B0/SECRETPATH",
                    "headers": { "Authorization": "Bearer SECRETHDR" }, "token": "t" } }),
                |v| {
                    let spec: crate::alert::NotificationTargetSpec =
                        serde_json::from_value(v.clone()).unwrap();
                    *v = json!({ "target": crate::alert::TargetConfigView::from(spec.target) });
                },
            ),
            (
                Kind::Settings,
                json!({ "image_catalog": [{ "url": "https://u:p@s3.example/a?sig=S",
                    "checksum": { "kind": "sums", "url": "https://s3.example/SUMS?sig=S" } }] }),
                // orbivisor-api `images::redact_image`와 같은 가림(그쪽 시험이 진짜 함수로 본다)
                |v| {
                    for image in v["image_catalog"].as_array_mut().unwrap() {
                        image["url"] = redact_source_url(image["url"].as_str().unwrap()).into();
                        let sums = image["checksum"]["url"].as_str().unwrap();
                        image["checksum"]["url"] = redact_source_url(sums).into();
                    }
                },
            ),
        ];
        for (kind, value, redact) in cases {
            // VM 가림은 리소스 JSON(`spec` 아래)을 받는다 — 이력은 spec을 받는다.
            let spec = if kind == Kind::Vm {
                value["spec"].clone()
            } else {
                value.clone()
            };
            let mut redacted = value.clone();
            redact(&mut redacted);
            let redacted_spec = if kind == Kind::Vm {
                redacted["spec"].clone()
            } else {
                redacted
            };
            let (mut before, mut after, mut masked) = (vec![], vec![], vec![]);
            leaves(&spec, String::new(), &mut before);
            leaves(&redacted_spec, String::new(), &mut after);
            leaves(&history_mask(kind, None, &spec), String::new(), &mut masked);
            for (path, v) in before {
                if after.contains(&(path.clone(), v.clone())) {
                    continue;
                }
                // 보기가 가린 칸: 이력에서는 평문이 남지 않아야 한다.
                assert!(
                    !masked.contains(&(path.clone(), v.clone())),
                    "{kind:?} {path} is redacted in views but kept in history"
                );
            }
        }
    }
}
