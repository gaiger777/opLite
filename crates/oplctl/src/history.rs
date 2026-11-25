//! `orbivisorctl history·restore·drift`(계획 73 §12·§13): 설정 이력 타임라인과 줄 하나의 비교, 리소스 하나·지운 것·한
//! 시점으로 되돌리기(서버가 선언형 계획을 세운다 — 보이고 묻고 그 계획을 적용한다), 적용 묶음 드리프트.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::bail;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::client::{Client, seg};
use crate::declarative::{Exit, post, print_plan, summary};
use crate::metrics::{ms_time, now_ms, time_ms};
use crate::output::{cell, json as print_json, table};

/// 되돌리기 Task를 기다리는 한도
const RESTORE_WAIT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct HistoryArgs {
    #[command(subcommand)]
    cmd: Option<HistoryCmd>,
    /// 이 종류만(`vm`·`Vm`) — NAME과 함께면 그 리소스 하나
    kind: Option<String>,
    /// 리소스 이름 또는 id(지운 것도)
    name: Option<String>,
    /// 바꾼 사용자
    #[arg(long)]
    user: Option<String>,
    /// 출처(`web`·`cli`·`api`·`apply`·`terraform`·`task`·`system`)
    #[arg(long)]
    source: Option<String>,
    /// 이때부터(`24h`·`7d` 전, 유닉스 밀리초, RFC 3339)
    #[arg(long)]
    since: Option<String>,
    /// 이 폴더 경로와 그 아래(`/prod` 또는 `vm:/prod`)
    #[arg(long)]
    folder: Option<String>,
    /// 이 적용 묶음이 쓴 줄
    #[arg(long = "set", value_name = "NAME")]
    set: Option<String>,
    /// 줄 수(기본 100)
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Subcommand)]
pub(crate) enum HistoryCmd {
    /// 줄 하나의 전체 spec과 비교(`--compare prev|live|REV`)
    Show {
        kind: String,
        name: String,
        revision: u64,
        #[arg(long, default_value = "prev")]
        compare: String,
    },
}

#[derive(Args)]
pub(crate) struct RestoreArgs {
    /// 리소스 하나: 종류(`vm`)
    #[arg(requires = "name", required_unless_present = "at")]
    kind: Option<String>,
    /// 리소스 하나: 이름 또는 id(지운 것도)
    name: Option<String>,
    /// 되돌릴 이력 줄(리소스 하나)
    #[arg(long, requires = "kind", conflicts_with = "at")]
    revision: Option<u64>,
    /// 지운 것의 이름이 그새 쓰이면 이 이름으로 살린다(새 id)
    #[arg(long = "name", value_name = "NEW", requires = "revision")]
    new_name: Option<String>,
    /// 한 시점으로: revision(정수) 또는 시각(`2h` 전·RFC 3339)
    #[arg(long, conflicts_with_all = ["kind", "revision"])]
    at: Option<String>,
    /// 시점 범위: 이 폴더와 그 아래
    #[arg(long, requires = "at")]
    folder: Option<String>,
    /// 시점 범위: 이 적용 묶음
    #[arg(long = "set", value_name = "NAME", requires = "at")]
    set: Option<String>,
    /// 시점 범위: 이 종류(여러 번)
    #[arg(long = "kind", value_name = "KIND", requires = "at")]
    kinds: Vec<String>,
    /// 시점 범위: `종류/이름`(여러 번)
    #[arg(long = "key", value_name = "KIND/NAME", requires = "at")]
    keys: Vec<String>,
    /// 그 뒤 만든 것을 지운다
    #[arg(long, requires = "at")]
    delete_created: bool,
    /// 그 뒤 만든 VM·볼륨·백업 저장소도 지운다
    #[arg(long, requires = "delete_created")]
    allow_data_loss: bool,
    /// 그 뒤 지운 것을 살리지 않는다
    #[arg(long, requires = "at")]
    no_recreate: bool,
    /// Terraform·다른 묶음이 관리하는 객체도 되돌린다
    #[arg(long)]
    force_conflicts: bool,
    /// 계획만 보인다
    #[arg(long)]
    dry_run: bool,
    /// 바꾸는 까닭(설정 이력에 남는다)
    #[arg(long)]
    reason: Option<String>,
    /// 묻지 않는다(단말이 아니면 있어야 한다)
    #[arg(short, long)]
    yes: bool,
    /// 적용 Task를 기다리지 않는다
    #[arg(long)]
    no_wait: bool,
}

/// 이력 표 한 줄
fn history_row(e: &Value) -> Vec<String> {
    let user = e["actor"]["user"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            e["actor"]["auth"]["name"]
                .as_str()
                .map(|t| format!("token:{t}"))
        })
        .unwrap_or_else(|| "-".into());
    let mut changed: Vec<String> = e["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect();
    if changed.len() > 4 {
        let n = changed.len() - 3;
        changed.truncate(3);
        changed.push(format!("+{n}"));
    }
    let mut source = cell(&e["source"]);
    if !e["restored_from"].is_null() {
        source = format!("{source} (restore r{})", cell(&e["restored_from"]));
    }
    vec![
        cell(&e["revision"]),
        e["at_ms"].as_i64().map_or_else(|| "-".into(), ms_time),
        user,
        source,
        cell(&e["op"]),
        cell(&e["kind"]),
        cell(&e["name"]),
        changed.join(","),
    ]
}

/// `history`
pub(crate) async fn history(
    client: &Client,
    args: HistoryArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if let Some(HistoryCmd::Show {
        kind,
        name,
        revision,
        compare,
    }) = args.cmd
    {
        let row = client
            .get(&format!(
                "/history/{}/{}/{revision}?compare={}",
                seg(&kind),
                seg(&name),
                seg(&compare)
            ))
            .await?;
        if json_out {
            return Ok(print_json(out, &row)?);
        }
        let r = &row["record"];
        writeln!(
            out,
            "{} {} r{} {} by {} via {}{}",
            cell(&r["kind"]),
            cell(&r["name"]),
            cell(&r["revision"]),
            cell(&r["op"]),
            r["actor"]["user"].as_str().unwrap_or("system"),
            cell(&r["source"]),
            r["reason"]
                .as_str()
                .map_or_else(String::new, |s| format!(": {s}"))
        )?;
        writeln!(
            out,
            "compared with {}{}:",
            cell(&row["compare"]),
            row["against"]
                .as_u64()
                .map_or_else(String::new, |a| format!(" (r{a})"))
        )?;
        let diff = row["diff"].as_array().cloned().unwrap_or_default();
        if diff.is_empty() {
            writeln!(out, "  (no differences)")?;
        }
        for c in &diff {
            let line = match c["op"].as_str() {
                Some("add") => format!("  + {}: {}", cell(&c["path"]), c["new"]),
                Some("remove") => format!("  - {}: {}", cell(&c["path"]), c["old"]),
                _ => format!("  ~ {}: {} -> {}", cell(&c["path"]), c["old"], c["new"]),
            };
            writeln!(out, "{line}")?;
        }
        return Ok(());
    }
    let mut q: Vec<String> = Vec::new();
    match (&args.kind, &args.name) {
        (Some(k), Some(n)) => {
            q.push(format!("kind={}", seg(k)));
            q.push(format!("key={}", seg(n)));
        }
        (Some(k), None) => q.push(format!("kind={}", seg(k))),
        _ => {}
    }
    for (k, v) in [
        ("user", &args.user),
        ("source", &args.source),
        ("folder", &args.folder),
        ("apply_set", &args.set),
    ] {
        if let Some(v) = v {
            q.push(format!("{k}={}", seg(v)));
        }
    }
    if let Some(s) = &args.since {
        q.push(format!("since={}", time_ms(s, now_ms())?));
    }
    if let Some(l) = args.limit {
        q.push(format!("limit={l}"));
    }
    let path = if q.is_empty() {
        "/history".to_string()
    } else {
        format!("/history?{}", q.join("&"))
    };
    let list = client.get(&path).await?;
    if json_out {
        return Ok(print_json(out, &list)?);
    }
    let rows: Vec<Vec<String>> = list["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(history_row)
        .collect();
    table(
        out,
        &[
            "REVISION", "TIME", "USER", "SOURCE", "OP", "KIND", "NAME", "CHANGED",
        ],
        &rows,
    )?;
    if let Some(next) = list["next_before"].as_u64() {
        writeln!(out, "(more: the server has older rows before r{next})")?;
    }
    Ok(())
}

/// 되돌리기 요청 본문과 경로
fn restore_request(args: &RestoreArgs) -> anyhow::Result<(String, Value)> {
    if let Some(at) = &args.at {
        let point = match at.parse::<u64>() {
            Ok(r) => json!({ "revision": r }),
            Err(_) => json!({ "time": time_ms(at, now_ms())?.to_string() }),
        };
        let mut scope = json!({});
        if let Some(f) = &args.folder {
            scope["folder"] = json!(f);
        }
        if let Some(s) = &args.set {
            scope["apply_set"] = json!(s);
        }
        if !args.kinds.is_empty() {
            scope["kinds"] = json!(args.kinds);
        }
        if !args.keys.is_empty() {
            scope["keys"] = json!(args.keys);
        }
        if scope.as_object().is_some_and(|o| o.is_empty()) {
            bail!("give a scope with --folder, --set, --kind or --key");
        }
        let mut body = json!({
            "at": point,
            "scope": scope,
            "dry_run": true,
            "recreate_deleted": !args.no_recreate,
            "delete_created": args.delete_created,
            "allow_data_loss": args.allow_data_loss,
        });
        if args.force_conflicts {
            body["force_conflicts"] = json!(true);
        }
        return Ok(("/history/restore".into(), body));
    }
    let (Some(kind), Some(name)) = (&args.kind, &args.name) else {
        bail!("give KIND NAME --revision REV, or --at");
    };
    let Some(revision) = args.revision else {
        bail!("give --revision REV (see `orbivisorctl history {kind} {name}`)");
    };
    let mut body = json!({ "revision": revision, "dry_run": true });
    if let Some(n) = &args.new_name {
        body["name"] = json!(n);
    }
    if args.force_conflicts {
        body["force_conflicts"] = json!(true);
    }
    Ok((
        format!("/history/{}/{}/restore", seg(kind), seg(name)),
        body,
    ))
}

/// `restore`: 서버의 계획을 보이고 묻고 그 계획을 적용한다(`POST /apply {plan}` — 그사이 바뀌었으면 412).
pub(crate) async fn restore(
    client: &Client,
    args: RestoreArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let (path, body) = restore_request(&args)?;
    let plan = post(client, &path, &body, None).await?;
    if json_out {
        print_json(out, &plan)?;
    } else {
        print_plan(out, &plan)?;
    }
    let [c, u, d, _, blocked] = summary(&plan);
    if args.dry_run {
        return Ok(());
    }
    if blocked > 0 {
        bail!("{blocked} step(s) are blocked; nothing was restored");
    }
    if c + u + d == 0 {
        if !json_out {
            writeln!(out, "No changes.")?;
        }
        return Ok(());
    }
    if !args.yes {
        if !std::io::stdin().is_terminal() {
            bail!("not a terminal: give --yes to restore");
        }
        eprint!("Restore? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            bail!("not restored");
        }
    }
    let started = post(
        client,
        "/apply",
        &json!({ "plan": plan["plan"] }),
        args.reason.as_deref(),
    )
    .await?;
    let task = &started["task"];
    if args.no_wait {
        if json_out {
            print_json(out, &started)?;
        } else {
            writeln!(out, "restore started (task {})", cell(&task["id"]))?;
        }
        return Ok(());
    }
    let done = client.wait_task_for(task, RESTORE_WAIT).await?;
    if json_out {
        print_json(out, &done)?;
    } else {
        writeln!(out, "Restored: {c} created, {u} updated, {d} deleted.")?;
    }
    Ok(())
}

/// 드리프트 표 한 줄
fn drift_rows(set: &Value) -> Vec<Vec<String>> {
    set["status"]["drift"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|i| {
            let by = &i["by"];
            let who = if by.is_null() {
                "-".to_string()
            } else {
                format!(
                    "{} via {} at {}{}",
                    by["actor"]["user"].as_str().unwrap_or("system"),
                    cell(&by["source"]),
                    by["at_ms"].as_i64().map_or_else(|| "-".into(), ms_time),
                    by["reason"]
                        .as_str()
                        .map_or_else(String::new, |r| format!(" ({r})"))
                )
            };
            vec![
                cell(&set["name"]),
                cell(&i["kind"]),
                cell(&i["name"]),
                cell(&i["state"]),
                i["paths"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
                who,
            ]
        })
        .collect()
}

/// `drift [SET]`: 끝 코드 0 드리프트 없음, 1 있음
pub(crate) async fn drift(
    client: &Client,
    set: Option<String>,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let sets: Vec<Value> = match &set {
        Some(name) => vec![client.get(&format!("/apply-sets/{}", seg(name))).await?],
        None => client.get("/apply-sets").await?["items"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    };
    let rows: Vec<Vec<String>> = sets.iter().flat_map(drift_rows).collect();
    if json_out {
        let reports: Vec<Value> = sets
            .iter()
            .map(|s| json!({ "set": s["name"], "drift": s["status"]["drift"] }))
            .collect();
        print_json(out, &Value::Array(reports))?;
    } else if rows.is_empty() {
        writeln!(out, "No drift.")?;
    } else {
        table(
            out,
            &["SET", "KIND", "NAME", "STATE", "PATHS", "CHANGED BY"],
            &rows,
        )?;
        writeln!(
            out,
            "Apply the files again to undo, or `orbivisorctl set adopt SET` to accept the live values."
        )?;
    }
    if rows.is_empty() {
        Ok(())
    } else {
        Err(Exit {
            code: 1,
            message: None,
        }
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        r: RestoreArgs,
    }

    fn restore_args(argv: &[&str]) -> RestoreArgs {
        let mut all = vec!["t"];
        all.extend_from_slice(argv);
        T::try_parse_from(all).unwrap().r
    }

    #[test]
    fn restore_bodies_for_one_and_a_point_in_time() {
        let (path, body) =
            restore_request(&restore_args(&["vm", "web01", "--revision", "42"])).unwrap();
        assert_eq!(path, "/history/vm/web01/restore");
        assert_eq!(body, json!({ "revision": 42, "dry_run": true }));
        let (path, body) = restore_request(&restore_args(&[
            "--at",
            "1200",
            "--folder",
            "/prod",
            "--delete-created",
            "--allow-data-loss",
        ]))
        .unwrap();
        assert_eq!(path, "/history/restore");
        assert_eq!(body["at"], json!({ "revision": 1200 }));
        assert_eq!(body["scope"], json!({ "folder": "/prod" }));
        assert_eq!(body["delete_created"], true);
        assert_eq!(body["allow_data_loss"], true);
        assert_eq!(body["recreate_deleted"], true);
        let (_, body) = restore_request(&restore_args(&[
            "--at",
            "2026-11-24T17:00:00Z",
            "--kind",
            "vm",
        ]))
        .unwrap();
        assert_eq!(body["at"]["time"], "1795539600000");
        assert!(restore_request(&restore_args(&["--at", "5"])).is_err());
    }

    #[tokio::test]
    async fn drift_exits_one_and_names_who_changed_it() {
        let (client, _) = fake_api(|_, path| {
            assert_eq!(path, "/api/v1/apply-sets/web");
            (
                200,
                json!({ "name": "web", "status": { "drift": { "checked_at": 1, "revision": 9, "items": [
                    { "kind": "vm", "id": "0193", "name": "web01", "state": "modified", "paths": ["cpu.cores"],
                      "by": { "revision": 8, "at_ms": 0, "actor": { "user": "admin", "auth": { "kind": "session" } },
                              "source": "web", "reason": "load" } }
                ] } } }),
            )
        })
        .await;
        let mut out = Vec::new();
        let e = drift(&client, Some("web".into()), false, &mut out)
            .await
            .unwrap_err();
        assert_eq!(e.downcast_ref::<Exit>().unwrap().code, 1);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("cpu.cores"), "{text}");
        assert!(
            text.contains("admin via web at 1970-01-01T00:00:00Z (load)"),
            "{text}"
        );
        let (client, _) =
            fake_api(|_, _| (200, json!({ "items": [{ "name": "a", "status": {} }] }))).await;
        let mut out = Vec::new();
        drift(&client, None, false, &mut out).await.unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "No drift.\n");
    }

    #[tokio::test]
    async fn history_lists_rows_with_filters() {
        let (client, seen) = fake_api(|_, _| {
            (
                200,
                json!({ "items": [{ "revision": 7, "kind": "vm", "id": "x", "name": "web01", "op": "update",
                    "at_ms": 0, "actor": { "auth": { "kind": "token", "name": "ci" } }, "source": "cli",
                    "restored_from": 5, "changed": ["a", "b", "c", "d", "e"] }], "next_before": 7 }),
            )
        })
        .await;
        let args = HistoryArgs {
            cmd: None,
            kind: Some("vm".into()),
            name: Some("web01".into()),
            user: None,
            source: Some("cli".into()),
            since: None,
            folder: None,
            set: None,
            limit: Some(5),
        };
        let mut out = Vec::new();
        history(&client, args, false, &mut out).await.unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("token:ci"), "{text}");
        assert!(text.contains("cli (restore r5)"), "{text}");
        assert!(text.contains("a,b,c,+2"), "{text}");
        assert!(text.contains("before r7"), "{text}");
        let line = &seen.lock().unwrap()[0].line;
        assert!(
            line.contains("/history?kind=vm&key=web01&source=cli&limit=5"),
            "{line}"
        );
    }
}
