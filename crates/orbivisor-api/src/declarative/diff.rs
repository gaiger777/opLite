//! 선언 비교의 순수 JSON 함수(계획 73 §8.2): 선언을 지금 값에 얹기(객체는 칸마다, 목록·값은 통째), 앞 선언에서 빠진
//! 칸 지우기(세 갈래), 경로 목록 비교, 내보내기의 `minimal`. `null`은 칸이 없는 것과 같다.

use serde::Serialize;
use serde_json::{Map, Value};
use utoipa::ToSchema;

/// 칸 하나의 바뀜
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Change {
    /// 점으로 이은 칸 경로(`cpu.cores`). 목록은 통째로 하나다(`nics`).
    pub path: String,
    pub op: ChangeOp,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub old: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub new: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOp {
    Add,
    Remove,
    Replace,
}

fn absent(v: Option<&Value>) -> bool {
    v.is_none_or(Value::is_null)
}

/// `old` → `new`의 바뀐 칸. 객체는 칸마다 내려가고 목록과 값은 통째로 견준다. 차례는 칸 이름 차례다.
pub fn diff(old: &Value, new: &Value) -> Vec<Change> {
    let mut out = Vec::new();
    walk(&mut Vec::new(), Some(old), Some(new), &mut out);
    out
}

fn walk(path: &mut Vec<String>, old: Option<&Value>, new: Option<&Value>, out: &mut Vec<Change>) {
    match (old, new) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                path.push(k.clone());
                walk(path, a.get(k), b.get(k), out);
                path.pop();
            }
        }
        (a, b) if absent(a) && absent(b) => {}
        (a, b) if absent(a) => out.push(Change {
            path: path.join("."),
            op: ChangeOp::Add,
            old: None,
            new: b.cloned(),
        }),
        (a, b) if absent(b) => out.push(Change {
            path: path.join("."),
            op: ChangeOp::Remove,
            old: a.cloned(),
            new: None,
        }),
        (Some(a), Some(b)) if a != b => out.push(Change {
            path: path.join("."),
            op: ChangeOp::Replace,
            old: Some(a.clone()),
            new: Some(b.clone()),
        }),
        _ => {}
    }
}

/// `top`을 `base`에 얹는다: 둘 다 객체면 칸마다 내려가고, 그 밖(목록·값)은 `top`으로 바꾼다. `top`의 `null`은 그 칸을
/// 지운다(기본값으로).
pub fn overlay(base: &mut Value, top: &Value) {
    match (base, top) {
        (Value::Object(b), Value::Object(t)) => {
            for (k, v) in t {
                if v.is_null() {
                    b.remove(k);
                } else {
                    match b.get_mut(k) {
                        Some(slot) if slot.is_object() && v.is_object() && same_tag(slot, v) => {
                            overlay(slot, v)
                        }
                        // 새 객체도 얹는 것처럼(안의 `null`은 칸이 없음)
                        _ if v.is_object() => {
                            let mut fresh = Value::Object(Map::new());
                            overlay(&mut fresh, v);
                            b.insert(k.clone(), fresh);
                        }
                        _ => {
                            b.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        (b, t) => *b = t.clone(),
    }
}

/// 태그 붙은 갈래(`kind`·`type` 칸) 객체가 같은 갈래인지. 갈래가 바뀌면 칸마다 얹지 않고 통째로 바꾼다 — 옛 갈래의 칸이
/// 남으면 새 갈래 꼴(`deny_unknown_fields`)에 맞지 않는다(DNS 공급자 `cloudflare` → `power_dns` 등).
fn same_tag(a: &Value, b: &Value) -> bool {
    ["kind", "type"]
        .iter()
        .all(|t| match (a.get(*t), b.get(*t)) {
            (Some(x), Some(y)) => x == y,
            _ => true,
        })
}

/// 객체 칸을 따라 내려간 끝 칸들의 경로(목록·값·빈 객체가 끝이다). 맨 위가 객체가 아니면 없다.
pub fn leaf_paths(v: &Value) -> Vec<Vec<String>> {
    fn go(v: &Map<String, Value>, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        for (k, child) in v {
            path.push(k.clone());
            match child {
                Value::Object(m) if !m.is_empty() => go(m, path, out),
                _ => out.push(path.clone()),
            }
            path.pop();
        }
    }
    let mut out = Vec::new();
    if let Value::Object(m) = v {
        go(m, &mut Vec::new(), &mut out);
    }
    out
}

/// 경로가 가리키는 값(객체 칸만 따라간다)
pub fn get_path<'a>(v: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut at = v;
    for k in path {
        at = at.as_object()?.get(k)?;
    }
    Some(at)
}

/// 경로의 칸을 지운다(없으면 그대로).
pub fn remove_path(v: &mut Value, path: &[String]) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut at = v;
    for k in parents {
        match at.as_object_mut().and_then(|m| m.get_mut(k)) {
            Some(next) => at = next,
            None => return,
        }
    }
    if let Some(m) = at.as_object_mut() {
        m.remove(last);
    }
}

/// 세 갈래(§8.2 4): 앞 선언 `prev`에 있었는데 이번 선언 `now`에 없는 끝 칸을 `merged`에서 지운다(그 칸은 기본값으로
/// 돌아간다). 이번 선언에 그 칸의 조상이 값(목록·값)으로 있으면 이번 값이 이긴 것이라 지우지 않는다.
pub fn forget_dropped(merged: &mut Value, prev: &Value, now: &Value) {
    for path in leaf_paths(prev) {
        let kept = (1..=path.len()).any(|n| {
            get_path(now, &path[..n])
                .is_some_and(|v| !v.is_null() && (n == path.len() || !v.is_object()))
        });
        if !kept {
            remove_path(merged, &path);
        }
    }
}

/// 내보내기 `minimal`(§8.4): 객체 안의 `null`·`false`·빈 목록·빈 객체 칸을 지운다(서버 기본값과 같은 것 대부분).
pub fn strip_empty(v: &mut Value) {
    if let Value::Object(m) = v {
        for child in m.values_mut() {
            strip_empty(child);
        }
        m.retain(|_, c| match c {
            Value::Null | Value::Bool(false) => false,
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => true,
        });
    }
}

/// 맨 위 칸 이름들(바뀐 경로의 첫 조각, 차례대로 하나씩)
pub fn top_fields(changes: &[Change]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in changes {
        let top = c.path.split('.').next().unwrap_or_default().to_string();
        if !out.contains(&top) {
            out.push(top);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn diff_walks_objects_and_compares_lists_whole() {
        let old = json!({ "cpu": { "cores": 2, "model": "host" }, "nics": [{ "network": "a" }], "mac": null });
        let new = json!({ "cpu": { "cores": 4, "model": "host" }, "nics": [{ "network": "b" }], "disk": 1 });
        let d = diff(&old, &new);
        let paths: Vec<_> = d.iter().map(|c| (c.path.as_str(), c.op)).collect();
        assert_eq!(
            paths,
            [
                ("cpu.cores", ChangeOp::Replace),
                ("disk", ChangeOp::Add),
                ("nics", ChangeOp::Replace)
            ]
        );
        assert_eq!(d[0].old, Some(json!(2)));
        assert_eq!(d[0].new, Some(json!(4)));
        // null은 없는 것과 같다.
        assert!(diff(&json!({ "a": null }), &json!({})).is_empty());
        assert_eq!(diff(&json!({ "a": 1 }), &json!({}))[0].op, ChangeOp::Remove);
    }

    #[test]
    fn overlay_merges_objects_and_replaces_lists() {
        let mut base = json!({ "cpu": { "cores": 2, "model": "host" }, "nics": [1, 2], "x": 1 });
        overlay(
            &mut base,
            &json!({ "cpu": { "cores": 4 }, "nics": [3], "x": null }),
        );
        assert_eq!(
            base,
            json!({ "cpu": { "cores": 4, "model": "host" }, "nics": [3] })
        );
    }

    #[test]
    fn a_changed_tag_replaces_the_object() {
        let mut base =
            json!({ "provider": { "kind": "cloudflare", "api_token": "t" }, "zones": [] });
        overlay(
            &mut base,
            &json!({ "provider": { "kind": "hook", "path": "/etc/opl/acme-hooks/x" } }),
        );
        assert_eq!(
            base["provider"],
            json!({ "kind": "hook", "path": "/etc/opl/acme-hooks/x" })
        );
        let mut base = json!({ "scope": { "kind": "per_node", "nodes": ["a"] } });
        overlay(&mut base, &json!({ "scope": { "nodes": ["b"] } }));
        assert_eq!(base["scope"], json!({ "kind": "per_node", "nodes": ["b"] }));
    }

    #[test]
    fn dropped_fields_go_back_to_defaults() {
        let prev =
            json!({ "description": "x", "cpu": { "cores": 4, "flags": ["a"] }, "rules": [1] });
        let now = json!({ "cpu": { "cores": 4 }, "rules": [2] });
        let mut merged = json!({ "description": "x", "cpu": { "cores": 4, "flags": ["a"], "model": "host" }, "rules": [2] });
        forget_dropped(&mut merged, &prev, &now);
        assert_eq!(
            merged,
            json!({ "cpu": { "cores": 4, "model": "host" }, "rules": [2] })
        );
        // 이번 선언이 조상을 값으로 두면 지우지 않는다.
        let mut merged = json!({ "a": [1] });
        forget_dropped(
            &mut merged,
            &json!({ "a": { "b": 1 } }),
            &json!({ "a": [1] }),
        );
        assert_eq!(merged, json!({ "a": [1] }));
    }

    #[test]
    fn minimal_drops_empty_values() {
        let mut v =
            json!({ "a": null, "b": false, "c": [], "d": {}, "e": { "f": [] }, "g": 0, "h": true });
        strip_empty(&mut v);
        assert_eq!(v, json!({ "g": 0, "h": true }));
    }

    fn json_leaf() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            (0i64..5).prop_map(|n| json!(n)),
            "[a-c]".prop_map(Value::String),
        ]
    }

    fn json_value() -> impl Strategy<Value = Value> {
        json_leaf().prop_recursive(3, 24, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
                prop::collection::btree_map("[a-d]", inner, 0..4)
                    .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    fn object() -> impl Strategy<Value = Value> {
        prop::collection::btree_map("[a-d]", json_value(), 0..5)
            .prop_map(|m| Value::Object(m.into_iter().collect()))
    }

    proptest! {
        /// 얹은 뒤에는 선언한 끝 칸이 모두 선언 값이고, 다시 얹어도 같다(몇 번 적용해도 같음).
        #[test]
        fn overlay_holds_every_declared_leaf(base in object(), top in object()) {
            let mut merged = base.clone();
            overlay(&mut merged, &top);
            for path in leaf_paths(&top) {
                let want = get_path(&top, &path).unwrap();
                // 빈 객체는 "칸을 선언하지 않음"이라 지금 칸을 남긴다.
                if want.is_null() || want.is_object() {
                    continue;
                }
                // 객체 안의 null은 칸이 없는 것과 같다.
                let mut got = get_path(&merged, &path).cloned();
                let mut want = want.clone();
                got.iter_mut().for_each(strip_nulls);
                strip_nulls(&mut want);
                prop_assert_eq!(got, Some(want));
            }
            let mut again = merged.clone();
            overlay(&mut again, &top);
            prop_assert_eq!(again, merged);
        }

        /// 같은 값은 바뀜이 없고, 바뀜이 없으면 null을 뺀 값이 같다.
        #[test]
        fn diff_is_empty_only_for_equal_values(a in object(), b in object()) {
            prop_assert!(diff(&a, &a).is_empty());
            let mut a2 = a.clone();
            let mut b2 = b.clone();
            strip_nulls(&mut a2);
            strip_nulls(&mut b2);
            prop_assert_eq!(diff(&a, &b).is_empty(), a2 == b2);
        }
    }

    fn strip_nulls(v: &mut Value) {
        if let Value::Object(m) = v {
            m.retain(|_, c| !c.is_null());
            for c in m.values_mut() {
                strip_nulls(c);
            }
        }
    }
}
