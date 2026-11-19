//! 게스트 CPU 모델 명령(계획 63): `vm create|set --cpu-model/--cpu-flags`, `vm cpu`, `cpu models|flags|baseline`,
//! `node cpu [--refresh]`.

use std::io::Write;

use anyhow::bail;
use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::cli::unix_time;
use crate::client::{Client, seg};
use crate::output::{cell, json as print_json, table};

/// VM의 CPU 모델·플래그
#[derive(Args, Debug, Default, Clone)]
pub(crate) struct CpuArgs {
    /// CPU 모델: `host`(기본), `max`, `cluster`(클러스터 기준), `x86-64-v2`…`v4`, QEMU 모델(`Icelake-Server-v7`; 별칭은
    /// 버전 모델로 고정해 저장한다). `orbivisorctl cpu models`가 목록이다. 실행 중이면 다음 전원 껐다 켜기 때 적용한다.
    #[arg(long, value_name = "MODEL")]
    pub cpu_model: Option<String>,
    /// CPU 플래그 전체(`-avx512vbmi,+aes`: `-`는 게스트에게 숨기고 `+`는 켠다). 이 목록으로 바꾼다. 실행 중이면 다음
    /// 전원 껐다 켜기 때 적용한다(게스트 안 재부팅으로는 안 된다).
    #[arg(long, value_name = "±FLAG,...", allow_hyphen_values = true, conflicts_with = "no_cpu_flags", value_parser = flag_list)]
    pub cpu_flags: Option<FlagList>,
    /// CPU 플래그를 모두 지운다.
    #[arg(long)]
    pub no_cpu_flags: bool,
}

/// `--cpu-flags`의 목록(`["-avx512vbmi", "+aes"]`)
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct FlagList(pub Vec<String>);

/// `-avx512vbmi,+aes`를 목록으로. 이름마다 `+`나 `-`로 시작해야 한다. 빈 값은 빈 목록(지움)이다.
pub(crate) fn parse_flags(list: &str) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !(item.starts_with('+') || item.starts_with('-')) || item.len() < 2 {
            bail!("CPU flag {item:?} must be +NAME or -NAME (e.g. -avx512vbmi)");
        }
        out.push(item.to_string());
    }
    Ok(out)
}

fn flag_list(s: &str) -> Result<FlagList, String> {
    parse_flags(s).map(FlagList).map_err(|e| e.to_string())
}

impl CpuArgs {
    /// 무엇이든 바꾸는지
    pub(crate) fn given(&self) -> bool {
        self.cpu_model.is_some() || self.cpu_flags.is_some() || self.no_cpu_flags
    }

    /// VM 스펙(`spec`)의 `cpu`에 적용한다.
    pub(crate) fn apply(&self, spec: &mut Value) {
        if let Some(model) = &self.cpu_model {
            spec["cpu"]["model"] = json!(model);
        }
        let flags = match (&self.cpu_flags, self.no_cpu_flags) {
            (Some(list), _) => Some(list.0.clone()),
            (None, true) => Some(Vec::new()),
            (None, false) => None,
        };
        match flags {
            Some(list) if !list.is_empty() => spec["cpu"]["flags"] = json!(list),
            Some(_) => {
                if let Some(cpu) = spec["cpu"].as_object_mut() {
                    cpu.remove("flags");
                }
            }
            None => {}
        }
    }

    /// 복제·가져오기·템플릿 요청의 `cpu_model`·`cpu_flags`
    pub(crate) fn request_fields(&self, body: &mut Value) {
        if let Some(model) = &self.cpu_model {
            body["cpu_model"] = json!(model);
        }
        if let Some(list) = &self.cpu_flags {
            body["cpu_flags"] = json!(list.0);
        } else if self.no_cpu_flags {
            body["cpu_flags"] = json!([]);
        }
    }
}

/// 해석된 CPU(`ResolvedCpu` JSON)를 `Icelake-Server-v7 -avx512vbmi +pcid`로
pub(crate) fn describe(resolved: &Value) -> String {
    let mut s = resolved["model"].as_str().unwrap_or("?").to_string();
    if let Some(flags) = resolved["flags"].as_object() {
        for (name, on) in flags {
            s.push_str(&format!(" {}{name}", if on == true { '+' } else { '-' }));
        }
    }
    s
}

/// 스펙의 CPU(`CpuSpec` JSON)를 `1x4 Icelake-Server-v7 -avx512vbmi`로
fn spec_name(cpu: &Value) -> String {
    let mut s = format!(
        "{}x{} {}",
        cpu["sockets"].as_u64().unwrap_or(1),
        cpu["cores"].as_u64().unwrap_or(1),
        cpu["model"].as_str().unwrap_or("host")
    );
    for f in cpu["flags"].as_array().into_iter().flatten() {
        s.push(' ');
        s.push_str(f.as_str().unwrap_or("?"));
    }
    s
}

/// `vm get`의 CPU 줄: `cpu: 1x4 Icelake-Server-v7 -avx512vbmi`, 도는 VM이면 `cpu running: … (power cycle pending: …)`
/// 모두 `host`(플래그·보류 없음)이면 줄이 없다(계획 63 전과 같은 출력).
pub(crate) fn cpu_lines(vm: &Value) -> Vec<String> {
    let hw = &vm["status"]["hardware"];
    let plain = |cpu: &Value| {
        cpu["model"].as_str().is_none_or(|m| m == "host")
            && cpu["flags"].as_array().is_none_or(Vec::is_empty)
    };
    let cpu_pending = hw["pending"]
        .as_array()
        .is_some_and(|p| p.iter().any(|p| p["field"] == "cpu"));
    if plain(&vm["spec"]["cpu"])
        && (hw.is_null() || plain(&hw["cpu_resolved"]) && plain(&hw["cpu"]))
        && !cpu_pending
    {
        return Vec::new();
    }
    let mut out = vec![format!("cpu: {}", spec_name(&vm["spec"]["cpu"]))];
    if hw.is_null() {
        return out;
    }
    let running = if hw["cpu_resolved"].is_object() {
        describe(&hw["cpu_resolved"])
    } else {
        hw["cpu"]["model"].as_str().unwrap_or("host").to_string()
    };
    let pending = hw["pending"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["field"] == "cpu");
    let mut line = format!("cpu running: {running}");
    if let Some(p) = pending {
        let detail = p["detail"].as_str().unwrap_or("");
        let what = detail.split("; ").next().unwrap_or(detail);
        line.push_str(&format!(" (power cycle pending: {what})"));
    }
    out.push(line);
    out
}

/// `vm cpu VM`: 스펙·지금 해석·도는 것·보류와 노드마다 켤 수 있는지
pub(crate) async fn vm_cpu(
    client: &Client,
    vm: &str,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let info = client.get(&format!("/vms/{}/cpu", seg(vm))).await?;
    if json_out {
        return Ok(print_json(out, &info)?);
    }
    writeln!(out, "spec: {}", spec_name(&info["spec"]))?;
    if info["resolved"].is_object() {
        writeln!(out, "resolved: {}", describe(&info["resolved"]))?;
    }
    if info["running"].is_object() {
        writeln!(out, "running: {}", describe(&info["running"]))?;
    }
    if let Some(detail) = info["pending"]["detail"].as_str() {
        writeln!(out, "pending: {detail}")?;
    }
    let rows: Vec<Vec<String>> = info["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| {
            let mut note: Vec<String> =
                n["reason"].as_str().map(String::from).into_iter().collect();
            note.extend(
                n["warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|w| format!("warning: {}", w.as_str().unwrap_or(""))),
            );
            vec![
                cell(&n["node"]),
                if n["can_run"] == true { "yes" } else { "no" }.into(),
                names(&n["missing"]),
                note.join("; "),
            ]
        })
        .collect();
    if !rows.is_empty() {
        table(out, &["NODE", "CAN-RUN", "MISSING", "NOTE"], &rows)?;
    }
    Ok(())
}

/// `vm migrate`가 먼저 찍는 경고(`host` VM을 CPU가 다른 노드로 옮김). 읽지 못하면 아무것도 찍지 않는다.
pub(crate) async fn migrate_warnings(
    client: &Client,
    vm: &str,
    node: &str,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let Ok(info) = client.get(&format!("/vms/{}/cpu", seg(vm))).await else {
        return Ok(());
    };
    for n in info["nodes"].as_array().into_iter().flatten() {
        if n["node"] != node {
            continue;
        }
        for w in n["warnings"].as_array().into_iter().flatten() {
            writeln!(err, "warning: {}", w.as_str().unwrap_or(""))?;
        }
    }
    Ok(())
}

/// 이름 목록(JSON 배열)을 `a, b`로. 비면 `-`.
fn names(list: &Value) -> String {
    let v: Vec<&str> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if v.is_empty() {
        "-".into()
    } else {
        v.join(", ")
    }
}

/// CPU 모델·플래그·클러스터 기준(계획 63)
#[derive(Subcommand, Debug)]
pub(crate) enum CpuCmd {
    /// 아키텍처의 CPU 모델과 켤 수 있는 노드 수(`host`·`max`·`cluster`·`x86-64-v*`·노드 QEMU의 모델)
    Models {
        /// `x86_64`·`aarch64`(기본: 노드들의 아키텍처)
        #[arg(long)]
        arch: Option<String>,
        /// 이 노드에서 켤 수 있는지만(빠진 플래그도 보인다)
        #[arg(long)]
        node: Option<String>,
        /// 켤 수 있는 노드가 있는 모델만
        #[arg(long)]
        usable: bool,
    },
    /// 아키텍처의 CPU 플래그(노드 QEMU가 아는 이름)와 그것을 줄 수 있는 노드 수
    Flags {
        #[arg(long)]
        arch: Option<String>,
    },
    /// 클러스터 CPU 기준(설정 `cpu.*`와 아키텍처마다 리더가 기록한 기준). 바꾸기는 `settings set cpu.baseline=…`
    Baseline,
}

pub(crate) async fn cpu(
    client: &Client,
    cmd: CpuCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        CpuCmd::Models { arch, node, usable } => {
            let mut query = Vec::new();
            if let Some(a) = &arch {
                query.push(format!("arch={}", seg(a)));
            }
            if let Some(n) = &node {
                query.push(format!("node={}", seg(n)));
            }
            if usable {
                query.push("usable=true".into());
            }
            let path = if query.is_empty() {
                "/cpu/models".to_string()
            } else {
                format!("/cpu/models?{}", query.join("&"))
            };
            let list = client.get(&path).await?;
            if json_out {
                return Ok(print_json(out, &list)?);
            }
            let total = list["nodes"].as_u64().unwrap_or(0);
            let rows: Vec<Vec<String>> = list["models"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| {
                    let mut note = Vec::new();
                    if let Some(a) = m["alias_of"].as_str() {
                        note.push(format!("alias of {a}"));
                    }
                    if let Some(c) = m["current"].as_str() {
                        note.push(format!("now {c}"));
                    } else if m["kind"] == "cluster" {
                        note.push("no baseline: runs as host".into());
                    }
                    if m["deprecated"] == true {
                        note.push("deprecated".into());
                    }
                    // 노드 하나만 보면 빠진 것·까닭
                    if node.is_some()
                        && let Some(n) = m["nodes"].as_array().and_then(|n| n.first())
                        && n["can_run"] != true
                    {
                        match n["missing"].as_array() {
                            Some(list) if !list.is_empty() => {
                                note.push(format!("missing {}", names(&n["missing"])))
                            }
                            _ => note.extend(n["reason"].as_str().map(String::from)),
                        }
                    }
                    vec![
                        cell(&m["name"]),
                        cell(&m["kind"]),
                        format!("{}/{total}", m["usable"].as_u64().unwrap_or(0)),
                        note.join("; "),
                    ]
                })
                .collect();
            table(out, &["MODEL", "KIND", "NODES", "NOTE"], &rows)?;
        }
        CpuCmd::Flags { arch } => {
            let path = match &arch {
                Some(a) => format!("/cpu/flags?arch={}", seg(a)),
                None => "/cpu/flags".into(),
            };
            let list = client.get(&path).await?;
            if json_out {
                return Ok(print_json(out, &list)?);
            }
            let total = list["nodes"].as_u64().unwrap_or(0);
            let rows: Vec<Vec<String>> = list["flags"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| {
                    vec![
                        cell(&f["name"]),
                        format!("{}/{total}", f["supported"].as_u64().unwrap_or(0)),
                        if f["masked"] == true { "masked" } else { "" }.into(),
                    ]
                })
                .collect();
            table(out, &["FLAG", "SUPPORTED", "NOTE"], &rows)?;
        }
        CpuCmd::Baseline => {
            let b = client.get("/cpu/baseline").await?;
            if json_out {
                return Ok(print_json(out, &b)?);
            }
            writeln!(out, "mode: {}", cell(&b["mode"]))?;
            writeln!(
                out,
                "auto flags: {}",
                if b["auto_flags"] == false {
                    "off"
                } else {
                    "on"
                }
            )?;
            writeln!(out, "mask: {}", names(&b["mask"]))?;
            let arches = b["arches"].as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = arches
                .iter()
                .map(|a| {
                    let mode = if a["pinned"] == true {
                        "pinned".to_string()
                    } else {
                        cell(&a["baseline"]["mode"])
                    };
                    vec![
                        cell(&a["arch"]),
                        a["cpu"]
                            .as_str()
                            .unwrap_or("host (no baseline)")
                            .to_string(),
                        mode,
                        names(&a["limited_by"]),
                        cell(&a["cluster_vms"]),
                    ]
                })
                .collect();
            table(
                out,
                &["ARCH", "CPU", "MODE", "LIMITED-BY", "CLUSTER-VMS"],
                &rows,
            )?;
            for a in &arches {
                for w in a["baseline"]["warnings"].as_array().into_iter().flatten() {
                    writeln!(
                        out,
                        "warning ({}): {}",
                        cell(&a["arch"]),
                        w.as_str().unwrap_or("")
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// `node cpu NODE [--refresh]`: 노드 CPU 탐침 결과와 카탈로그의 모델. `--refresh`면 먼저 노드 QEMU에 다시 묻는다(Task가
/// 끝날 때까지 기다린다).
pub(crate) async fn node_cpu(
    client: &Client,
    node: &str,
    refresh: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    if refresh {
        let task = client
            .post(&format!("/nodes/{}/cpu/refresh", seg(node)), &json!({}))
            .await?;
        client.wait_task(&task).await?;
    }
    let info = client.get(&format!("/nodes/{}/cpu", seg(node))).await?;
    if json_out {
        return Ok(print_json(out, &info)?);
    }
    let cpu = &info["cpu"];
    if !cpu.is_object() {
        writeln!(
            out,
            "node {node} has not reported its CPU models; only host and max run there"
        )?;
        return Ok(());
    }
    writeln!(out, "qemu: {}", cell(&cpu["qemu"]))?;
    writeln!(out, "accel: {}", cell(&cpu["accel"]))?;
    if cpu["vendor"].is_string() || cpu["model_id"].is_string() {
        let host: Vec<&str> = [&cpu["vendor"], &cpu["model_id"]]
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        writeln!(out, "host cpu: {}", host.join(" "))?;
    }
    let probed = cpu["probed_at"]
        .as_i64()
        .map_or_else(|| "never".to_string(), unix_time);
    writeln!(out, "probed: {probed}")?;
    if let Some(hash) = cpu["catalog"].as_str().filter(|h| !h.is_empty()) {
        writeln!(out, "catalog: {}", &hash[..hash.len().min(12)])?;
    }
    if let Some(error) = cpu["error"].as_str() {
        writeln!(out, "error: {error}")?;
    }
    if let Some(problem) = info["baseline_problem"].as_str() {
        writeln!(out, "cluster baseline: {problem}")?;
    }
    let rows: Vec<Vec<String>> = info["models"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|m| {
            let mut note = Vec::new();
            if let Some(a) = m["alias_of"].as_str() {
                note.push(format!("alias of {a}"));
            }
            let other = names(&m["missing_other"]);
            if other != "-" {
                note.push(format!("not available: {other}"));
            }
            if m["deprecated"] == true {
                note.push("deprecated".into());
            }
            vec![
                cell(&m["name"]),
                if m["usable"] == true { "yes" } else { "no" }.into(),
                names(&m["missing"]),
                note.join("; "),
            ]
        })
        .collect();
    if !rows.is_empty() {
        table(out, &["MODEL", "USABLE", "MISSING", "NOTE"], &rows)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_api::fake_api;

    #[test]
    fn flags_are_signed_names() {
        assert_eq!(
            parse_flags("-avx512vbmi, +aes,").unwrap(),
            ["-avx512vbmi", "+aes"]
        );
        assert!(parse_flags("avx").is_err());
        assert!(parse_flags("-").is_err());
        assert_eq!(parse_flags("").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn args_change_the_spec() {
        let mut spec = json!({ "cpu": { "sockets": 1, "cores": 2, "flags": ["-aes"] } });
        let args = CpuArgs {
            cpu_model: Some("Icelake-Server-v7".into()),
            cpu_flags: Some(FlagList(vec!["-avx512vbmi".into()])),
            no_cpu_flags: false,
        };
        args.apply(&mut spec);
        assert_eq!(
            spec["cpu"],
            json!({ "sockets": 1, "cores": 2, "model": "Icelake-Server-v7", "flags": ["-avx512vbmi"] })
        );
        let clear = CpuArgs {
            no_cpu_flags: true,
            ..CpuArgs::default()
        };
        clear.apply(&mut spec);
        assert!(spec["cpu"].get("flags").is_none());
        let mut body = json!({});
        args.request_fields(&mut body);
        assert_eq!(
            body,
            json!({ "cpu_model": "Icelake-Server-v7", "cpu_flags": ["-avx512vbmi"] })
        );
    }

    #[test]
    fn vm_get_shows_the_cpu_and_a_pending_power_cycle() {
        let vm = json!({
            "spec": { "cpu": { "sockets": 1, "cores": 4, "model": "Icelake-Server-v7", "flags": ["-avx512vbmi"] } },
            "status": { "hardware": {
                "cpu": { "sockets": 1, "cores": 4, "model": "Icelake-Server-v7" },
                "cpu_resolved": { "model": "Icelake-Server-v7", "enforce": true, "source": "spec" },
                "pending": [ {
                    "field": "cpu", "running": "1x4 Icelake-Server-v7", "wanted": "1x4 Icelake-Server-v7 -avx512vbmi",
                    "reason": "restart",
                    "detail": "-avx512vbmi; power cycle required; a reboot inside the guest does not apply it"
                } ]
            } }
        });
        assert_eq!(
            cpu_lines(&vm),
            [
                "cpu: 1x4 Icelake-Server-v7 -avx512vbmi",
                "cpu running: Icelake-Server-v7 (power cycle pending: -avx512vbmi)"
            ]
        );
        let stopped = json!({ "spec": { "cpu": { "cores": 2 } }, "status": {} });
        assert!(cpu_lines(&stopped).is_empty());
        let max = json!({ "spec": { "cpu": { "cores": 2, "model": "max" } }, "status": {} });
        assert_eq!(cpu_lines(&max), ["cpu: 1x2 max"]);
    }

    #[tokio::test]
    async fn commands_print_tables() {
        let (client, seen) = fake_api(|method, path| match (method, path) {
            ("GET", "/api/v1/cpu/models?arch=x86_64&node=n4") => (
                200,
                json!({ "arch": "x86_64", "nodes": 1, "models": [
                    { "name": "host", "kind": "basic", "usable": 1,
                      "nodes": [ { "node": "n4", "can_run": true } ] },
                    { "name": "cluster", "kind": "cluster", "current": "Skylake-Server-v5", "usable": 1,
                      "nodes": [ { "node": "n4", "can_run": true } ] },
                    { "name": "Icelake-Server-v7", "kind": "qemu", "usable": 0,
                      "nodes": [ { "node": "n4", "can_run": false, "missing": ["avx512vbmi"],
                                   "reason": "node \"n4\" cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)" } ] }
                ] }),
            ),
            ("GET", "/api/v1/cpu/models?arch=aarch64") => (
                200,
                json!({ "arch": "aarch64", "nodes": 1, "models": [
                    { "name": "cluster", "kind": "cluster", "usable": 1 }
                ] }),
            ),
            ("GET", "/api/v1/cpu/flags") => (
                200,
                json!({ "arch": "x86_64", "nodes": 2, "flags": [
                    { "name": "aes", "supported": 2, "known": 2 },
                    { "name": "avx512vbmi", "supported": 1, "known": 2, "masked": true }
                ] }),
            ),
            ("GET", "/api/v1/cpu/baseline") => (
                200,
                json!({ "mode": "auto", "auto_flags": true, "mask": ["avx512vbmi"], "arches": [
                    { "arch": "x86_64", "pinned": false, "cpu": "Skylake-Server-v5 -avx512vbmi",
                      "baseline": { "model": "Skylake-Server-v5", "mode": "auto", "since": 1,
                                    "warnings": ["node \"n5\" has no CPU catalog yet and is left out"] },
                      "limited_by": ["n4"], "cluster_vms": 3 }
                ] }),
            ),
            ("POST", "/api/v1/nodes/n4/cpu/refresh") => (202, json!({ "id": "t1" })),
            ("GET", "/api/v1/tasks/t1") => (
                200,
                json!({ "id": "t1", "status": { "state": "succeeded" } }),
            ),
            ("GET", "/api/v1/nodes/n4/cpu") => (
                200,
                json!({ "node": "n4", "cpu": {
                    "catalog": "0123456789abcdef", "qemu": "9.2.0", "accel": "kvm",
                    "vendor": "GenuineIntel", "model_id": "Xeon", "probed_at": 1_793_000_000
                }, "models": [
                    { "name": "Icelake-Server", "alias_of": "Icelake-Server-v7", "usable": false,
                      "missing": ["avx512vbmi"], "migration_safe": true },
                    { "name": "Skylake-Server-v5", "usable": true, "migration_safe": true }
                ], "flags": [] }),
            ),
            ("GET", "/api/v1/vms/web/cpu") => (
                200,
                json!({ "spec": { "sockets": 1, "cores": 2, "model": "host" },
                        "running": { "model": "host", "source": "spec" },
                        "nodes": [
                    { "node": "n1", "can_run": true },
                    { "node": "n4", "can_run": true,
                      "warnings": ["target node \"n4\" lacks avx512vbmi that the guest may be using"] }
                ] }),
            ),
            _ => (404, json!({ "code": "not_found", "message": "no" })),
        })
        .await;
        let run = |cmd: CpuCmd| {
            let client = client.clone();
            async move {
                let mut out = Vec::new();
                cpu(&client, cmd, false, &mut out).await.unwrap();
                String::from_utf8(out).unwrap()
            }
        };
        let text = run(CpuCmd::Models {
            arch: Some("x86_64".into()),
            node: Some("n4".into()),
            usable: false,
        })
        .await;
        assert_eq!(
            text,
            "MODEL              KIND     NODES  NOTE\n\
             host               basic    1/1\n\
             cluster            cluster  1/1    now Skylake-Server-v5\n\
             Icelake-Server-v7  qemu     0/1    missing avx512vbmi\n"
        );
        // 기준이 없으면 `current`가 없다(리뷰 1 웹 #1).
        let text = run(CpuCmd::Models {
            arch: Some("aarch64".into()),
            node: None,
            usable: false,
        })
        .await;
        assert_eq!(
            text,
            "MODEL    KIND     NODES  NOTE\n\
             cluster  cluster  1/1    no baseline: runs as host\n"
        );
        let text = run(CpuCmd::Flags { arch: None }).await;
        assert_eq!(
            text,
            "FLAG        SUPPORTED  NOTE\naes         2/2\navx512vbmi  1/2        masked\n"
        );
        let text = run(CpuCmd::Baseline).await;
        assert_eq!(
            text,
            "mode: auto\nauto flags: on\nmask: avx512vbmi\n\
             ARCH    CPU                            MODE  LIMITED-BY  CLUSTER-VMS\n\
             x86_64  Skylake-Server-v5 -avx512vbmi  auto  n4          3\n\
             warning (x86_64): node \"n5\" has no CPU catalog yet and is left out\n"
        );

        let mut out = Vec::new();
        node_cpu(&client, "n4", true, false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "qemu: 9.2.0\naccel: kvm\nhost cpu: GenuineIntel Xeon\nprobed: 2026-10-26 07:33 UTC\n\
             catalog: 0123456789ab\n\
             MODEL              USABLE  MISSING     NOTE\n\
             Icelake-Server     no      avx512vbmi  alias of Icelake-Server-v7\n\
             Skylake-Server-v5  yes     -\n"
        );
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|s| s.line.starts_with("POST /api/v1/nodes/n4/cpu/refresh"))
        );

        let mut out = Vec::new();
        vm_cpu(&client, "web", false, &mut out).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "spec: 1x2 host\nrunning: host\n\
             NODE  CAN-RUN  MISSING  NOTE\n\
             n1    yes      -\n\
             n4    yes      -        warning: target node \"n4\" lacks avx512vbmi that the guest may be using\n"
        );
        let mut err = Vec::new();
        migrate_warnings(&client, "web", "n4", &mut err)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "warning: target node \"n4\" lacks avx512vbmi that the guest may be using\n"
        );
    }
}
