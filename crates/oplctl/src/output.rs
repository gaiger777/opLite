//! 표와 JSON 출력.

use std::io::Write;

use serde_json::Value;

/// 열 너비를 맞춘 표. 마지막 열은 채우지 않는다.
pub fn table(out: &mut dyn Write, headers: &[&str], rows: &[Vec<String>]) -> std::io::Result<()> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |out: &mut dyn Write, cells: Vec<&str>| -> std::io::Result<()> {
        let last = cells.len() - 1;
        let mut s = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i == last {
                s.push_str(cell);
            } else {
                s.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        writeln!(out, "{}", s.trim_end())
    };
    line(out, headers.to_vec())?;
    for row in rows {
        line(out, row.iter().map(String::as_str).collect())?;
    }
    Ok(())
}

pub fn json(out: &mut dyn Write, v: &Value) -> std::io::Result<()> {
    writeln!(out, "{}", serde_json::to_string_pretty(v).unwrap())
}

/// 값을 표의 칸으로. 없으면 `-`.
pub fn cell(v: &Value) -> String {
    match v {
        Value::Null => "-".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// 바이트 수를 사람이 읽기 쉽게(1024 단위)
pub fn bytes(v: &Value) -> String {
    let Some(n) = v.as_u64() else {
        return "-".into();
    };
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n}B")
    } else {
        format!("{size:.1}{}", units[unit])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn aligns_columns() {
        let mut out = Vec::new();
        table(
            &mut out,
            &["NAME", "STATE"],
            &[
                vec!["a".into(), "running".into()],
                vec!["longer".into(), "-".into()],
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "NAME    STATE\na       running\nlonger  -\n"
        );
    }

    #[test]
    fn formats_cells_and_sizes() {
        assert_eq!(cell(&Value::Null), "-");
        assert_eq!(cell(&json!("x")), "x");
        assert_eq!(cell(&json!(3)), "3");
        assert_eq!(bytes(&json!(512)), "512B");
        assert_eq!(bytes(&json!(10u64 << 30)), "10.0GiB");
        assert_eq!(bytes(&Value::Null), "-");
    }
}
