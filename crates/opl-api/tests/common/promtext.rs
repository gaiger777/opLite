//! 시험용 엄격한 Prometheus 텍스트 형식(0.0.4) 파서. 받아들이는 것보다 좁게 본다: `# HELP`·`# TYPE`은 이름마다 한 번이고
//! TYPE은 그 이름의 첫 줄보다 앞, 같은 이름의 줄은 이어져 있고, 이름·레이블 이름 규칙, 레이블 값 이스케이프는 `\\`·`\"`·
//! `\n`만, 같은 이름·레이블 묶음은 한 번만, 빈 줄과 끝의 줄바꿈 없음은 오류다.

use std::collections::{BTreeMap, BTreeSet};

/// 지표 하나(같은 이름의 줄들)
#[derive(Debug, Clone, PartialEq)]
pub struct Family {
    pub name: String,
    pub help: Option<String>,
    pub kind: Option<String>,
    pub samples: Vec<(BTreeMap<String, String>, f64)>,
}

fn metric_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == ':')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

fn label_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    !name.starts_with("__")
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn value(s: &str) -> Result<f64, String> {
    match s {
        "NaN" => Ok(f64::NAN),
        "+Inf" => Ok(f64::INFINITY),
        "-Inf" => Ok(f64::NEG_INFINITY),
        _ if s.contains(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E') => {
            Err(format!("bad value {s:?}"))
        }
        _ => s.parse().map_err(|_| format!("bad value {s:?}")),
    }
}

/// `{a="1",b="2"}` 다음의 나머지와 레이블
fn labels(rest: &str) -> Result<(BTreeMap<String, String>, &str), String> {
    let mut out = BTreeMap::new();
    let Some(mut rest) = rest.strip_prefix('{') else {
        return Ok((out, rest));
    };
    loop {
        if let Some(after) = rest.strip_prefix('}') {
            return Ok((out, after));
        }
        let eq = rest.find('=').ok_or("label without =")?;
        let name = &rest[..eq];
        if !label_name_ok(name) {
            return Err(format!("bad label name {name:?}"));
        }
        rest = rest[eq + 1..]
            .strip_prefix('"')
            .ok_or("label value must be quoted")?;
        let mut v = String::new();
        let mut chars = rest.char_indices();
        let end = loop {
            match chars.next() {
                None => return Err("unterminated label value".into()),
                Some((i, '"')) => break i,
                Some((_, '\\')) => match chars.next() {
                    Some((_, '\\')) => v.push('\\'),
                    Some((_, '"')) => v.push('"'),
                    Some((_, 'n')) => v.push('\n'),
                    other => return Err(format!("bad escape {other:?}")),
                },
                Some((_, '\n')) => return Err("raw newline in a label value".into()),
                Some((_, c)) => v.push(c),
            }
        };
        if out.insert(name.to_string(), v).is_some() {
            return Err(format!("label {name:?} twice"));
        }
        rest = &rest[end + 1..];
        if let Some(after) = rest.strip_prefix(',') {
            rest = after;
            if rest.starts_with('}') {
                // 끝의 쉼표는 0.0.4가 받지만 여기서는 쓰지 않는다.
                return Err("trailing comma".into());
            }
        } else if !rest.starts_with('}') {
            return Err("expected , or }".into());
        }
    }
}

/// 텍스트를 지표들로 푼다(나온 순서).
pub fn parse(text: &str) -> Result<Vec<Family>, String> {
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("no newline at the end".into());
    }
    let mut families: Vec<Family> = Vec::new();
    let mut closed: BTreeSet<String> = BTreeSet::new();
    let mut seen: BTreeSet<(String, Vec<(String, String)>)> = BTreeSet::new();
    let family = |families: &mut Vec<Family>, closed: &mut BTreeSet<String>, name: &str| {
        if families.last().is_some_and(|f| f.name == name) {
            return Ok(families.len() - 1);
        }
        if let Some(last) = families.last() {
            closed.insert(last.name.clone());
        }
        if closed.contains(name) || families.iter().any(|f| f.name == name) {
            return Err(format!("{name} is not contiguous"));
        }
        families.push(Family {
            name: name.to_string(),
            help: None,
            kind: None,
            samples: vec![],
        });
        Ok(families.len() - 1)
    };
    for (n, line) in text.lines().enumerate() {
        let at = |e: String| format!("line {}: {e}: {line:?}", n + 1);
        if line.is_empty() {
            return Err(at("empty line".into()));
        }
        if let Some(comment) = line.strip_prefix('#') {
            let mut parts = comment.trim_start().splitn(3, ' ');
            let (word, name, rest) = (parts.next(), parts.next(), parts.next());
            match (word, name) {
                (Some("HELP"), Some(name)) | (Some("TYPE"), Some(name)) => {
                    if !metric_name_ok(name) {
                        return Err(at("bad metric name".into()));
                    }
                    let i = family(&mut families, &mut closed, name).map_err(at)?;
                    let f = &mut families[i];
                    if word == Some("HELP") {
                        if f.help.replace(rest.unwrap_or("").to_string()).is_some() {
                            return Err(at("HELP twice".into()));
                        }
                    } else {
                        let kind = rest.ok_or_else(|| at("TYPE without a type".into()))?;
                        if !["counter", "gauge", "histogram", "summary", "untyped"].contains(&kind)
                        {
                            return Err(at(format!("unknown type {kind}")));
                        }
                        if !f.samples.is_empty() {
                            return Err(at("TYPE after samples".into()));
                        }
                        if f.kind.replace(kind.to_string()).is_some() {
                            return Err(at("TYPE twice".into()));
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let name_end = line.find(['{', ' ']).ok_or_else(|| at("no value".into()))?;
        let name = &line[..name_end];
        if !metric_name_ok(name) {
            return Err(at("bad metric name".into()));
        }
        let (labels, rest) = labels(&line[name_end..]).map_err(at)?;
        let rest = rest
            .strip_prefix(' ')
            .ok_or_else(|| at("expected a space before the value".into()))?;
        let mut parts = rest.split(' ');
        let v = value(parts.next().unwrap_or("")).map_err(at)?;
        if let Some(ts) = parts.next() {
            ts.parse::<i64>()
                .map_err(|_| at(format!("bad timestamp {ts:?}")))?;
        }
        if parts.next().is_some() {
            return Err(at("trailing text".into()));
        }
        let key = (
            name.to_string(),
            labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        );
        if !seen.insert(key) {
            return Err(at("same name and labels twice".into()));
        }
        let i = family(&mut families, &mut closed, name).map_err(at)?;
        families[i].samples.push((labels, v));
    }
    Ok(families)
}

/// 이름이 `name`인 지표의 줄들(없으면 빈 목록)
pub fn samples<'a>(families: &'a [Family], name: &str) -> &'a [(BTreeMap<String, String>, f64)] {
    families
        .iter()
        .find(|f| f.name == name)
        .map_or(&[], |f| f.samples.as_slice())
}
