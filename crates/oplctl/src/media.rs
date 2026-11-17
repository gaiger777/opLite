//! 설치 매체와 cloud-init(계획 57): `orbivisorctl iso …`, URL에서 볼륨 받기, `vm cdrom …`, `vm create|set`의
//! `--cdrom`·`--boot`·`--ci-*`, `vm get`의 CD-ROM·부팅 순서·cloud-init 줄.

use std::io::{BufRead, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use opl_types::cloud_init::{IpConfig, is_crypt_hash};
use opl_types::vm::effective_boot;
use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::cli::{hotplug_result, items, name_of, node_local, upload_volume};
use crate::client::{Client, IMPORT_WAIT, seg, task_header, warning_header};
use crate::output::{bytes, cell, json as print_json, table};

#[derive(Subcommand)]
pub(crate) enum IsoCmd {
    /// ISO 이미지(내용 종류가 iso인 볼륨)와 그것을 CD-ROM 드라이브에 넣은 VM
    List {
        /// 이 풀의 ISO만
        #[arg(long)]
        pool: Option<String>,
    },
    /// ISO 파일을 올린다(dir·NFS 풀만, ISO 9660이나 UDF 이미지여야 함).
    Upload {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        file: PathBuf,
    },
    /// 노드가 http·https URL에서 ISO를 받는다. 그 노드가 URL에 닿아야 한다.
    Download {
        name: String,
        #[arg(long)]
        pool: String,
        #[arg(long)]
        url: String,
        /// 받은 파일의 SHA-256(16진 64자). 다르면 실패하고 지운다.
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
        /// 받기 Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// ISO를 지운다. CD-ROM 드라이브에 꽂혀 있거나 스냅샷이 가리키면 지우지 못한다.
    Delete { name: String },
}

/// `iso list`의 열
const ISO_HEADERS: &[&str] = &["NAME", "POOL", "SIZE", "NODE", "USED-BY"];

/// 볼륨이 ISO 이미지인지(`spec.content`, 디스크면 없음)
fn is_iso(volume: &Value) -> bool {
    volume["spec"]["content"] == "iso"
}

/// VM이 ISO `iso`를 CD-ROM 드라이브에 넣었는지(스펙이나, 빼기를 아직 마치지 못한 실행 중 기록)
fn uses_iso(vm: &Value, iso: &str) -> bool {
    [&vm["spec"]["cdroms"], &vm["status"]["hardware"]["cdroms"]]
        .iter()
        .filter_map(|list| list.as_array())
        .flatten()
        .any(|c| c["iso"] == iso)
}

/// `iso list`의 행: 볼륨 가운데 ISO만(`pool`이 있으면 그 풀만). NODE는 노드 로컬 풀이면 그 노드, USED-BY는 그
/// ISO를 넣은 VM 이름들이다. `volumes`·`vms`·`nodes`·`pools`는 목록 응답의 항목이다.
pub(crate) fn iso_rows(
    volumes: &[Value],
    vms: &[Value],
    nodes: &[Value],
    pools: &[Value],
    pool: Option<&str>,
) -> Vec<Vec<String>> {
    volumes
        .iter()
        .filter(|v| is_iso(v) && pool.is_none_or(|p| v["spec"]["pool"] == p))
        .map(|v| {
            let name = cell(&v["name"]);
            let local = pools
                .iter()
                .find(|p| p["name"] == v["spec"]["pool"])
                .is_some_and(node_local);
            let node = if local && !v["status"]["node"].is_null() {
                name_of(nodes, &v["status"]["node"])
            } else {
                "-".into()
            };
            let users: Vec<String> = vms
                .iter()
                .filter(|vm| uses_iso(vm, &name))
                .map(|vm| cell(&vm["name"]))
                .collect();
            vec![
                name,
                cell(&v["spec"]["pool"]),
                bytes(&v["spec"]["size_bytes"]),
                node,
                if users.is_empty() {
                    "-".into()
                } else {
                    users.join(", ")
                },
            ]
        })
        .collect()
}

pub(crate) async fn iso(
    client: &Client,
    cmd: IsoCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        IsoCmd::List { pool } => {
            let volumes = items(&client.get("/volumes").await?);
            if json_out {
                let isos: Vec<Value> = volumes
                    .into_iter()
                    .filter(|v| is_iso(v) && pool.as_deref().is_none_or(|p| v["spec"]["pool"] == p))
                    .collect();
                print_json(out, &Value::Array(isos))?;
                return Ok(());
            }
            let vms = items(&client.get("/vms").await?);
            let nodes = items(&client.get("/nodes").await?);
            let pools = items(&client.get("/storage-pools").await?);
            let rows = iso_rows(&volumes, &vms, &nodes, &pools, pool.as_deref());
            table(out, ISO_HEADERS, &rows)?;
        }
        IsoCmd::Upload { name, pool, file } => {
            let query = [
                ("name", name.as_str()),
                ("pool", pool.as_str()),
                ("content", "iso"),
                ("source_format", "raw"),
                ("format", "raw"),
            ];
            upload_volume(client, &query, &file).await?;
            writeln!(out, "iso {name} uploaded")?;
        }
        IsoCmd::Download {
            name,
            pool,
            url,
            sha256,
            no_wait,
        } => {
            let mut body = json!({ "name": name, "pool": pool, "url": url, "content": "iso" });
            if let Some(sum) = sha256 {
                body["sha256"] = json!(sum);
            }
            download(client, &body, "iso", no_wait, out, err).await?;
        }
        IsoCmd::Delete { name } => {
            let volume = client.get(&format!("/volumes/{}", seg(&name))).await?;
            if !is_iso(&volume) {
                bail!("volume {name} is not an ISO image; use `orbivisorctl volume delete`");
            }
            let task = client.delete(&format!("/volumes/{}", seg(&name))).await?;
            client.wait_task(&task).await?;
            writeln!(out, "iso {name} deleted")?;
        }
    }
    Ok(())
}

/// `POST /volumes/download`로 받기 Task를 만들고 기본으로 끝날 때까지 기다리며 진행 줄을 `err`에 쓴다. `what`은
/// 출력의 이름(`iso`·`volume`)이다.
pub(crate) async fn download(
    client: &Client,
    body: &Value,
    what: &str,
    no_wait: bool,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let name = cell(&body["name"]);
    let task = client.post("/volumes/download", body).await?;
    if no_wait {
        writeln!(
            out,
            "{what} {name}: download task {} started",
            cell(&task["id"])
        )?;
        return Ok(());
    }
    let mut shown = None;
    client
        .wait_task_watch(&task, IMPORT_WAIT, |t| {
            if let Some(line) = download_progress(&t["status"]["progress"], &mut shown) {
                // 진행 줄을 쓰지 못해도(stderr가 닫힘) 받기는 계속 기다린다.
                let _ = writeln!(err, "{what} {name}: {line}");
            }
            Ok(())
        })
        .await?;
    writeln!(out, "{what} {name} downloaded")?;
    Ok(())
}

/// 마지막으로 알린 받기 진행: (단계, 받은 바이트)
type DownloadShown = Option<(String, u64)>;

/// 받기 Task의 `progress`에서 알릴 줄: 단계가 바뀔 때, 받는 중이면 크기를 알 때 5%마다, 모르면 256 MiB마다
/// (`downloading 1.2 GiB / 2.5 GiB`).
pub(crate) fn download_progress(progress: &Value, shown: &mut DownloadShown) -> Option<String> {
    let stage = progress["stage"].as_str()?;
    let done = progress["done_bytes"].as_u64().unwrap_or(0);
    let total = progress["total_bytes"].as_u64().unwrap_or(0);
    let step = if total == 0 { 256 << 20 } else { total / 20 };
    let show = match shown {
        Some((last, at)) if last == stage => stage == "downloading" && done >= *at + step.max(1),
        _ => true,
    };
    if !show {
        return None;
    }
    *shown = Some((stage.to_string(), done));
    Some(match stage {
        "downloading" if total > 0 => format!("downloading {} / {}", gib(done), gib(total)),
        "downloading" => format!("downloading {}", gib(done)),
        other => other.to_string(),
    })
}

/// 바이트를 소수 한 자리 GiB로(`1.2 GiB`)
fn gib(n: u64) -> String {
    format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64)
}

#[derive(Subcommand)]
pub(crate) enum VmCdromCmd {
    /// ISO를 CD-ROM 드라이브에 넣는다. 실행 중이면 바로 바꾼다(게스트가 트레이를 잠가도 강제로 연다).
    Insert {
        vm: String,
        iso: String,
        /// 드라이브 번호(0부터, `cdroms` 순서)
        #[arg(long, default_value_t = 0)]
        drive: u8,
        /// 실행 중인 VM의 핫플러그 Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// CD-ROM 드라이브의 매체를 뺀다(드라이브는 빈 채로 남는다). 실행 중이면 바로 뺀다.
    Eject {
        vm: String,
        /// 드라이브 번호(0부터, `cdroms` 순서)
        #[arg(long, default_value_t = 0)]
        drive: u8,
        /// 실행 중인 VM의 핫플러그 Task를 기다리지 않는다.
        #[arg(long)]
        no_wait: bool,
    },
    /// CD-ROM 드라이브를 끝에 더한다(꺼진 VM만, 4개까지). ISO를 주지 않으면 빈 드라이브다.
    Add {
        vm: String,
        iso: Option<String>,
        /// 부팅 순서(1이 먼저). 없으면 디스크 뒤다.
        #[arg(long)]
        boot_order: Option<u8>,
    },
    /// CD-ROM 드라이브를 뺀다(꺼진 VM만). 번호를 주지 않으면 마지막 드라이브다.
    Remove {
        vm: String,
        #[arg(long)]
        drive: Option<usize>,
    },
}

pub(crate) async fn vm_cdrom(
    client: &Client,
    cmd: VmCdromCmd,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        VmCdromCmd::Insert {
            vm,
            iso,
            drive,
            no_wait,
        } => {
            let done = format!("vm {vm}: {iso} inserted in cdrom{drive}");
            let body = json!({ "drive": drive, "iso": iso });
            change_medium(client, &vm, &body, no_wait, &done, out, err).await?;
        }
        VmCdromCmd::Eject { vm, drive, no_wait } => {
            let done = format!("vm {vm}: cdrom{drive} ejected");
            let body = json!({ "drive": drive, "iso": null });
            change_medium(client, &vm, &body, no_wait, &done, out, err).await?;
        }
        VmCdromCmd::Add {
            vm,
            iso,
            boot_order,
        } => {
            let drive = std::cell::Cell::new(0);
            crate::cli::put_spec(client, &vm, |current| {
                let mut cdroms = current["spec"]["cdroms"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                drive.set(cdroms.len());
                cdroms.push(cdrom_value(iso.as_deref(), boot_order));
                Ok(crate::cli::spec_update(
                    current,
                    "cdroms",
                    Value::Array(cdroms),
                ))
            })
            .await?;
            writeln!(out, "vm {vm}: cdrom{} added", drive.get())?;
        }
        VmCdromCmd::Remove { vm, drive } => {
            let removed = std::cell::Cell::new(0);
            crate::cli::put_spec(client, &vm, |current| {
                let mut cdroms = current["spec"]["cdroms"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let i = match drive {
                    Some(i) if i < cdroms.len() => i,
                    Some(i) => bail!("VM {} has no CD-ROM drive {i}", cell(&current["name"])),
                    None if cdroms.is_empty() => {
                        bail!("VM {} has no CD-ROM drives", cell(&current["name"]))
                    }
                    None => cdroms.len() - 1,
                };
                cdroms.remove(i);
                removed.set(i);
                Ok(crate::cli::spec_update(
                    current,
                    "cdroms",
                    Value::Array(cdroms),
                ))
            })
            .await?;
            writeln!(out, "vm {vm}: cdrom{} removed", removed.get())?;
        }
    }
    Ok(())
}

/// `POST /vms/{key}/cdrom`(`body`는 `{drive, iso}`): 드라이브 하나의 매체를 바꾸고(실행 중이면 핫플러그 Task를
/// 기다림) 결과를 알린다.
async fn change_medium(
    client: &Client,
    vm: &str,
    body: &Value,
    no_wait: bool,
    done: &str,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    let r = client
        .request(Method::POST, &format!("/vms/{}/cdrom", seg(vm)))
        .json(body);
    let (headers, _) = client.send_with_headers(r).await?;
    if let Some(warning) = warning_header(&headers) {
        writeln!(err, "warning: {warning}")?;
    }
    hotplug_result(client, vm, task_header(&headers), no_wait, done, out).await
}

/// 스펙의 CD-ROM 드라이브 하나(`iso`가 없으면 빈 드라이브)
pub(crate) fn cdrom_value(iso: Option<&str>, boot_order: Option<u8>) -> Value {
    let mut cdrom = json!({});
    if let Some(iso) = iso {
        cdrom["iso"] = json!(iso);
    }
    if let Some(n) = boot_order {
        cdrom["boot_order"] = json!(n);
    }
    cdrom
}

/// `vm create --cdrom`의 드라이브들: `none`은 빈 드라이브다.
pub(crate) fn cdroms_value(isos: &[String]) -> Vec<Value> {
    isos.iter()
        .map(|iso| cdrom_value((iso != "none").then_some(iso.as_str()), None))
        .collect()
}

/// 디스크들과 CD-ROM 드라이브들의 `boot_order`
pub(crate) type BootOrders = (Vec<Option<u8>>, Vec<Option<u8>>);

/// `--boot disk0,cdrom0`: 목록 순서대로 1, 2, …를 준 디스크·CD-ROM 드라이브의 `boot_order`. 목록에 없는 장치는
/// 없음이다(디스크를 하나도 적지 않으면 서버가 디스크를 목록 뒤에 차례로 둔다 — `effective_boot`). `disk`는 `disk0`,
/// `cdrom`은 `cdrom0`이다.
pub(crate) fn parse_boot(list: &str, disks: usize, cdroms: usize) -> Result<BootOrders, String> {
    let mut disk_orders = vec![None; disks];
    let mut cdrom_orders = vec![None; cdroms];
    for (i, word) in list.split(',').map(str::trim).enumerate() {
        let (kind, index) = match word {
            "disk" => ("disk", "0"),
            "cdrom" => ("cdrom", "0"),
            w => match (w.strip_prefix("disk"), w.strip_prefix("cdrom")) {
                (Some(n), _) => ("disk", n),
                (_, Some(n)) => ("cdrom", n),
                _ => return Err(format!("invalid boot device {w:?}: use diskN or cdromN")),
            },
        };
        let index: usize = index
            .parse()
            .map_err(|_| format!("invalid boot device {word:?}: use diskN or cdromN"))?;
        let slots = if kind == "disk" {
            &mut disk_orders
        } else {
            &mut cdrom_orders
        };
        let order = u8::try_from(i + 1).map_err(|_| "at most 255 boot devices".to_string())?;
        match slots.get_mut(index) {
            None => return Err(format!("the VM has no {kind}{index}")),
            Some(Some(_)) => return Err(format!("{kind}{index} is listed twice in --boot")),
            Some(slot) => *slot = Some(order),
        }
    }
    Ok((disk_orders, cdrom_orders))
}

/// 스펙(`spec`)의 디스크·CD-ROM에 `--boot` 목록의 부팅 순서를 넣는다.
pub(crate) fn apply_boot(spec: &mut Value, list: &str) -> anyhow::Result<()> {
    let count = |key: &str| spec[key].as_array().map_or(0, Vec::len);
    let (disks, cdroms) =
        parse_boot(list, count("disks"), count("cdroms")).map_err(anyhow::Error::msg)?;
    for (key, orders) in [("disks", disks), ("cdroms", cdroms)] {
        for (device, order) in spec[key].as_array_mut().into_iter().flatten().zip(orders) {
            match order {
                Some(n) => device["boot_order"] = json!(n),
                None => {
                    if let Some(d) = device.as_object_mut() {
                        d.remove("boot_order");
                    }
                }
            }
        }
    }
    Ok(())
}

/// `--ci-ip`·`--ci-ip6` 하나: `[nicN=]dhcp|ADDR/PFX[,gw=GW]`(IPv6는 `slaac`도). NIC 번호가 없으면 None(순서대로)이다.
pub(crate) fn parse_ci_ip(s: &str, ipv6: bool) -> Result<(Option<usize>, IpConfig), String> {
    let (flag, modes) = if ipv6 {
        ("--ci-ip6", "dhcp, slaac")
    } else {
        ("--ci-ip", "dhcp")
    };
    let bad = || format!("invalid {flag} {s:?}: use [nicN=]{modes} or ADDRESS/PREFIX[,gw=GATEWAY]");
    let (nic, value) = match s.split_once('=') {
        Some((nic, value)) if nic.starts_with("nic") => {
            let n = nic[3..].parse::<usize>().map_err(|_| bad())?;
            (Some(n), value)
        }
        _ => (None, s),
    };
    let family = |ip: &IpAddr| ip.is_ipv6() == ipv6;
    let kind = if ipv6 { "IPv6" } else { "IPv4" };
    let config = match value {
        "dhcp" => IpConfig::Dhcp,
        "slaac" if ipv6 => IpConfig::Slaac,
        "slaac" => return Err(format!("{flag} {s:?}: slaac is only for --ci-ip6")),
        _ => {
            let mut parts = value.split(',');
            let address = parts.next().unwrap_or_default();
            let (ip, prefix) = address.split_once('/').ok_or_else(bad)?;
            let ip: IpAddr = ip.parse().map_err(|_| bad())?;
            let prefix: u8 = prefix.parse().map_err(|_| bad())?;
            if !family(&ip) || prefix > if ipv6 { 128 } else { 32 } {
                return Err(format!(
                    "{flag} {s:?}: {address} is not an {kind} address with a prefix length"
                ));
            }
            let mut gateway = None;
            for part in parts {
                let gw = part.strip_prefix("gw=").ok_or_else(bad)?;
                let ip: IpAddr = gw.parse().map_err(|_| bad())?;
                if gateway.is_some() || !family(&ip) {
                    return Err(format!(
                        "{flag} {s:?}: gateway {gw} is not an {kind} address"
                    ));
                }
                gateway = Some(gw.to_string());
            }
            IpConfig::Static {
                address: address.to_string(),
                gateway,
            }
        }
    };
    Ok((nic, config))
}

fn parse_ci_ip4(s: &str) -> Result<(Option<usize>, IpConfig), String> {
    parse_ci_ip(s, false)
}

fn parse_ci_ip6(s: &str) -> Result<(Option<usize>, IpConfig), String> {
    parse_ci_ip(s, true)
}

/// `vm create|set`의 cloud-init 값. 하나라도 주면 cloud-init을 켠다. `vm set`은 준 값만 바꾼다.
#[derive(Debug, Default, Clone, PartialEq, Eq, Args)]
pub(crate) struct CloudInitArgs {
    /// cloud-init NoCloud 씨앗 드라이브를 단다(다른 `--ci-*`를 주면 저절로 켠다). 배포판 기본 사용자·DHCP다.
    #[arg(long)]
    pub cloud_init: bool,
    /// 호스트 이름(FQDN도 됨). 없으면 VM 이름
    #[arg(long, value_name = "NAME")]
    pub ci_hostname: Option<String>,
    /// 배포판 기본 사용자의 새 이름(root는 안 됨)
    #[arg(long, value_name = "USER")]
    pub ci_user: Option<String>,
    /// 기본 사용자의 비밀번호를 이 파일의 첫 줄로(`-`는 표준 입력). 서버는 SHA-512 crypt 해시만 저장한다.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["ci_password_prompt", "ci_password_hash"])]
    pub ci_password_file: Option<PathBuf>,
    /// 기본 사용자의 비밀번호를 묻는다(두 번).
    #[arg(long, conflicts_with = "ci_password_hash")]
    pub ci_password_prompt: bool,
    /// 기본 사용자의 비밀번호를 crypt 해시로(SHA-512 `$6$…` — `mkpasswd -m sha-512`; `$5$`·`$1$`·`$2a$`·`$2y$`도 됨).
    /// cloud-init이 yescrypt(`$y$`)·bcrypt `$2b$`는 해시로 보지 않으므로 받지 않는다.
    #[arg(long, value_name = "HASH")]
    pub ci_password_hash: Option<String>,
    /// 기본 사용자의 SSH 공개 키 한 줄, 또는 `@FILE`(줄마다 키, 빈 줄과 `#` 줄은 건너뜀). 여러 번 줄 수 있다.
    #[arg(long = "ci-ssh-key", value_name = "KEY|@FILE")]
    pub ci_ssh_keys: Vec<String>,
    /// NIC의 IPv4: `dhcp`나 `ADDRESS/PREFIX[,gw=GATEWAY]`. `nicN=`이 없으면 준 순서대로 NIC 0, 1, …이다.
    #[arg(long = "ci-ip", value_name = "[nicN=]dhcp|ADDR/PFX[,gw=GW]", value_parser = parse_ci_ip4)]
    pub ci_ip: Vec<(Option<usize>, IpConfig)>,
    /// NIC의 IPv6: `dhcp`, `slaac`나 `ADDRESS/PREFIX[,gw=GATEWAY]`. 그 NIC에 `--ci-ip`가 없으면 IPv4를 두지 않는다
    /// (IPv4 DHCP도 쓰려면 `--ci-ip nicN=dhcp`).
    #[arg(long = "ci-ip6", value_name = "[nicN=]dhcp|slaac|ADDR/PFX[,gw=GW]", value_parser = parse_ci_ip6)]
    pub ci_ip6: Vec<(Option<usize>, IpConfig)>,
    /// DNS 서버(3개까지). 주면 목록을 통째로 바꾼다.
    #[arg(long = "ci-dns", value_name = "IP")]
    pub ci_dns: Vec<IpAddr>,
    /// DNS 검색 도메인(6개까지). 주면 목록을 통째로 바꾼다.
    #[arg(long = "ci-search", value_name = "DOMAIN")]
    pub ci_search: Vec<String>,
    /// 만든 user-data 대신 쓸 user-data 파일(사용자·비밀번호·SSH 키는 쓰이지 않는다)
    #[arg(long, value_name = "FILE")]
    pub ci_user_data: Option<PathBuf>,
    /// vendor-data 파일(만든 user-data와 함께 돈다 — 꾸러미·runcmd)
    #[arg(long, value_name = "FILE")]
    pub ci_vendor_data: Option<PathBuf>,
}

/// `vm set`만의 cloud-init 값: 끄기, 비밀번호 지우기, SSH 키 지우기
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CloudInitClear {
    pub off: bool,
    pub password: bool,
    pub ssh_keys: bool,
}

/// `--ci-*`에서 파일·표준 입력·프롬프트로 읽은 값
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CloudInitInput {
    pub password: Option<String>,
    pub ssh_keys: Vec<String>,
    pub user_data: Option<String>,
    pub vendor_data: Option<String>,
}

impl CloudInitArgs {
    /// cloud-init 값을 하나라도 줬는지(주면 켠다)
    pub(crate) fn wanted(&self) -> bool {
        *self != Self::default()
    }

    /// 파일·표준 입력·프롬프트에서 값을 읽는다.
    pub(crate) fn read(&self) -> anyhow::Result<CloudInitInput> {
        let password = if let Some(file) = &self.ci_password_file {
            Some(first_line(file)?)
        } else if self.ci_password_prompt {
            let first = rpassword::prompt_password("cloud-init password: ")?;
            let again = rpassword::prompt_password("cloud-init password again: ")?;
            if first != again {
                bail!("the passwords do not match");
            }
            Some(first)
        } else if let Some(hash) = &self.ci_password_hash {
            if hash.starts_with("$y$") || hash.starts_with("$2b$") {
                bail!(
                    "--ci-password-hash: cloud-init cannot use yescrypt/bcrypt-2b hashes; use a SHA-512 crypt \
                     hash ($6$, e.g. `mkpasswd -m sha-512`)"
                );
            }
            if !is_crypt_hash(hash) {
                bail!("--ci-password-hash is not a crypt hash ($6$…, $5$…, $1$…, $2a$… or $2y$…)");
            }
            Some(hash.clone())
        } else {
            None
        };
        let mut ssh_keys = Vec::new();
        for key in &self.ci_ssh_keys {
            match key.strip_prefix('@') {
                Some(file) => {
                    let path = home_path(file);
                    let text = std::fs::read_to_string(&path)
                        .with_context(|| format!("cannot read {}", path.display()))?;
                    ssh_keys.extend(key_lines(&text));
                }
                None => ssh_keys.push(key.trim().to_string()),
            }
        }
        let read = |file: &Option<PathBuf>| -> anyhow::Result<Option<String>> {
            file.as_ref()
                .map(|f| {
                    std::fs::read_to_string(f)
                        .with_context(|| format!("cannot read {}", f.display()))
                })
                .transpose()
        };
        Ok(CloudInitInput {
            password,
            ssh_keys,
            user_data: read(&self.ci_user_data)?,
            vendor_data: read(&self.ci_vendor_data)?,
        })
    }
}

/// 파일의 첫 줄(`-`는 표준 입력). 비밀번호 파일에 쓴다.
fn first_line(file: &Path) -> anyhow::Result<String> {
    let mut line = String::new();
    if file == Path::new("-") {
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .context("cannot read the password from standard input")?;
    } else {
        let text = std::fs::read_to_string(file)
            .with_context(|| format!("cannot read {}", file.display()))?;
        line = text.lines().next().unwrap_or_default().to_string();
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// `~/`로 시작하면 홈 디렉터리 아래(셸이 `@~/…`를 펼치지 않으므로)
fn home_path(file: &str) -> PathBuf {
    match (file.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(file),
    }
}

/// 키 파일의 키들: 줄마다 하나, 빈 줄과 `#` 주석 줄은 건너뛴다.
pub(crate) fn key_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// 새 `cloud_init`(Null이면 끔): 지금 값 `current`(없으면 Null)에 준 값만 바꾼다. 아무것도 주지 않았으면 그대로다.
/// 비밀번호를 바꾸지 않으면 받은 값(가린 `********`)을 그대로 두어 서버가 저장된 해시를 지킨다. SSH 키는 지금 키
/// 뒤에 더하고(같은 키는 한 번), NIC 주소는 그 NIC의 칸만 바꾼다.
pub(crate) fn cloud_init_update(
    current: &Value,
    args: &CloudInitArgs,
    input: &CloudInitInput,
    clear: CloudInitClear,
) -> Value {
    if clear.off {
        return Value::Null;
    }
    if !args.wanted() && (current.is_null() || !(clear.password || clear.ssh_keys)) {
        return current.clone();
    }
    let mut ci: Map<String, Value> = current.as_object().cloned().unwrap_or_default();
    let mut set = |key: &str, value: Option<Value>| {
        if let Some(v) = value {
            ci.insert(key.into(), v);
        }
    };
    set("hostname", args.ci_hostname.as_ref().map(|h| json!(h)));
    set("user", args.ci_user.as_ref().map(|u| json!(u)));
    set("password", input.password.as_ref().map(|p| json!(p)));
    set("user_data", input.user_data.as_ref().map(|d| json!(d)));
    set("vendor_data", input.vendor_data.as_ref().map(|d| json!(d)));
    if clear.password {
        ci.remove("password");
    }
    let mut keys: Vec<Value> = if clear.ssh_keys {
        Vec::new()
    } else {
        ci.get("ssh_keys")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for key in &input.ssh_keys {
        if !keys.iter().any(|k| k == key.as_str()) {
            keys.push(json!(key));
        }
    }
    if keys.is_empty() {
        ci.remove("ssh_keys");
    } else {
        ci.insert("ssh_keys".into(), Value::Array(keys));
    }
    let mut networks = ci
        .get("networks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (field, list) in [("ipv4", &args.ci_ip), ("ipv6", &args.ci_ip6)] {
        for (k, (nic, config)) in list.iter().enumerate() {
            let i = nic.unwrap_or(k);
            if networks.len() <= i {
                networks.resize(i + 1, json!({}));
            }
            networks[i][field] = serde_json::to_value(config).expect("IpConfig is JSON");
        }
    }
    if !networks.is_empty() {
        ci.insert("networks".into(), Value::Array(networks));
    }
    if !args.ci_dns.is_empty() || !args.ci_search.is_empty() {
        let mut dns = ci.get("dns").cloned().unwrap_or_else(|| json!({}));
        if !args.ci_dns.is_empty() {
            dns["servers"] = json!(args.ci_dns);
        }
        if !args.ci_search.is_empty() {
            dns["search"] = json!(args.ci_search);
        }
        ci.insert("dns".into(), dns);
    }
    Value::Object(ci)
}

/// `vm get`의 CD-ROM 줄: `cdroms:` 뒤에 드라이브마다 `  cdrom0: ubuntu-24-04 (boot 2)`(빈 드라이브는 `empty`).
/// 실행 중 기록의 매체가 스펙과 다르면(바꾸는 중이거나 실패) `<- changing from X`를 붙인다. 드라이브가 없으면 빈다.
pub(crate) fn cdrom_lines(vm: &Value) -> Vec<String> {
    let cdroms = vm["spec"]["cdroms"].as_array().cloned().unwrap_or_default();
    if cdroms.is_empty() {
        return Vec::new();
    }
    let (_, boot) = boot_orders(&vm["spec"]);
    let running = vm["status"]["hardware"]["cdroms"].as_array();
    let medium = |c: &Value| c["iso"].as_str().unwrap_or("empty").to_string();
    let mut lines = vec!["cdroms:".to_string()];
    for (i, c) in cdroms.iter().enumerate() {
        let mut line = format!("  cdrom{i}: {}", medium(c));
        if let Some(n) = boot.get(i).copied().flatten() {
            line.push_str(&format!(" (boot {n})"));
        }
        if let Some(hw) = running.and_then(|r| r.get(i))
            && hw["iso"] != c["iso"]
        {
            line.push_str(&format!(" <- changing from {}", medium(hw)));
        }
        lines.push(line);
    }
    lines
}

/// 스펙의 실제 부팅 순서(`effective_boot`): (디스크, CD-ROM)
fn boot_orders(spec: &Value) -> BootOrders {
    let orders = |key: &str| -> Vec<Option<u8>> {
        spec[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| d["boot_order"].as_u64().and_then(|n| u8::try_from(n).ok()))
            .collect()
    };
    effective_boot(&orders("disks"), &orders("cdroms"))
}

/// `vm get`의 부팅 순서 줄: `boot: disk0, cdrom0`(실제 순서). 부팅할 장치가 없으면 None이다.
pub(crate) fn boot_line(vm: &Value) -> Option<String> {
    let (disks, cdroms) = boot_orders(&vm["spec"]);
    let mut devices: Vec<(u8, String)> = disks
        .iter()
        .enumerate()
        .filter_map(|(i, o)| o.map(|o| (o, format!("disk{i}"))))
        .chain(
            cdroms
                .iter()
                .enumerate()
                .filter_map(|(i, o)| o.map(|o| (o, format!("cdrom{i}")))),
        )
        .collect();
    if devices.is_empty() {
        return None;
    }
    devices.sort();
    let names: Vec<String> = devices.into_iter().map(|(_, name)| name).collect();
    Some(format!("boot: {}", names.join(", ")))
}

/// 주소 설정 하나(`dhcp`, `slaac`, `static 10.0.0.5/24 gw 10.0.0.1`)
fn ip_text(config: &Value) -> String {
    match config["mode"].as_str() {
        Some("static") => {
            let mut s = format!("static {}", cell(&config["address"]));
            if let Some(gw) = config["gateway"].as_str() {
                s.push_str(&format!(" gw {gw}"));
            }
            s
        }
        _ => cell(&config["mode"]),
    }
}

/// `vm get`의 cloud-init 줄: `cloud-init: user orbi, 2 ssh keys, password set, nic0 static 10.0.0.5/24 gw 10.0.0.1,
/// instance iid-…`. `user_data`가 사용자·비밀번호·키를 덮으면 경고 줄을 더한다. cloud-init이 없으면 빈다.
pub(crate) fn cloud_init_lines(vm: &Value) -> Vec<String> {
    let ci = &vm["spec"]["cloud_init"];
    if ci.is_null() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    if let Some(h) = ci["hostname"].as_str() {
        parts.push(format!("hostname {h}"));
    }
    if let Some(u) = ci["user"].as_str() {
        parts.push(format!("user {u}"));
    }
    let keys = ci["ssh_keys"].as_array().map_or(0, Vec::len);
    match keys {
        0 => {}
        1 => parts.push("1 ssh key".into()),
        n => parts.push(format!("{n} ssh keys")),
    }
    if !ci["password"].is_null() {
        parts.push("password set".into());
    }
    for (i, net) in ci["networks"].as_array().into_iter().flatten().enumerate() {
        // IPv6만 준 NIC에는 IPv4가 없다(빈 항목은 DHCP).
        let mut s = match (net["ipv4"].is_null(), net["ipv6"].is_null()) {
            (true, false) => format!("nic{i} no ipv4"),
            (true, true) => format!("nic{i} dhcp"),
            (false, _) => format!("nic{i} {}", ip_text(&net["ipv4"])),
        };
        if !net["ipv6"].is_null() {
            s.push_str(&format!(" ipv6 {}", ip_text(&net["ipv6"])));
        }
        parts.push(s);
    }
    let list = |v: &Value| {
        v.as_array()
            .into_iter()
            .flatten()
            .map(cell)
            .collect::<Vec<_>>()
            .join(" ")
    };
    if ci["dns"]["servers"].is_array() {
        parts.push(format!("dns {}", list(&ci["dns"]["servers"])));
    }
    if ci["dns"]["search"].is_array() {
        parts.push(format!("search {}", list(&ci["dns"]["search"])));
    }
    if !ci["user_data"].is_null() {
        parts.push("own user-data".into());
    }
    if !ci["vendor_data"].is_null() {
        parts.push("vendor-data".into());
    }
    if let Some(id) = vm["status"]["hardware"]["cloud_init"].as_str() {
        parts.push(format!("instance {id}"));
    }
    let mut lines = vec![if parts.is_empty() {
        "cloud-init: on (distribution defaults)".to_string()
    } else {
        format!("cloud-init: {}", parts.join(", "))
    }];
    let generated = !ci["user"].is_null() || !ci["password"].is_null() || keys > 0;
    if !ci["user_data"].is_null() && generated {
        lines.push(
            "  warning: cloud_init.user_data replaces the generated user-data; user, password and ssh_keys are not used"
                .into(),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cloud_init_addresses() {
        assert_eq!(
            parse_ci_ip("nic1=10.0.0.5/24,gw=10.0.0.1", false),
            Ok((
                Some(1),
                IpConfig::Static {
                    address: "10.0.0.5/24".into(),
                    gateway: Some("10.0.0.1".into())
                }
            ))
        );
        assert_eq!(parse_ci_ip("dhcp", false), Ok((None, IpConfig::Dhcp)));
        assert_eq!(parse_ci_ip("slaac", true), Ok((None, IpConfig::Slaac)));
        assert_eq!(
            parse_ci_ip("nic0=2001:db8::5/64", true),
            Ok((
                Some(0),
                IpConfig::Static {
                    address: "2001:db8::5/64".into(),
                    gateway: None
                }
            ))
        );
        assert_eq!(
            parse_ci_ip("slaac", false).unwrap_err(),
            "--ci-ip \"slaac\": slaac is only for --ci-ip6"
        );
        assert_eq!(
            parse_ci_ip("2001:db8::5/64", false).unwrap_err(),
            "--ci-ip \"2001:db8::5/64\": 2001:db8::5/64 is not an IPv4 address with a prefix length"
        );
        assert_eq!(
            parse_ci_ip("10.0.0.5/33", false).unwrap_err(),
            "--ci-ip \"10.0.0.5/33\": 10.0.0.5/33 is not an IPv4 address with a prefix length"
        );
        assert_eq!(
            parse_ci_ip("10.0.0.5/24,gw=fe80::1", false).unwrap_err(),
            "--ci-ip \"10.0.0.5/24,gw=fe80::1\": gateway fe80::1 is not an IPv4 address"
        );
        for bad in ["10.0.0.5", "nicx=dhcp", "10.0.0.5/24,via=1.2.3.4", "static"] {
            assert_eq!(
                parse_ci_ip(bad, false).unwrap_err(),
                format!("invalid --ci-ip {bad:?}: use [nicN=]dhcp or ADDRESS/PREFIX[,gw=GATEWAY]")
            );
        }
        assert!(
            parse_ci_ip("x", true)
                .unwrap_err()
                .contains("use [nicN=]dhcp, slaac or")
        );
    }

    #[test]
    fn parses_boot_lists() {
        assert_eq!(
            parse_boot("cdrom,disk", 1, 1),
            Ok((vec![Some(2)], vec![Some(1)]))
        );
        assert_eq!(
            parse_boot("disk1, cdrom0", 2, 2),
            Ok((vec![None, Some(1)], vec![Some(2), None]))
        );
        assert_eq!(
            parse_boot("disk1", 1, 0).unwrap_err(),
            "the VM has no disk1"
        );
        assert_eq!(
            parse_boot("cdrom", 1, 0).unwrap_err(),
            "the VM has no cdrom0"
        );
        assert_eq!(
            parse_boot("disk,disk0", 1, 0).unwrap_err(),
            "disk0 is listed twice in --boot"
        );
        assert_eq!(
            parse_boot("net0", 1, 0).unwrap_err(),
            "invalid boot device \"net0\": use diskN or cdromN"
        );
        assert_eq!(
            parse_boot("diskx", 1, 0).unwrap_err(),
            "invalid boot device \"diskx\": use diskN or cdromN"
        );
    }

    /// `--boot`는 목록에 없는 장치의 `boot_order`를 뺀다.
    #[test]
    fn applies_boot_to_a_spec() {
        let mut spec = json!({
            "disks": [{ "volume": "root", "boot_order": 1 }, { "volume": "data" }],
            "cdroms": [{ "iso": "inst" }],
        });
        apply_boot(&mut spec, "cdrom0,disk1").unwrap();
        assert_eq!(
            spec,
            json!({
                "disks": [{ "volume": "root" }, { "volume": "data", "boot_order": 2 }],
                "cdroms": [{ "iso": "inst", "boot_order": 1 }],
            })
        );
        assert!(apply_boot(&mut spec, "cdrom1").is_err());
    }

    #[test]
    fn cdrom_values() {
        assert_eq!(
            cdroms_value(&["inst".into(), "none".into()]),
            [json!({ "iso": "inst" }), json!({})]
        );
        assert_eq!(cdrom_value(None, Some(1)), json!({ "boot_order": 1 }));
    }

    /// `@FILE`의 키: 빈 줄과 주석 줄은 건너뛴다.
    #[test]
    fn reads_ssh_keys_from_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("keys");
        std::fs::write(
            &file,
            "# laptop\nssh-ed25519 AAAA a@b\n\n  ssh-rsa BBBB c@d  \n#ssh-rsa old\n",
        )
        .unwrap();
        let args = CloudInitArgs {
            ci_ssh_keys: vec![
                format!("@{}", file.display()),
                "ssh-ed25519 CCCC e@f".into(),
            ],
            ..Default::default()
        };
        assert_eq!(
            args.read().unwrap().ssh_keys,
            [
                "ssh-ed25519 AAAA a@b",
                "ssh-rsa BBBB c@d",
                "ssh-ed25519 CCCC e@f"
            ]
        );
        let missing = CloudInitArgs {
            ci_ssh_keys: vec!["@/nonexistent/keys".into()],
            ..Default::default()
        };
        assert_eq!(
            missing.read().unwrap_err().to_string(),
            "cannot read /nonexistent/keys"
        );
    }

    /// 비밀번호 파일은 첫 줄이고, 해시는 crypt 해시여야 한다.
    #[test]
    fn reads_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pw");
        std::fs::write(&file, "s3cret\nsecond line\n").unwrap();
        let args = CloudInitArgs {
            ci_password_file: Some(file),
            ..Default::default()
        };
        assert_eq!(args.read().unwrap().password.as_deref(), Some("s3cret"));
        let sha512 = opl_types::crypt::sha512_crypt(b"pw", "saltsalt");
        let hash = CloudInitArgs {
            ci_password_hash: Some(sha512.clone()),
            ..Default::default()
        };
        assert_eq!(hash.read().unwrap().password, Some(sha512));
        let bad = |h: &str| {
            CloudInitArgs {
                ci_password_hash: Some(h.into()),
                ..Default::default()
            }
            .read()
            .unwrap_err()
            .to_string()
        };
        assert!(bad("plain").contains("is not a crypt hash"));
        assert!(bad("$6$salt$abc").contains("is not a crypt hash"));
        // cloud-init이 평문으로 설정해 버리는 해시
        assert!(bad("$y$j9T$salt$hash").contains("cannot use yescrypt/bcrypt-2b"));
        assert!(
            bad("$2b$10$N9qo8uLOickgx2ZMRZoMyeIjZAgcfl7p92ldGxad68LJZdL17lhWy").contains("$6$")
        );
    }

    #[test]
    fn builds_a_new_cloud_init() {
        let args = CloudInitArgs {
            ci_hostname: Some("web.example.com".into()),
            ci_user: Some("orbi".into()),
            ci_ip: vec![(
                Some(1),
                IpConfig::Static {
                    address: "10.0.0.5/24".into(),
                    gateway: Some("10.0.0.1".into()),
                },
            )],
            ci_ip6: vec![(None, IpConfig::Slaac)],
            ci_dns: vec!["10.0.0.2".parse().unwrap()],
            ci_search: vec!["example.com".into()],
            ..Default::default()
        };
        let input = CloudInitInput {
            password: Some("pw".into()),
            ssh_keys: vec!["ssh-ed25519 AAAA a@b".into()],
            vendor_data: Some("#cloud-config\n{}".into()),
            ..Default::default()
        };
        let ci = cloud_init_update(&Value::Null, &args, &input, CloudInitClear::default());
        assert_eq!(
            ci,
            json!({
                "hostname": "web.example.com",
                "user": "orbi",
                "password": "pw",
                "ssh_keys": ["ssh-ed25519 AAAA a@b"],
                "vendor_data": "#cloud-config\n{}",
                "networks": [
                    { "ipv6": { "mode": "slaac" } },
                    { "ipv4": { "mode": "static", "address": "10.0.0.5/24", "gateway": "10.0.0.1" } },
                ],
                "dns": { "servers": ["10.0.0.2"], "search": ["example.com"] },
            })
        );
        // 이름만 켜면 빈 설정(배포판 기본)이다.
        let on = CloudInitArgs {
            cloud_init: true,
            ..Default::default()
        };
        let empty = CloudInitInput::default();
        assert_eq!(
            cloud_init_update(&Value::Null, &on, &empty, CloudInitClear::default()),
            json!({})
        );
        // 아무것도 주지 않으면 그대로다(없는 cloud-init을 지우기 값으로 켜지 않는다).
        let none = CloudInitArgs::default();
        let clear = CloudInitClear {
            password: true,
            ..Default::default()
        };
        assert_eq!(
            cloud_init_update(&Value::Null, &none, &empty, clear),
            Value::Null
        );
    }

    /// `vm set`은 준 값만 바꾼다: 가린 비밀번호(`********`)는 그대로 되보내고, 키는 더하고, 끄기·지우기를 한다.
    #[test]
    fn updates_an_existing_cloud_init() {
        let current = json!({
            "user": "orbi",
            "password": "********",
            "ssh_keys": ["ssh-ed25519 AAAA a@b"],
            "networks": [{ "ipv4": { "mode": "dhcp" } }],
        });
        let args = CloudInitArgs {
            ci_hostname: Some("web2".into()),
            ..Default::default()
        };
        let empty = CloudInitInput::default();
        let ci = cloud_init_update(&current, &args, &empty, CloudInitClear::default());
        assert_eq!(ci["password"], "********");
        assert_eq!(ci["hostname"], "web2");
        assert_eq!(ci["ssh_keys"], current["ssh_keys"]);
        assert_eq!(ci["networks"], current["networks"]);

        let keys = CloudInitInput {
            ssh_keys: vec!["ssh-ed25519 AAAA a@b".into(), "ssh-rsa BBBB".into()],
            ..Default::default()
        };
        let none = CloudInitArgs::default();
        // 인자를 하나도 주지 않으면 그대로다. `--ci-ssh-key`를 주면 지금 키 뒤에 더한다(같은 키는 한 번).
        assert_eq!(
            cloud_init_update(&current, &none, &keys, CloudInitClear::default()),
            current
        );
        let with_keys = CloudInitArgs {
            ci_ssh_keys: vec!["x".into()],
            ..Default::default()
        };
        let ci = cloud_init_update(&current, &with_keys, &keys, CloudInitClear::default());
        assert_eq!(
            ci["ssh_keys"],
            json!(["ssh-ed25519 AAAA a@b", "ssh-rsa BBBB"])
        );

        let clear = CloudInitClear {
            password: true,
            ssh_keys: true,
            ..Default::default()
        };
        let ci = cloud_init_update(&current, &none, &empty, clear);
        assert_eq!(
            ci,
            json!({ "user": "orbi", "networks": [{ "ipv4": { "mode": "dhcp" } }] })
        );
        let off = CloudInitClear {
            off: true,
            ..Default::default()
        };
        assert_eq!(cloud_init_update(&current, &none, &empty, off), Value::Null);
        // 둘째 NIC의 IPv4만 바꾸면 첫째는 그대로다.
        let second = CloudInitArgs {
            ci_ip: vec![(Some(1), IpConfig::Dhcp)],
            ..Default::default()
        };
        let ci = cloud_init_update(&current, &second, &empty, CloudInitClear::default());
        assert_eq!(
            ci["networks"],
            json!([{ "ipv4": { "mode": "dhcp" } }, { "ipv4": { "mode": "dhcp" } }])
        );
    }

    #[test]
    fn vm_get_lines() {
        let vm = json!({
            "spec": {
                "disks": [{ "volume": "root", "boot_order": 1 }],
                "cdroms": [{ "iso": "debian" }, {}],
                "cloud_init": {
                    "user": "orbi",
                    "password": "********",
                    "ssh_keys": ["ssh-ed25519 A", "ssh-ed25519 B"],
                    "networks": [{ "ipv4": { "mode": "static", "address": "10.0.0.5/24", "gateway": "10.0.0.1" } }],
                },
            },
            "status": { "hardware": {
                "cdroms": [{ "iso": "ubuntu-24-04" }, {}],
                "cloud_init": "iid-0123456789abcdef",
            } },
        });
        assert_eq!(
            cdrom_lines(&vm),
            [
                "cdroms:",
                "  cdrom0: debian (boot 2) <- changing from ubuntu-24-04",
                "  cdrom1: empty (boot 3)",
            ]
        );
        assert_eq!(
            boot_line(&vm).as_deref(),
            Some("boot: disk0, cdrom0, cdrom1")
        );
        assert_eq!(
            cloud_init_lines(&vm),
            [
                "cloud-init: user orbi, 2 ssh keys, password set, nic0 static 10.0.0.5/24 gw 10.0.0.1, instance iid-0123456789abcdef"
            ]
        );

        // `--boot cdrom,disk`: CD가 먼저다.
        let vm = json!({ "spec": {
            "disks": [{ "volume": "root", "boot_order": 2 }],
            "cdroms": [{ "iso": "inst", "boot_order": 1 }],
            "cloud_init": { "user": "orbi", "user_data": "#cloud-config\n{}", "networks": [{ "ipv6": { "mode": "slaac" } }] },
        } });
        assert_eq!(cdrom_lines(&vm), ["cdroms:", "  cdrom0: inst (boot 1)"]);
        assert_eq!(boot_line(&vm).as_deref(), Some("boot: cdrom0, disk0"));
        // `--boot cdrom`: 순서가 CD에만 있으면 디스크가 그 뒤에 온다(매체를 뺀 뒤 디스크로 켜짐).
        let only_cd = json!({ "spec": {
            "disks": [{ "volume": "root" }, { "volume": "data" }],
            "cdroms": [{ "iso": "inst", "boot_order": 1 }],
        } });
        assert_eq!(
            boot_line(&only_cd).as_deref(),
            Some("boot: cdrom0, disk0, disk1")
        );
        assert_eq!(
            cloud_init_lines(&vm),
            [
                "cloud-init: user orbi, nic0 no ipv4 ipv6 slaac, own user-data",
                "  warning: cloud_init.user_data replaces the generated user-data; user, password and ssh_keys are not used",
            ]
        );

        // CD도 cloud-init도 없는 VM은 줄이 없고, 부팅 순서는 그대로다.
        let vm = json!({ "spec": { "disks": [{ "volume": "root", "boot_order": 1 }] } });
        assert!(cdrom_lines(&vm).is_empty());
        assert!(cloud_init_lines(&vm).is_empty());
        assert_eq!(boot_line(&vm).as_deref(), Some("boot: disk0"));
        assert_eq!(boot_line(&json!({ "spec": {} })), None);
        let vm = json!({ "spec": { "cloud_init": {} } });
        assert_eq!(
            cloud_init_lines(&vm),
            ["cloud-init: on (distribution defaults)"]
        );
    }

    #[test]
    fn iso_list_rows() {
        let volumes = [
            json!({ "name": "inst", "spec": { "pool": "nfs", "size_bytes": 1u64 << 30, "content": "iso" }, "status": { "node": "n1" } }),
            json!({ "name": "root", "spec": { "pool": "nfs", "size_bytes": 1024 } }),
            json!({ "name": "local-iso", "spec": { "pool": "local", "size_bytes": 2048, "content": "iso" }, "status": { "node": "n1" } }),
        ];
        let vms = [
            json!({ "name": "web", "spec": { "cdroms": [{ "iso": "inst" }] } }),
            json!({ "name": "db", "spec": { "cdroms": [{}] }, "status": { "hardware": { "cdroms": [{ "iso": "inst" }] } } }),
            json!({ "name": "app", "spec": {} }),
        ];
        let nodes = [json!({ "id": "n1", "name": "node1" })];
        let pools = [
            json!({ "name": "nfs", "spec": { "backend": { "kind": "nfs" } } }),
            json!({ "name": "local", "spec": { "backend": { "kind": "dir" } } }),
        ];
        assert_eq!(
            iso_rows(&volumes, &vms, &nodes, &pools, None),
            [
                ["inst", "nfs", "1.0GiB", "-", "web, db"],
                ["local-iso", "local", "2.0KiB", "node1", "-"],
            ]
        );
        assert_eq!(
            iso_rows(&volumes, &vms, &nodes, &pools, Some("local")).len(),
            1
        );
    }

    #[test]
    fn download_progress_lines() {
        let mut shown = None;
        let p = |stage: &str, done: u64, total: u64| json!({ "stage": stage, "done_bytes": done, "total_bytes": total });
        let g = 1u64 << 30;
        assert_eq!(
            download_progress(&p("downloading", 0, 10 * g), &mut shown).as_deref(),
            Some("downloading 0.0 GiB / 10.0 GiB")
        );
        assert_eq!(
            download_progress(&p("downloading", g / 4, 10 * g), &mut shown),
            None
        );
        assert_eq!(
            download_progress(&p("downloading", g, 10 * g), &mut shown).as_deref(),
            Some("downloading 1.0 GiB / 10.0 GiB")
        );
        assert_eq!(
            download_progress(&p("verifying", 10 * g, 10 * g), &mut shown).as_deref(),
            Some("verifying")
        );
        assert_eq!(
            download_progress(&p("verifying", 10 * g, 10 * g), &mut shown),
            None
        );
        assert_eq!(
            download_progress(&p("importing", 0, 10 * g), &mut shown).as_deref(),
            Some("importing")
        );
        // 크기를 모르면 256 MiB마다
        let mut shown = None;
        assert!(download_progress(&p("downloading", 0, 0), &mut shown).is_some());
        assert_eq!(
            download_progress(&p("downloading", 100 << 20, 0), &mut shown),
            None
        );
        assert_eq!(
            download_progress(&p("downloading", 300 << 20, 0), &mut shown).as_deref(),
            Some("downloading 0.3 GiB")
        );
        assert_eq!(download_progress(&Value::Null, &mut shown), None);
    }
}
