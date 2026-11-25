//! NUMA·전용 CPU·hugepages 명령(계획 64): `vm create|set --cpu-policy/--cpu-threads/--emulator-threads/--numa/
//! --numa-memory/--hugepages`(복제·배포·가져오기·템플릿도 같은 이름), `vm numa`, `node topology`, `node resources`.

use std::io::Write;

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};
use reqwest::Method;
use serde_json::{Value, json};

use crate::client::{Client, seg};
use crate::output::{cell, json as print_json, table};

/// CPU 정책
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CpuPolicyArg {
    Shared,
    Dedicated,
}

/// 전용 VM의 SMT 형제 스레드 정책
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ThreadsArg {
    Prefer,
    Isolate,
    Require,
}

/// 전용 VM의 에뮬레이터 스레드 정책
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum EmulatorArg {
    Shared,
    Isolate,
}

/// 게스트 노드 메모리를 호스트 노드에 묶는 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum NumaMemoryArg {
    Strict,
    Preferred,
}

/// `--numa`: `none`, `auto`, 노드 수, 또는 `cpus:memory_mib` 목록
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NumaArg {
    None,
    Nodes(Value),
}

fn parse_numa(s: &str) -> Result<NumaArg, String> {
    let s = s.trim();
    match s {
        "none" => return Ok(NumaArg::None),
        "auto" => return Ok(NumaArg::Nodes(json!("auto"))),
        _ => {}
    }
    if let Ok(n) = s.parse::<u16>() {
        return Ok(NumaArg::Nodes(json!(n)));
    }
    let bad = || format!("invalid --numa {s:?}: use none, auto, a node count or CPUS:MIB,CPUS:MIB");
    let list = s
        .split(',')
        .map(|item| {
            let (cpus, mib) = item.trim().split_once(':').ok_or_else(bad)?;
            let cpus: u16 = cpus.parse().map_err(|_| bad())?;
            let mib: u32 = mib.parse().map_err(|_| bad())?;
            Ok(json!({ "cpus": cpus, "memory_mib": mib }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(NumaArg::Nodes(Value::Array(list)))
}

/// `--hugepages`의 값: 크기(`2M`·`1G`·`any`), `none`이면 None
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PagesArg(Option<String>);

/// `--hugepages`: `none`, `2M`, `1G`, `any`
fn parse_hugepages(s: &str) -> Result<PagesArg, String> {
    match s.trim() {
        "none" => Ok(PagesArg(None)),
        "2M" | "2m" => Ok(PagesArg(Some("2M".into()))),
        "1G" | "1g" => Ok(PagesArg(Some("1G".into()))),
        "any" => Ok(PagesArg(Some("any".into()))),
        other => Err(format!(
            "invalid --hugepages {other:?}: use none, 2M, 1G or any"
        )),
    }
}

/// VM의 CPU 배치(계획 64). 실행 중이면 다음 전원 껐다 켜기 때 적용한다.
#[derive(Args, Debug, Default, Clone)]
pub(crate) struct NumaArgs {
    /// CPU 정책: `shared`(기본, 노드의 공유 CPU) 또는 `dedicated`(vCPU마다 노드의 전용 CPU 하나 — 권한 `vm.dedicated`,
    /// 핫플러그 상한까지 잡는다). `shared`는 스레드·에뮬레이터 정책도 지운다.
    #[arg(long, value_enum, value_name = "POLICY")]
    pub cpu_policy: Option<CpuPolicyArg>,
    /// 전용 VM의 SMT 형제 정책: `prefer`(기본), `isolate`(코어 하나씩, 형제는 비움), `require`(형제 짝)
    #[arg(long, value_enum, value_name = "POLICY")]
    pub cpu_threads: Option<ThreadsArg>,
    /// 전용 VM의 QEMU 에뮬레이터 스레드: `shared`(기본) 또는 `isolate`(전용 CPU 하나를 더 잡음)
    #[arg(long, value_enum, value_name = "POLICY")]
    pub emulator_threads: Option<EmulatorArg>,
    /// 게스트 NUMA 노드: `none`, `auto`(소켓 수의 약수 가운데 노드에 맞는 것), 노드 수(소켓 수를 나눠야 한다), 또는
    /// 노드마다 `vCPU자리:MiB`(`8:16384,8:16384`)
    #[arg(long, value_name = "none|auto|N|CPUS:MIB,...", value_parser = parse_numa)]
    pub numa: Option<NumaArg>,
    /// 게스트 노드 메모리를 호스트 NUMA 노드에 묶는다: `strict` 또는 `preferred`(전용·hugepages VM은 주지 않아도
    /// `strict`)
    #[arg(long, value_enum, value_name = "POLICY")]
    pub numa_memory: Option<NumaMemoryArg>,
    /// hugepages: `none`, `2M`, `1G`(메모리 핫플러그와 함께 못 씀), `any`(노드가 가진 가장 큰 것) — 권한
    /// `vm.dedicated`
    #[arg(long, value_name = "none|2M|1G|any", value_parser = parse_hugepages)]
    pub hugepages: Option<PagesArg>,
}

fn name<T: ValueEnum>(v: T) -> String {
    v.to_possible_value()
        .map(|p| p.get_name().to_string())
        .unwrap_or_default()
}

impl NumaArgs {
    /// 무엇이든 바꾸는지
    pub(crate) fn given(&self) -> bool {
        self.cpu_policy.is_some()
            || self.cpu_threads.is_some()
            || self.emulator_threads.is_some()
            || self.numa.is_some()
            || self.numa_memory.is_some()
            || self.hugepages.is_some()
    }

    /// VM 스펙(`spec`)에 적용한다(`vm create`·`vm set`). `--numa-memory`만 주면 지금 vNUMA(없으면 `auto`)의 묶기를 바꾼다.
    pub(crate) fn apply(&self, spec: &mut Value) -> anyhow::Result<()> {
        let cpu = &mut spec["cpu"];
        if let Some(p) = self.cpu_policy {
            cpu["policy"] = json!(name(p));
            if p == CpuPolicyArg::Shared
                && let Some(c) = cpu.as_object_mut()
            {
                c.remove("policy");
                c.remove("thread_policy");
                c.remove("emulator");
            }
        }
        if let Some(t) = self.cpu_threads {
            cpu["thread_policy"] = json!(name(t));
        }
        if let Some(e) = self.emulator_threads {
            cpu["emulator"] = json!(name(e));
        }
        match &self.numa {
            Some(NumaArg::None) => {
                if self.numa_memory.is_some() {
                    bail!("--numa-memory needs guest NUMA nodes; it cannot go with --numa none");
                }
                if let Some(s) = spec.as_object_mut() {
                    s.remove("numa");
                }
            }
            Some(NumaArg::Nodes(nodes)) => {
                let mut numa = json!({ "nodes": nodes });
                // 노드만 바꾸면 지금 묶기를 둔다.
                if let Some(m) = spec["numa"]["memory"].as_str() {
                    numa["memory"] = json!(m);
                }
                spec["numa"] = numa;
            }
            None => {}
        }
        if let Some(m) = self.numa_memory {
            if !spec["numa"].is_object() {
                spec["numa"] = json!({ "nodes": "auto" });
            }
            spec["numa"]["memory"] = json!(name(m));
        }
        match &self.hugepages {
            Some(PagesArg(Some(h))) => spec["hugepages"] = json!(h),
            Some(PagesArg(None)) => {
                if let Some(s) = spec.as_object_mut() {
                    s.remove("hugepages");
                }
            }
            None => {}
        }
        Ok(())
    }

    /// 복제·배포·가져오기·템플릿 요청의 칸(`cpu_policy`·`cpu_thread_policy`·`cpu_emulator`·`numa`·`hugepages`).
    /// `clears`면 `none`이 원본의 것을 지우는 `no_numa`·`no_hugepages`(복제·배포)이고, 아니면 `none`은 칸을 뺀다(새 VM).
    pub(crate) fn request_fields(&self, body: &mut Value, clears: bool) -> anyhow::Result<()> {
        if let Some(p) = self.cpu_policy {
            body["cpu_policy"] = json!(name(p));
        }
        if let Some(t) = self.cpu_threads {
            body["cpu_thread_policy"] = json!(name(t));
        }
        if let Some(e) = self.emulator_threads {
            body["cpu_emulator"] = json!(name(e));
        }
        match (&self.numa, self.numa_memory) {
            (Some(NumaArg::Nodes(nodes)), m) => {
                let mut numa = json!({ "nodes": nodes });
                if let Some(m) = m {
                    numa["memory"] = json!(name(m));
                }
                body["numa"] = numa;
            }
            (Some(NumaArg::None), Some(_)) => {
                bail!("--numa-memory needs guest NUMA nodes; it cannot go with --numa none")
            }
            (Some(NumaArg::None), None) => {
                if clears {
                    body["no_numa"] = json!(true);
                }
            }
            (None, Some(m)) => body["numa"] = json!({ "nodes": "auto", "memory": name(m) }),
            (None, None) => {}
        }
        match &self.hugepages {
            Some(PagesArg(Some(h))) => body["hugepages"] = json!(h),
            Some(PagesArg(None)) if clears => body["no_hugepages"] = json!(true),
            _ => {}
        }
        Ok(())
    }
}

/// `vm get`의 NUMA 줄: `numa: 2 nodes x (8 vCPU, 16 GiB), dedicated, hugepages 1G`, 도는 VM이면 `placement: host nodes
/// 0,1, vCPU 0-7 -> CPU 4-11, emulator 1-3`과 보류. 계획 64 칸을 쓰지 않고 기록·보류도 없으면 줄이 없다(지금 출력 그대로).
pub(crate) fn numa_lines(vm: &Value) -> Vec<String> {
    let spec = &vm["spec"];
    let hw = &vm["status"]["hardware"];
    let pending = hw["pending"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["field"] == "numa");
    let used = spec["cpu"]["policy"]
        .as_str()
        .is_some_and(|p| p != "shared")
        || spec["numa"].is_object()
        || spec["hugepages"].is_string();
    if !used && hw["numa"].is_null() && pending.is_none() {
        return Vec::new();
    }
    let mut out = vec![format!("numa: {}", spec_text(spec))];
    if hw["numa"].is_object() {
        out.push(format!(
            "numa running: {}",
            running_text(&hw["numa"], &hw["cpu"])
        ));
    }
    if hw["host_placement"].is_object() {
        out.push(format!(
            "placement: {}",
            placement_text(&hw["host_placement"])
        ));
    }
    if let Some(p) = pending {
        let detail = p["detail"].as_str().unwrap_or("");
        let what = detail.split("; ").next().unwrap_or(detail);
        out.push(format!("numa pending: {what} (power cycle)"));
    }
    out
}

/// 스펙의 짧은 이름(`2 nodes x (8 vCPU, 16 GiB), dedicated (isolate threads), hugepages 1G, memory strict`)
fn spec_text(spec: &Value) -> String {
    let cpu = &spec["cpu"];
    let dedicated = cpu["policy"] == "dedicated";
    let mut parts = Vec::new();
    match &spec["numa"]["nodes"] {
        Value::Null if !dedicated && spec["hugepages"].is_null() => parts.push("none".to_string()),
        Value::Null => parts.push("auto".to_string()),
        Value::String(s) => parts.push(s.clone()),
        Value::Number(n) => {
            let n = n.as_u64().unwrap_or(1).max(1);
            let slots = cpu["max_vcpus"].as_u64().unwrap_or_else(|| {
                cpu["sockets"].as_u64().unwrap_or(1) * cpu["cores"].as_u64().unwrap_or(1)
            });
            let mib = spec["memory_mib"].as_u64().unwrap_or(0);
            parts.push(format!(
                "{n} node{} x ({} vCPU, {})",
                if n == 1 { "" } else { "s" },
                slots / n,
                mib_text(mib / n)
            ));
        }
        Value::Array(list) => parts.push(
            list.iter()
                .map(|n| {
                    format!(
                        "({} vCPU, {})",
                        n["cpus"].as_u64().unwrap_or(0),
                        mib_text(n["memory_mib"].as_u64().unwrap_or(0))
                    )
                })
                .collect::<Vec<_>>()
                .join(" + "),
        ),
        other => parts.push(other.to_string()),
    }
    if dedicated {
        let mut extra = Vec::new();
        if let Some(t) = cpu["thread_policy"].as_str().filter(|t| *t != "prefer") {
            extra.push(format!("{t} threads"));
        }
        if cpu["emulator"] == "isolate" {
            extra.push("isolated emulator".into());
        }
        parts.push(if extra.is_empty() {
            "dedicated".into()
        } else {
            format!("dedicated ({})", extra.join(", "))
        });
    }
    if let Some(h) = spec["hugepages"].as_str() {
        parts.push(format!("hugepages {h}"));
    }
    if let Some(m) = spec["numa"]["memory"].as_str() {
        parts.push(format!("memory {m}"));
    }
    parts.join(", ")
}

/// 기록(`ResolvedNuma`)의 짧은 이름(`2 nodes x (8 vCPU, 16 GiB), dedicated, hugepages 1G`)
fn running_text(numa: &Value, cpu: &Value) -> String {
    let nodes = numa["nodes"].as_array().cloned().unwrap_or_default();
    let per_socket = cpu["max_vcpus"]
        .as_u64()
        .zip(cpu["sockets"].as_u64())
        .map(|(m, s)| m / s.max(1))
        .unwrap_or_else(|| cpu["cores"].as_u64().unwrap_or(1));
    let shape: Vec<String> = nodes
        .iter()
        .map(|n| {
            let sockets = n["sockets"].as_array().map_or(0, Vec::len) as u64;
            format!(
                "({} vCPU, {})",
                sockets * per_socket,
                mib_text(n["memory_mib"].as_u64().unwrap_or(0))
            )
        })
        .collect();
    let mut parts = vec![match shape.as_slice() {
        [one] => format!("1 node {one}"),
        all if all.iter().all(|s| *s == all[0]) => format!("{} nodes x {}", all.len(), all[0]),
        all => all.join(" + "),
    }];
    if numa["policy"] == "dedicated" {
        parts.push("dedicated".into());
    }
    if let Some(p) = numa["page"].as_str() {
        parts.push(format!("hugepages {p}"));
    }
    if let Some(m) = numa["memory"].as_str() {
        parts.push(format!("memory {m}"));
    }
    parts.join(", ")
}

/// 배치(`HostPlacement`)의 짧은 이름(`host nodes 0,1, vCPU 0-7 -> CPU 4-11, emulator 1-3`)
fn placement_text(p: &Value) -> String {
    let mut parts = Vec::new();
    let nodes: Vec<String> = p["host_nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| n.to_string())
        .collect();
    if !nodes.is_empty() {
        parts.push(format!(
            "host node{} {}",
            if nodes.len() == 1 { "" } else { "s" },
            nodes.join(",")
        ));
    }
    let vcpus: Vec<u64> = p["vcpus"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    if !vcpus.is_empty() {
        // 차례로 이어지면 범위(`4-11`), 아니면 자리 순서의 목록
        let run = vcpus.windows(2).all(|w| w[1] == w[0] + 1);
        let cpus = if run && vcpus.len() > 1 {
            format!("{}-{}", vcpus[0], vcpus[vcpus.len() - 1])
        } else {
            vcpus
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        let slots = if vcpus.len() == 1 {
            "0".to_string()
        } else {
            format!("0-{}", vcpus.len() - 1)
        };
        parts.push(format!("vCPU {slots} -> CPU {cpus}"));
    }
    if let Some(e) = p["emulator"].as_str().filter(|e| !e.is_empty()) {
        parts.push(format!("emulator {e}"));
    }
    if parts.is_empty() {
        "-".into()
    } else {
        parts.join(", ")
    }
}

/// MiB를 `16 GiB`·`512 MiB`로
fn mib_text(mib: u64) -> String {
    if mib >= 1024 && mib.is_multiple_of(1024) {
        format!("{} GiB", mib / 1024)
    } else {
        format!("{mib} MiB")
    }
}

/// `vm numa VM`: 스펙·기록·배치·보류와 노드마다 맞는지·까닭·계획
pub(crate) async fn vm_numa(
    client: &Client,
    vm: &str,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let info = client.get(&format!("/vms/{}/numa", seg(vm))).await?;
    if json_out {
        return Ok(print_json(out, &info)?);
    }
    writeln!(out, "spec: {}", cell(&info["wanted"]))?;
    if info["running"].is_object() {
        writeln!(
            out,
            "running: {}",
            running_text(&info["running"], &Value::Null)
        )?;
    }
    if info["placement"].is_object() {
        writeln!(out, "placement: {}", placement_text(&info["placement"]))?;
    }
    if let Some(detail) = info["pending"]["detail"].as_str() {
        writeln!(out, "pending: {detail}")?;
    }
    let rows: Vec<Vec<String>> = info["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|n| {
            let plan = &n["plan"];
            let plan = if plan.is_object() {
                let mut s = format!(
                    "host nodes {}",
                    plan["host_nodes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|h| h.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                );
                if let Some(p) = plan["pcpus"].as_str().filter(|p| !p.is_empty()) {
                    s.push_str(&format!(", CPU {p}"));
                }
                if let Some(p) = plan["page"].as_str() {
                    s.push_str(&format!(", {p} pages"));
                }
                s
            } else {
                "-".into()
            };
            vec![
                cell(&n["node"]),
                if n["fits"] == true { "yes" } else { "no" }.into(),
                plan,
                n["reason"].as_str().unwrap_or("").to_string(),
            ]
        })
        .collect();
    if !rows.is_empty() {
        table(out, &["NODE", "FITS", "PLAN", "REASON"], &rows)?;
    }
    Ok(())
}

/// `vm migrate`가 먼저 찍는 NUMA 까닭(대상이 기록 모양을 받을 수 없음). 읽지 못하면 아무것도 찍지 않는다.
pub(crate) async fn migrate_reason(
    client: &Client,
    vm: &str,
    node: &str,
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let Ok(info) = client.get(&format!("/vms/{}/numa", seg(vm))).await else {
        return Ok(());
    };
    if let Some(n) = info["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|n| n["node"] == node && n["fits"] == false)
    {
        writeln!(err, "warning: {}", n["reason"].as_str().unwrap_or(""))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// 노드
// ---------------------------------------------------------------------------------------------------------------

/// `node topology NODE`: 소켓·NUMA 노드·세 CPU 집합·호스트 노드마다 사용·도는 VM의 배치·경고, `--live`면 노드에 물은
/// 빈 메모리·빈 hugepages
pub(crate) async fn node_topology(
    client: &Client,
    node: &str,
    live: bool,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let info = client
        .get(&format!("/nodes/{}/topology", seg(node)))
        .await?;
    if json_out {
        return Ok(print_json(out, &info)?);
    }
    let t = &info["topology"];
    if !t.is_object() {
        writeln!(
            out,
            "node {}: no NUMA topology (only Linux nodes report one)",
            cell(&info["node"])
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "cpus: {} ({} socket(s), {} thread(s) per core)",
        cell(&t["cpus"]),
        cell(&t["sockets"]),
        cell(&t["threads_per_core"])
    )?;
    let sets = &info["sets"];
    writeln!(
        out,
        "reserved: {}  dedicated: {}  shared: {}",
        dash(&sets["reserved"]),
        dash(&sets["dedicated"]),
        dash(&sets["shared"])
    )?;
    if let Some(i) = t["isolated"].as_str() {
        writeln!(out, "isolcpus: {i}")?;
    }
    if let Some(e) = t["error"].as_str() {
        writeln!(out, "error: {e}")?;
    }
    let rows: Vec<Vec<String>> = info["host_nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| {
            let pages: Vec<String> = h["hugepages"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|p| {
                    format!(
                        "{} {}/{}",
                        cell(&p["size"]),
                        cell(&p["used"]),
                        cell(&p["total"])
                    )
                })
                .collect();
            vec![
                cell(&h["id"]),
                cell(&h["cpus"]),
                mib_text(h["memory_mib"].as_u64().unwrap_or(0)),
                format!("{}/{}", dash(&h["dedicated_used"]), dash(&h["dedicated"])),
                if pages.is_empty() {
                    "-".into()
                } else {
                    pages.join(", ")
                },
            ]
        })
        .collect();
    table(
        out,
        &[
            "HOST-NODE",
            "CPUS",
            "MEMORY",
            "DEDICATED(USED/ALL)",
            "HUGEPAGES(USED/ALL)",
        ],
        &rows,
    )?;
    let distances: Vec<String> = t["distances"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            row.as_array()
                .into_iter()
                .flatten()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    if distances.len() > 1 {
        writeln!(out, "distances: {}", distances.join(" | "))?;
    }
    let vms: Vec<Vec<String>> = info["vms"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            vec![
                cell(&v["vm"]),
                placement_text(v),
                v["page"]
                    .as_str()
                    .map_or("-".into(), |p| format!("{} x {p}", cell(&v["pages"]))),
            ]
        })
        .collect();
    if !vms.is_empty() {
        table(out, &["VM", "PLACEMENT", "PAGES"], &vms)?;
    }
    if live {
        match info["live"].as_object() {
            None => writeln!(out, "live: the node did not answer")?,
            Some(l) => {
                for n in l
                    .get("nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let pages: Vec<String> = n["hugepages"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|p| {
                            format!(
                                "{} kB free {} surplus {}",
                                cell(&p["size_kib"]),
                                cell(&p["free"]),
                                cell(&p["surplus"])
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "live node {}: {} free{}",
                        cell(&n["id"]),
                        mib_text(n["memory_free_mib"].as_u64().unwrap_or(0)),
                        if pages.is_empty() {
                            String::new()
                        } else {
                            format!(", hugepages {}", pages.join(", "))
                        }
                    )?;
                }
                if l.get("affinity_ok") == Some(&json!(false)) {
                    writeln!(out, "live: orbivisord does not run on the reserved CPUs")?;
                }
            }
        }
    }
    for w in info["warnings"].as_array().into_iter().flatten() {
        writeln!(out, "warning: {}", w.as_str().unwrap_or(""))?;
    }
    Ok(())
}

fn dash(v: &Value) -> String {
    match v.as_str() {
        Some("") | None => "-".into(),
        Some(s) => s.into(),
    }
}

/// `node resources NODE`의 인자
#[derive(Args, Debug, Clone, Default)]
pub(crate) struct ResourcesArgs {
    /// 호스트와 Orbivisor의 CPU(`0-1`, 빈 값은 없음). VM 스레드는 두지 않는다.
    #[arg(long, value_name = "CPUS")]
    pub reserved_cpus: Option<String>,
    /// 전용 VM만 쓰는 CPU(`4-31`, 빈 값은 없음). 나머지가 공유 CPU다.
    #[arg(long, value_name = "CPUS")]
    pub dedicated_cpus: Option<String>,
    /// 실행 중 hugepages 예약(`2M:1024@0,2M:1024@1` — 크기:페이지 수[@호스트 노드], 빈 값은 없음). 설정에서 뺀
    /// 예약은 커널의 수를 그대로 둔다.
    #[arg(long, value_name = "SIZE:COUNT[@NODE],...")]
    pub hugepages: Option<String>,
    /// 설정을 지운다(공유 CPU = 모든 CPU, 지금과 같다).
    #[arg(long, conflicts_with_all = ["reserved_cpus", "dedicated_cpus", "hugepages"])]
    pub clear: bool,
    /// 도는 VM의 pCPU·hugepages를 빼앗아도 쓴다(409 `host_cpus_in_use`·`hugepages_in_use`를 넘김).
    #[arg(long)]
    pub force: bool,
}

impl ResourcesArgs {
    fn given(&self) -> bool {
        self.reserved_cpus.is_some()
            || self.dedicated_cpus.is_some()
            || self.hugepages.is_some()
            || self.clear
    }
}

/// `2M:1024@0,1G:4`를 `host_resources.hugepages`로
pub(crate) fn parse_reserves(s: &str) -> anyhow::Result<Value> {
    let mut out = Vec::new();
    for item in s.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let bad = || format!("invalid --hugepages {item:?}: use SIZE:COUNT[@NODE] (2M:1024@0)");
        let (size, rest) = item.split_once(':').with_context(bad)?;
        let size = match size {
            "2M" | "2m" => "2M",
            "1G" | "1g" => "1G",
            _ => bail!(bad()),
        };
        let (count, node) = match rest.split_once('@') {
            Some((c, n)) => (c, Some(n.parse::<u16>().ok().with_context(bad)?)),
            None => (rest, None),
        };
        let count: u32 = count.parse().ok().with_context(bad)?;
        let mut r = json!({ "size": size, "count": count });
        if let Some(n) = node {
            r["host_node"] = json!(n);
        }
        out.push(r);
    }
    Ok(Value::Array(out))
}

/// 지금 설정(`host_resources`, 없으면 null)에 준 칸을 덮은 새 설정. `--clear`면 `{}`(지우기)다.
pub(crate) fn resources_body(current: &Value, args: &ResourcesArgs) -> anyhow::Result<Value> {
    if args.clear {
        return Ok(json!({}));
    }
    let mut r = current.as_object().cloned().unwrap_or_default();
    for (key, value) in [
        ("reserved_cpus", &args.reserved_cpus),
        ("dedicated_cpus", &args.dedicated_cpus),
    ] {
        match value.as_deref().map(str::trim) {
            None => {}
            Some("") => {
                r.remove(key);
            }
            Some(v) => {
                r.insert(key.into(), json!(v));
            }
        }
    }
    if let Some(h) = &args.hugepages {
        let list = parse_reserves(h)?;
        if list.as_array().is_some_and(Vec::is_empty) {
            r.remove("hugepages");
        } else {
            r.insert("hugepages".into(), list);
        }
    }
    Ok(Value::Object(r))
}

/// 설정의 짧은 이름(`dedicated=4-31 reserved=0-1 hugepages=2M:1024@0`, 없으면 `none`)
fn resources_text(r: &Value) -> String {
    let mut parts = Vec::new();
    for (key, label) in [
        ("dedicated_cpus", "dedicated"),
        ("reserved_cpus", "reserved"),
    ] {
        if let Some(v) = r[key].as_str().filter(|v| !v.is_empty()) {
            parts.push(format!("{label}={v}"));
        }
    }
    let pages: Vec<String> = r["hugepages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| {
            let at = h["host_node"]
                .as_u64()
                .map(|n| format!("@{n}"))
                .unwrap_or_default();
            format!("{}:{}{at}", cell(&h["size"]), cell(&h["count"]))
        })
        .collect();
    if !pages.is_empty() {
        parts.push(format!("hugepages={}", pages.join(",")));
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(" ")
    }
}

/// `node resources NODE [...]`: 칸을 주면 노드 설정 `host_resources`를 바꾸고(나머지 칸은 그대로), 아니면 지금 설정을
/// 보인다.
pub(crate) async fn node_resources(
    client: &Client,
    node: &str,
    args: &ResourcesArgs,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let path = format!("/nodes/{}", seg(node));
    let current = client.get(&path).await?;
    if !args.given() {
        if json_out {
            return Ok(print_json(out, &current["spec"]["host_resources"])?);
        }
        writeln!(
            out,
            "{}",
            resources_text(&current["spec"]["host_resources"])
        )?;
        return Ok(());
    }
    let resources = resources_body(&current["spec"]["host_resources"], args)?;
    let mut spec = current["spec"].clone();
    spec["host_resources"] = resources;
    let mut target = path;
    if args.force {
        target.push_str("?force=true");
    }
    // GET은 빈 레이블을 빼므로 그대로 옮기면 `null`이다(리뷰 1 GitHub 탐침).
    let labels = current.get("labels").cloned().unwrap_or_else(|| json!({}));
    let body = json!({ "spec": spec, "labels": labels });
    let req = client
        .request(Method::PUT, &target)
        .header("if-match", current["revision"].to_string())
        .json(&body);
    let (headers, updated) = client.send_with_headers(req).await?;
    if let Some(w) = headers
        .get("orbivisor-warning")
        .and_then(|v| v.to_str().ok())
    {
        eprintln!("warning: {w}");
    }
    if json_out {
        return Ok(print_json(out, &updated)?);
    }
    writeln!(
        out,
        "node {}: {}",
        cell(&updated["name"]),
        resources_text(&updated["spec"]["host_resources"])
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct T {
        #[command(flatten)]
        numa: NumaArgs,
    }

    #[derive(Parser)]
    struct R {
        #[command(flatten)]
        res: ResourcesArgs,
    }

    fn parse(args: &[&str]) -> T {
        T::try_parse_from(std::iter::once("t").chain(args.iter().copied())).unwrap()
    }

    fn parse_res(args: &[&str]) -> R {
        R::try_parse_from(std::iter::once("r").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn numa_options_change_the_spec() {
        let t = parse(&[
            "--cpu-policy",
            "dedicated",
            "--cpu-threads",
            "isolate",
            "--numa",
            "8:16384,8:16384",
            "--numa-memory",
            "preferred",
            "--hugepages",
            "1G",
        ]);
        let mut spec = json!({ "cpu": { "sockets": 2, "cores": 8 } });
        t.numa.apply(&mut spec).unwrap();
        assert_eq!(
            spec,
            json!({
                "cpu": { "sockets": 2, "cores": 8, "policy": "dedicated", "thread_policy": "isolate" },
                "numa": { "nodes": [{ "cpus": 8, "memory_mib": 16384 }, { "cpus": 8, "memory_mib": 16384 }],
                          "memory": "preferred" },
                "hugepages": "1G",
            })
        );
        // shared는 정책 칸을 모두 지우고, none은 vNUMA·hugepages를 뺀다.
        let back = parse(&[
            "--cpu-policy",
            "shared",
            "--numa",
            "none",
            "--hugepages",
            "none",
        ]);
        back.numa.apply(&mut spec).unwrap();
        assert_eq!(spec, json!({ "cpu": { "sockets": 2, "cores": 8 } }));
        // --numa-memory만이면 auto에 묶기
        let mut spec = json!({ "cpu": {} });
        parse(&["--numa-memory", "strict"])
            .numa
            .apply(&mut spec)
            .unwrap();
        assert_eq!(spec["numa"], json!({ "nodes": "auto", "memory": "strict" }));
        // 노드 수만 바꾸면 묶기는 그대로
        parse(&["--numa", "2"]).numa.apply(&mut spec).unwrap();
        assert_eq!(spec["numa"], json!({ "nodes": 2, "memory": "strict" }));
        assert!(
            parse(&["--numa", "none", "--numa-memory", "strict"])
                .numa
                .apply(&mut spec)
                .is_err()
        );
        for bad in [
            vec!["--numa", "two"],
            vec!["--numa", "8:x"],
            vec!["--hugepages", "4K"],
            vec!["--cpu-policy", "mixed"],
        ] {
            assert!(
                T::try_parse_from(std::iter::once("t").chain(bad.iter().copied())).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn request_fields_use_the_shared_names() {
        let t = parse(&[
            "--cpu-policy",
            "dedicated",
            "--numa",
            "none",
            "--hugepages",
            "none",
        ]);
        let mut body = json!({});
        t.numa.request_fields(&mut body, true).unwrap();
        assert_eq!(
            body,
            json!({ "cpu_policy": "dedicated", "no_numa": true, "no_hugepages": true })
        );
        let mut body = json!({});
        t.numa.request_fields(&mut body, false).unwrap();
        assert_eq!(body, json!({ "cpu_policy": "dedicated" }));
        let t = parse(&[
            "--numa",
            "auto",
            "--numa-memory",
            "strict",
            "--hugepages",
            "2M",
        ]);
        let mut body = json!({});
        t.numa.request_fields(&mut body, false).unwrap();
        assert_eq!(
            body,
            json!({ "numa": { "nodes": "auto", "memory": "strict" }, "hugepages": "2M" })
        );
    }

    #[test]
    fn vm_get_lines_show_the_placement() {
        assert!(numa_lines(&json!({ "spec": { "cpu": { "cores": 2 } }, "status": {} })).is_empty());
        let vm = json!({
            "spec": { "cpu": { "sockets": 2, "cores": 8, "policy": "dedicated" }, "memory_mib": 32768,
                      "numa": { "nodes": 2 }, "hugepages": "1G" },
            "status": { "hardware": {
                "cpu": { "sockets": 2, "cores": 8 },
                "numa": { "nodes": [{ "sockets": [0], "memory_mib": 16384 }, { "sockets": [1], "memory_mib": 16384 }],
                          "page": "1G", "policy": "dedicated", "memory": "strict" },
                "host_placement": { "node": "n", "vcpus": [4, 5, 6, 7], "host_nodes": [0, 1], "emulator": "1-3" },
                "pending": [{ "field": "numa", "detail": "hugepages 1G -> 2M; needs a power cycle" }]
            } }
        });
        assert_eq!(
            numa_lines(&vm),
            [
                "numa: 2 nodes x (8 vCPU, 16 GiB), dedicated, hugepages 1G",
                "numa running: 2 nodes x (8 vCPU, 16 GiB), dedicated, hugepages 1G, memory strict",
                "placement: host nodes 0,1, vCPU 0-3 -> CPU 4-7, emulator 1-3",
                "numa pending: hugepages 1G -> 2M (power cycle)",
            ]
        );
    }

    #[test]
    fn node_resources_merge_the_given_fields() {
        let t = parse_res(&["--dedicated-cpus", "4-31", "--hugepages", "2M:1024@0,1G:4"]);
        let body = resources_body(&json!({ "reserved_cpus": "0-1" }), &t.res).unwrap();
        assert_eq!(
            body,
            json!({ "reserved_cpus": "0-1", "dedicated_cpus": "4-31", "hugepages": [
                { "size": "2M", "count": 1024, "host_node": 0 }, { "size": "1G", "count": 4 }
            ] })
        );
        assert_eq!(
            resources_text(&body),
            "dedicated=4-31 reserved=0-1 hugepages=2M:1024@0,1G:4"
        );
        let t = parse_res(&["--reserved-cpus", "", "--hugepages", ""]);
        assert_eq!(
            resources_body(&body, &t.res).unwrap(),
            json!({ "dedicated_cpus": "4-31" })
        );
        let t = parse_res(&["--clear"]);
        assert_eq!(resources_body(&body, &t.res).unwrap(), json!({}));
        assert!(parse_reserves("3M:1").is_err());
        assert!(parse_reserves("2M:x").is_err());
        assert!(R::try_parse_from(["r", "--clear", "--dedicated-cpus", "1"]).is_err());
    }

    /// 계획 64 GitHub 탐침: 레이블이 없는 노드(GET이 `labels`를 뺌)를 `labels: null`로 PUT해 서버가 400이었다. 빈 맵을
    /// 보내고 `If-Match`·나머지 스펙은 읽은 그대로다.
    #[tokio::test]
    async fn node_resources_send_labels_as_a_map() {
        let (client, seen) = crate::fake_api::fake_api(|method, _| {
            let node = json!({
                "id": "n", "name": "node1", "revision": 9,
                "spec": { "address": "a:1", "maintenance": false },
                "status": {}
            });
            if method == "PUT" {
                let mut n = node;
                n["spec"]["host_resources"] = json!({ "dedicated_cpus": "2-3" });
                (200, n)
            } else {
                (200, node)
            }
        })
        .await;
        let args = R::try_parse_from(["r", "--dedicated-cpus", "2-3"])
            .unwrap()
            .res;
        let mut out = Vec::new();
        node_resources(&client, "node1", &args, false, &mut out)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "node node1: dedicated=2-3\n"
        );
        let seen = seen.lock().unwrap();
        assert_eq!(seen[1].line, "PUT /api/v1/nodes/node1 HTTP/1.1");
        assert_eq!(seen[1].if_match.as_deref(), Some("9"));
        assert_eq!(
            seen[1].body,
            json!({ "labels": {}, "spec": { "address": "a:1", "maintenance": false,
                    "host_resources": { "dedicated_cpus": "2-3" } } })
        );
    }
}
