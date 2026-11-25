//! `orbivisorctl vm export …`·`export …`(계획 72 §12.3): 꺼진 VM을 파일 풀에 OVF 패키지로 내보내고, OVA로 내려받고, 지운다.

use std::io::Write;

use anyhow::Context;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::client::{BACKUP_WAIT, Client, seg};
use crate::output::{bytes, cell, json as print_json, table};

/// `vm export`의 인자
#[derive(Args)]
pub(crate) struct VmExportArgs {
    /// 내보낼 VM(꺼져 있어야 한다)
    vm: String,
    /// 파일을 둘 풀(dir·NFS)
    #[arg(long)]
    pool: String,
    /// 내보내기 이름(없으면 `{VM}-{시각}`)
    #[arg(long)]
    name: Option<String>,
    /// OVF `vmw:osType`(VMware guestId, 예: `ubuntu64Guest`)
    #[arg(long, value_name = "ID")]
    guest_os: Option<String>,
    /// VMware 하드웨어 버전(`vmx-N`, 7~21, 기본 14)
    #[arg(long, value_name = "N")]
    hw_version: Option<u8>,
    /// NIC의 MAC 주소를 그대로 적는다.
    #[arg(long)]
    keep_macs: bool,
    /// 다 쓰면 OVA로 내려받는다(`--wait`를 품는다, `-`면 표준 출력).
    #[arg(long, value_name = "FILE.ova")]
    download: Option<String>,
    /// 내보내기 Task가 끝날 때까지 기다린다.
    #[arg(long)]
    wait: bool,
}

#[derive(Subcommand)]
pub(crate) enum ExportCmd {
    /// 모든 VM 내보내기
    List,
    Get {
        export: String,
    },
    /// OVA로 내려받는다(`--file`이면 OVF 폴더의 파일 하나). 진행은 표준 오류에.
    Download {
        export: String,
        /// 쓸 파일(`-`면 표준 출력). 없으면 `{VM}.ova`(`-o`는 전역 출력 형식이라 wget처럼 `-O`)
        #[arg(short = 'O', long, value_name = "FILE")]
        out_file: Option<String>,
        /// OVF 폴더의 파일 하나만(`web.ovf`·`web.mf`·`web-disk0.vmdk`)
        #[arg(long, value_name = "NAME")]
        file: Option<String>,
    },
    /// 내보낸 파일과 기록을 지운다.
    Delete {
        export: String,
    },
}

/// `export list`의 열
const HEADERS: &[&str] = &["NAME", "VM", "POOL", "PHASE", "OVA", "FILES", "ERROR"];

fn row(e: &Value, vms: &[Value]) -> Vec<String> {
    let vm = cell(&e["spec"]["vm"]);
    let vm = vms
        .iter()
        .find(|v| cell(&v["id"]) == vm)
        .map_or(vm, |v| cell(&v["name"]));
    let st = &e["status"];
    vec![
        cell(&e["name"]),
        vm,
        cell(&e["spec"]["pool"]),
        cell(&st["phase"]),
        bytes(&st["ova_bytes"]),
        st["files"].as_array().map_or(0, |f| f.len()).to_string(),
        st["error"].as_str().unwrap_or("").to_string(),
    ]
}

/// 내려받기 링크를 받아 `out_file`에 쓴다.
async fn download(
    client: &Client,
    export: &str,
    out_file: Option<&str>,
    file: Option<&str>,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let link = client
        .post(
            &format!("/vm-exports/{}/download-link", seg(export)),
            &json!({}),
        )
        .await?;
    let mut url = link["url"].as_str().context("no download url")?.to_string();
    if let Some(file) = file {
        url.push_str(&format!("?format=ovf&file={}", seg(file)));
    }
    crate::file_restore::download(client, &url, out_file, out, err).await
}

/// `vm export`: 내보내기를 시작하고(그리고 기다리고, 내려받고) 이름을 알린다.
pub(crate) async fn vm_export(
    client: &Client,
    args: VmExportArgs,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let mut body = json!({ "pool": args.pool });
    // 청하지 않은 것은 보내지 않는다(서버 기본값).
    if let Some(name) = &args.name {
        body["name"] = json!(name);
    }
    if let Some(g) = &args.guest_os {
        body["guest_os"] = json!(g);
    }
    if let Some(v) = args.hw_version {
        body["hw_version"] = json!(v);
    }
    if args.keep_macs {
        body["keep_macs"] = json!(true);
    }
    let started = client
        .post(&format!("/vms/{}/export", seg(&args.vm)), &body)
        .await?;
    let name = cell(&started["export"]["name"]);
    if !args.wait && args.download.is_none() {
        writeln!(
            out,
            "vm-export {name} started (task {})",
            cell(&started["task"]["id"])
        )?;
        return Ok(());
    }
    client.wait_task_for(&started["task"], BACKUP_WAIT).await?;
    let export = client.get(&format!("/vm-exports/{}", seg(&name))).await?;
    for w in export["status"]["warnings"]
        .as_array()
        .into_iter()
        .flatten()
    {
        writeln!(err, "warning: {}", w.as_str().unwrap_or_default())?;
    }
    writeln!(
        out,
        "vm-export {name} ready ({})",
        bytes(&export["status"]["ova_bytes"])
    )?;
    if let Some(file) = &args.download {
        download(client, &name, Some(file), None, out, err).await?;
    }
    Ok(())
}

pub(crate) async fn export(
    client: &Client,
    cmd: ExportCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        ExportCmd::List => {
            let list = client.get("/vm-exports").await?;
            let items = list["items"].as_array().cloned().unwrap_or_default();
            if json_out {
                return Ok(print_json(out, &Value::Array(items))?);
            }
            let vms = client.get("/vms").await?;
            let vms = vms["items"].as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = items.iter().map(|e| row(e, &vms)).collect();
            table(out, HEADERS, &rows)?;
        }
        ExportCmd::Get { export } => {
            let e = client.get(&format!("/vm-exports/{}", seg(&export))).await?;
            print_json(out, &e)?;
        }
        ExportCmd::Download {
            export,
            out_file,
            file,
        } => {
            download(
                client,
                &export,
                out_file.as_deref(),
                file.as_deref(),
                out,
                err,
            )
            .await?
        }
        ExportCmd::Delete { export } => {
            let task = client
                .delete(&format!("/vm-exports/{}", seg(&export)))
                .await?;
            client.wait_task(&task).await?;
            writeln!(out, "vm-export {export} deleted")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;

    #[test]
    fn rows_name_the_vm() {
        let e = json!({
            "name": "web-out", "spec": { "vm": "v1", "pool": "nfs1" },
            "status": { "phase": "ready", "ova_bytes": 3u64 << 30, "files": [{}, {}, {}] },
        });
        let vms = vec![json!({ "id": "v1", "name": "web" })];
        assert_eq!(
            row(&e, &vms),
            ["web-out", "web", "nfs1", "ready", "3.0GiB", "3", ""]
        );
        let pending = json!({ "name": "x", "spec": { "vm": "gone", "pool": "p" }, "status": { "phase": "pending" } });
        assert_eq!(row(&pending, &[])[1], "gone");
        assert_eq!(row(&pending, &[])[4], "-");
    }

    /// `vm export`는 준 것만 보내고, 기다리지 않으면 이름과 Task를 알린다.
    #[tokio::test]
    async fn vm_export_sends_only_what_was_given() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("POST", "/api/v1/vms/web/export") => (
                202,
                json!({ "export": { "name": "web-out" }, "task": { "id": "t1" } }),
            ),
            _ => (404, json!({ "code": "not_found", "message": path })),
        })
        .await;
        let args = VmExportArgs {
            vm: "web".into(),
            pool: "nfs1".into(),
            name: None,
            guest_os: Some("ubuntu64Guest".into()),
            hw_version: None,
            keep_macs: true,
            download: None,
            wait: false,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        vm_export(&client, args, &mut out, &mut err).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "vm-export web-out started (task t1)\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0].body,
            json!({ "pool": "nfs1", "guest_os": "ubuntu64Guest", "keep_macs": true })
        );
    }
}
