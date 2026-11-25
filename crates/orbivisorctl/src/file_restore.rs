//! `orbivisorctl backup files …`(계획 69 §14): 백업을 일회용 도우미 VM에 붙인 파일 복원 세션으로 파일을 찾아보고 꺼낸다.
//! 세션은 사용자·백업으로 다시 쓰므로 명령마다 `open`이 따로 필요 없다 — 없으면 열고 `ready`까지 기다린다(첫 명령은 도우미
//! 부팅만큼 느리다, 안내 한 줄은 표준 오류에).

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use futures_util::StreamExt;
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

/// 세션이 `ready`가 되기를 기다리는 한도(TCG 노드의 부팅 + 탐지)
const READY_WAIT: Duration = Duration::from_secs(600);
/// 세션 단계를 다시 묻는 간격
const POLL: Duration = Duration::from_millis(500);

#[derive(Subcommand)]
pub(crate) enum FilesCmd {
    /// 세션을 연다(이미 열려 있으면 그것). 세션 id를 보인다.
    Open {
        backup: String,
        /// 도우미가 디스크를 다 찾을 때까지(`ready`) 기다린다.
        #[arg(long)]
        wait: bool,
    },
    /// 백업의 볼륨(디스크·파티션·LVM 논리 볼륨)과 파일시스템
    Volumes { backup: String },
    /// 디렉터리 나열. 경로는 `VOLUME:/dir`(볼륨만 주면 루트)
    Ls {
        backup: String,
        #[arg(value_name = "VOLUME:/PATH")]
        path: String,
        /// 모드·소유자·크기·수정 시각도
        #[arg(short, long)]
        long: bool,
    },
    /// 파일·디렉터리를 내려받는다(여럿·디렉터리는 zip, `--format tar.gz`는 권한·링크 보존). 진행은 표준 오류에.
    Get {
        backup: String,
        /// 같은 볼륨의 `VOLUME:/path`(여러 개)
        #[arg(value_name = "VOLUME:/PATH", required = true)]
        paths: Vec<String>,
        /// 쓸 파일(`-`면 표준 출력). 없으면 서버가 정한 이름으로 지금 디렉터리에(`-o`는 전역 출력 형식이라 wget처럼 `-O`)
        #[arg(short = 'O', long, value_name = "FILE")]
        out_file: Option<String>,
        #[arg(long, value_enum, default_value_t = FormatArg::Auto)]
        format: FormatArg,
    },
    /// 실행 중인 VM 안으로 되돌려 쓴다(게스트 에이전트 `guest-file-*` — 디렉터리·덮어쓰기는 `guest-exec`도). 소유자·모드·
    /// 시각은 되돌리지 않는다.
    Restore {
        backup: String,
        /// 같은 볼륨의 `VOLUME:/path`(여러 개)
        #[arg(value_name = "VOLUME:/PATH", required = true)]
        paths: Vec<String>,
        /// 대상 VM(실행 중, 게스트 에이전트 채널)
        #[arg(long)]
        vm: String,
        /// 게스트 안 대상 디렉터리(Linux `/srv/restored`, Windows `C:\restored`)
        #[arg(long = "to", value_name = "DIR")]
        to: String,
        /// 같은 이름이 있을 때: keep(`.restored-…`로 새 이름), skip, overwrite(임시 파일에 다 쓴 뒤 바꿔 넣음)
        #[arg(long, value_enum, default_value_t = ConflictArg::Keep)]
        conflict: ConflictArg,
        /// Task가 끝날 때까지 기다린다.
        #[arg(long)]
        wait: bool,
    },
    /// 열린 세션(자기 것, `--all`이면 모두 — admin)
    Sessions {
        #[arg(long)]
        all: bool,
    },
    /// 세션을 닫는다(세션 id 또는 백업 — 그 백업의 자기 세션)
    Close {
        #[arg(value_name = "BACKUP|SESSION")]
        target: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FormatArg {
    Auto,
    Raw,
    Zip,
    #[value(name = "tar.gz")]
    TarGz,
}

impl FormatArg {
    fn as_str(self) -> &'static str {
        match self {
            FormatArg::Auto => "auto",
            FormatArg::Raw => "raw",
            FormatArg::Zip => "zip",
            FormatArg::TarGz => "tar.gz",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ConflictArg {
    Keep,
    Skip,
    Overwrite,
}

impl ConflictArg {
    fn as_str(self) -> &'static str {
        match self {
            ConflictArg::Keep => "keep",
            ConflictArg::Skip => "skip",
            ConflictArg::Overwrite => "overwrite",
        }
    }
}

/// 되돌려 쓰기 Task를 기다리는 한도(4 GiB를 TCG 게스트에 쓰는 시간까지)
const RESTORE_WAIT: Duration = Duration::from_secs(6 * 3600);

const VOLUME_HEADERS: &[&str] = &[
    "VOLUME", "DISK", "KIND", "FS", "LABEL", "SIZE", "STATE", "NOTE",
];
const LONG_HEADERS: &[&str] = &["MODE", "OWNER", "SIZE", "MODIFIED", "NAME"];
const SESSION_HEADERS: &[&str] = &["SESSION", "BACKUP", "NODE", "PHASE", "OPENED", "EXPIRES"];

pub(crate) async fn files(
    client: &Client,
    cmd: FilesCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        FilesCmd::Open { backup, wait } => {
            let session = open(client, &backup).await?;
            let session = if wait {
                wait_ready(client, &session, err).await?
            } else {
                session
            };
            if json_out {
                print_json(out, &session)?;
            } else {
                writeln!(
                    out,
                    "file restore session {} ({})",
                    cell(&session["id"]),
                    cell(&session["phase"])
                )?;
            }
        }
        FilesCmd::Volumes { backup } => {
            let session = ensure(client, &backup, err).await?;
            if json_out {
                return Ok(print_json(out, &session["volumes"])?);
            }
            let rows: Vec<Vec<String>> = items(&session["volumes"])
                .iter()
                .map(|v| {
                    vec![
                        cell(&v["id"]),
                        cell(&v["disk"]),
                        cell(&v["kind"]),
                        cell(&v["fs"]),
                        cell(&v["label"]),
                        bytes(&v["size"]),
                        cell(&v["state"]),
                        cell(&v["note"]),
                    ]
                })
                .collect();
            table(out, VOLUME_HEADERS, &rows)?;
        }
        FilesCmd::Ls { backup, path, long } => {
            let (volume, dir) = split_path(&path)?;
            let session = ensure(client, &backup, err).await?;
            let entries = list_all(client, &session, &volume, &dir).await?;
            if json_out {
                return Ok(print_json(out, &Value::Array(entries))?);
            }
            if long {
                let rows: Vec<Vec<String>> = entries.iter().map(long_row).collect();
                table(out, LONG_HEADERS, &rows)?;
            } else {
                for e in &entries {
                    writeln!(out, "{}", short_name(e))?;
                }
            }
        }
        FilesCmd::Get {
            backup,
            paths,
            out_file,
            format,
        } => {
            let (volume, picked) = same_volume(&paths)?;
            let session = ensure(client, &backup, err).await?;
            let id = cell(&session["id"]);
            let link = client
                .post(
                    &format!("/file-restore/sessions/{id}/download-link"),
                    &json!({ "volume": volume, "paths": picked, "format": format.as_str() }),
                )
                .await?;
            let url = link["url"].as_str().context("no download url")?;
            download(client, url, out_file.as_deref(), out, err).await?;
        }
        FilesCmd::Restore {
            backup,
            paths,
            vm,
            to,
            conflict,
            wait,
        } => {
            let (volume, picked) = same_volume(&paths)?;
            let session = ensure(client, &backup, err).await?;
            let id = cell(&session["id"]);
            let task = client
                .post(
                    &format!("/file-restore/sessions/{}/restore-to-vm", seg(&id)),
                    &json!({
                        "volume": volume, "paths": picked, "vm": vm, "target_dir": to,
                        "conflict": conflict.as_str(),
                    }),
                )
                .await?;
            if !wait {
                if json_out {
                    return Ok(print_json(out, &task)?);
                }
                writeln!(
                    out,
                    "restoring into vm {vm}:{to} (task {})",
                    cell(&task["id"])
                )?;
                return Ok(());
            }
            let done = client.wait_task_for(&task, RESTORE_WAIT).await?;
            if json_out {
                return Ok(print_json(out, &done)?);
            }
            match done["status"]["error"].as_str() {
                Some(summary) => writeln!(out, "{summary}")?,
                None => writeln!(out, "restored into vm {vm}:{to}")?,
            }
        }
        FilesCmd::Sessions { all } => {
            let mut r = client.request(Method::GET, "/file-restore/sessions");
            if all {
                r = r.query(&[("all", "true")]);
            }
            let list = client.send(r).await?;
            if json_out {
                return Ok(print_json(out, &list)?);
            }
            let rows: Vec<Vec<String>> = items(&list)
                .iter()
                .map(|s| {
                    vec![
                        cell(&s["id"]),
                        cell(&s["backup_name"]),
                        cell(&s["node"]),
                        cell(&s["phase"]),
                        time_cell(&s["opened_at"]),
                        time_cell(&s["expires_at"]),
                    ]
                })
                .collect();
            table(out, SESSION_HEADERS, &rows)?;
        }
        FilesCmd::Close { target } => {
            let ids: Vec<String> = if uuid::Uuid::parse_str(&target).is_ok()
                && client
                    .get(&format!("/file-restore/sessions/{}", seg(&target)))
                    .await
                    .is_ok()
            {
                vec![target.clone()]
            } else {
                let mut r = client.request(Method::GET, "/file-restore/sessions");
                r = r.query(&[("backup", target.as_str())]);
                items(&client.send(r).await?)
                    .iter()
                    .map(|s| cell(&s["id"]))
                    .collect()
            };
            if ids.is_empty() {
                bail!("no open file restore session of {target:?}");
            }
            for id in ids {
                client
                    .delete(&format!("/file-restore/sessions/{}", seg(&id)))
                    .await?;
                writeln!(out, "file restore session {id} closed")?;
            }
        }
    }
    Ok(())
}

fn items(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// 세션을 연다(있으면 그것)
async fn open(client: &Client, backup: &str) -> anyhow::Result<Value> {
    let r = client.request(
        Method::POST,
        &format!("/backups/{}/file-restore", seg(backup)),
    );
    Ok(client.send(r).await?["session"].clone())
}

/// 세션이 `ready`가 될 때까지 기다린다(실패면 그 까닭).
async fn wait_ready(
    client: &Client,
    session: &Value,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<Value> {
    let id = cell(&session["id"]);
    let started = Instant::now();
    if session["phase"] != "ready" {
        writeln!(
            err,
            "opening a file restore session for backup {} (the helper VM is booting; this can take a minute)",
            cell(&session["backup_name"])
        )?;
    }
    loop {
        let s = client
            .get(&format!("/file-restore/sessions/{}", seg(&id)))
            .await?;
        match s["phase"].as_str() {
            Some("ready") => return Ok(s),
            Some("failed") | Some("closed") => bail!(
                "file restore session {id} {}: {}",
                cell(&s["phase"]),
                cell(&s["error"])
            ),
            _ => {}
        }
        if started.elapsed() > READY_WAIT {
            bail!("file restore session {id} is not ready after {READY_WAIT:?}");
        }
        tokio::time::sleep(POLL).await;
    }
}

/// 백업의 자기 세션(없으면 열고) — `ready`까지 기다린다.
async fn ensure(
    client: &Client,
    backup: &str,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<Value> {
    let session = open(client, backup).await?;
    if session["phase"] == "ready" && session["volumes"].is_array() {
        return Ok(session);
    }
    wait_ready(client, &session, err).await
}

/// 여러 `VOLUME:/path` → (볼륨, 경로들). 볼륨이 다르면 오류
fn same_volume(paths: &[String]) -> anyhow::Result<(String, Vec<String>)> {
    let mut volume: Option<String> = None;
    let mut picked = Vec::new();
    for p in paths {
        let (v, path) = split_path(p)?;
        if let Some(x) = &volume
            && *x != v
        {
            bail!("all paths must be on the same volume ({x} and {v})");
        }
        volume = Some(v);
        picked.push(path);
    }
    Ok((volume.unwrap_or_default(), picked))
}

/// `VOLUME:/path` → (볼륨, 경로). 볼륨만이면 루트
fn split_path(s: &str) -> anyhow::Result<(String, String)> {
    let (volume, path) = s.split_once(':').unwrap_or((s, "/"));
    if volume.is_empty() {
        bail!("give the path as VOLUME:/path (see `backup files volumes`)");
    }
    let path = if path.is_empty() { "/" } else { path };
    Ok((volume.to_string(), path.to_string()))
}

/// 디렉터리의 모든 쪽
async fn list_all(
    client: &Client,
    session: &Value,
    volume: &str,
    dir: &str,
) -> anyhow::Result<Vec<Value>> {
    let id = cell(&session["id"]);
    let path = format!(
        "/file-restore/sessions/{}/volumes/{}/entries",
        seg(&id),
        seg(volume)
    );
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut r = client
            .request(Method::GET, &path)
            .query(&[("path", dir), ("limit", "1000")]);
        if let Some(c) = &cursor {
            r = r.query(&[("cursor", c.as_str())]);
        }
        let page = client.send(r).await?;
        out.extend(items(&page["entries"]));
        match page["next"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => return Ok(out),
        }
    }
}

/// `ls`의 이름(디렉터리는 `/`, 링크는 `-> 대상`)
fn short_name(e: &Value) -> String {
    let name = cell(&e["name"]);
    match e["kind"].as_str() {
        Some("dir") => format!("{name}/"),
        Some("symlink") => format!("{name} -> {}", cell(&e["link_target"])),
        _ => name,
    }
}

fn long_row(e: &Value) -> Vec<String> {
    let kind = match e["kind"].as_str() {
        Some("dir") => 'd',
        Some("symlink") => 'l',
        Some("other") => '?',
        _ => '-',
    };
    let mode = e["mode"].as_u64().unwrap_or(0) as u32;
    let mut perms = String::from(kind);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        perms.push(if bits & 4 != 0 { 'r' } else { '-' });
        perms.push(if bits & 2 != 0 { 'w' } else { '-' });
        perms.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    let size = if kind == '-' {
        e["size"].as_u64().unwrap_or(0).to_string()
    } else {
        "-".into()
    };
    vec![
        perms,
        format!("{}:{}", cell(&e["uid"]), cell(&e["gid"])),
        size,
        e["mtime"]
            .as_i64()
            .map_or_else(|| "-".into(), crate::cli::unix_time),
        short_name(e),
    ]
}

fn time_cell(v: &Value) -> String {
    match v.as_i64() {
        Some(0) | None => "-".into(),
        Some(t) => crate::cli::unix_time(t),
    }
}

/// `Content-Disposition`의 ASCII 이름(`filename="…"`)
fn disposition_name(header: &str) -> Option<String> {
    let start = header.find("filename=\"")? + "filename=\"".len();
    let rest = &header[start..];
    let name = &rest[..rest.find('"')?];
    let name = name.rsplit('/').next().unwrap_or(name);
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_string())
}

/// 링크를 받아 `output`(없으면 서버 이름, `-`면 표준 출력)에 쓴다. 끊기면(받은 크기가 다르면) 실패다. VM 내보내기
/// 내려받기(계획 72)도 쓴다.
pub(crate) async fn download(
    client: &Client,
    url: &str,
    output: Option<&str>,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let resp = client
        .request(Method::GET, url)
        .send()
        .await
        .with_context(|| format!("cannot reach {}", client.base))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        bail!(
            "{}: {}",
            body["code"].as_str().unwrap_or(status.as_str()),
            body["message"].as_str().unwrap_or("download failed")
        );
    }
    let length = resp.content_length();
    let name = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(disposition_name)
        .unwrap_or_else(|| "download".into());
    let to_stdout = output == Some("-");
    let path = PathBuf::from(output.unwrap_or(&name));
    let mut file = if to_stdout {
        None
    } else {
        Some(
            std::fs::File::create(&path)
                .with_context(|| format!("cannot create {}", path.display()))?,
        )
    };
    let mut stream = resp.bytes_stream();
    let mut got = 0u64;
    let mut shown = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("the download was cut off")?;
        got += chunk.len() as u64;
        match &mut file {
            Some(f) => f.write_all(&chunk)?,
            None => out.write_all(&chunk)?,
        }
        if shown.elapsed() > Duration::from_secs(1) {
            write!(err, "\r{} received", bytes(&json!(got)))?;
            shown = Instant::now();
        }
    }
    if let Some(len) = length
        && len != got
    {
        bail!("the download was cut off after {got} of {len} bytes");
    }
    if let Some(f) = file {
        f.sync_all()?;
        writeln!(
            err,
            "\r{} written to {}",
            bytes(&json!(got)),
            path.display()
        )?;
    } else {
        out.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;

    #[test]
    fn paths_split_at_the_volume() {
        assert_eq!(
            split_path("disk0p1:/etc/nginx").unwrap(),
            ("disk0p1".into(), "/etc/nginx".into())
        );
        assert_eq!(
            split_path("lvm/vg0/root").unwrap(),
            ("lvm/vg0/root".into(), "/".into())
        );
        assert_eq!(split_path("disk0:").unwrap().1, "/");
        assert!(split_path(":/etc").is_err());
        assert_eq!(
            disposition_name(
                "attachment; filename=\"web-1-etc.zip\"; filename*=UTF-8''web-1-etc.zip"
            ),
            Some("web-1-etc.zip".into())
        );
        assert_eq!(disposition_name("attachment; filename=\"..\""), None);
    }

    #[test]
    fn long_rows_read_like_ls() {
        let e = json!({ "name": "nginx.conf", "kind": "file", "size": 24, "mtime": 0, "mode": 0o644, "uid": 0, "gid": 0 });
        assert_eq!(
            long_row(&e),
            [
                "-rw-r--r--",
                "0:0",
                "24",
                "1970-01-01 00:00 UTC",
                "nginx.conf"
            ]
        );
        let l = json!({ "name": "localtime", "kind": "symlink", "size": 0, "mtime": 0, "mode": 0o777, "uid": 0, "gid": 0, "link_target": "/usr/share/zoneinfo/UTC" });
        assert_eq!(long_row(&l)[0], "lrwxrwxrwx");
        assert_eq!(long_row(&l)[4], "localtime -> /usr/share/zoneinfo/UTC");
    }

    fn session(phase: &str) -> Value {
        json!({ "id": "0190a5b2-0000-7000-8000-000000000001", "backup_name": "web-1", "node": "n", "phase": phase,
            "opened_at": 0, "expires_at": 60, "last_used_at": 0, "backup": "b", "opened_by": "u",
            "volumes": [{ "id": "disk0p1", "disk": 0, "kind": "partition", "size": 1024, "fs": "ext4",
                "mountable": true, "state": "unmounted" }] })
    }

    #[tokio::test]
    async fn ls_opens_the_session_and_reads_every_page() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/backups/web-1/file-restore") => {
                (202, json!({ "session": session("booting"), "task": "t" }))
            }
            ("GET", "/api/v1/file-restore/sessions/0190a5b2-0000-7000-8000-000000000001") => {
                (200, session("ready"))
            }
            ("GET", p) if p.contains("cursor=c1") => (
                200,
                json!({ "entries": [{ "name": "nginx", "kind": "dir", "size": 0, "mtime": 0, "mode": 493, "uid": 0, "gid": 0 }] }),
            ),
            ("GET", p) if p.contains("/entries") => (
                200,
                json!({ "entries": [{ "name": "hostname", "kind": "file", "size": 7, "mtime": 0, "mode": 420, "uid": 0, "gid": 0 }], "next": "c1" }),
            ),
            _ => (404, json!({ "code": "not_found", "message": "no" })),
        })
        .await;
        let cmd = FilesCmd::Ls {
            backup: "web-1".into(),
            path: "disk0p1:/etc".into(),
            long: false,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        files(&client, cmd, false, &mut out, &mut err)
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "hostname\nnginx/\n");
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("helper VM is booting")
        );
        let lines: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.line.clone())
            .collect();
        assert!(
            lines[0].starts_with("POST /api/v1/backups/web-1/file-restore"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("/volumes/disk0p1/entries?path=%2Fetc&limit=1000&cursor=c1")),
            "{lines:?}"
        );
    }

    /// `restore`는 세션을 열거나 다시 쓰고 `restore-to-vm`에 경로·VM·대상·충돌 정책을 보낸 뒤 `--wait`이면 Task 결과를 보인다.
    #[tokio::test]
    async fn restore_sends_the_request_and_waits_for_the_task() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/backups/web-1/file-restore") => {
                (200, json!({ "session": session("ready"), "task": "t" }))
            }
            ("POST", p) if p.ends_with("/restore-to-vm") => (
                202,
                json!({ "id": "t2", "status": { "state": "pending" } }),
            ),
            ("GET", "/api/v1/tasks/t2") => (
                200,
                json!({ "id": "t2", "status": { "state": "succeeded",
                    "error": "restored 2 file(s) (10 bytes) and 0 directories into VM web:/srv" } }),
            ),
            _ => (404, json!({ "code": "not_found", "message": "no" })),
        })
        .await;
        let cmd = FilesCmd::Restore {
            backup: "web-1".into(),
            paths: vec!["disk0p1:/etc/hosts".into(), "disk0p1:/etc/hostname".into()],
            vm: "web".into(),
            to: "/srv".into(),
            conflict: ConflictArg::Overwrite,
            wait: true,
        };
        let mut out = Vec::new();
        files(&client, cmd, false, &mut out, &mut Vec::new())
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "restored 2 file(s) (10 bytes) and 0 directories into VM web:/srv\n"
        );
        let body = {
            let seen = seen.lock().unwrap();
            seen.iter()
                .find(|s| s.line.contains("/restore-to-vm"))
                .unwrap()
                .body
                .clone()
        };
        assert_eq!(
            body,
            json!({ "volume": "disk0p1", "paths": ["/etc/hosts", "/etc/hostname"], "vm": "web",
                "target_dir": "/srv", "conflict": "overwrite" })
        );
        let bad = FilesCmd::Restore {
            backup: "web-1".into(),
            paths: vec!["disk0p1:/a".into(), "disk1:/b".into()],
            vm: "web".into(),
            to: "/srv".into(),
            conflict: ConflictArg::Keep,
            wait: false,
        };
        let err = files(&client, bad, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("same volume"), "{err}");
    }

    #[tokio::test]
    async fn volumes_print_a_table() {
        let (client, _) = fake_api(|method, path| match (method, path) {
            ("POST", _) => (200, json!({ "session": session("ready"), "task": "t" })),
            _ => (404, json!({ "code": "not_found", "message": "no" })),
        })
        .await;
        let mut out = Vec::new();
        files(
            &client,
            FilesCmd::Volumes {
                backup: "web-1".into(),
            },
            false,
            &mut out,
            &mut Vec::new(),
        )
        .await
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("VOLUME   DISK  KIND"), "{text}");
        assert!(
            text.contains("disk0p1  0     partition  ext4  -      1.0KiB  unmounted  -"),
            "{text}"
        );
    }
}
