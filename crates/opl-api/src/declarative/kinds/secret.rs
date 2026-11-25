//! 비밀 칸의 "그대로 둠" 참조(계획 73 §6 `SecretValue::Ref`)를 저장 spec으로 잇는 공통 함수. 매니페스트의 참조는 저장
//! 꼴에서 [`KEEP`] 표시가 되고, 바꾸기는 그 자리를 지금 저장한 값으로 채운다(같음 — 비밀이 바뀌지 않는다). 채울 값이 없으면
//! (만들기·지금 비어 있음) 미리 확인이 `secret_missing`으로 막는다. 표시는 처리기에 닿지 않는다.

use serde_json::Value;

use crate::declarative::Problem;
use crate::error::ApiError;

/// 저장 꼴에서 참조 자리의 표시(평문 비밀로 쓰일 수 없는 값 — 처리기에 보내기 전에 막는다)
pub(crate) const KEEP: &str = "\u{1}secretRef";

fn pointer(path: &str) -> String {
    format!("/{}", path.replace('.', "/"))
}

/// 참조(`{"secretRef": …}`)를 [`KEEP`]으로 바꾼다(`to_store`).
pub(crate) fn refs_to_keep(spec: &mut Value, paths: &[&str]) {
    for path in paths {
        if let Some(v) = spec.pointer_mut(&pointer(path))
            && v.get("secretRef").is_some()
        {
            *v = Value::String(KEEP.into());
        }
    }
}

/// [`KEEP`] 자리를 지금 저장한 값으로 채운다(`keep_server_fields`). 지금 값이 없으면 표시가 남는다.
pub(crate) fn restore_kept(desired: &mut Value, live: &Value, paths: &[&str]) {
    for path in paths {
        let p = pointer(path);
        let Some(slot) = desired.pointer_mut(&p) else {
            continue;
        };
        if slot.as_str() != Some(KEEP) {
            continue;
        }
        if let Some(v) = live.pointer(&p).filter(|v| !v.is_null()) {
            *slot = v.clone();
        }
    }
}

/// 채우지 못한 참조(미리 확인의 문제 — 만들기에는 저장한 비밀이 없다)
pub(crate) fn missing(desired: &Value, paths: &[&str]) -> Vec<Problem> {
    paths
        .iter()
        .filter(|path| desired.pointer(&pointer(path)).and_then(Value::as_str) == Some(KEEP))
        .map(|path| {
            Problem::error(
                "secret_missing",
                format!(
                    "{path} is a secretRef but nothing is stored to keep; give the secret (secretFrom)"
                ),
                Some((*path).to_string()),
            )
        })
        .collect()
}

/// 처리기에 보내기 전에: 채우지 못한 참조가 있으면 400 `secret_missing`.
pub(crate) fn ensure_resolved(desired: &Value, paths: &[&str]) -> Result<(), ApiError> {
    match missing(desired, paths).into_iter().next() {
        Some(p) => Err(ApiError::bad_request(p.message).with_code("secret_missing")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn references_keep_the_stored_secret_or_report_it_missing() {
        let paths = ["token", "backend.bind_password"];
        let mut spec = json!({ "token": { "secretRef": "Remote/dr/token" },
                               "backend": { "bind_password": { "secretRef": "" }, "url": "u" } });
        refs_to_keep(&mut spec, &paths);
        assert_eq!(spec["token"], KEEP);
        assert_eq!(missing(&spec, &paths).len(), 2);
        assert_eq!(
            ensure_resolved(&spec, &paths).unwrap_err().code,
            "secret_missing"
        );
        let live = json!({ "token": "t0k", "backend": {} });
        restore_kept(&mut spec, &live, &paths);
        assert_eq!(spec["token"], "t0k");
        // 지금 값이 없는 자리는 표시가 남는다.
        assert_eq!(spec["backend"]["bind_password"], KEEP);
        assert_eq!(
            missing(&spec, &paths)[0].path.as_deref(),
            Some("backend.bind_password")
        );
        // 평문은 건드리지 않는다.
        let mut plain = json!({ "token": "new" });
        refs_to_keep(&mut plain, &paths);
        restore_kept(&mut plain, &live, &paths);
        assert_eq!(plain["token"], "new");
        assert!(ensure_resolved(&plain, &paths).is_ok());
    }
}
