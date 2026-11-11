# Orbivisor 계획 45: 실행기 격리와 systemd watchdog 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** API 부하, 작업, 느린 디스크가 Raft 하트비트·HA 리스 갱신·watchdog 쓰다듬기를 늦춰 멀쩡한 노드가 펜싱되는 일을 막고, 데몬이 멈추면 systemd가 데몬만 다시 띄우게 한다.

**Architecture:**
- **실행기 셋:** `core`(Raft·피어 서버·HA, 2스레드), `api`(axum), `work`(VM·작업·지표 등)를 따로 둔다. 실행기 사이 호출은 지금처럼 `Arc` 객체와 채널이다.
- **전용 OS 스레드:** redb 쓰기(`orbivisor-store`), HA watchdog 쓰다듬기(`orbivisor-watchdog`, 원자값으로 판단), systemd 알림·실행기 확인(`orbivisor-notify`).
- **피어 연결:** Raft·리스·리더 전달은 core에 묶인 채널, 대량 전송은 work에 묶인 채널. 피어 서버의 무거운 처리기는 work에서 돈다.
- **systemd:** `Type=notify`, `WatchdogSec=30s`, `WatchdogSignal=SIGKILL`. core·api가 2초 안에 답하는 회차에만 `WATCHDOG=1`.

**Tech Stack:** Rust 2024(MSRV 1.88), tokio(실행기 여러 개), openraft 0.9.25, redb, tonic 0.14, axum, systemd 알림(직접 유닉스 데이터그램). 새로 받는 크레이트는 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-11-availability-scale-design.md` (§1~§3, §11 "구현(계획 45)", §12)
**선행 계획:** 계획 1~44 (완료)

---

## 범위

| 포함 | 제외 |
|---|---|
| 실행기 셋, `[runtime]` 설정 | 헬스 체크·VIP·세션 공유(계획 46) |
| redb 쓰기 스레드, watchdog 쓰다듬기 스레드, Raft 멈춤 감시 | 투표 멤버 상한·대규모(계획 47·48) |
| 피어 채널의 실행기 고정, 무거운 피어 처리기 work로 | 대량 전송 전용 포트(계획 49 다중 링크에서) |
| systemd `Type=notify`·watchdog, 패키징 | HA 기본값 줄이기(계획 51 빠른 HA) |
| 굶김 시험(`starve-it`), README "제어부 가용성", 설계 §11 | |

## 실행 환경

- **이 Mac에서 하는 것:** 모든 Rust 변경과 시험, opld 실제 프로세스 시험(`starve-it`, `ha-it`, `migration-it`).
- **Linux CI만:** `@` 추상 소켓 코드, Ubuntu 러너에서의 굶김 시험 시간 여유, `deb-test`의 `systemd-analyze verify`.
- **테스트 VM(배포 뒤 확인):** 실제 systemd에서 `READY`·watchdog 재시작, 재시작 시간과 HA 장치 제한의 여유, 정족수 없는 시작(설계 §11 표).

## 설계 결정

- **프로세스를 나누지 않고 실행기를 나눈다:** 상태를 한 프로세스가 갖는 지금 구조를 지키면서 서로 굶기지 않게 한다(설계 §1.1).
- **work가 늦으면 데몬을 죽이지 않는다:** systemd가 데몬을 죽이면 HA VM이 있는 노드는 새 프로세스가 장치 제한 안에 다시 arm해야 한다. work 정체는 HA와 무관하므로 경고만 남긴다.
- **openraft는 자기 로그 쓰기를 기다린다:** 저장소 스레드는 실행기 스레드를 막지 않게 할 뿐, 느린 디스크의 노드(리더면 클러스터)의 Raft는 여전히 멈칫한다(설계 §11).

## 파일 구조

```
crates/opld/{Cargo.toml, src/{main.rs, member.rs, config.rs, control.rs, runtime.rs, notify.rs, starve.rs}, tests/{common/mod.rs, notify.rs, starve.rs}}
crates/opl-cluster/src/{cluster.rs, lib.rs, log_store.rs, state_machine.rs, store.rs, writer.rs}
crates/opl-peer/{Cargo.toml, src/{client.rs, server.rs}, tests/grpc.rs}
crates/opl-reconciler/{src/ha.rs, tests/migration.rs}
crates/opl-api/tests/{vm_imports.rs, backup_tasks.rs, common/fake_apt.rs}
packaging/deb/{opld.service, opld.toml.example}, packaging/tests/build-deb-test.sh
.github/workflows/ci.yml, README.md, docs/superpowers/specs/2026-11-11-availability-scale-design.md
```

## 편집 블록 읽는 법

코드는 Task마다 세 가지 모양으로 싣는다.
- "`파일`을 만든다": 전체 내용이다.
- "`파일` 전체": 바꾼 뒤의 전체 내용이다.
- "`파일`에서 … 을 다음으로 바꾼다": 앞 블록을 찾아 뒤 블록으로 바꾼다.

바꾸는 블록은 위에서부터 차례로 적용한다. 찾을 코드는 그 시점의 파일에서 줄 단위로 한 번만 나온다.

---

### Task 1: 실행기 셋

`main`이 `#[tokio::main]` 대신 실행기 셋(`orbivisor-core` 2개 고정, `orbivisor-api`, `orbivisor-work` — 기본 CPU 수의 절반, 최소 2)을 만들고 주 루프를 `core.block_on`으로 돌린다. 설정 `[runtime] api_threads`·`work_threads`. `Member::start`·`start_witness`가 모든 서비스를 실행기를 밝혀 spawn한다(core: `Cluster::open`과 openraft, 피어 서버, `HaGuard`, `HaManager`, `NodeMonitor`; api: TLS 받기와 axum; work: 하이퍼바이저, `Reconciler`, `TaskRunner`, 풀, 지표, 스케줄러, 정리, 백업, 원격, 업데이트, 방화벽·VXLAN, 노드 등록). `runtime::on`·`run_on` 도우미와 배치 시험.

**Files:**
- Create: `crates/opld/src/runtime.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/src/config.rs`에서

```rust
    pub remotes: RemotesSection,
}
```

을 다음으로 바꾼다:

```rust
    pub remotes: RemotesSection,
    #[serde(default)]
    pub runtime: RuntimeSection,
}
```

`crates/opld/src/config.rs`에서

```rust

/// 감사 로그(`{data_dir}/audit/audit.log`). API를 여는 노드만 쓴다.
```

을 다음으로 바꾼다:

```rust

/// 실행기 크기(설계 §3.1). `core` 실행기는 2로 고정이다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSection {
    /// API(axum) 실행기의 작업자 스레드 수. 기본은 CPU 수의 절반(최소 2)이다.
    pub api_threads: Option<usize>,
    /// reconciler·Task 실행기·스케줄러 등의 실행기의 작업자 스레드 수. 기본은 CPU 수의 절반(최소 2)이다.
    pub work_threads: Option<usize>,
}

impl RuntimeSection {
    pub fn api_threads(&self) -> usize {
        self.api_threads.unwrap_or_else(default_runtime_threads)
    }

    pub fn work_threads(&self) -> usize {
        self.work_threads.unwrap_or_else(default_runtime_threads)
    }
}

/// CPU 수의 절반, 최소 2
fn default_runtime_threads() -> usize {
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cpus / 2).max(2)
}

/// 감사 로그(`{data_dir}/audit/audit.log`). API를 여는 노드만 쓴다.
```

`crates/opld/src/config.rs`에서

```rust
                "audit.max_file_mib must be between 1 and 256 and audit.max_files at least 1"
            );
        }
        if self.ha.fence_after_secs == 0 {
```

을 다음으로 바꾼다:

```rust
                "audit.max_file_mib must be between 1 and 256 and audit.max_files at least 1"
            );
        }
        if self.runtime.api_threads == Some(0) || self.runtime.work_threads == Some(0) {
            anyhow::bail!("runtime.api_threads and runtime.work_threads must be at least 1");
        }
        if self.ha.fence_after_secs == 0 {
```

`crates/opld/src/config.rs`에서

```rust
    #[test]
    fn telemetry_section_defaults_and_overrides() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn runtime_section_defaults_to_half_the_cpus_and_rejects_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        assert_eq!(c.runtime, RuntimeSection::default());
        let cpus = std::thread::available_parallelism().unwrap().get();
        assert_eq!(c.runtime.api_threads(), (cpus / 2).max(2));
        assert_eq!(c.runtime.work_threads(), (cpus / 2).max(2));

        let c =
            parse("name = \"n\"\naddress = \"h\"\n[runtime]\napi_threads = 1\nwork_threads = 6");
        c.validate().unwrap();
        assert_eq!((c.runtime.api_threads(), c.runtime.work_threads()), (1, 6));

        for bad in ["api_threads = 0", "work_threads = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[runtime]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        // core는 고정이다.
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[runtime]\ncore_threads = 4")
                .is_err()
        );
    }

    #[test]
    fn telemetry_section_defaults_and_overrides() {
```

`crates/opld/src/main.rs` 전체:

```rust
//! Orbivisor 노드 데몬. 클러스터 저장소, 노드 간 통신, reconciler, Task 실행기, REST API를 한 프로세스로
//! 묶는다.
//!
//! 노드 인증서(`{data_dir}/pki`)가 없으면 아직 클러스터에 속하지 않은 노드다. 이때는 제어 소켓만 열고
//! `oplctl cluster init`(새 클러스터)이나 `oplctl cluster join`(합류)을 기다린다. 멤버가 되면 모든
//! 것을 띄우고, `oplctl cluster leave`로 빠지면 다시 기다린다.
//!
//! 위트니스(설정 `witness = true`)는 VM을 돌리지 않는 Raft 투표 멤버다. 합류만 할 수 있고, 멤버가
//! 되면 클러스터 저장소와 노드 간 서버만 띄운다.
//!
//! QEMU는 `-daemonize`로 떠 있으므로 opld가 멈추거나 죽어도 VM은 계속 돈다. 다시 시작하면
//! 하이퍼바이저 드라이버가 살아 있는 VM에 다시 붙고 reconciler가 이어서 맞춘다.

mod bootstrap;
mod config;
mod control;
mod firewall;
mod host;
mod join;
mod member;
mod metrics;
mod overlay;
mod pools;
mod runtime;
mod web;

use std::fs::File;
use std::io::IsTerminal;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use opl_api::cluster::JoinUrl;
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use serde_json::json;
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::config::Config;
use crate::control::{ControlError, Request, Status};
use crate::member::{Bootstrap, Member};
use crate::runtime::{Handles, Runtimes};

#[derive(Parser)]
#[command(
    name = "orbivisord",
    version = opl_types::version::BUILD,
    about = "Orbivisor node daemon"
)]
struct Args {
    #[arg(long, default_value = "/etc/opl/opld.toml")]
    config: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,openraft=warn")),
        )
        // journald나 파일로 갈 때는 색 코드를 넣지 않는다(감사 줄을 그대로 전달할 수 있게).
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    // 실행기 셋(설계 §3.1). 주 루프는 주 스레드에서 `core`의 문맥으로 돌고(제어 소켓도 `core`),
    // 서비스는 `Member::start`가 맞는 실행기에 띄운다.
    let runtimes = Runtimes::new(config.runtime.api_threads(), config.runtime.work_threads())
        .context("cannot start the runtimes")?;
    let rt = runtimes.handles();
    runtimes.core.block_on(run(config, rt))
}

async fn run(config: Config, rt: Handles) -> anyhow::Result<()> {
    prepare_data_dir(&config.data_dir)?;
    // 끝날 때까지 쥐고 있는다.
    let _lock = lock_data_dir(&config.data_dir)?;
    let node = bootstrap::node_id(&config.data_dir)?;
    let status = Arc::new(Mutex::new(Status {
        node,
        name: config.name.clone(),
        state: "uninitialized",
        cluster: None,
    }));
    let (requests, mut control) = mpsc::channel(4);
    let control_task = control::serve(&config.control_socket(), requests, status.clone()).await?;
    let mut shutdown = pin!(shutdown_signal());

    let result = async {
        'node: loop {
            // 멤버가 아니면 init이나 join을 기다린다.
            let mut member = match NodeTls::load(&member::pki_dir(&config))? {
                Some(tls) => {
                    Member::start(&config, node, tls, Bootstrap::Restart, &rt)
                        .await?
                        .0
                }
                None => {
                    tracing::info!(
                        socket = %config.control_socket().display(),
                        "this node is not in a cluster; waiting for `orbivisorctl cluster init` or `orbivisorctl cluster join`"
                    );
                    loop {
                        let req = tokio::select! {
                            () = &mut shutdown => return anyhow::Ok(()),
                            req = control.recv() => req,
                        };
                        let Some(req) = req else { return Ok(()) };
                        if let Some(member) = become_member(&config, node, req, &rt).await {
                            break member;
                        }
                        // 인증서를 저장한 뒤 실패했으면 이미 멤버(learner)다. 멤버로 다시 띄워 승격을 이어 간다.
                        if member::pki_dir(&config).exists() {
                            continue 'node;
                        }
                    }
                }
            };
            let mut cluster_name = read_cluster_name(&member);
            {
                let mut s = status.lock().unwrap();
                s.state = "member";
                s.cluster = cluster_name.clone();
            }
            // 멤버로 돈다.
            let left = loop {
                tokio::select! {
                    () = &mut shutdown => {
                        tracing::info!("shutting down; running VMs keep running");
                        member.close(false).await?;
                        return Ok(());
                    }
                    e = member.failed() => {
                        let _ = member.close(false).await;
                        return Err(e);
                    }
                    // learner로 다시 시작하면 저장소가 비어 있어 리더에게서 받을 때까지 이름을 모른다.
                    () = tokio::time::sleep(Duration::from_secs(1)), if cluster_name.is_none() => {
                        cluster_name = read_cluster_name(&member);
                        status.lock().unwrap().cluster = cluster_name.clone();
                    }
                    req = control.recv() => match req {
                        Some(Request::Leave { reply }) => match member.leave().await {
                            Ok(()) => break reply,
                            Err(e) => {
                                let _ = reply.send(Err(ControlError::failed(e)));
                            }
                        },
                        Some(Request::Init { reply, .. } | Request::Join { reply, .. }) => {
                            let message = format!(
                                "this node is already a member of cluster {:?}",
                                cluster_name.as_deref().unwrap_or("?")
                            );
                            let _ = reply.send(Err(ControlError::conflict(message)));
                        }
                        None => {
                            member.close(false).await?;
                            return Ok(());
                        }
                    },
                }
            };
            let closed = member.close(true).await;
            {
                let mut s = status.lock().unwrap();
                s.state = "uninitialized";
                s.cluster = None;
            }
            match closed {
                Ok(()) => {
                    tracing::info!("left the cluster");
                    let _ = left.send(Ok(json!({})));
                }
                Err(e) => {
                    let _ = left.send(Err(ControlError::failed(e)));
                }
            }
        }
    }
    .await;
    control_task.abort();
    let _ = std::fs::remove_file(config.control_socket());
    result
}

/// 클러스터 저장소에 기록된 클러스터 이름
fn read_cluster_name(member: &Member) -> Option<String> {
    member
        .cluster()
        .list::<opl_types::cluster::ClusterSpec>()
        .ok()
        .and_then(|(_, l)| l.into_iter().next())
        .map(|c| c.name)
}

/// 미초기화 노드가 받은 요청을 처리한다. 멤버가 되면 그 멤버를 돌려준다.
async fn become_member(config: &Config, node: Uuid, req: Request, rt: &Handles) -> Option<Member> {
    let (bootstrap, tls, reply, cluster) = match req {
        Request::Leave { reply } => {
            let _ = reply.send(Err(ControlError::conflict(
                "this node is not a member of a cluster",
            )));
            return None;
        }
        Request::Init { reply, .. } if config.witness => {
            let _ = reply.send(Err(ControlError::conflict(member::WITNESS_CANNOT_INIT)));
            return None;
        }
        Request::Init {
            cluster_name,
            reply,
        } => {
            match check_ports(config).and_then(|()| new_cluster_tls(config, node, &cluster_name)) {
                Ok((ca, tls)) => (
                    Bootstrap::Init {
                        name: cluster_name.clone(),
                        ca,
                    },
                    tls,
                    reply,
                    cluster_name,
                ),
                Err(e) => {
                    let _ = reply.send(Err(ControlError::failed(e)));
                    return None;
                }
            }
        }
        Request::Join { url, reply } => {
            let joined = async {
                let url: JoinUrl = url.parse().map_err(anyhow::Error::msg)?;
                // 포트가 쓰이고 있으면 조인 토큰을 쓰기 전에 실패한다.
                check_ports(config)?;
                join::request(config, node, &url).await
            };
            match joined.await {
                Ok(joined) => (
                    Bootstrap::Join { via: joined.via },
                    joined.tls,
                    reply,
                    joined.cluster,
                ),
                Err(e) => {
                    let _ = reply.send(Err(ControlError::failed(e)));
                    return None;
                }
            }
        }
    };
    // 인증서가 없는데 클러스터 저장소가 있으면 끝나지 않은 init/join이 남긴 것이다.
    if member::cluster_dir(config).exists() {
        tracing::warn!("removing cluster data left by an unfinished init or join");
        if let Err(e) = member::remove_cluster_data(config) {
            let _ = reply.send(Err(ControlError::failed(e.into())));
            return None;
        }
    }
    match Member::start(config, node, tls, bootstrap, rt).await {
        Ok((member, created)) => {
            let mut answer = json!({ "cluster": cluster, "node": config.name });
            if let Some(c) = created {
                answer["admin_password"] = json!(c.admin_password);
                answer["ca_fingerprint"] = json!(c.ca_fingerprint);
                answer["join_url"] = json!(c.join_url);
                answer["api_address"] = json!(config.api_address());
            }
            let _ = reply.send(Ok(answer));
            Some(member)
        }
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "cannot become a cluster member");
            // 인증서를 저장한 뒤의 실패면(합류해 learner가 된 뒤 승격 실패) 이미 멤버이므로 저장소를
            // 지우지 않는다. main 루프가 멤버로 다시 띄워 승격을 다시 요청한다.
            let e = if member::pki_dir(config).exists() {
                e.context("this node joined as a non-voting member and keeps asking to become a voting member")
            } else {
                let _ = member::remove_cluster_data(config);
                e
            };
            let _ = reply.send(Err(ControlError::failed(e)));
            None
        }
    }
}

/// 데이터 디렉터리를 만들고 주인만 들어갈 수 있게(0700) 한다. 클러스터 저장소에 CA 개인 키와
/// 비밀번호 해시가 있다. 다른 사용자가 가진 디렉터리는 그대로 둔다.
fn prepare_data_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    let meta = std::fs::metadata(dir)?;
    if meta.uid() == rustix::process::geteuid().as_raw() && meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// `{data_dir}/opld.lock`을 배타적으로 잠근다. 같은 데이터 디렉터리로 opld를 둘 띄우지 못하게 한다.
fn lock_data_dir(dir: &Path) -> anyhow::Result<File> {
    let path = dir.join("opld.lock");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(rustix::io::Errno::WOULDBLOCK) => anyhow::bail!(
            "{} is locked; is another orbivisord running with this data directory?",
            path.display()
        ),
        Err(e) => Err(e).with_context(|| format!("cannot lock {}", path.display())),
    }
}

/// API와 노드 간 포트를 잠깐 묶어 본다. 쓰이고 있으면 init/join을 시작하기 전에 실패한다.
/// 위트니스는 API를 열지 않는다.
fn check_ports(config: &Config) -> anyhow::Result<()> {
    let api = (!config.witness).then_some(config.listen);
    for addr in api.into_iter().chain([config.cluster_listen]) {
        std::net::TcpListener::bind(addr).with_context(|| format!("cannot listen on {addr}"))?;
    }
    Ok(())
}

/// 새 클러스터의 CA와 이 노드의 인증서
fn new_cluster_tls(
    config: &Config,
    node: Uuid,
    name: &str,
) -> anyhow::Result<(ClusterCa, NodeTls)> {
    opl_types::resource::validate_name(name)?;
    let ca = ClusterCa::generate(name)?;
    let (key_pem, csr) = node_key_and_csr()?;
    let cert_pem = ca.sign_node(&csr, node, &config.cert_names())?;
    let tls = NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem,
        key_pem,
    };
    Ok((ca, tls))
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn a_witness_refuses_to_start_a_cluster() {
        let dir = tempfile::tempdir().unwrap();
        let mut config: Config =
            toml::from_str("name = \"w\"\naddress = \"127.0.0.1\"\nwitness = true").unwrap();
        config.data_dir = dir.path().to_path_buf();
        let (reply, rx) = oneshot::channel();
        let req = Request::Init {
            cluster_name: "c".into(),
            reply,
        };
        let current = tokio::runtime::Handle::current();
        let rt = Handles {
            core: current.clone(),
            api: current.clone(),
            work: current,
        };
        assert!(
            become_member(&config, Uuid::now_v7(), req, &rt)
                .await
                .is_none()
        );
        let e = rx.await.unwrap().unwrap_err();
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(
            e.message,
            "a witness cannot start a cluster; join an existing cluster instead"
        );
        assert!(!member::pki_dir(&config).exists());
    }
}
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, 사용량 지표, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig,
    IncomingConfig, IncomingMigrations, RemotePoller, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, TelemetryReporter, UefiVarsFetcher, UpdateAgent, Updates,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Typed};
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::firewall::{FirewallAgent, FirewallRules};
use crate::host::Host;
use crate::metrics::host as metrics_host;
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::overlay::{OverlayAgent, cluster_tag};
use crate::pools::PoolMounter;
use crate::runtime::{Handles, on, run_on};

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 데이터 디렉터리 아래 마이그레이션 디스크 기록 파일을 두는 디렉터리(로컬 디스크 마이그레이션 설계 §3.6)
const MIGRATION_DISKS_DIR: &str = "migration-disks";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록과 기능 표시(위트니스는 기능 표시만)를 다시 확인하는 간격. 관리자가 브리지를 만들면
/// 이만큼 뒤에 스케줄러가 알고, 옛 버전 리더가 지운 기능 표시는 이만큼 뒤에 되살아난다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    /// 리더가 백업 작업을 실행한다.
    backups: BackupJobScheduler,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    pools: PoolMounter,
    /// 이 노드와 그 VM의 사용량 지표를 잰다.
    metrics: MetricsSampler,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
    overlay: Option<OverlayAgent>,
    /// VM 방화벽 규칙을 맞춘다(방화벽이 없으면 없다).
    firewall: Option<FirewallAgent>,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
    /// APT 소스 파일과 주기 업데이트 확인(업데이트가 꺼졌으면 없다)
    updates: Option<UpdateAgent>,
    /// 리더가 텔레메트리 보고를 보낸다(설정에서 켰을 때).
    telemetry: TelemetryReporter,
    /// 리더가 원격 클러스터의 상태를 모은다.
    remotes: RemotePoller,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
    audit: Arc<AuditLog>,
}

impl Services {
    /// `left`면 클러스터에서 빠진 것이다(VXLAN 장치와 방화벽 테이블을 지운다).
    async fn shutdown(mut self, left: bool) {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.audit.shutdown().await;
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.backups.shutdown().await;
        self.telemetry.shutdown().await;
        self.remotes.shutdown().await;
        self.recovery.shutdown().await;
        self.pools.shutdown().await;
        self.metrics.shutdown().await;
        if let Some(updates) = self.updates {
            updates.shutdown().await;
        }
        if let Some(overlay) = self.overlay {
            overlay.shutdown(left).await;
        }
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 멈춰야 테이블을 지운 뒤 시작 직전 확인이 다시 넣지 않는다.
        if let Some(firewall) = self.firewall {
            firewall.shutdown(left).await;
        }
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// 서버 태스크가 끝나면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    ///
    /// 서비스를 실행기 셋에 나눠 띄운다(설계 §3.1). Raft(openraft 작업은 `Raft::new`를 부른 실행기에
    /// 생긴다), 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시, 승격은 `rt.core`, API는 `rt.api`, VM·풀을
    /// 다스리는 일과 스케줄러는 `rt.work`에서 돈다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        // `[telemetry] ca_file`을 읽지 못하면 아무것도 열기 전에 실패한다.
        let telemetry_config = config.telemetry.telemetry_config()?;
        let id = NodeId(node);
        let peers = Arc::new(Peers::new(&tls));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let network = GrpcNetwork::new(peers.clone());
        let cluster = Arc::new(
            run_on(&rt.core, Cluster::open(cluster_config, network))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap, rt).await?;
            return Ok((member, None));
        }
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 이 클러스터가 만든 VXLAN 장치의 표시. 남의 관리 이름 브리지는 브리지 목록에서 뺀다.
        let host = cluster_tag(&tls.ca_pem).and_then(|tag| {
            let host = Host::from_config(config, &tag)?;
            Ok((host, tag))
        });
        let (mut host, tag) = match host {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 등록 때 방화벽 여부를 기록하므로 등록 전에 찾는다.
        host.detect_firewall(&tag).await;
        // 워커와 마이그레이션 받기가 VM을 켜기 직전에 규칙을 넣는다. 에이전트는 아래에서 띄운다.
        let firewall = FirewallRules::new(cluster.clone(), config, &host, tag.clone());
        let gate = firewall.clone().map(|f| f as Arc<dyn FirewallGate>);
        // 살아 있는 VM에 다시 붙으며 띄우는 QMP·모니터 작업이 `work`에 생긴다.
        let hv: Arc<dyn Hypervisor> =
            match run_on(&rt.work, QemuHypervisor::new(host.qemu_config(config))).await {
                Ok(hv) => Arc::new(hv),
                Err(e) => {
                    let _ = cluster.shutdown().await;
                    return Err(e.into());
                }
            };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        let (exited_tx, exited) = mpsc::channel(2);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
                migration_disks: config.data_dir.join(MIGRATION_DISKS_DIR),
            },
        );
        // API가 적고, 다른 노드가 노드 간 서버로 읽는다.
        let audit = match config.audit.audit_config(&config.data_dir, &config.name) {
            Some(audit) => {
                let dir = audit.dir.clone();
                match AuditLog::open(audit) {
                    Ok(audit) => Arc::new(audit),
                    Err(e) => {
                        let _ = cluster.shutdown().await;
                        return Err(anyhow::Error::new(e)
                            .context(format!("cannot open the audit log in {}", dir.display())));
                    }
                }
            }
            None => Arc::new(AuditLog::disabled()),
        };
        // 측정기가 적고, API와 다른 노드(노드 간 서버)가 읽는다.
        let metrics_store = Arc::new(MetricsStore::default());
        let metrics_source = Arc::new(StoreSource::new(node, metrics_store.clone()));
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone())
            .with_metrics(metrics_source.clone());
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.mount_root = mount_root.clone();
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            state.audit = audit;
            state.metrics = Some(metrics_source);
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            let api_tls = api_server_config(&tls)?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = on(&rt.core, || spawn_promotion(first_time, &cluster, &peers));
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        // 장치를 만들거나 지우면 곧바로 브리지 목록을 기록한다(아래 주기 갱신을 기다리지 않는다).
        let overlay = on(&rt.work, || {
            OverlayAgent::spawn(cluster.clone(), config, &host, tag)
        });
        let firewall = on(&rt.work, || firewall.map(FirewallAgent::spawn));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| on(&rt.core, || HaGuard::spawn(cluster.clone(), ha)));
        reconciler_config.ha = ha.clone();
        reconciler_config.firewall = gate;
        let reconciler = on(&rt.work, || {
            Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config)
        });
        let pools = on(&rt.work, || {
            PoolMounter::spawn(
                cluster.clone(),
                mount_root.clone(),
                qemu_binary,
                config.pools.refresh(),
            )
        });
        // 지표는 메모리에만 둔다(Raft에 쓰지 않는다).
        let metrics = on(&rt.work, || {
            MetricsSampler::spawn(
                hv.clone(),
                metrics_host::native(),
                cluster.clone(),
                config.data_dir.clone(),
                metrics_store,
            )
        });
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        runner_config.mount_root = mount_root.clone();
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        runner_config.migration_disk_timeout = config.migration.disk_timeout;
        // 받는 쪽(IncomingConfig)과 같은 디렉터리다.
        runner_config.migration_disks = Some(config.data_dir.join(MIGRATION_DISKS_DIR));
        // ESXi 가져오기의 자격 증명은 API가 받아 두고 가져오기 Task가 쓴다.
        runner_config.import_credentials = state.import_credentials.clone();
        // 노드 비우기 Task가 이 노드의 API와 같은 확인으로 마이그레이션 Task를 만든다.
        let state = Arc::new(state);
        runner_config.api = Some(state.clone());
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
        let updates = config
            .updates
            .updates_config()
            .map(|c| Arc::new(Updates::new(c)));
        runner_config.updates = updates.clone();
        let runner = run_on(
            &rt.work,
            TaskRunner::spawn(
                cluster.clone(),
                storage,
                hv,
                reconciler.handle(),
                runner_config,
            ),
        )
        .await;
        let monitor = on(&rt.core, || {
            NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default())
        });
        let updates = on(&rt.work, || {
            updates.map(|u| UpdateAgent::spawn(cluster.clone(), node, u))
        });

        // TLS 핸드셰이크 작업이 `api`에 생긴다.
        let listener = on(&rt.api, || TlsListener::new(api_listener, api_tls))?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "orbivisord is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let (schedule, janitor, backups, telemetry, remotes) = on(&rt.work, || {
            (
                SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config()),
                ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config()),
                BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config()),
                TelemetryReporter::spawn(state.clone(), telemetry_config),
                RemotePoller::spawn(state.clone(), config.remotes.poller_config()),
            )
        });
        let recovery = on(&rt.core, || {
            HaManager::spawn(state.clone(), HaManagerConfig::default())
        });
        let audit = state.audit.clone();
        let app = opl_api::router(state).merge(crate::web::router());
        let task = rt.api.spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            services: Some(Services {
                api: Server { stop, task },
                reconciler,
                runner,
                monitor,
                schedule,
                janitor,
                backups,
                telemetry,
                remotes,
                audit,
                recovery,
                pools,
                metrics,
                overlay,
                firewall,
                ha,
                updates,
            }),
            register,
            promotion,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx)
        });
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = on(&rt.core, || spawn_promotion(first_time, &cluster, &peers));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "orbivisord is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            exited,
        })
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        if let Some(services) = self.services.take() {
            services.shutdown(left).await;
        }
        self.register.abort();
        self.promotion.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop orbivisord and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다. 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(first_time: bool, cluster: &Arc<Cluster>, peers: &Arc<Peers>) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtimes;
    use opl_types::resource::Resource;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// 새 클러스터를 만든 멤버의 서비스가 실행기 셋에 나뉘어 돈다. 부른 쪽의 실행기에는 아무 작업도
    /// 남지 않고(고르지 않고 띄운 서비스가 없다), 닫으면 세 실행기의 작업이 모두 끝난다.
    #[test]
    fn services_run_on_the_three_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        let mut config: Config = toml::from_str(&format!(
            "name = \"n1\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{}\"\n\
             cluster_listen = \"127.0.0.1:{}\"\n[hypervisor]\naccel = \"tcg\"\n\
             systemd_scope = false\n[ha]\nfencing = \"process\"\n[updates]\nenabled = false",
            free_port(),
            free_port()
        ))
        .unwrap();
        config.data_dir = dir.path().join("data");
        config.run_dir = dir.path().join("run");
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let alive = |h: &tokio::runtime::Handle| h.metrics().num_alive_tasks();
        // 주 루프 대신 부르는 쪽
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (member, created) = Member::start(&config, node, tls, bootstrap, &rt)
                .await
                .unwrap();
            assert!(created.is_some());
            let here = tokio::runtime::Handle::current();
            assert_eq!(alive(&here), 0, "tasks left on the caller's runtime");
            for (name, h) in [("core", &rt.core), ("api", &rt.api), ("work", &rt.work)] {
                assert!(alive(h) > 0, "no task on {name}");
            }
            // 빠진 것처럼 닫아 root로 도는 Linux CI에서 만든 방화벽 테이블도 지운다.
            member.close(true).await.unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while [&rt.core, &rt.api, &rt.work].iter().any(|h| alive(h) > 0) {
            assert!(
                std::time::Instant::now() < deadline,
                "tasks left after close: core {} api {} work {}",
                alive(&rt.core),
                alive(&rt.api),
                alive(&rt.work)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
```

`crates/opld/src/runtime.rs`를 만든다:

```rust
//! 실행기 셋(설계 §3.1). API·작업이 바빠도 Raft 하트비트와 HA 리스 갱신이 늦지 않게 서로 다른
//! 작업자 스레드에서 돌린다.
//!
//! - `core`(2개, `orbivisor-core`): Raft, 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시
//! - `api`(`orbivisor-api`): HTTPS API
//! - `work`(`orbivisor-work`): reconciler, Task 실행기, 스케줄러, 정리, 지표, 풀, 텔레메트리 등
//!
//! 실행기 사이 호출은 `Arc`로 나눈 객체의 메서드이고, tokio의 채널·`Notify`·`watch`는 실행기를
//! 넘어 동작한다.

use std::future::Future;

use tokio::runtime::{Builder, Handle, Runtime};

/// `core` 실행기의 작업자 스레드 수(고정)
pub const CORE_THREADS: usize = 2;

pub struct Runtimes {
    pub core: Runtime,
    pub api: Runtime,
    pub work: Runtime,
}

/// 실행기 셋의 핸들. 서비스를 띄울 때 쓴다.
#[derive(Clone)]
pub struct Handles {
    pub core: Handle,
    pub api: Handle,
    pub work: Handle,
}

impl Runtimes {
    pub fn new(api_threads: usize, work_threads: usize) -> std::io::Result<Self> {
        let build = |name: &str, threads: usize| {
            Builder::new_multi_thread()
                .worker_threads(threads)
                .thread_name(name)
                .enable_all()
                .build()
        };
        Ok(Self {
            core: build("orbivisor-core", CORE_THREADS)?,
            api: build("orbivisor-api", api_threads)?,
            work: build("orbivisor-work", work_threads)?,
        })
    }

    pub fn handles(&self) -> Handles {
        Handles {
            core: self.core.handle().clone(),
            api: self.api.handle().clone(),
            work: self.work.handle().clone(),
        }
    }
}

/// `handle` 실행기의 문맥에서 `f`를 부른다. 안에서 `tokio::spawn`한 작업은 그 실행기에서 돈다.
pub fn on<T>(handle: &Handle, f: impl FnOnce() -> T) -> T {
    let _guard = handle.enter();
    f()
}

/// `fut`을 `handle` 실행기에서 돌리고 끝나기를 기다린다. 안에서 띄운 작업도 그 실행기에서 돈다.
/// `fut`이 panic하면 그대로 다시 던진다. 대상 실행기가 끝나 작업이 취소되어도 panic하므로, 대상
/// 실행기가 부른 쪽보다 먼저 끝날 수 있는 곳에서는 쓰지 않는다.
pub async fn run_on<F>(handle: &Handle, fut: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match handle.spawn(fut).await {
        Ok(output) => output,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_name() -> String {
        std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn each_runtime_has_named_threads_of_its_size() {
        let rt = Runtimes::new(3, 4).unwrap();
        let h = rt.handles();
        for (handle, name, threads) in [
            (&h.core, "orbivisor-core", 2),
            (&h.api, "orbivisor-api", 3),
            (&h.work, "orbivisor-work", 4),
        ] {
            assert_eq!(handle.metrics().num_workers(), threads, "{name}");
            let ran = rt.core.block_on(run_on(handle, async { thread_name() }));
            assert_eq!(ran, name);
        }
    }

    /// `core`에서 도는 코드가 `on`·`run_on`으로 띄운 작업은 고른 실행기에서 돈다.
    #[test]
    fn on_and_run_on_spawn_into_the_chosen_runtime() {
        let rt = Runtimes::new(2, 2).unwrap();
        let h = rt.handles();
        rt.core.block_on(async {
            let here = tokio::spawn(async { thread_name() }).await.unwrap();
            assert_eq!(here, "orbivisor-core");
            let api = on(&h.api, || tokio::spawn(async { thread_name() }));
            assert_eq!(api.await.unwrap(), "orbivisor-api");
            // run_on 안에서 다시 띄운 작업도 그 실행기다.
            let work = run_on(&h.work, async {
                tokio::spawn(async { thread_name() }).await.unwrap()
            });
            assert_eq!(work.await, "orbivisor-work");
        });
    }
}
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1694개, 실패 0

Run: `cargo test -p opld --features ha-it --test ha`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opld): 실행기 셋 — core(2, orbivisor-core)·api(orbivisor-api)·work(orbivisor-work)"
```

---

### Task 2: watchdog 스레드, 저장소 스레드, 피어 실행기 나누기

opl-cluster `writer.rs` `StoreWriter`(OS 스레드 `orbivisor-store` 하나가 redb 쓰기 트랜잭션을 차례로 커밋, `append`의 `LogFlushed`는 커밋 뒤, `save_committed`는 기다리지 않음, 작업 panic은 기록 뒤 스레드 멈춤)와 `Cluster::stopped()`, opl-reconciler `HaGuard`의 쓰다듬기 OS 스레드 `orbivisor-watchdog`(원자값으로 fresh 판단, 장치를 받으면 바로 한 번, 잠금 밖 magic close, `close` 뒤 `arm` 거절), opl-peer `Peers::with_runtimes(control, bulk)`(Raft·리스·리더 전달·ping 채널은 core, 콘솔·UEFI·마이그레이션·감사·지표 채널은 work — `Endpoint::executor`)와 `PeerService::with_work`(무거운 처리기는 work, panic은 `internal`), `member.rs`의 Raft 멈춤 감시. 느린 팔로어·저장소 순서·panic·쓰다듬기 스레드·채널 실행기 시험, 그리고 이 변경으로 드러난 시험 경합 세 곳(마이그레이션 되돌리기, 가져오기 Task, 가짜 apt 파일) 고침.

**Files:**
- Create: `crates/opl-cluster/src/writer.rs`
- Modify: `crates/opl-api/tests/backup_tasks.rs`
- Modify: `crates/opl-api/tests/common/fake_apt.rs`
- Modify: `crates/opl-api/tests/vm_imports.rs`
- Modify: `crates/opl-cluster/src/cluster.rs`
- Modify: `crates/opl-cluster/src/lib.rs`
- Modify: `crates/opl-cluster/src/log_store.rs`
- Modify: `crates/opl-cluster/src/state_machine.rs`
- Modify: `crates/opl-cluster/src/store.rs`
- Modify: `crates/opl-peer/Cargo.toml`
- Modify: `crates/opl-peer/src/client.rs`
- Modify: `crates/opl-peer/src/server.rs`
- Modify: `crates/opl-peer/tests/grpc.rs`
- Modify: `crates/opl-reconciler/src/ha.rs`
- Modify: `crates/opl-reconciler/tests/migration.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/src/runtime.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/tests/backup_tasks.rs`에서

```rust
/// Task를 저장소에 직접 만든다(API의 검사 없이). `state`가 `Running`이면 이미 돌고 있는 것처럼 둔다.
async fn put_task(
```

을 다음으로 바꾼다:

```rust
/// Task를 저장소에 직접 만든다(API의 검사 없이). `state`가 `Running`이면 이미 돌고 있는 것처럼 둔다.
/// `state`로 바로 만든다. `pending`으로 만든 뒤 바꾸면 그사이 실행기가 Task를 시작할 수 있다.
async fn put_task(
```

`crates/opl-api/tests/backup_tasks.rs`에서

```rust
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    if state != TaskState::Pending {
        let status = TaskStatus {
            state,
            error: None,
            progress: None,
        };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    json!({ "id": task.id })
```

을 다음으로 바꾼다:

```rust
    task.name = task.id.to_string();
    task.status = TaskStatus {
        state,
        error: None,
        progress: None,
    };
    env.cluster.create(&task).await.unwrap();
    json!({ "id": task.id })
```

`crates/opl-api/tests/common/fake_apt.rs`에서

```rust
    pub fn set(&self, name: &str, contents: &str) {
        std::fs::write(self.path(name), contents).unwrap();
    }
```

을 다음으로 바꾼다:

```rust
    pub fn set(&self, name: &str, contents: &str) {
        // 가짜 명령이 자르고 쓰는 사이의 빈 파일을 읽지 않게 임시 파일을 쓴 뒤 바꿔 끼운다.
        let tmp = self.path(&format!("{name}.tmp"));
        std::fs::write(&tmp, contents).unwrap();
        std::fs::rename(&tmp, self.path(name)).unwrap();
    }
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust

/// Task를 저장소에 직접 만든다(API의 검사 없이).
async fn put_task(env: &Env, op: TaskOp, target: Uuid, state: TaskState) -> Value {
```

을 다음으로 바꾼다:

```rust

/// Task를 저장소에 직접 만든다(API의 검사 없이). `state`로 바로 만든다. `pending`으로 만든 뒤 바꾸면
/// 그사이 실행기가 Task를 시작해 그 정리가 가져오기를 바꿀 수 있다.
async fn put_task(env: &Env, op: TaskOp, target: Uuid, state: TaskState) -> Value {
```

`crates/opl-api/tests/vm_imports.rs`에서

```rust
    task.name = task.id.to_string();
    env.cluster.create(&task).await.unwrap();
    if state != TaskState::Pending {
        let status = TaskStatus {
            state,
            error: None,
            progress: None,
        };
        env.cluster
            .update_status::<TaskSpec>(task.id, &status)
            .await
            .unwrap();
    }
    json!({ "id": task.id })
```

을 다음으로 바꾼다:

```rust
    task.name = task.id.to_string();
    task.status = TaskStatus {
        state,
        error: None,
        progress: None,
    };
    env.cluster.create(&task).await.unwrap();
    json!({ "id": task.id })
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
    leases: Arc<LeaseTable>,
}
```

을 다음으로 바꾼다:

```rust
    leases: Arc<LeaseTable>,
    /// 시험이 저장소 스레드를 붙잡아 느린 디스크를 흉내 낸다.
    #[cfg(test)]
    writer: crate::writer::StoreWriter,
}
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            leases: Arc::default(),
        })
```

을 다음으로 바꾼다:

```rust
            leases: Arc::default(),
            #[cfg(test)]
            writer: store.writer().clone(),
        })
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }
}
```

을 다음으로 바꾼다:

```rust
            .map_err(|e| ClusterError::Raft(e.to_string()))
    }

    /// Raft가 멈추면 그 이유와 함께 끝나는 future(저장소를 붙잡지 않는다). 저장소 쓰기가 실패하거나
    /// 저장소 스레드가 멈추면 openraft가 이 노드의 Raft를 멈춘다(`running_state`가 `Err`). `shutdown`으로
    /// 멈춰도 끝난다.
    pub fn stopped(&self) -> impl Future<Output = String> + Send + 'static {
        let mut rx = self.raft.metrics();
        async move {
            loop {
                let state = rx.borrow_and_update().running_state.clone();
                if let Err(e) = state {
                    return e.to_string();
                }
                // 보내는 쪽이 사라졌다: Raft 작업이 끝났다(panic 포함).
                if rx.changed().await.is_err() {
                    return "the Raft task ended".to_owned();
                }
            }
        }
    }
}
```

`crates/opl-cluster/src/cluster.rs`에서

```rust
        status: serde_json::from_value(r.status)?,
    })
}
```

을 다음으로 바꾼다:

```rust
        status: serde_json::from_value(r.status)?,
    })
}

#[cfg(test)]
mod tests {
    use opl_types::runtime::ImageFormat;
    use opl_types::storage::{Volume, VolumeSpec};
    use tokio::time::Instant;

    use super::*;
    use crate::memnet::MemRouter;

    const SLOW_WRITE: Duration = Duration::from_secs(3);

    fn volume(name: &str) -> Volume {
        Resource::new(
            name,
            VolumeSpec {
                pool: "local".into(),
                size_bytes: 1 << 30,
                format: ImageFormat::Qcow2,
            },
        )
    }

    /// 저장소 쓰기가 3초 걸리는 팔로워가 있어도 (작업자 스레드가 하나뿐인) 실행기는 막히지 않는다.
    /// 리더의 하트비트와 리스 갱신(`confirm_leader` 두 번)은 시간 안에 돌고 리더도 그대로다. 쓰기를
    /// 실행기 스레드에서 동기로 하면 3초 동안 모든 노드가 멈춘다.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_store_on_a_follower_does_not_stall_heartbeats_or_lease_checks() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let mut nodes = BTreeMap::new();
        for i in 1..=3u64 {
            let id = NodeId::from(i);
            let mut config = ClusterConfig::new(id, root.path().join(i.to_string()));
            config.raft = openraft::Config {
                heartbeat_interval: 50,
                election_timeout_min: 1000,
                election_timeout_max: 2000,
                ..Default::default()
            };
            let cluster = Cluster::open(config, router.network(id)).await.unwrap();
            router.register(id, &cluster);
            nodes.insert(id, cluster);
        }
        let members = (1..=3u64)
            .map(|i| (NodeId::from(i), BasicNode::new(format!("node-{i}"))))
            .collect();
        let first = &nodes[&NodeId::from(1)];
        first.initialize(members).await.unwrap();
        let leader = loop {
            if let Some(l) = first.raft().metrics().borrow().current_leader
                && nodes.values().all(|n| n.members().leader == Some(l))
            {
                break l;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let term = nodes[&leader].raft().metrics().borrow().current_term;
        let slow = *nodes.keys().find(|id| **id != leader).unwrap();
        // openraft 0.9의 Raft 루프는 자기 로그 쓰기가 끝나기를 기다리므로 느린 노드의 루프는 3초 멈추고,
        // 그 뒤 선거에 나설 수 있다. 여기서 보는 것은 실행기와 다른 노드라 선거는 끈다.
        nodes[&slow].raft().runtime_config().elect(false);

        // 느린 팔로워의 저장소 스레드를 3초 붙잡은 뒤 쓴다. 리더와 다른 팔로워로 커밋된다.
        nodes[&slow]
            .writer
            .submit(|_| std::thread::sleep(SLOW_WRITE))
            .unwrap();
        let stalled = Instant::now();
        let v = volume("v");
        let index = nodes[&leader].create(&v).await.unwrap();
        assert!(stalled.elapsed() < Duration::from_millis(500));
        let mut rounds = 0;
        while stalled.elapsed() < SLOW_WRITE {
            let started = Instant::now();
            nodes[&leader]
                .renew_lease(Duration::from_millis(500))
                .await
                .unwrap();
            assert!(
                started.elapsed() < Duration::from_millis(500),
                "{:?}",
                started.elapsed()
            );
            rounds += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(rounds >= 5, "{rounds}");
        let m = nodes[&leader].raft().metrics().borrow().clone();
        assert_eq!((m.current_leader, m.current_term), (Some(leader), term));
        // 느린 팔로워도 붙잡힌 뒤에 차례대로 따라온다.
        nodes[&slow]
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .applied_index_at_least(Some(index), "slow follower caught up")
            .await
            .unwrap();
        assert!(nodes[&slow].get::<VolumeSpec>(v.id).unwrap().is_some());
        for node in nodes.values() {
            node.shutdown().await.unwrap();
        }
    }

    /// 저장소 쓰기가 panic해 저장소 스레드가 멈추면 다음 쓰기에서 openraft가 Raft를 멈추고
    /// `stopped`가 그 이유와 함께 끝난다.
    #[tokio::test]
    async fn stopped_ends_when_a_store_write_panics() {
        let root = tempfile::TempDir::new().unwrap();
        let router = MemRouter::default();
        let id = NodeId::from(1u64);
        let cluster = Cluster::open(ClusterConfig::new(id, root.path()), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new("node-1"))]))
            .await
            .unwrap();
        cluster.create(&volume("a")).await.unwrap();
        let stopped = cluster.stopped();
        tokio::pin!(stopped);
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut stopped)
                .await
                .is_err()
        );

        cluster.writer.submit(|_| panic!("store bug")).unwrap();
        assert!(cluster.create(&volume("b")).await.is_err());
        let why = tokio::time::timeout(Duration::from_secs(10), stopped)
            .await
            .unwrap();
        assert!(why.contains("the store thread has stopped"), "{why}");
        let _ = cluster.shutdown().await;
    }
}
```

`crates/opl-cluster/src/lib.rs`에서

```rust
pub mod watch;
```

을 다음으로 바꾼다:

```rust
pub mod watch;
mod writer;
```

`crates/opl-cluster/src/log_store.rs` 전체:

```rust
use std::fmt::Debug;
use std::ops::{Bound, RangeBounds};
use std::sync::Arc;

use openraft::storage::{LogFlushed, RaftLogStorage};
use openraft::{
    Entry, LogId, LogState, OptionalSend, RaftLogReader, StorageError, StorageIOError, Vote,
};
use redb::{Database, ReadableDatabase, ReadableTable};

use crate::store::{
    KEY_COMMITTED, KEY_LAST_PURGED, KEY_VOTE, LOG, META, StoreError, get_json, put_json,
};
use crate::types::{NodeId, TypeConfig};
use crate::writer::StoreWriter;

/// Raft 로그 저장소. 쓰기는 저장소 스레드에서 돌고, 커밋(fsync)이 끝난 뒤에 완료를 알린다.
/// `save_committed`만 기다리지 않는다(아래 참고).
#[derive(Clone)]
pub struct LogStore {
    db: Arc<Database>,
    writer: StoreWriter,
}

impl LogStore {
    pub(crate) fn new(db: Arc<Database>, writer: StoreWriter) -> Self {
        Self { db, writer }
    }

    fn read_meta<T: serde::de::DeserializeOwned>(
        db: &Database,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let txn = db.begin_read()?;
        let meta = txn.open_table(META)?;
        get_json(&meta, key)
    }

    fn write_meta<T: serde::Serialize>(
        db: &Database,
        key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            put_json(&mut meta, key, value)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn read_entries(
        &self,
        range: (Bound<u64>, Bound<u64>),
    ) -> Result<Vec<Entry<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(LOG)?;
        let mut out = Vec::new();
        for item in log.range::<u64>(range)? {
            let (_, v) = item?;
            out.push(serde_json::from_slice(v.value())?);
        }
        Ok(out)
    }

    fn last_entry_id(&self) -> Result<Option<LogId<NodeId>>, StoreError> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(LOG)?;
        match log.last()? {
            Some((_, v)) => {
                let entry: Entry<TypeConfig> = serde_json::from_slice(v.value())?;
                Ok(Some(entry.log_id))
            }
            None => Ok(None),
        }
    }

    fn append_entries(db: &Database, entries: Vec<Entry<TypeConfig>>) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            for entry in entries {
                log.insert(entry.log_id.index, serde_json::to_vec(&entry)?.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// `index` 이상의 엔트리를 지운다(충돌한 로그 잘라내기).
    fn truncate_from(db: &Database, index: u64) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut log = txn.open_table(LOG)?;
            log.retain_in(index.., |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }

    /// `log_id`까지의 엔트리를 지우고 마지막으로 지운 id를 기록한다.
    fn purge_upto(db: &Database, log_id: &LogId<NodeId>) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            put_json(&mut meta, KEY_LAST_PURGED, log_id)?;
            let mut log = txn.open_table(LOG)?;
            log.retain_in(..=log_id.index, |_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }
}

impl RaftLogReader<TypeConfig> for LogStore {
    async fn try_get_log_entries<RB: RangeBounds<u64> + Clone + Debug + OptionalSend>(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let range = (range.start_bound().cloned(), range.end_bound().cloned());
        self.read_entries(range)
            .map_err(|e| StorageIOError::read_logs(&e).into())
    }
}

impl RaftLogStorage<TypeConfig> for LogStore {
    type LogReader = Self;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last_purged: Option<LogId<NodeId>> = Self::read_meta(&self.db, KEY_LAST_PURGED)
            .map_err(|e| StorageIOError::read_logs(&e))?;
        let last = self
            .last_entry_id()
            .map_err(|e| StorageIOError::read_logs(&e))?
            .or(last_purged);
        Ok(LogState {
            last_purged_log_id: last_purged,
            last_log_id: last,
        })
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        self.clone()
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        let vote = *vote;
        self.writer
            .run(move |db| Self::write_meta(db, KEY_VOTE, &vote))
            .await
            .map_err(|e| StorageIOError::write_vote(&e).into())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        Self::read_meta(&self.db, KEY_VOTE).map_err(|e| StorageIOError::read_vote(&e).into())
    }

    async fn save_committed(
        &mut self,
        committed: Option<LogId<NodeId>>,
    ) -> Result<(), StorageError<NodeId>> {
        // 기다리지 않는다. openraft는 이것을 Raft 루프 안에서 부르므로, 앞서 넣은 느린 쓰기(상태 머신
        // 적용 등) 뒤에서 기다리면 그동안 하트비트도 멈춘다. 저장된 커밋 위치는 다시 시작할 때 적용을
        // 앞당기는 데만 쓰고 늦어도 안전하다(openraft는 저장하지 않는 것이 기본이다). 차례는 큐가
        // 지키므로 이 위치보다 앞의 로그 쓰기가 먼저 커밋된다.
        self.writer
            .submit(move |db| {
                if let Err(e) = Self::write_meta(db, KEY_COMMITTED, &committed) {
                    tracing::warn!(error = %e, "cannot save the committed log id");
                }
            })
            .map_err(|e| StorageIOError::write(&e).into())
    }

    async fn read_committed(&mut self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        // 기다리지 않고 넣은 `save_committed`보다 뒤에 읽도록 저장소 스레드의 큐를 거친다.
        let committed: Option<Option<LogId<NodeId>>> = self
            .writer
            .run(|db| Self::read_meta(db, KEY_COMMITTED))
            .await
            .map_err(|e| StorageIOError::read(&e))?;
        Ok(committed.flatten())
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        // openraft 0.9는 돌아온 뒤 곧바로 엔트리를 읽을 수 있어야 하고 콜백까지 기다리므로, 커밋을 마친
        // 뒤에 돌아온다. 콜백은 저장소 스레드가 커밋 뒤에 부른다.
        let entries: Vec<_> = entries.into_iter().collect();
        self.writer
            .run(move |db| {
                Self::append_entries(db, entries)?;
                callback.log_io_completed(Ok(()));
                Ok(())
            })
            .await
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.writer
            .run(move |db| Self::truncate_from(db, log_id.index))
            .await
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        self.writer
            .run(move |db| Self::purge_upto(db, &log_id))
            .await
            .map_err(|e| StorageIOError::write_logs(&e).into())
    }
}
```

`crates/opl-cluster/src/state_machine.rs` 전체:

```rust
use std::io::Cursor;
use std::sync::Arc;

use openraft::storage::RaftStateMachine;
use openraft::{
    Entry, EntryPayload, LogId, OptionalSend, RaftSnapshotBuilder, Snapshot, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use opl_types::resource::Kind;
use redb::{Database, ReadableDatabase, ReadableTable, Table};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::exclusive::exclusive_values;
use crate::store::{
    KEY_LAST_APPLIED, KEY_LAST_MEMBERSHIP, KEY_SNAPSHOT_DATA, KEY_SNAPSHOT_META, META, NAMES,
    OBJECTS, SCHEMA_VERSION, StoreError, get_json, put_json,
};
use crate::types::{Command, NodeId, RawResource, Rejection, Response, TypeConfig};
use crate::watch::{WatchEvent, WatchHub};
use crate::writer::StoreWriter;

type Membership = StoredMembership<NodeId, openraft::BasicNode>;
type Meta = SnapshotMeta<NodeId, openraft::BasicNode>;

/// 스냅샷 본문의 머리. 본문 전체를 읽기 전에 형식 버전부터 확인한다.
#[derive(Deserialize)]
struct SnapshotHeader {
    schema_version: u32,
}

/// 스냅샷 본문. 상태 머신의 리소스 전체를 담는다.
#[derive(Serialize, Deserialize)]
struct SnapshotData {
    schema_version: u32,
    objects: Vec<(Kind, RawResource)>,
}

/// 리소스 상태 머신. 엔트리 묶음 하나를 redb 트랜잭션 하나로 적용하므로
/// 리소스와 `last_applied`는 항상 함께 기록된다. 쓰기는 저장소 스레드에서 돈다.
#[derive(Clone)]
pub struct StateMachine {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    writer: StoreWriter,
}

impl StateMachine {
    pub(crate) fn new(db: Arc<Database>, watch: Arc<WatchHub>, writer: StoreWriter) -> Self {
        Self { db, watch, writer }
    }

    fn read_applied(&self) -> Result<(Option<LogId<NodeId>>, Membership), StoreError> {
        let txn = self.db.begin_read()?;
        let meta = txn.open_table(META)?;
        Ok((
            get_json(&meta, KEY_LAST_APPLIED)?,
            get_json(&meta, KEY_LAST_MEMBERSHIP)?.unwrap_or_default(),
        ))
    }

    fn apply_entries(
        db: &Database,
        entries: Vec<Entry<TypeConfig>>,
    ) -> Result<(Vec<Response>, Vec<WatchEvent>), StoreError> {
        let mut responses = Vec::with_capacity(entries.len());
        let mut events = Vec::new();
        let txn = db.begin_write()?;
        {
            let mut meta = txn.open_table(META)?;
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            for entry in entries {
                let index = entry.log_id.index;
                let result = match entry.payload {
                    EntryPayload::Blank => Ok(index),
                    EntryPayload::Membership(m) => {
                        put_json(
                            &mut meta,
                            KEY_LAST_MEMBERSHIP,
                            &Membership::new(Some(entry.log_id), m),
                        )?;
                        Ok(index)
                    }
                    EntryPayload::Normal(cmd) => {
                        apply_command(&mut objects, &mut names, cmd, index)?.map(|event| {
                            events.push(event);
                            index
                        })
                    }
                };
                put_json(&mut meta, KEY_LAST_APPLIED, &entry.log_id)?;
                responses.push(Response(result));
            }
        }
        txn.commit()?;
        Ok((responses, events))
    }

    /// 한 읽기 트랜잭션에서 상태 전체를 떠서 스냅샷 본문을 만든다. 저장은 부른 쪽이 한다.
    fn snapshot_data(db: &Database) -> Result<(Meta, Vec<u8>), StoreError> {
        let txn = db.begin_read()?;
        let meta_table = txn.open_table(META)?;
        let last_log_id: Option<LogId<NodeId>> = get_json(&meta_table, KEY_LAST_APPLIED)?;
        let last_membership: Membership =
            get_json(&meta_table, KEY_LAST_MEMBERSHIP)?.unwrap_or_default();
        let mut objects = Vec::new();
        for item in txn.open_table(OBJECTS)?.iter()? {
            let (key, value) = item?;
            let kind: Kind = serde_json::from_value(key.value().0.into())?;
            objects.push((kind, serde_json::from_slice(value.value())?));
        }
        drop(txn);

        let data = serde_json::to_vec(&SnapshotData {
            schema_version: SCHEMA_VERSION,
            objects,
        })?;
        let meta = Meta {
            snapshot_id: format!(
                "{}-{}",
                last_log_id.map_or(0, |l| l.index),
                Uuid::new_v4().simple()
            ),
            last_log_id,
            last_membership,
        };
        Ok((meta, data))
    }

    /// 스냅샷을 저장한다. 이미 저장된 스냅샷이 같거나 더 새로우면 그대로 둔다.
    /// openraft는 스냅샷 빌드를 별도 태스크에서 돌리므로, 느린 빌드가 끝나기 전에
    /// 더 새 스냅샷이 설치될 수 있다.
    fn save_snapshot(db: &Database, meta: &Meta, data: &[u8]) -> Result<(), StoreError> {
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(META)?;
            if let Some(existing) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)?
                && existing.last_log_id >= meta.last_log_id
            {
                return Ok(());
            }
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data)?;
        }
        txn.commit()?;
        Ok(())
    }

    fn current_snapshot(&self) -> Result<Option<Snapshot<TypeConfig>>, StoreError> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META)?;
        let Some(meta) = get_json::<Meta>(&table, KEY_SNAPSHOT_META)? else {
            return Ok(None);
        };
        let data = table
            .get(KEY_SNAPSHOT_DATA)?
            .map(|v| v.value().to_vec())
            .unwrap_or_default();
        Ok(Some(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        }))
    }

    /// 리소스 전체를 스냅샷 내용으로 바꾼다. 리소스, 적용 위치, 스냅샷을 한 트랜잭션에 기록한다.
    fn install(db: &Database, meta: &Meta, data: Vec<u8>) -> Result<(), StoreError> {
        let header: SnapshotHeader = serde_json::from_slice(&data)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                found: header.schema_version,
            });
        }
        let snapshot: SnapshotData = serde_json::from_slice(&data)?;
        let txn = db.begin_write()?;
        {
            let mut objects = txn.open_table(OBJECTS)?;
            let mut names = txn.open_table(NAMES)?;
            objects.retain(|_, _| false)?;
            names.retain(|_, _| false)?;
            for (kind, resource) in &snapshot.objects {
                objects.insert(
                    (kind.as_str(), resource.id.as_u128()),
                    serde_json::to_vec(resource)?.as_slice(),
                )?;
                names.insert(
                    (kind.as_str(), resource.name.as_str()),
                    resource.id.as_u128(),
                )?;
            }
            let mut table = txn.open_table(META)?;
            put_json(&mut table, KEY_LAST_APPLIED, &meta.last_log_id)?;
            put_json(&mut table, KEY_LAST_MEMBERSHIP, &meta.last_membership)?;
            put_json(&mut table, KEY_SNAPSHOT_META, meta)?;
            table.insert(KEY_SNAPSHOT_DATA, data.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }
}

/// 명령 하나를 적용한다. 거부는 결정적이므로 모든 노드가 같은 결과를 낸다.
/// 바깥 `Err`는 저장소 오류이고, 안쪽 `Err`는 명령 거부다.
fn apply_command(
    objects: &mut Table<(&'static str, u128), &'static [u8]>,
    names: &mut Table<(&'static str, &'static str), u128>,
    cmd: Command,
    index: u64,
) -> Result<Result<WatchEvent, Rejection>, StoreError> {
    let load = |objects: &Table<(&'static str, u128), &'static [u8]>,
                kind: Kind,
                id: Uuid|
     -> Result<Option<RawResource>, StoreError> {
        match objects.get((kind.as_str(), id.as_u128()))? {
            Some(v) => Ok(Some(serde_json::from_slice(v.value())?)),
            None => Ok(None),
        }
    };
    let check = |expected: Option<u64>, actual: u64| match expected {
        Some(expected) if expected != actual => {
            Err(Rejection::RevisionMismatch { expected, actual })
        }
        _ => Ok(()),
    };

    let (kind, resource) = match cmd {
        Command::Create { kind, mut resource } => {
            if load(objects, kind, resource.id)?.is_some() {
                return Ok(Err(Rejection::AlreadyExists {
                    kind,
                    id: resource.id,
                }));
            }
            if names
                .get((kind.as_str(), resource.name.as_str()))?
                .is_some()
            {
                return Ok(Err(Rejection::NameTaken {
                    kind,
                    name: resource.name,
                }));
            }
            if let Err(r) = check_exclusive(objects, kind, resource.id, &resource.spec)? {
                return Ok(Err(r));
            }
            names.insert(
                (kind.as_str(), resource.name.as_str()),
                resource.id.as_u128(),
            )?;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateSpec {
            kind,
            id,
            expected_revision,
            labels,
            spec,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            if let Err(r) = check_exclusive(objects, kind, id, &spec)? {
                return Ok(Err(r));
            }
            resource.labels = labels;
            resource.spec = spec;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatus { kind, id, status } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::UpdateStatusIf {
            kind,
            id,
            expected_revision,
            status,
        } => {
            let Some(mut resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(Some(expected_revision), resource.revision) {
                return Ok(Err(r));
            }
            resource.status = status;
            resource.revision = index;
            (kind, resource)
        }
        Command::Delete {
            kind,
            id,
            expected_revision,
        } => {
            let Some(resource) = load(objects, kind, id)? else {
                return Ok(Err(Rejection::NotFound { kind, id }));
            };
            if let Err(r) = check(expected_revision, resource.revision) {
                return Ok(Err(r));
            }
            objects.remove((kind.as_str(), id.as_u128()))?;
            names.remove((kind.as_str(), resource.name.as_str()))?;
            return Ok(Ok(WatchEvent::Deleted {
                revision: index,
                kind,
                resource,
            }));
        }
    };
    objects.insert(
        (kind.as_str(), resource.id.as_u128()),
        serde_json::to_vec(&resource)?.as_slice(),
    )?;
    Ok(Ok(WatchEvent::Put { kind, resource }))
}

/// `spec`의 독점 값이 같은 종류의 다른 리소스나 같은 spec 안에서 겹치면 거부한다.
fn check_exclusive(
    objects: &Table<(&'static str, u128), &'static [u8]>,
    kind: Kind,
    id: Uuid,
    spec: &serde_json::Value,
) -> Result<Result<(), Rejection>, StoreError> {
    let wanted = exclusive_values(kind, spec);
    if wanted.is_empty() {
        return Ok(Ok(()));
    }
    let in_use = |field: &str, value: &str, holder: Uuid| Rejection::InUse {
        kind,
        field: field.to_string(),
        value: value.to_string(),
        holder,
    };
    for (i, (field, value)) in wanted.iter().enumerate() {
        if wanted[..i].contains(&(field, value.clone())) {
            return Ok(Err(in_use(field, value, id)));
        }
    }
    for item in objects.range((kind.as_str(), 0u128)..=(kind.as_str(), u128::MAX))? {
        let (key, v) = item?;
        let other = Uuid::from_u128(key.value().1);
        if other == id {
            continue;
        }
        let other_spec: RawResource = serde_json::from_slice(v.value())?;
        for (field, value) in exclusive_values(kind, &other_spec.spec) {
            if wanted.iter().any(|(f, v)| *f == field && *v == value) {
                return Ok(Err(in_use(field, &value, other)));
            }
        }
    }
    Ok(Ok(()))
}

impl RaftSnapshotBuilder<TypeConfig> for StateMachine {
    /// 큰 읽기는 `spawn_blocking`에서, 저장은 저장소 스레드에서 한다.
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let db = self.db.clone();
        let (meta, data) = tokio::task::spawn_blocking(move || Self::snapshot_data(&db))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        let (meta, data) = self
            .writer
            .run(move |db| Self::save_snapshot(db, &meta, &data).map(|()| (meta, data)))
            .await
            .map_err(|e| StorageIOError::write_snapshot(None, &e))?;
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for StateMachine {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> Result<(Option<LogId<NodeId>>, Membership), StorageError<NodeId>> {
        self.read_applied()
            .map_err(|e| StorageIOError::read_state_machine(&e).into())
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<Response>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + OptionalSend,
        I::IntoIter: OptionalSend,
    {
        let entries: Vec<_> = entries.into_iter().collect();
        let (responses, events) = self
            .writer
            .run(move |db| Self::apply_entries(db, entries))
            .await
            .map_err(|e| StorageIOError::write_state_machine(&e))?;
        self.watch.publish(events);
        Ok(responses)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &Meta,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let (m, data) = (meta.clone(), snapshot.into_inner());
        self.writer
            .run(move |db| Self::install(db, &m, data))
            .await
            .map_err(|e| StorageIOError::read_snapshot(Some(meta.signature()), &e))?;
        self.watch.reset(meta.last_log_id.map_or(0, |l| l.index));
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        self.current_snapshot()
            .map_err(|e| StorageIOError::read_snapshot(None, &e).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use openraft::CommittedLeaderId;

    fn meta_at(index: u64) -> Meta {
        Meta {
            last_log_id: Some(LogId::new(
                CommittedLeaderId::new(1, NodeId::from(1)),
                index,
            )),
            last_membership: Membership::default(),
            snapshot_id: format!("{index}"),
        }
    }

    /// openraft는 스냅샷을 다른 태스크에서 만들므로, 느린 빌드가 그 사이 설치된
    /// 더 새 스냅샷을 덮어쓰면 안 된다.
    #[test]
    fn older_snapshot_does_not_replace_newer_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        StateMachine::save_snapshot(&sm.db, &meta_at(10), b"new").unwrap();
        StateMachine::save_snapshot(&sm.db, &meta_at(5), b"old").unwrap();

        let snap = sm.current_snapshot().unwrap().unwrap();
        assert_eq!(snap.meta, meta_at(10));
        assert_eq!(snap.snapshot.into_inner(), b"new");
    }

    #[test]
    fn snapshot_with_newer_schema_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let sm = Store::open(&dir.path().join("db")).unwrap().state_machine();
        // 미래 형식은 본문 구조도 다를 수 있다. 헤더만 보고 거부해야 한다.
        let data = br#"{"schema_version": 99, "objects": {"format": "v99"}}"#.to_vec();
        assert!(matches!(
            StateMachine::install(&sm.db, &meta_at(3), data),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
    }
}
```

`crates/opl-cluster/src/store.rs`에서

```rust
use crate::watch::WatchHub;
```

을 다음으로 바꾼다:

```rust
use crate::watch::WatchHub;
use crate::writer::StoreWriter;
```

`crates/opl-cluster/src/store.rs`에서

```rust
    UnsupportedSchema { found: u32 },
}
```

을 다음으로 바꾼다:

```rust
    UnsupportedSchema { found: u32 },
    #[error("cannot start the store thread: {0}")]
    Thread(std::io::Error),
    #[error("the store thread has stopped")]
    WriterStopped,
    #[error("a store write panicked")]
    WriterPanicked,
}
```

`crates/opl-cluster/src/store.rs`에서

```rust
/// 노드 하나의 저장소. `log_store()`와 `state_machine()`을 openraft에 넘긴다.
pub struct Store {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
}
```

을 다음으로 바꾼다:

```rust
/// 노드 하나의 저장소. `log_store()`와 `state_machine()`을 openraft에 넘긴다.
/// 쓰기 트랜잭션은 저장소 스레드(`orbivisor-store`)에서 돈다.
pub struct Store {
    db: Arc<Database>,
    watch: Arc<WatchHub>,
    writer: StoreWriter,
}
```

`crates/opl-cluster/src/store.rs`에서

```rust
        let applied = last_applied_index(&db)?;
        Ok(Self {
            db,
            watch: Arc::new(WatchHub::new(applied)),
        })
```

을 다음으로 바꾼다:

```rust
        let applied = last_applied_index(&db)?;
        let writer = StoreWriter::spawn(&db)?;
        Ok(Self {
            db,
            watch: Arc::new(WatchHub::new(applied)),
            writer,
        })
```

`crates/opl-cluster/src/store.rs`에서

```rust
    pub fn log_store(&self) -> LogStore {
        LogStore::new(self.db.clone())
    }

    pub fn state_machine(&self) -> StateMachine {
        StateMachine::new(self.db.clone(), self.watch.clone())
    }
```

을 다음으로 바꾼다:

```rust
    pub fn log_store(&self) -> LogStore {
        LogStore::new(self.db.clone(), self.writer.clone())
    }

    pub fn state_machine(&self) -> StateMachine {
        StateMachine::new(self.db.clone(), self.watch.clone(), self.writer.clone())
    }

    #[cfg(test)]
    pub(crate) fn writer(&self) -> &StoreWriter {
        &self.writer
    }
```

`crates/opl-cluster/src/writer.rs`를 만든다:

```rust
//! redb 쓰기 전용 스레드(`orbivisor-store`, 설계 §3.2). 쓰기 트랜잭션의 커밋(fsync)이 느려도
//! 비동기 실행기의 작업자 스레드를 막지 않는다. 쓰기는 큐 하나에서 넣은 차례대로 돈다.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::mpsc;

use redb::Database;
use tokio::sync::oneshot;

use crate::store::StoreError;

type Job = Box<dyn FnOnce(&Database) + Send>;

/// 저장소 스레드에 쓰기를 넣는 손잡이. 모든 손잡이가 사라지면 남은 쓰기를 마치고 스레드가 끝난다.
/// 스레드는 데이터베이스를 약하게만 쥔다. 저장소를 모두 놓으면 곧바로 닫혀 같은 파일을 다시 열 수
/// 있고, 그 뒤에 남은 쓰기는 버린다.
///
/// 쓰기가 panic하면 오류 로그를 남기고 스레드를 멈춘다. 커밋하지 않은 트랜잭션은 버려지지만 메모리의
/// Raft 상태(적용한 위치, 기다리는 콜백)는 디스크와 어긋났을 수 있으므로 계속 쓰지 않는다. 그 뒤의
/// 쓰기는 `WriterStopped`다. openraft는 저장소 오류를 받으면 이 노드의 Raft를 멈추고
/// ([`Cluster::stopped`](crate::Cluster::stopped)가 끝남), opld는 그것을 보고 0이 아닌 코드로 끝나
/// systemd가 다시 띄운다.
#[derive(Clone)]
pub(crate) struct StoreWriter {
    tx: mpsc::Sender<Job>,
}

impl StoreWriter {
    pub(crate) fn spawn(db: &Arc<Database>) -> Result<Self, StoreError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let db = Arc::downgrade(db);
        std::thread::Builder::new()
            .name("orbivisor-store".into())
            .spawn(move || {
                for job in rx {
                    let Some(db) = db.upgrade() else {
                        continue;
                    };
                    if let Err(panic) = catch_unwind(AssertUnwindSafe(|| job(&db))) {
                        let message = panic
                            .downcast_ref::<&str>()
                            .map(|m| m.to_string())
                            .or_else(|| panic.downcast_ref::<String>().cloned())
                            .unwrap_or_default();
                        tracing::error!(panic = %message, "a store write panicked; stopping the store thread");
                        break;
                    }
                }
            })
            .map_err(StoreError::Thread)?;
        Ok(Self { tx })
    }

    /// 쓰기를 큐에 넣고 기다리지 않는다.
    pub(crate) fn submit(
        &self,
        job: impl FnOnce(&Database) + Send + 'static,
    ) -> Result<(), StoreError> {
        self.tx
            .send(Box::new(job))
            .map_err(|_| StoreError::WriterStopped)
    }

    /// 쓰기를 큐에 넣고 저장소 스레드가 마칠 때까지 기다린다. 쓰기가 panic하면 `WriterPanicked`다.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Database) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let (tx, rx) = oneshot::channel();
        self.submit(move |db| match catch_unwind(AssertUnwindSafe(|| f(db))) {
            Ok(result) => {
                let _ = tx.send(result);
            }
            Err(panic) => {
                let _ = tx.send(Err(StoreError::WriterPanicked));
                // 스레드가 로그를 남기고 멈춘다.
                resume_unwind(panic);
            }
        })?;
        rx.await.map_err(|_| StoreError::WriterStopped)?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use super::*;

    fn writer() -> (tempfile::TempDir, Arc<Database>, StoreWriter) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::create(dir.path().join("db")).unwrap());
        let writer = StoreWriter::spawn(&db).unwrap();
        (dir, db, writer)
    }

    /// 쓰기는 이름 붙은 스레드 하나에서 넣은 차례대로 돈다.
    #[tokio::test]
    async fn writes_run_in_order_on_the_store_thread() {
        let (_dir, _db, writer) = writer();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for i in 0..20 {
            let seen = seen.clone();
            writer
                .submit(move |_| {
                    let name = std::thread::current().name().map(str::to_owned);
                    seen.lock().unwrap().push((i, name));
                })
                .unwrap();
        }
        let last = writer.run(|_| Ok(20)).await.unwrap();
        assert_eq!(last, 20);
        let seen = seen.lock().unwrap();
        assert_eq!(
            *seen,
            (0..20)
                .map(|i| (i, Some("orbivisor-store".to_owned())))
                .collect::<Vec<_>>()
        );
    }

    /// 쓰기가 panic하면 그 쓰기는 `WriterPanicked`이고, 스레드가 멈춰 뒤의 쓰기는 `WriterStopped`다.
    #[tokio::test]
    async fn a_panicking_write_stops_the_store_thread() {
        let (_dir, _db, writer) = writer();
        let err = writer
            .run(|_| -> Result<(), StoreError> { panic!("write bug") })
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::WriterPanicked), "{err}");
        let err = writer.run(|_| Ok(())).await.unwrap_err();
        assert!(matches!(err, StoreError::WriterStopped), "{err}");
    }

    /// 느린 쓰기를 기다리는 동안에도 (작업자 스레드 하나인) 실행기의 다른 작업은 돈다.
    #[tokio::test]
    async fn a_slow_write_does_not_block_the_runtime() {
        let (_dir, _db, writer) = writer();
        let slow = tokio::spawn({
            let writer = writer.clone();
            async move {
                writer
                    .run(|_| {
                        std::thread::sleep(Duration::from_millis(500));
                        Ok(())
                    })
                    .await
            }
        });
        let start = Instant::now();
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(start.elapsed() < Duration::from_millis(400));
        assert!(!slow.is_finished());
        slow.await.unwrap().unwrap();
    }
}
```

`crates/opl-peer/Cargo.toml`에서

```toml
hex.workspace = true
openraft.workspace = true
```

을 다음으로 바꾼다:

```toml
hex.workspace = true
hyper.workspace = true
openraft.workspace = true
```

`crates/opl-peer/src/client.rs` 전체:

```rust
//! 다른 노드로 가는 gRPC 클라이언트: Raft 네트워크, 리더 전달, 생존 확인, 콘솔, 마이그레이션 받기,
//! UEFI 변수 받기, 감사 로그 읽기, 지표 읽기.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use openraft::BasicNode;
use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use opl_cluster::types::{LeaderRequest, NodeId, TypeConfig};
use opl_cluster::{ClusterError, Forwarder};
use opl_types::audit::{AuditEntry, AuditQuery};
use opl_types::metrics::MetricsQuery;
use tokio::runtime::Handle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Code, Request, Status, Streaming};
use uuid::Uuid;

use crate::firmware::{UefiVarsError, UefiVarsRequest};
use crate::metrics::MetricsReply;
use crate::migration::{
    IncomingError, IncomingMigration, MigrateIncomingRequest, MigrateIncomingResponse,
};
use crate::pki::NodeTls;
use crate::proto::peer_client::PeerClient;
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};

/// 연결을 맺기까지 기다리는 최대 시간
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 리더 요청 하나를 기다리는 최대 시간. 리더가 AddVoter에서 새 노드를 따라잡게 기다리는 시간
/// (`opl_cluster::CATCH_UP_TIMEOUT`, 60초)에 쓰기 시간을 더한 것보다 길어야 한다.
const LEADER_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

/// 받을 준비(`MigrateIncoming`)를 기다리는 최대 시간. 대상 노드가 VM 상태를 따라잡고(최대 10초),
/// 디스크를 켜고, QEMU를 띄우는 시간이다.
const MIGRATE_INCOMING_TIMEOUT: Duration = Duration::from_secs(120);

/// 받기 확인(`MigrateIncomingWait`)을 기다리는 최대 시간. 대상 노드의 드라이버가 받기를 최대 120초
/// 기다리므로(`incoming_wait`) 그보다 길어야 그 결과를 받는다.
const MIGRATE_INCOMING_WAIT_TIMEOUT: Duration = Duration::from_secs(180);

/// 받기 취소(`MigrateIncomingAbort`)를 기다리는 최대 시간. 받는 QEMU를 강제로 끄고 디스크를 끈다.
const MIGRATE_INCOMING_ABORT_TIMEOUT: Duration = Duration::from_secs(60);

/// UEFI 변수 파일 받기(`UefiVars`)를 기다리는 최대 시간. AAVMF 변수 파일은 64 MiB지만 대부분 0이라
/// 길이만 온다.
const UEFI_VARS_TIMEOUT: Duration = Duration::from_secs(60);

/// 다른 노드로 가는 연결. 주소마다 채널 하나를 만들어 다시 쓴다(끊기면 tonic이 다시 잇는다).
/// 연결할 때 이 노드의 인증서를 내밀고, 상대 인증서는 클러스터 CA로 확인한다.
///
/// 실행기를 주면(`with_runtimes`) 주소마다 채널을 둘 둔다. Raft·리더 전달(리스 갱신과 쓰기 전달)·ping은
/// control 채널, 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk 채널이고, 채널의 연결
/// 작업은 부른 쪽 실행기와 관계없이 그 채널의 실행기에서 돈다. 주지 않으면 채널 하나를 처음 부른
/// 쪽 실행기에서 돌린다.
pub struct Peers {
    tls: ClientTlsConfig,
    channels: Mutex<HashMap<(Lane, String), Channel>>,
    /// control·bulk 채널의 연결 작업을 돌릴 실행기
    runtimes: Option<(Handle, Handle)>,
}

/// 채널의 쓰임
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Lane {
    Control,
    Bulk,
}

/// tonic 채널의 작업(요청 버퍼, h2 연결)을 정한 실행기에 띄운다.
#[derive(Clone)]
struct OnRuntime(Handle);

impl<F> hyper::rt::Executor<F> for OnRuntime
where
    F: Future<Output = ()> + Send + 'static,
{
    fn execute(&self, fut: F) {
        self.0.spawn(fut);
    }
}

impl Peers {
    pub fn new(tls: &NodeTls) -> Self {
        Self {
            tls: ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&tls.ca_pem))
                .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem)),
            channels: Mutex::new(HashMap::new()),
            runtimes: None,
        }
    }

    /// control 채널의 연결 작업은 `control`, bulk 채널의 것은 `bulk` 실행기에서 돌린다.
    pub fn with_runtimes(mut self, control: Handle, bulk: Handle) -> Self {
        self.runtimes = Some((control, bulk));
        self
    }

    fn client(&self, addr: &str) -> Result<PeerClient<Channel>, Status> {
        self.client_on(Lane::Control, addr)
    }

    fn bulk_client(&self, addr: &str) -> Result<PeerClient<Channel>, Status> {
        self.client_on(Lane::Bulk, addr)
    }

    fn client_on(&self, lane: Lane, addr: &str) -> Result<PeerClient<Channel>, Status> {
        let (lane, executor) = match &self.runtimes {
            Some((control, bulk)) => match lane {
                Lane::Control => (lane, Some(control.clone())),
                Lane::Bulk => (lane, Some(bulk.clone())),
            },
            None => (Lane::Control, None),
        };
        let mut channels = self.channels.lock().unwrap();
        let channel = match channels.get(&(lane, addr.to_string())) {
            Some(channel) => channel.clone(),
            None => {
                let mut endpoint = Endpoint::from_shared(format!("https://{addr}"))
                    .and_then(|e| e.tls_config(self.tls.clone()))
                    .map_err(|e| Status::invalid_argument(format!("bad address {addr}: {e}")))?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
                    .keep_alive_timeout(KEEPALIVE_TIMEOUT)
                    .keep_alive_while_idle(true)
                    .tcp_keepalive(Some(KEEPALIVE_INTERVAL));
                if let Some(handle) = executor {
                    endpoint = endpoint.executor(OnRuntime(handle));
                }
                let channel = endpoint.connect_lazy();
                channels.insert((lane, addr.to_string()), channel.clone());
                channel
            }
        };
        Ok(PeerClient::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES))
    }

    /// `addr`의 노드가 응답하면 그 노드의 id를 돌려준다.
    pub async fn ping(&self, addr: &str, timeout: Duration) -> Result<NodeId, Status> {
        let mut req = Request::new(());
        req.set_timeout(timeout);
        Ok(self.client(addr)?.ping(req).await?.into_inner())
    }

    /// `addr`의 노드에 리더 요청을 보낸다. 그 노드가 리더가 아니면 `NotLeader`다.
    pub async fn leader_request(
        &self,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        let mut req = Request::new(req);
        req.set_timeout(LEADER_REQUEST_TIMEOUT);
        // 서버는 처리 결과를 늘 응답 본문에 담아 보내므로, `Status`는 전송 문제다. 새로 연결하지
        // 못하면 `Unavailable`이지만 요청 중에 끊기면 `Cancelled`, `Unknown`, `Internal` 등이 오므로
        // 권한·요청 오류가 아니면 모두 `Unreachable`로 돌려 다시 시도하게 한다.
        let reply = self
            .client(addr)
            .map_err(|s| ClusterError::Raft(s.to_string()))?
            .leader(req)
            .await
            .map_err(|s| match s.code() {
                Code::PermissionDenied | Code::Unauthenticated | Code::InvalidArgument => {
                    ClusterError::Raft(s.to_string())
                }
                _ => ClusterError::Unreachable(s.message().to_string()),
            })?;
        reply.into_inner().map_err(ClusterError::from)
    }

    /// `addr`의 노드에서 도는 VM의 콘솔(VNC 바이트 스트림)을 연다.
    pub async fn console(&self, addr: &str, vm: Uuid) -> Result<ConsoleStream, ConsoleError> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let mut req = Request::new(ReceiverStream::new(rx));
        let value = MetadataValue::try_from(vm.to_string())
            .map_err(|e| ConsoleError::Unavailable(e.to_string()))?;
        req.metadata_mut().insert(VM_METADATA, value);
        let unavailable = |s: Status| match s.code() {
            Code::FailedPrecondition => ConsoleError::NotRunning,
            _ => ConsoleError::Unavailable(s.message().to_string()),
        };
        let inbound = self
            .bulk_client(addr)
            .map_err(unavailable)?
            .console(req)
            .await
            .map_err(unavailable)?
            .into_inner();
        Ok(ConsoleStream {
            sender: ConsoleSender { tx },
            receiver: ConsoleReceiver { inbound },
        })
    }
}

impl Peers {
    /// `addr`의 노드에 VM을 받을 준비를 시킨다. 받는 QEMU가 연 포트(디스크를 복사하면 NBD 포트도)를
    /// 돌려준다.
    pub async fn migrate_incoming(
        &self,
        addr: &str,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let mut req = Request::new(req);
        req.set_timeout(MIGRATE_INCOMING_TIMEOUT);
        let reply = self
            .bulk_client(addr)
            .map_err(unreachable)?
            .migrate_incoming(req)
            .await
            .map_err(unreachable)?;
        reply.into_inner()
    }

    /// `addr`의 노드에서 받는 QEMU가 받기를 마칠 때까지 기다린다.
    pub async fn migrate_incoming_wait(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let mut req = Request::new(migration);
        req.set_timeout(MIGRATE_INCOMING_WAIT_TIMEOUT);
        self.bulk_client(addr)
            .map_err(unreachable)?
            .migrate_incoming_wait(req)
            .await
            .map_err(unreachable)?
            .into_inner()
    }

    /// `addr`의 노드에 확정 전의 마이그레이션을 버리게 한다(받는 QEMU를 끄고 디스크를 끈다).
    pub async fn migrate_incoming_abort(
        &self,
        addr: &str,
        migration: IncomingMigration,
    ) -> Result<(), IncomingError> {
        let mut req = Request::new(migration);
        req.set_timeout(MIGRATE_INCOMING_ABORT_TIMEOUT);
        self.bulk_client(addr)
            .map_err(unreachable)?
            .migrate_incoming_abort(req)
            .await
            .map_err(unreachable)?
            .into_inner()
    }
}

impl Peers {
    /// `addr`의 노드에서 VM의 UEFI 변수 파일을 받아 `dest`를 바꾼다(같은 디렉터리의 임시 파일에 받아
    /// fsync한 뒤 이름을 바꾼다). 받은 크기가 `expect_len`과 다르거나 실패하면 `dest`는 그대로다.
    /// 받은 파일의 크기를 돌려준다.
    pub async fn uefi_vars(
        &self,
        addr: &str,
        vm: Uuid,
        dest: &Path,
        expect_len: Option<u64>,
    ) -> Result<u64, UefiVarsError> {
        let mut req = Request::new(UefiVarsRequest { vm });
        req.set_timeout(UEFI_VARS_TIMEOUT);
        let chunks = self.bulk_client(addr)?.uefi_vars(req).await?.into_inner();
        crate::firmware::write_file(chunks, dest, expect_len).await
    }
}

impl Peers {
    /// `addr`의 노드의 감사 로그에서 `query`에 맞는 항목을 읽는다(새 것부터, 최대 `limit + 1`개).
    /// 그 노드가 감사 로그를 껐으면 None이다.
    pub async fn read_audit(
        &self,
        addr: &str,
        query: AuditQuery,
        timeout: Duration,
    ) -> Result<Option<Vec<AuditEntry>>, Status> {
        let mut req = Request::new(query);
        req.set_timeout(timeout);
        match self.bulk_client(addr)?.read_audit(req).await {
            Ok(reply) => Ok(Some(reply.into_inner().entries)),
            Err(s) if s.code() == Code::FailedPrecondition => Ok(None),
            Err(s) => Err(s),
        }
    }
}

impl Peers {
    /// `addr`의 노드의 메모리에 있는 지표를 읽는다. 그 노드가 지표를 모르면(옛 노드, 지표 소스가 없는
    /// 노드) None이다.
    pub async fn read_metrics(
        &self,
        addr: &str,
        query: MetricsQuery,
        timeout: Duration,
    ) -> Result<Option<MetricsReply>, Status> {
        let mut req = Request::new(query);
        req.set_timeout(timeout);
        match self.bulk_client(addr)?.read_metrics(req).await {
            Ok(reply) => Ok(Some(reply.into_inner())),
            Err(s) if s.code() == Code::Unimplemented => Ok(None),
            Err(s) => Err(s),
        }
    }
}

/// 받는 쪽의 처리 결과는 응답 본문에 오므로 gRPC 상태는 전송·권한 문제다.
fn unreachable(s: Status) -> IncomingError {
    IncomingError::Unavailable(s.message().to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum ConsoleError {
    #[error("the VM is not running on that node")]
    NotRunning,
    #[error("cannot open the console: {0}")]
    Unavailable(String),
}

/// 다른 노드의 VM 콘솔 연결
pub struct ConsoleStream {
    sender: ConsoleSender,
    receiver: ConsoleReceiver,
}

impl ConsoleStream {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.sender.send(data).await
    }

    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.receiver.recv().await
    }

    /// 보내는 쪽과 받는 쪽으로 나눈다(두 방향을 따로 돌릴 때).
    pub fn into_split(self) -> (ConsoleSender, ConsoleReceiver) {
        (self.sender, self.receiver)
    }
}

/// 콘솔 연결의 보내는 쪽
pub struct ConsoleSender {
    tx: tokio::sync::mpsc::Sender<Bytes>,
}

impl ConsoleSender {
    /// VNC로 보낸다. 연결이 끝났으면 false다.
    pub async fn send(&self, data: Bytes) -> bool {
        self.tx.send(data).await.is_ok()
    }
}

/// 콘솔 연결의 받는 쪽
pub struct ConsoleReceiver {
    inbound: Streaming<Bytes>,
}

impl ConsoleReceiver {
    /// VNC가 보낸 다음 바이트. 연결이 끝났으면 `None`이다.
    pub async fn recv(&mut self) -> Option<Bytes> {
        self.inbound.message().await.ok().flatten()
    }
}

#[async_trait]
impl Forwarder for Peers {
    async fn forward(
        &self,
        _leader: NodeId,
        addr: &str,
        req: LeaderRequest,
    ) -> Result<u64, ClusterError> {
        self.leader_request(addr, req).await
    }
}

/// gRPC로 가는 Raft 네트워크. `Cluster::open`에 넘긴다. `BasicNode.addr`는 노드의 클러스터 주소다.
#[derive(Clone)]
pub struct GrpcNetwork {
    peers: Arc<Peers>,
}

impl GrpcNetwork {
    pub fn new(peers: Arc<Peers>) -> Self {
        Self { peers }
    }
}

impl RaftNetworkFactory<TypeConfig> for GrpcNetwork {
    type Network = GrpcConnection;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> GrpcConnection {
        GrpcConnection {
            target,
            addr: node.addr.clone(),
            peers: self.peers.clone(),
        }
    }
}

pub struct GrpcConnection {
    target: NodeId,
    addr: String,
    peers: Arc<Peers>,
}

type RpcResult<T, E = openraft::error::Infallible> =
    Result<T, RPCError<NodeId, BasicNode, RaftError<NodeId, E>>>;

/// 연결하지 못한 것과 권한이 없는 것(멤버에서 빠진 노드)은 `Unreachable`로 돌려 openraft가 잠시
/// 쉬었다가 다시 시도하게 한다.
fn rpc_error<E: std::error::Error>(s: Status) -> RPCError<NodeId, BasicNode, RaftError<NodeId, E>> {
    if matches!(
        s.code(),
        Code::Unavailable | Code::PermissionDenied | Code::Unauthenticated
    ) {
        RPCError::Unreachable(Unreachable::new(&s))
    } else {
        RPCError::Network(NetworkError::new(&s))
    }
}

impl GrpcConnection {
    fn request<T>(&self, message: T, option: &RPCOption) -> Request<T> {
        let mut req = Request::new(message);
        req.set_timeout(option.hard_ttl());
        req
    }
}

impl RaftNetwork<TypeConfig> for GrpcConnection {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<AppendEntriesResponse<NodeId>> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .append_entries(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> RpcResult<InstallSnapshotResponse<NodeId>, InstallSnapshotError> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .install_snapshot(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> RpcResult<VoteResponse<NodeId>> {
        let mut client = self.peers.client(&self.addr).map_err(rpc_error)?;
        let reply = client
            .vote(self.request(rpc, &option))
            .await
            .map_err(rpc_error)?
            .into_inner();
        reply.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}
```

`crates/opl-peer/src/server.rs` 전체:

```rust
//! 다른 노드의 요청을 받는 gRPC 서버.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use opl_cluster::Cluster;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_driver::{DriverError, Hypervisor, uefi_vars_path};
use opl_types::audit::AuditQuery;
use opl_types::metrics::MetricsQuery;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::audit::{AuditReply, AuditSource};
use crate::firmware::{UefiVarsChunk, UefiVarsRequest};
use crate::metrics::{MetricsReply, MetricsSource};
use crate::migration::{IncomingMigration, MigrateIncomingRequest, MigrationReceiver};
use crate::pki::{NodeTls, node_id_of};
use crate::proto::peer_server::{Peer, PeerServer};
use crate::wire::{
    AppendEntriesReply, AppendEntriesRequest, IncomingReply, InstallSnapshotReply,
    InstallSnapshotRequest, LeaderReply, MigrateIncomingReply, VoteReply, VoteRequest,
};
use crate::{KEEPALIVE_INTERVAL, KEEPALIVE_TIMEOUT, MAX_MESSAGE_BYTES, VM_METADATA};

/// 다른 노드가 부르는 서비스. 상대 인증서의 노드 id가 이 노드가 아는 멤버여야 한다. 아직 멤버십을
/// 받지 못한 노드(막 합류하는 중)는 클러스터 CA가 서명한 인증서면 받는다.
pub struct PeerService {
    cluster: Arc<Cluster>,
    hv: Option<Arc<dyn Hypervisor>>,
    migrations: Option<Arc<dyn MigrationReceiver>>,
    audit: Option<Arc<dyn AuditSource>>,
    metrics: Option<Arc<dyn MetricsSource>>,
    /// 드라이버의 `state_dir`. UEFI 변수 파일을 여기서 찾아 보낸다.
    state_dir: Option<PathBuf>,
    /// Raft·리스가 아닌 처리(마이그레이션 받기, 콘솔 중계, UEFI 변수·감사 로그·지표 읽기)를 돌릴
    /// 실행기. 없으면 서버의 실행기에서 돈다.
    work: Option<Handle>,
}

impl PeerService {
    /// `hv`는 콘솔 중계에 쓴다. 없으면 콘솔 요청을 거부한다.
    pub fn new(cluster: Arc<Cluster>, hv: Option<Arc<dyn Hypervisor>>) -> Self {
        Self {
            cluster,
            hv,
            migrations: None,
            audit: None,
            metrics: None,
            state_dir: None,
            work: None,
        }
    }

    /// Raft·리스가 아닌 처리를 `work` 실행기에서 돌린다. 그 처리가 띄우는 작업(받은 VM의 QMP·모니터,
    /// 콘솔 중계, 파일 보내기)도 그 실행기에 생긴다.
    pub fn with_work(mut self, work: Handle) -> Self {
        self.work = Some(work);
        self
    }

    /// 드라이버의 `state_dir`에 있는 VM의 UEFI 변수 파일을 보낸다. 없으면 보내기 요청을 거부한다.
    pub fn with_uefi_vars(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(state_dir.into());
        self
    }

    /// 라이브 마이그레이션을 `receiver`로 받는다. 없으면 받기 요청을 거부한다.
    pub fn with_migrations(mut self, receiver: Arc<dyn MigrationReceiver>) -> Self {
        self.migrations = Some(receiver);
        self
    }

    /// 이 노드의 감사 로그를 `source`로 읽어 준다. 없으면 읽기 요청을 거부한다.
    pub fn with_audit(mut self, source: Arc<dyn AuditSource>) -> Self {
        self.audit = Some(source);
        self
    }

    /// 이 노드의 지표를 `source`로 읽어 준다. 없으면 읽기 요청을 `unimplemented`로 거부한다(옛 노드와 같다).
    pub fn with_metrics(mut self, source: Arc<dyn MetricsSource>) -> Self {
        self.metrics = Some(source);
        self
    }

    fn migrations(&self) -> Result<Arc<dyn MigrationReceiver>, Status> {
        self.migrations
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not receive migrations"))
    }

    /// `fut`을 `work` 실행기(없으면 여기)에서 돌리고 기다린다. 처리가 panic하면 다시 던지지 않고
    /// 로그를 남기고 `internal`로 답한다(같은 h2 연결의 Raft RPC를 함께 끊지 않게).
    async fn offload<F>(&self, fut: F) -> Result<F::Output, Status>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let Some(work) = &self.work else {
            return Ok(fut.await);
        };
        match work.spawn(fut).await {
            Ok(output) => Ok(output),
            Err(e) if e.is_panic() => {
                let panic = e.into_panic();
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|m| m.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::error!(panic = %message, "a peer request handler panicked");
                Err(Status::internal("the request handler panicked"))
            }
            Err(_) => Err(Status::unavailable("this node is shutting down")),
        }
    }

    fn authorize<T>(&self, req: &Request<T>) -> Result<(), Status> {
        let certs = req
            .peer_certs()
            .ok_or_else(|| Status::unauthenticated("client certificate required"))?;
        let id = certs
            .first()
            .and_then(|c| node_id_of(c))
            .ok_or_else(|| Status::unauthenticated("certificate does not name a node"))?;
        let members = self.cluster.members();
        let uninitialized = members.voters.is_empty() && members.learners.is_empty();
        if uninitialized || members.contains(NodeId(id)) {
            Ok(())
        } else {
            Err(Status::permission_denied(format!(
                "node {id} is not a member of this cluster"
            )))
        }
    }
}

#[tonic::async_trait]
impl Peer for PeerService {
    async fn append_entries(
        &self,
        req: Request<AppendEntriesRequest>,
    ) -> Result<Response<AppendEntriesReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().append_entries(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn install_snapshot(
        &self,
        req: Request<InstallSnapshotRequest>,
    ) -> Result<Response<InstallSnapshotReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().install_snapshot(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn vote(&self, req: Request<VoteRequest>) -> Result<Response<VoteReply>, Status> {
        self.authorize(&req)?;
        let reply = self.cluster.raft().vote(req.into_inner()).await;
        Ok(Response::new(reply))
    }

    async fn leader(&self, req: Request<LeaderRequest>) -> Result<Response<LeaderReply>, Status> {
        self.authorize(&req)?;
        let reply = self
            .cluster
            .execute_as_leader(req.into_inner())
            .await
            .map_err(Into::into);
        Ok(Response::new(reply))
    }

    async fn ping(&self, req: Request<()>) -> Result<Response<NodeId>, Status> {
        self.authorize(&req)?;
        Ok(Response::new(self.cluster.id()))
    }

    async fn migrate_incoming(
        &self,
        req: Request<MigrateIncomingRequest>,
    ) -> Result<Response<MigrateIncomingReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.migrations()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.incoming(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn migrate_incoming_wait(
        &self,
        req: Request<IncomingMigration>,
    ) -> Result<Response<IncomingReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.migrations()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.wait(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn migrate_incoming_abort(
        &self,
        req: Request<IncomingMigration>,
    ) -> Result<Response<IncomingReply>, Status> {
        self.authorize(&req)?;
        let (receiver, req) = (self.migrations()?, req.into_inner());
        let reply = self
            .offload(async move { receiver.abort(req).await })
            .await?;
        Ok(Response::new(reply))
    }

    async fn read_audit(&self, req: Request<AuditQuery>) -> Result<Response<AuditReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .audit
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not keep an audit log"))?;
        if !source.enabled() {
            return Err(Status::failed_precondition("audit log disabled"));
        }
        let mut query = req.into_inner();
        query.limit = query.limit.min(AuditQuery::MAX_LIMIT);
        let entries = self
            .offload(async move { source.read(query).await })
            .await?
            .map_err(Status::internal)?;
        Ok(Response::new(AuditReply { entries }))
    }

    async fn read_metrics(
        &self,
        req: Request<MetricsQuery>,
    ) -> Result<Response<MetricsReply>, Status> {
        self.authorize(&req)?;
        let source = self
            .metrics
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not keep metrics"))?;
        let query = req.into_inner();
        let reply = self.offload(async move { source.read(&query) }).await?;
        Ok(Response::new(reply))
    }

    type UefiVarsStream = ReceiverStream<Result<UefiVarsChunk, Status>>;

    async fn uefi_vars(
        &self,
        req: Request<UefiVarsRequest>,
    ) -> Result<Response<Self::UefiVarsStream>, Status> {
        self.authorize(&req)?;
        let dir = self
            .state_dir
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve UEFI variables"))?;
        let vm = req.into_inner().vm;
        let stream = self
            .offload(async move {
                let file = match tokio::fs::File::open(uefi_vars_path(&dir, vm)).await {
                    Ok(file) => file,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Err(Status::not_found(format!(
                            "no UEFI variables for VM {vm} on this node"
                        )));
                    }
                    Err(e) => {
                        return Err(Status::internal(format!(
                            "cannot open the UEFI variables of VM {vm}: {e}"
                        )));
                    }
                };
                let (tx, rx) = mpsc::channel(4);
                tokio::spawn(crate::firmware::send_file(file, tx));
                Ok(ReceiverStream::new(rx))
            })
            .await??;
        Ok(Response::new(stream))
    }

    type ConsoleStream = ReceiverStream<Result<Bytes, Status>>;

    async fn console(
        &self,
        req: Request<Streaming<Bytes>>,
    ) -> Result<Response<Self::ConsoleStream>, Status> {
        self.authorize(&req)?;
        let vm = req
            .metadata()
            .get(VM_METADATA)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| Uuid::try_parse(s).ok())
            .ok_or_else(|| Status::invalid_argument("missing VM id"))?;
        let hv = self
            .hv
            .clone()
            .ok_or_else(|| Status::unimplemented("this node does not serve consoles"))?;
        let inbound = req.into_inner();
        // VNC 소켓은 연 실행기에 묶이므로 연결과 중계를 함께 work에서 한다.
        let stream = self
            .offload(async move {
                let not_running =
                    || Status::failed_precondition(format!("VM {vm} is not running here"));
                let path = match hv.console_endpoint(vm).await {
                    Ok(path) => path,
                    Err(DriverError::NotRunning(_)) => return Err(not_running()),
                    Err(e) => return Err(Status::internal(e.to_string())),
                };
                let vnc = UnixStream::connect(&path)
                    .await
                    .map_err(|_| not_running())?;
                let (tx, rx) = mpsc::channel(16);
                tokio::spawn(bridge(inbound, vnc, tx));
                Ok(ReceiverStream::new(rx))
            })
            .await??;
        Ok(Response::new(stream))
    }
}

/// 요청 스트림을 VNC 소켓으로, VNC 소켓을 응답 스트림으로 잇는다. 어느 쪽이든 끝나면 모두 닫는다.
/// 두 방향은 따로 돈다. 한쪽 쓰기가 막혀도 다른 쪽은 계속 흐른다.
async fn bridge(
    mut inbound: Streaming<Bytes>,
    vnc: UnixStream,
    tx: mpsc::Sender<Result<Bytes, Status>>,
) {
    let (mut vnc_read, mut vnc_write) = vnc.into_split();
    let to_vnc = async {
        while let Ok(Some(data)) = inbound.message().await {
            if vnc_write.write_all(&data).await.is_err() {
                break;
            }
        }
    };
    let from_vnc = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match vnc_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx
                        .send(Ok(Bytes::copy_from_slice(&buf[..n])))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    tokio::select! {
        _ = to_vnc => {}
        _ = from_vnc => {}
    }
}

/// 노드 간 서버가 멈춘 이유
pub type ServeError = tonic::transport::Error;

/// `listener`에서 노드 간 gRPC를 받는다. 클라이언트 인증서가 클러스터 CA로 서명되어 있어야 한다.
/// `shutdown`이 끝나면 멈춘다.
pub async fn serve(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(&tls.cert_pem, &tls.key_pem))
        .client_ca_root(Certificate::from_pem(&tls.ca_pem));
    let service = PeerServer::new(service)
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    Server::builder()
        .http2_keepalive_interval(Some(KEEPALIVE_INTERVAL))
        .http2_keepalive_timeout(Some(KEEPALIVE_TIMEOUT))
        .tcp_keepalive(Some(KEEPALIVE_INTERVAL))
        .tls_config(tls)?
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), shutdown)
        .await
}
```

`crates/opl-peer/tests/grpc.rs` 전체:

```rust
//! 실제 gRPC + mTLS로 잇는 노드들(한 프로세스 안, 127.0.0.1의 빈 포트).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{Hypervisor, MockHypervisor, uefi_vars_path};
use opl_peer::pki::{ClusterCa, NodeTls, node_key_and_csr};
use opl_peer::{
    AuditSource, GrpcNetwork, IncomingDisk, IncomingError, IncomingMigration, MetricsReply,
    MetricsSource, MigrateIncomingRequest, MigrateIncomingResponse, MigrationReceiver, NodeMonitor,
    NodeMonitorConfig, PeerService, Peers, UefiVarsError,
};
use opl_types::audit::{AuditAuth, AuditEntry, AuditQuery};
use opl_types::metrics::{MetricsQuery, MetricsScope, NodeSample, NodeSeries};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{Volume, VolumeSpec};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use uuid::Uuid;

/// 테스트가 동시에 많이 돌아 CPU가 바쁘면 선거가 몇 초씩 걸린다(openraft는 리더 임기 동안 다른
/// 노드에 투표하지 않으므로 리더를 잃으면 보통 선거 제한 시간의 두 배쯤 걸린다).
const WAIT: Duration = Duration::from_secs(30);

fn tls_for(ca: &ClusterCa, id: NodeId) -> NodeTls {
    let (key_pem, csr) = node_key_and_csr().unwrap();
    NodeTls {
        ca_pem: ca.cert_pem.clone(),
        cert_pem: ca.sign_node(&csr, id.0, &["127.0.0.1".into()]).unwrap(),
        key_pem,
    }
}

fn volume(name: &str) -> Volume {
    Resource::new(
        name,
        VolumeSpec {
            pool: "local".into(),
            size_bytes: 1 << 30,
            format: ImageFormat::Qcow2,
        },
    )
}

struct Node {
    addr: String,
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    hv: Arc<MockHypervisor>,
    /// 이 노드의 드라이버 `state_dir`(UEFI 변수 파일을 보내는 곳)
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<()>>,
}

impl Node {
    async fn start(ca: &ClusterCa, root: &TempDir) -> Node {
        Self::start_with(ca, root, None, None, None, None).await
    }

    /// `migrations`가 있으면 마이그레이션을 그것으로 받고, `audit`이 있으면 감사 로그를, `metrics`가
    /// 있으면 지표를 그것으로 읽는다. `work`가 있으면 Raft·리스가 아닌 처리를 그 실행기에서 돌린다.
    async fn start_with(
        ca: &ClusterCa,
        root: &TempDir,
        migrations: Option<Arc<dyn MigrationReceiver>>,
        audit: Option<Arc<dyn AuditSource>>,
        metrics: Option<Arc<dyn MetricsSource>>,
        work: Option<tokio::runtime::Handle>,
    ) -> Node {
        let id = NodeId(Uuid::now_v7());
        let tls = tls_for(ca, id);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let peers = Arc::new(Peers::new(&tls));
        let mut config = ClusterConfig::new(id, root.path().join(id.to_string()));
        config.write_timeout = WAIT;
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 1000,
            election_timeout_max: 2000,
            ..Default::default()
        };
        let cluster = Arc::new(
            Cluster::open(config, GrpcNetwork::new(peers.clone()))
                .await
                .unwrap()
                .with_forwarder(peers.clone()),
        );
        let hv = Arc::new(MockHypervisor::default());
        let state_dir = root.path().join(format!("{id}-state"));
        let mut service =
            PeerService::new(cluster.clone(), Some(hv.clone() as Arc<dyn Hypervisor>))
                .with_uefi_vars(&state_dir);
        if let Some(migrations) = migrations {
            service = service.with_migrations(migrations);
        }
        if let Some(audit) = audit {
            service = service.with_audit(audit);
        }
        if let Some(metrics) = metrics {
            service = service.with_metrics(metrics);
        }
        if let Some(work) = work {
            service = service.with_work(work);
        }
        let (stop, rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            opl_peer::serve(listener, &tls, service, async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        Node {
            addr,
            cluster,
            peers,
            hv,
            state_dir,
            stop: Some(stop),
            server: Some(server),
        }
    }

    fn id(&self) -> NodeId {
        self.cluster.id()
    }

    async fn stop(&mut self) {
        // opld처럼 노드 간 서버가 끝난 뒤 Raft를 멈춘다. 먼저 멈추면 서버가 아직 받는 전달 요청에
        // 다시 시도하지 않는 "raft stopped"로 답한다.
        let _ = self.stop.take().unwrap().send(());
        self.server.take().unwrap().await.unwrap();
        self.cluster.shutdown().await.unwrap();
    }
}

/// 받은 요청을 적고 정해 둔 결과를 돌려주는 받는 쪽
#[derive(Default)]
struct FakeReceiver {
    calls: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl MigrationReceiver for FakeReceiver {
    async fn incoming(
        &self,
        req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        self.calls.lock().unwrap().push(format!(
            "incoming {} {} {} {:?}",
            req.vm, req.task, req.qemu_version, req.disks
        ));
        if req.qemu_version != "8.2.2" {
            return Err(IncomingError::Incompatible(format!(
                "QEMU {} differs from 8.2.2",
                req.qemu_version
            )));
        }
        // 복사할 디스크가 있으면 NBD 포트도 연다.
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: (!req.disks.is_empty()).then_some(60008),
        })
    }

    async fn wait(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wait {} {}", req.vm, req.task));
        Err(IncomingError::Failed("the receiving QEMU exited".into()))
    }

    async fn abort(&self, req: IncomingMigration) -> Result<(), IncomingError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("abort {} {}", req.vm, req.task));
        Ok(())
    }
}

/// 받은 조건을 적고 항목 하나를 돌려주는 감사 로그
#[derive(Default)]
struct FakeAudit {
    queries: std::sync::Mutex<Vec<AuditQuery>>,
}

#[async_trait::async_trait]
impl AuditSource for FakeAudit {
    async fn read(&self, query: AuditQuery) -> Result<Vec<AuditEntry>, String> {
        if query.user.as_deref() == Some("broken") {
            return Err("cannot read the audit log".into());
        }
        self.queries.lock().unwrap().push(query);
        Ok(vec![AuditEntry {
            id: Uuid::now_v7(),
            time: 1_000,
            node: "n2".into(),
            user: Some("alice".into()),
            user_id: None,
            role: None,
            auth: AuditAuth::Session,
            ip: "127.0.0.1".into(),
            method: "POST".into(),
            path: "/vms".into(),
            action: "POST /vms".into(),
            target: None,
            status: 201,
            duration_ms: 2,
            detail: None,
        }])
    }
}

/// 받은 조건을 적고 `since`보다 뒤의 점을 돌려주는 지표(노드 점은 `t` 1000·2000)
#[derive(Default)]
struct FakeMetrics {
    queries: std::sync::Mutex<Vec<MetricsQuery>>,
}

impl MetricsSource for FakeMetrics {
    fn read(&self, q: &MetricsQuery) -> MetricsReply {
        self.queries.lock().unwrap().push(q.clone());
        let samples = [1_000, 2_000]
            .into_iter()
            .filter(|t| q.since.is_none_or(|since| *t > since))
            .map(|t| NodeSample {
                t,
                cpu: Some(12.5),
                mem_used_mib: Some(100),
                mem_total_mib: Some(200),
                disk_read_bps: None,
                disk_write_bps: None,
                net_rx_bps: None,
                net_tx_bps: None,
                fs_used_mib: None,
                fs_total_mib: None,
            })
            .collect();
        MetricsReply {
            node: Some(NodeSeries {
                node: Uuid::nil(),
                samples,
            }),
            vms: vec![],
        }
    }
}

/// 노드 `n`개를 띄우고 첫 노드에서 클러스터를 초기화한 뒤 모두가 같은 리더를 알 때까지 기다린다.
async fn start(n: usize) -> (ClusterCa, TempDir, Vec<Node>) {
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let mut nodes = Vec::new();
    for _ in 0..n {
        nodes.push(Node::start(&ca, &root).await);
    }
    let members: BTreeMap<_, _> = nodes
        .iter()
        .map(|n| (n.id(), BasicNode::new(&n.addr)))
        .collect();
    nodes[0].cluster.initialize(members).await.unwrap();
    wait_leader(&nodes).await;
    (ca, root, nodes)
}

async fn wait_leader(nodes: &[Node]) -> NodeId {
    let deadline = Instant::now() + WAIT;
    loop {
        let leaders: Vec<_> = nodes
            .iter()
            .map(|n| n.cluster.raft().metrics().borrow().current_leader)
            .collect();
        if let Some(Some(leader)) = leaders.first()
            && leaders.iter().all(|l| *l == Some(*leader))
            && let Some(node) = nodes.iter().find(|n| n.id() == *leader)
            && node.cluster.raft().metrics().borrow().state == ServerState::Leader
        {
            return *leader;
        }
        assert!(Instant::now() < deadline, "no leader");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn nodes_replicate_and_forward_over_mtls() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();

    let v = volume("disk-a");
    nodes[follower].cluster.create(&v).await.unwrap();
    assert!(
        nodes[follower]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );

    // 리더가 죽어도 남은 두 노드로 쓰기가 이어진다.
    let leader = nodes.iter().position(|n| n.id() == leader).unwrap();
    nodes[leader].stop().await;
    let rest: Vec<usize> = (0..3).filter(|i| *i != leader).collect();
    let v = volume("disk-b");
    nodes[rest[0]].cluster.create(&v).await.unwrap();
    assert!(
        nodes[rest[0]]
            .cluster
            .get::<VolumeSpec>(v.id)
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn strangers_and_non_members_are_refused() {
    let (ca, root, nodes) = start(1).await;
    // 다른 CA의 인증서로는 TLS 연결이 맺어지지 않는다.
    let other = ClusterCa::generate("other").unwrap();
    let stranger = Peers::new(&tls_for(&other, NodeId(Uuid::now_v7())));
    assert!(stranger.ping(&nodes[0].addr, WAIT).await.is_err());
    // 같은 CA라도 멤버가 아니면 거부한다(제거된 노드).
    let outsider = Node::start(&ca, &root).await;
    let err = outsider.peers.ping(&nodes[0].addr, WAIT).await.unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err}");
    let err = outsider
        .peers
        .leader_request(&nodes[0].addr, LeaderRequest::Remove { id: nodes[0].id() })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a member"), "{err}");
    // 멤버는 된다.
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

/// 시험이 실패해도 따로 만든 실행기를 끝낸다(async 문맥에서 그냥 놓으면 panic한다).
struct Background(Option<tokio::runtime::Runtime>);

impl Background {
    fn new() -> Self {
        Self::of(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap(),
        )
    }

    fn of(rt: tokio::runtime::Runtime) -> Self {
        Self(Some(rt))
    }

    fn handle(&self) -> &tokio::runtime::Handle {
        self.0.as_ref().unwrap().handle()
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            rt.shutdown_background();
        }
    }
}

/// 실행기를 받은 `Peers`의 control 채널은 처음 연 실행기가 끝나도 된다(채널의 연결 작업이 받은
/// control 실행기에서 돈다). 실행기를 주지 않으면 처음 연 실행기와 함께 채널이 멈춘다.
#[tokio::test]
async fn control_channels_outlive_the_runtime_that_opened_them() {
    let (ca, _root, nodes) = start(1).await;
    let control = tokio::runtime::Handle::current();
    let bulk = Background::new();
    let peers = Arc::new(
        Peers::new(&tls_for(&ca, nodes[0].id())).with_runtimes(control, bulk.handle().clone()),
    );
    let addr = nodes[0].addr.clone();

    // 따로 만든 실행기(work·api에 해당)가 먼저 채널을 열고 끝난다.
    let first = Background::new();
    let opened = {
        let (peers, addr) = (peers.clone(), addr.clone());
        first
            .handle()
            .spawn(async move { peers.ping(&addr, WAIT).await })
    };
    assert_eq!(opened.await.unwrap().unwrap(), nodes[0].id());
    drop(first);

    assert_eq!(peers.ping(&addr, WAIT).await.unwrap(), nodes[0].id());
}

#[tokio::test]
async fn a_new_node_joins_as_learner_then_voter() {
    let (ca, root, mut nodes) = start(1).await;
    let rev = nodes[0].cluster.create(&volume("before")).await.unwrap();

    let new = Node::start(&ca, &root).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: new.id(),
            addr: new.addr.clone(),
        })
        .await
        .unwrap();
    new.cluster
        .raft()
        .wait(Some(WAIT))
        .applied_index_at_least(Some(rev), "learner caught up")
        .await
        .unwrap();
    // 새 노드가 스스로 승격을 요청한다(리더로 전달된다).
    new.cluster
        .request(LeaderRequest::AddVoter { id: new.id() })
        .await
        .unwrap();
    let members = new.cluster.members();
    assert_eq!(members.voters.len(), 2);
    assert!(members.voters.contains_key(&new.id()));
    assert!(
        new.cluster
            .get_by_name::<VolumeSpec>("before")
            .unwrap()
            .is_some()
    );
    nodes.push(new);
    // 새 노드에서 한 쓰기도 커밋된다(이제 과반수는 2).
    nodes[1].cluster.create(&volume("after")).await.unwrap();
}

#[tokio::test]
async fn console_is_relayed_to_the_node_running_the_vm() {
    let (_ca, _root, nodes) = start(2).await;
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let target = &nodes[1];
    target.hv.set_console_dir(tmp.path());
    let vm = Uuid::now_v7();
    let spec: VmRuntimeSpec = serde_json::from_value(serde_json::json!({
        "id": vm, "name": "web", "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256,
        "firmware": "uefi", "machine": "virt", "disks": [], "nics": []
    }))
    .unwrap();
    target.hv.start(&spec, None).await.unwrap();
    let listener = tokio::net::UnixListener::bind(tmp.path().join(format!("{vm}.vnc"))).unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"RFB 003.008\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = conn.read(&mut buf).await.unwrap();
        conn.write_all(&buf[..n]).await.unwrap();
    });

    let mut console = nodes[0].peers.console(&target.addr, vm).await.unwrap();
    assert_eq!(&console.recv().await.unwrap()[..], b"RFB 003.008\n");
    assert!(console.send(bytes::Bytes::from_static(b"ping")).await);
    assert_eq!(&console.recv().await.unwrap()[..], b"ping");

    // 그 노드에서 돌지 않는 VM은 거부한다.
    let err = nodes[0]
        .peers
        .console(&nodes[0].addr, vm)
        .await
        .err()
        .unwrap();
    assert!(matches!(err, opl_peer::ConsoleError::NotRunning), "{err}");
}

#[tokio::test]
async fn leader_records_when_nodes_go_offline_and_come_back() {
    let (_ca, _root, mut nodes) = start(3).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        // 하나는 오프라인으로 시작해, 응답하면 온라인이 되는지 본다.
        r.status.online = node.id() == leader;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let leader_cluster = nodes
        .iter()
        .find(|n| n.id() == leader)
        .unwrap()
        .cluster
        .clone();
    let online = |id: NodeId| {
        leader_cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };
    let wait_for = |want: bool, id: NodeId| {
        let online = &online;
        async move {
            let deadline = Instant::now() + WAIT;
            while online(id) != want {
                assert!(
                    Instant::now() < deadline,
                    "node {id} never became online={want}"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    };
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let follower_id = nodes[follower].id();
    wait_for(true, follower_id).await;
    nodes[follower].stop().await;
    wait_for(false, follower_id).await;
    assert!(online(leader));
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn new_leader_does_not_revive_nodes_recorded_offline() {
    let (_ca, _root, mut nodes) = start(5).await;
    let leader = wait_leader(&nodes).await;
    for (i, node) in nodes.iter().enumerate() {
        let mut r: Resource<NodeSpec, NodeStatus> = Resource::new(
            format!("node-{i}"),
            NodeSpec {
                address: node.addr.clone(),
                maintenance: false,
                witness: false,
            },
        );
        r.id = node.id().0;
        r.status.online = true;
        nodes[0].cluster.create(&r).await.unwrap();
    }
    let config = NodeMonitorConfig {
        interval: Duration::from_millis(300),
        offline_after: Duration::from_secs(3),
    };
    let monitors: Vec<NodeMonitor> = nodes
        .iter()
        .map(|n| NodeMonitor::spawn(n.cluster.clone(), n.peers.clone(), config.clone()))
        .collect();
    let online = |cluster: &Cluster, id: NodeId| {
        cluster
            .get::<NodeSpec>(id.0)
            .unwrap()
            .unwrap()
            .status
            .online
    };

    // 팔로워 하나를 멈추고 리더가 오프라인으로 기록할 때까지 기다린다.
    let follower = nodes.iter().position(|n| n.id() != leader).unwrap();
    let mut dead = nodes.remove(follower);
    let dead_id = dead.id();
    dead.stop().await;
    let old = nodes.iter().position(|n| n.id() == leader).unwrap();
    let deadline = Instant::now() + WAIT;
    while online(&nodes[old].cluster, dead_id) {
        assert!(
            Instant::now() < deadline,
            "dead follower never went offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 리더도 멈추면 새 리더가 그 기록을 다시 온라인으로 바꾸지 않아야 한다.
    let mut old = nodes.remove(old);
    old.stop().await;
    let new_leader = wait_leader(&nodes).await;
    let new_leader = nodes.iter().find(|n| n.id() == new_leader).unwrap();
    // 새 리더가 이전 커밋을 모두 적용한 뒤부터 본다. openraft는 리더 확인 RPC를 하트비트 간격
    // (50ms)만 기다리고, 노드 둘이 죽어 남는 여유가 없어 CPU가 바쁘면 한 번에 실패할 수 있다.
    // 운영 코드(노드 감시, 예약 작업)처럼 다시 한다.
    let deadline = Instant::now() + WAIT;
    while let Err(e) = new_leader.cluster.read_barrier().await {
        assert!(
            Instant::now() < deadline,
            "read barrier never succeeded: {e}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 감시 간격보다 길고 `offline_after`보다 짧게 지켜본다.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            !online(&new_leader.cluster, dead_id),
            "new leader revived a node recorded offline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for m in monitors {
        m.shutdown().await;
    }
}

#[tokio::test]
async fn migration_requests_reach_the_receiver_of_a_member() {
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(FakeReceiver::default());
    let target = Node::start_with(&ca, &root, Some(receiver.clone()), None, None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    // 대상 노드가 멤버십을 받아 원본 노드를 멤버로 알 때까지 기다린다.
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (vm, task) = (Uuid::now_v7(), Uuid::now_v7());
    let peers = &nodes[0].peers;
    let req = |version: &str| MigrateIncomingRequest {
        vm,
        task,
        qemu_version: version.into(),
        disks: vec![],
    };
    assert_eq!(
        peers
            .migrate_incoming(&target.addr, req("8.2.2"))
            .await
            .unwrap(),
        MigrateIncomingResponse {
            port: 60007,
            nbd_port: None
        }
    );
    // 복사할 디스크는 요청에 실려 가고 NBD 포트가 돌아온다.
    let disk = IncomingDisk {
        volume: Uuid::now_v7(),
        size_bytes: 1 << 30,
    };
    let reply = peers
        .migrate_incoming(
            &target.addr,
            MigrateIncomingRequest {
                disks: vec![disk],
                ..req("8.2.2")
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.nbd_port, Some(60008));
    // 받는 쪽의 거절과 실패는 그 종류 그대로 온다.
    let err = peers
        .migrate_incoming(&target.addr, req("9.0.0"))
        .await
        .unwrap_err();
    assert!(matches!(err, IncomingError::Incompatible(_)), "{err:?}");
    let migration = IncomingMigration { vm, task };
    let err = peers
        .migrate_incoming_wait(&target.addr, migration)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        IncomingError::Failed("the receiving QEMU exited".into())
    );
    peers
        .migrate_incoming_abort(&target.addr, migration)
        .await
        .unwrap();
    assert_eq!(
        *receiver.calls.lock().unwrap(),
        [
            format!("incoming {vm} {task} 8.2.2 []"),
            format!("incoming {vm} {task} 8.2.2 {:?}", [disk]),
            format!("incoming {vm} {task} 9.0.0 []"),
            format!("wait {vm} {task}"),
            format!("abort {vm} {task}"),
        ]
    );

    // 받는 처리가 없는 노드는 거부한다.
    let err = target
        .peers
        .migrate_incoming_abort(&nodes[0].addr, migration)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("does not receive migrations")),
        "{err:?}"
    );
    // 멤버가 아닌 노드의 요청은 받는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .migrate_incoming(&target.addr, req("8.2.2"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert_eq!(receiver.calls.lock().unwrap().len(), 5);
}

/// 받는 처리가 돈 스레드와, 그 처리가 띄운 작업(받은 VM의 QMP·모니터에 해당)이 돈 스레드를 적는다.
#[derive(Default)]
struct ThreadReceiver {
    threads: std::sync::Mutex<Vec<String>>,
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or_default()
        .to_string()
}

#[async_trait::async_trait]
impl MigrationReceiver for ThreadReceiver {
    async fn incoming(
        &self,
        _req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        let spawned = tokio::spawn(async { thread_name() }).await.unwrap();
        self.threads
            .lock()
            .unwrap()
            .extend([thread_name(), spawned]);
        Ok(MigrateIncomingResponse {
            port: 60007,
            nbd_port: None,
        })
    }

    async fn wait(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }

    async fn abort(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }
}

/// `work` 실행기를 받은 노드 간 서버는 마이그레이션 받기를 그 실행기에서 돌린다. 받는 처리가 띄운
/// 작업도 그 실행기에 생긴다(노드 간 서버의 실행기에는 Raft·리스만 남는다).
#[tokio::test]
async fn incoming_migrations_run_on_the_work_runtime() {
    let work = Background::of(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("peer-work")
            .enable_all()
            .build()
            .unwrap(),
    );
    let (ca, root, nodes) = start(1).await;
    let receiver = Arc::new(ThreadReceiver::default());
    let target = Node::start_with(
        &ca,
        &root,
        Some(receiver.clone()),
        None,
        None,
        Some(work.handle().clone()),
    )
    .await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(nodes[0].id()) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let req = MigrateIncomingRequest {
        vm: Uuid::now_v7(),
        task: Uuid::now_v7(),
        qemu_version: "8.2.2".into(),
        disks: vec![],
    };
    nodes[0]
        .peers
        .migrate_incoming(&target.addr, req)
        .await
        .unwrap();
    assert_eq!(
        *receiver.threads.lock().unwrap(),
        ["peer-work", "peer-work"]
    );
}

/// 받는 처리가 panic한다.
struct PanickingReceiver;

#[async_trait::async_trait]
impl MigrationReceiver for PanickingReceiver {
    async fn incoming(
        &self,
        _req: MigrateIncomingRequest,
    ) -> Result<MigrateIncomingResponse, IncomingError> {
        panic!("receiver bug");
    }

    async fn wait(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }

    async fn abort(&self, _req: IncomingMigration) -> Result<(), IncomingError> {
        Ok(())
    }
}

/// `work` 실행기에서 돈 처리가 panic하면 그 요청만 `internal`로 답하고 서버는 계속 답한다.
#[tokio::test]
async fn a_panicking_handler_answers_internal() {
    let work = Background::new();
    let ca = ClusterCa::generate("test").unwrap();
    let root = TempDir::new().unwrap();
    let node = Node::start_with(
        &ca,
        &root,
        Some(Arc::new(PanickingReceiver)),
        None,
        None,
        Some(work.handle().clone()),
    )
    .await;
    let members = BTreeMap::from([(node.id(), BasicNode::new(&node.addr))]);
    node.cluster.initialize(members).await.unwrap();
    let nodes = [node];
    wait_leader(&nodes).await;

    let req = MigrateIncomingRequest {
        vm: Uuid::now_v7(),
        task: Uuid::now_v7(),
        qemu_version: "8.2.2".into(),
        disks: vec![],
    };
    let err = nodes[0]
        .peers
        .migrate_incoming(&nodes[0].addr, req)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, IncomingError::Unavailable(m) if m.contains("panicked")),
        "{err:?}"
    );
    assert_eq!(
        nodes[0].peers.ping(&nodes[0].addr, WAIT).await.unwrap(),
        nodes[0].id()
    );
}

#[tokio::test]
async fn uefi_vars_are_fetched_from_a_member() {
    let (ca, root, nodes) = start(2).await;
    let (from, to) = (&nodes[1], &nodes[0]);
    let vm = Uuid::now_v7();
    // 데이터 조각, 0만 있는 조각 둘, 끝이 짧은 조각(마지막 바이트만 0이 아님)
    let chunk = opl_peer::firmware::CHUNK_BYTES;
    let mut vars: Vec<u8> = (0..chunk).map(|i| (i % 251) as u8 + 1).collect();
    vars.resize(3 * chunk + 100, 0);
    *vars.last_mut().unwrap() = 0xaa;
    let src = uefi_vars_path(&from.state_dir, vm);
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, &vars).unwrap();

    // 받는 노드의 옛 파일을 바꾼다.
    let dest = uefi_vars_path(&to.state_dir, vm);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, b"stale").unwrap();
    let len = to
        .peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(len, vars.len() as u64);
    assert!(std::fs::read(&dest).unwrap() == vars, "content differs");

    // 0만 있는 파일, 빈 파일도 크기 그대로 온다.
    std::fs::write(&src, vec![0u8; chunk + 1]).unwrap();
    to.peers
        .uefi_vars(&from.addr, vm, &dest, None)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), vec![0u8; chunk + 1]);
    std::fs::write(&src, b"").unwrap();
    assert_eq!(
        to.peers
            .uefi_vars(&from.addr, vm, &dest, None)
            .await
            .unwrap(),
        0
    );
    assert!(std::fs::read(&dest).unwrap().is_empty());

    // 그 노드에 파일이 없으면 NotFound이고 받는 쪽 파일은 그대로다.
    std::fs::write(&dest, b"kept").unwrap();
    let err = to
        .peers
        .uefi_vars(&from.addr, Uuid::now_v7(), &dest, None)
        .await
        .unwrap_err();
    assert_eq!(err, UefiVarsError::NotFound);
    assert_eq!(std::fs::read(&dest).unwrap(), b"kept");

    // 멤버가 아닌 노드에는 보내지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .uefi_vars(&from.addr, vm, &root.path().join("outsider.fd"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, UefiVarsError::Unavailable(m) if m.contains("not a member")),
        "{err:?}"
    );
    assert!(!root.path().join("outsider.fd").exists());
}

#[tokio::test]
async fn audit_logs_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let audit = Arc::new(FakeAudit::default());
    let target = Node::start_with(&ca, &root, None, Some(audit.clone()), None, None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |limit, user: Option<&str>| AuditQuery {
        since: 10,
        until: 20_000,
        user: user.map(Into::into),
        action: Some("/vms".into()),
        limit,
    };
    let peers = &nodes[0].peers;
    let entries = peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "POST /vms");
    // 너무 큰 limit은 받는 쪽이 줄인다.
    peers
        .read_audit(&target.addr, query(1_000_000, None), WAIT)
        .await
        .unwrap();
    assert_eq!(
        *audit.queries.lock().unwrap(),
        [query(7, None), query(AuditQuery::MAX_LIMIT, None)]
    );
    // 읽기 실패는 오류로 온다.
    let err = peers
        .read_audit(&target.addr, query(7, Some("broken")), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Internal, "{err:?}");

    // 감사 로그가 없는 노드는 거부한다.
    let err = target
        .peers
        .read_audit(&nodes[0].addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::Unimplemented, "{err:?}");
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_audit(&target.addr, query(7, None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(audit.queries.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn metrics_are_read_from_a_member() {
    let (ca, root, nodes) = start(1).await;
    let metrics = Arc::new(FakeMetrics::default());
    let target = Node::start_with(&ca, &root, None, None, Some(metrics.clone()), None).await;
    nodes[0]
        .cluster
        .request(LeaderRequest::AddLearner {
            id: target.id(),
            addr: target.addr.clone(),
        })
        .await
        .unwrap();
    let source = nodes[0].id();
    let deadline = Instant::now() + WAIT;
    while !target.cluster.members().contains(source) {
        assert!(
            Instant::now() < deadline,
            "membership never reached the target"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let query = |since| MetricsQuery {
        scope: MetricsScope::Nodes,
        since,
    };
    let peers = &nodes[0].peers;
    let reply = peers
        .read_metrics(&target.addr, query(Some(1_000)), WAIT)
        .await
        .unwrap()
        .unwrap();
    let samples = reply.node.unwrap().samples;
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].t, 2_000);
    assert_eq!(samples[0].cpu, Some(12.5));
    assert_eq!(*metrics.queries.lock().unwrap(), [query(Some(1_000))]);

    // 지표 소스가 없는 노드(옛 노드처럼)는 None이다.
    let none = target
        .peers
        .read_metrics(&nodes[0].addr, query(None), WAIT)
        .await
        .unwrap();
    assert_eq!(none, None);
    // 멤버가 아닌 노드의 요청은 읽는 쪽에 닿지 않는다.
    let outsider = Node::start(&ca, &root).await;
    let err = outsider
        .peers
        .read_metrics(&target.addr, query(None), WAIT)
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::PermissionDenied, "{err:?}");
    assert_eq!(metrics.queries.lock().unwrap().len(), 1);
}
```

`crates/opl-reconciler/src/ha.rs` 전체:

```rust
//! 노드 쪽 HA 펜싱(`HaGuard`). 리더에게 리스를 갱신하고, 리스가 살아 있는지(`fresh`)와 끝났는지
//! (`stale`)를 알려 주고, watchdog 장치를 쓰다듬는다. 규칙은 설계 문서
//! (2026-11-02-ha-fencing-witness-design.md) §3.2·§3.3을 따른다. 쓰다듬기는 실행기가 막혀도 멈추지
//! 않게 전용 스레드(`orbivisor-watchdog`)에서 한다(2026-11-11-availability-scale-design.md §3.1).

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_cluster::{Cluster, ClusterError};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use uuid::Uuid;

/// watchdog를 쓰다듬는 간격
const PING_INTERVAL: Duration = Duration::from_secs(1);

/// 리더에게 이 노드의 리스를 갱신한다. 시험은 가짜를 쓴다.
#[async_trait]
pub trait LeaseRenewer: Send + Sync {
    /// `timeout` 안에 리더가 갱신을 확인하고 그 인덱스까지 이 노드에 적용되면 성공이다.
    async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError>;
}

#[async_trait]
impl LeaseRenewer for Cluster {
    async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
        Cluster::renew_lease(self, timeout).await
    }
}

/// 연 watchdog 장치. 쓰다듬지 않고 제한 시간이 지나면 노드가 재시작된다.
pub trait Watchdog: Send {
    fn ping(&mut self) -> io::Result<()>;
    /// `V`를 쓰고 닫아 타이머를 멈춘다(magic close). 커널이 `nowayout`이면 멈추지 않는다.
    fn magic_close(self: Box<Self>) -> io::Result<()>;
    /// 장치가 제한 시간을 받아들이지 않았으면 그 이유. 그런 장치도 닫지 않고 쥐어 평소처럼 쓰다듬지만
    /// HA VM에는 쓰지 않는다(`arm` 실패).
    fn rejected(&self) -> Option<&str> {
        None
    }
}

/// watchdog 장치를 연다. 열면 장치의 타이머가 (다시) 돈다.
pub trait WatchdogOpener: Send + Sync {
    fn open(&self) -> io::Result<Box<dyn Watchdog>>;
}

/// Linux watchdog 장치(`/dev/watchdog`, 하드웨어 또는 `softdog`). 다른 OS에서는 열기가 실패한다.
#[derive(Debug, Clone)]
pub struct DeviceWatchdog {
    pub path: PathBuf,
    /// 장치에 정하는 제한 시간(초, `WDIOC_SETTIMEOUT`)
    pub timeout_secs: u64,
}

/// `WDIOC_SETTIMEOUT` = `_IOWR('W', 6, int)`
const WDIOC_SETTIMEOUT: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read_write::<std::ffi::c_int>(b'W', 6);
/// `WDIOC_GETTIMEOUT` = `_IOR('W', 7, int)`
const WDIOC_GETTIMEOUT: rustix::ioctl::Opcode =
    rustix::ioctl::opcode::read::<std::ffi::c_int>(b'W', 7);

impl WatchdogOpener for DeviceWatchdog {
    fn open(&self) -> io::Result<Box<dyn Watchdog>> {
        if !cfg!(target_os = "linux") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "watchdog devices need Linux",
            ));
        }
        let mut timeout = std::ffi::c_int::try_from(self.timeout_secs).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "watchdog timeout too large")
        })?;
        let fd = rustix::fs::open(
            &self.path,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        // SAFETY: WDIOC_SETTIMEOUT는 int 하나를 읽고, 장치가 실제로 정한 값을 그 자리에 쓴다.
        let set = unsafe {
            rustix::ioctl::ioctl(
                &fd,
                rustix::ioctl::Updater::<WDIOC_SETTIMEOUT, std::ffi::c_int>::new(&mut timeout),
            )
        }
        .map(|()| timeout)
        .map_err(io::Error::from);
        // SAFETY: WDIOC_GETTIMEOUT는 지금 제한 시간을 int 하나에 쓴다.
        let get = || unsafe {
            rustix::ioctl::ioctl(
                &fd,
                rustix::ioctl::Getter::<WDIOC_GETTIMEOUT, std::ffi::c_int>::new(),
            )
            .map_err(io::Error::from)
        };
        // 받아들이지 못해도 닫지 않고 쥐어 둔다. 열면 타이머가 돌므로 그냥 닫으면 나중에 노드가 재시작되고,
        // 열기는 리스가 살아 있을 때만 하므로 쥔 채 쓰다듬고 정상 종료 때 멈춰도 된다(`HaGuard`).
        let actual = match accept_timeout(self.timeout_secs, set, get) {
            Ok(actual) => actual,
            Err(e) => {
                let reason = format!(
                    "cannot set the watchdog timeout to {}s: {e}",
                    self.timeout_secs
                );
                return Ok(Box::new(Device {
                    fd,
                    rejected: Some(reason),
                }));
            }
        };
        if actual != self.timeout_secs {
            tracing::warn!(
                device = %self.path.display(),
                requested = self.timeout_secs,
                actual,
                "watchdog device chose a shorter timeout"
            );
        }
        Ok(Box::new(Device { fd, rejected: None }))
    }
}

/// 장치의 제한 시간을 받아들일지 정한다. `set`은 `WDIOC_SETTIMEOUT`의 결과(장치가 정한 값)이고,
/// 실패했으면 `get`(`WDIOC_GETTIMEOUT`)으로 지금 값을 읽는다. 요청보다 길면 리스가 끝난 뒤에도 노드가
/// 늦게 재시작되어 리더의 복구와 겹칠 수 있으므로 받아들이지 않는다.
fn accept_timeout(
    requested: u64,
    set: io::Result<std::ffi::c_int>,
    get: impl FnOnce() -> io::Result<std::ffi::c_int>,
) -> io::Result<u64> {
    let ok = |t: std::ffi::c_int| {
        u64::try_from(t)
            .ok()
            .filter(|t| (1..=requested).contains(t))
    };
    match set {
        Ok(t) => ok(t).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the device chose {t}s"),
            )
        }),
        Err(e) => get().ok().and_then(ok).ok_or(e),
    }
}

struct Device {
    fd: rustix::fd::OwnedFd,
    /// 제한 시간을 받아들이지 않은 이유
    rejected: Option<String>,
}

impl Watchdog for Device {
    fn ping(&mut self) -> io::Result<()> {
        rustix::io::write(&self.fd, b"\0")?;
        Ok(())
    }

    fn magic_close(self: Box<Self>) -> io::Result<()> {
        rustix::io::write(&self.fd, b"V")?;
        Ok(())
    }

    fn rejected(&self) -> Option<&str> {
        self.rejected.as_deref()
    }
}

/// `HaGuard` 설정. opld가 `[ha]`에서 만든다.
#[derive(Clone)]
pub struct HaConfig {
    /// 마지막으로 성공한 갱신(보내기 전 시각) 뒤 이만큼 지나면 리스가 끝난다.
    pub fence_after: Duration,
    /// 갱신 간격. 한 번의 갱신은 이 두 배까지 기다린다.
    pub renew_interval: Duration,
    /// watchdog 장치. None이면 `process` 펜싱(워커가 끄는 것만)이다.
    pub watchdog: Option<Arc<dyn WatchdogOpener>>,
}

/// 리스 상태. 바뀌면 [`HaGuard::subscribe`]로 알린다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseState {
    /// 이 프로세스에서 갱신에 성공했고 마지막 갱신 뒤 `fence_after`가 지나지 않았다.
    pub fresh: bool,
    /// 시작이나 마지막 갱신 중 늦은 것에서 `fence_after`가 지났다. HA VM을 꺼야 한다.
    pub stale: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ArmError {
    #[error("the HA lease is not fresh")]
    NotFresh,
    #[error("cannot open the watchdog device: {0}")]
    Open(io::Error),
    #[error("the watchdog device is unusable: {0}")]
    Rejected(String),
    #[error("the HA guard is closed")]
    Closed,
}

struct State {
    /// 마지막으로 성공한 갱신을 보내기 전 시각
    last_renewed: Option<Instant>,
    /// 이 노드에서 도는 HA VM
    running: HashSet<Uuid>,
    /// 연 watchdog 장치(장치는 쓰다듬기 스레드가 쥔다). 안쪽은 장치가 제한 시간을 받아들이지 않은
    /// 이유다. None이면 아직 열지 않았다.
    watchdog: Option<Option<String>>,
}

struct Shared {
    /// 기준점. 다시 시작한 opld가 갱신하기 전에 곧바로 끄지 않게 한다.
    start: Instant,
    fence_after: Duration,
    state: Mutex<State>,
    tx: watch::Sender<LeaseState>,
    /// 갱신에 성공하면 알린다(상태가 바뀔 다음 시각이 늦춰진다).
    renewed: Notify,
    /// 쓰다듬기 스레드가 잠금 없이 읽는 값들. 시각은 `epoch`에서 지난 밀리초 + 1이고 0은 갱신 없음이다.
    epoch: std::time::Instant,
    renewed_at: AtomicU64,
    /// 이 노드에서 도는 HA VM이 있다(`State::running`이 비지 않았다).
    running_any: AtomicBool,
}

impl Shared {
    /// 쓰다듬기 스레드가 보는 `fresh`. 실행기가 막혀 갱신 루프가 돌지 않아도 스레드가 스스로 시간을
    /// 재어 판단한다. 갱신 루프가 이 시각을 async 시각보다 먼저 재고 밀리초를 버림하므로 `lease`의
    /// `fresh`보다 늦게 끝나지 않는다.
    fn fresh_for_ping(&self) -> bool {
        match self.renewed_at.load(Ordering::Acquire) {
            0 => false,
            at => self.epoch.elapsed() < Duration::from_millis(at - 1) + self.fence_after,
        }
    }

    /// 성공한 갱신을 보내기 전 시각(`sent`)을 쓰다듬기 스레드에 알린다.
    fn record_renewal(&self, sent: std::time::Instant) {
        let at = u64::try_from(sent.saturating_duration_since(self.epoch).as_millis())
            .unwrap_or(u64::MAX - 1)
            + 1;
        self.renewed_at.fetch_max(at, Ordering::AcqRel);
    }

    fn set_running_any(&self, state: &State) {
        self.running_any
            .store(!state.running.is_empty(), Ordering::Release);
    }

    fn lease(&self, state: &State, now: Instant) -> LeaseState {
        let fresh = state
            .last_renewed
            .is_some_and(|s| now.saturating_duration_since(s) < self.fence_after);
        let base = state.last_renewed.map_or(self.start, |s| s.max(self.start));
        LeaseState {
            fresh,
            stale: now.saturating_duration_since(base) >= self.fence_after,
        }
    }

    /// 지금 상태를 알리고, 갱신 없이 상태가 바뀔 다음 시각을 돌려준다. 이미 stale이면 None이다.
    fn publish(&self) -> Option<Instant> {
        let state = self.state.lock().unwrap();
        let now = Instant::now();
        let lease = self.lease(&state, now);
        self.tx.send_if_modified(|old| {
            let changed = *old != lease;
            *old = lease;
            changed
        });
        // 갱신했으면 마지막 갱신이 기준점보다 늦으므로 fresh와 stale이 같은 때 바뀐다.
        let next = state.last_renewed.map_or(self.start, |s| s.max(self.start)) + self.fence_after;
        (next > now).then_some(next)
    }
}

/// 노드의 HA 리스와 watchdog. 갱신 루프와 쓰다듬기 루프를 뒤에서 돌린다. 정상 종료 때 [`close`]한다.
///
/// [`close`]: HaGuard::close
pub struct HaGuard {
    shared: Arc<Shared>,
    opener: Option<Arc<dyn WatchdogOpener>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// 쓰다듬기 스레드. watchdog 장치를 쓸 때(`opener`가 있을 때)만 띄운다.
    pinger: Mutex<Option<Pinger>>,
}

/// 쓰다듬기 스레드. 연 장치를 `tx`로 받고, `tx`가 닫히면 쥔 장치를 돌려주며 끝난다.
struct Pinger {
    tx: mpsc::Sender<Box<dyn Watchdog>>,
    thread: std::thread::JoinHandle<Option<Box<dyn Watchdog>>>,
}

impl HaGuard {
    /// 지금을 기준점으로 삼고 곧바로 첫 갱신을 보낸다.
    pub fn spawn(renewer: Arc<dyn LeaseRenewer>, config: HaConfig) -> Arc<Self> {
        Self::start(renewer, config, PING_INTERVAL)
    }

    /// [`spawn`](Self::spawn). 시험은 쓰다듬기 간격을 줄인다.
    fn start(renewer: Arc<dyn LeaseRenewer>, config: HaConfig, ping: Duration) -> Arc<Self> {
        let (tx, _) = watch::channel(LeaseState::default());
        let shared = Arc::new(Shared {
            start: Instant::now(),
            fence_after: config.fence_after,
            state: Mutex::new(State {
                last_renewed: None,
                running: HashSet::new(),
                watchdog: None,
            }),
            tx,
            renewed: Notify::new(),
            epoch: std::time::Instant::now(),
            renewed_at: AtomicU64::new(0),
            running_any: AtomicBool::new(false),
        });
        let tasks = vec![
            tokio::spawn(renew_loop(shared.clone(), renewer, config.renew_interval)),
            tokio::spawn(watch_loop(shared.clone())),
        ];
        let pinger = config.watchdog.is_some().then(|| {
            let (tx, rx) = mpsc::channel();
            let shared = shared.clone();
            let thread = std::thread::Builder::new()
                .name("orbivisor-watchdog".into())
                .spawn(move || ping_loop(&shared, &rx, ping))
                .expect("cannot start the watchdog thread");
            Pinger { tx, thread }
        });
        Arc::new(Self {
            shared,
            opener: config.watchdog,
            tasks: Mutex::new(tasks),
            pinger: Mutex::new(pinger),
        })
    }

    pub fn state(&self) -> LeaseState {
        let state = self.shared.state.lock().unwrap();
        self.shared.lease(&state, Instant::now())
    }

    /// 리스가 살아 있다. HA VM을 켜도 된다.
    pub fn fresh(&self) -> bool {
        self.state().fresh
    }

    /// 리스가 끝났다. 도는 HA VM을 꺼야 한다.
    pub fn stale(&self) -> bool {
        self.state().stale
    }

    /// fresh·stale이 바뀔 때마다 알린다.
    pub fn subscribe(&self) -> watch::Receiver<LeaseState> {
        self.shared.tx.subscribe()
    }

    /// HA VM을 켜거나 도는 HA VM에 붙기 전에 watchdog를 연다(이미 열었으면 그대로). 리스가 지금 살아
    /// 있을 때(`fresh`)만 성공한다. 이전 프로세스가 켜 둔 타이머를 새 리스 없이 늘리면 안 된다.
    /// watchdog가 없으면(`process`) 리스만 본다. 제한 시간을 받아들이지 않은 장치는 쥔 채(다시 열지
    /// 않는다) 실패한다. 닫은 뒤에는 장치를 열지 않는다(쓰다듬을 스레드가 없어 연 장치가 그냥 닫히면
    /// 타이머가 돌아 노드가 재시작된다).
    pub fn arm(&self) -> Result<(), ArmError> {
        let mut state = self.shared.state.lock().unwrap();
        if !self.shared.lease(&state, Instant::now()).fresh {
            return Err(ArmError::NotFresh);
        }
        if let (Some(opener), None) = (&self.opener, &state.watchdog) {
            let pinger = self.pinger.lock().unwrap();
            let Some(pinger) = &*pinger else {
                return Err(ArmError::Closed);
            };
            let watchdog = opener.open().map_err(ArmError::Open)?;
            let rejected = watchdog.rejected().map(str::to_owned);
            match &rejected {
                Some(reason) => {
                    tracing::warn!(
                        reason,
                        "watchdog opened but unusable; HA VMs will not start"
                    )
                }
                None => tracing::info!("watchdog armed"),
            }
            // 장치는 쓰다듬기 스레드에 넘긴다. 잠금을 쥐고 있으므로 그 사이에 닫히지 않는다.
            let _ = pinger.tx.send(watchdog);
            state.watchdog = Some(rejected);
        }
        match state.watchdog.as_ref().and_then(Option::as_deref) {
            Some(reason) => Err(ArmError::Rejected(reason.to_owned())),
            None => Ok(()),
        }
    }

    /// 이 노드에서 HA VM `vm`이 도는지 알린다. 리스가 끝났는데 도는 HA VM이 있으면 쓰다듬지 않는다.
    pub fn set_running(&self, vm: Uuid, running: bool) {
        let mut state = self.shared.state.lock().unwrap();
        if running {
            state.running.insert(vm);
        } else {
            state.running.remove(&vm);
        }
        self.shared.set_running_any(&state);
    }

    /// 도는 HA VM 가운데 `keep`에 없는 것을 뺀다. 작업자가 없어 아무도 빼지 않는 VM(옮겨 간 뒤 멈춘
    /// 원본 등)을 reconciler가 정리한다.
    pub fn retain_running(&self, keep: &HashSet<Uuid>) {
        let mut state = self.shared.state.lock().unwrap();
        state.running.retain(|vm| keep.contains(vm));
        self.shared.set_running_any(&state);
    }

    /// 갱신과 쓰다듬기를 멈춘다. 도는 HA VM이 없으면 watchdog를 magic close로 멈추고 닫는다. 있으면
    /// 그대로 두어 opld가 제한 시간 안에 돌아오지 않으면 노드가 재시작된다.
    pub fn close(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        let Some(pinger) = self.pinger.lock().unwrap().take() else {
            return;
        };
        // 스레드를 멈추고 쥐던 장치를 돌려받는다. 장치에는 잠금을 놓고 쓴다.
        drop(pinger.tx);
        let Some(watchdog) = pinger.thread.join().ok().flatten() else {
            return;
        };
        let running = self.shared.state.lock().unwrap().running.len();
        if running == 0 {
            if let Err(e) = watchdog.magic_close() {
                tracing::warn!(error = %e, "cannot stop the watchdog");
            }
        } else {
            tracing::warn!(
                vms = running,
                "HA VMs are running; leaving the watchdog armed, so this node restarts unless orbivisord comes back in time"
            );
            drop(watchdog);
        }
    }
}

impl Drop for HaGuard {
    /// 작업을 멈추고 쓰다듬기 스레드를 닫는다(기다리지 않는다). 장치는 magic close 없이 닫힌다.
    fn drop(&mut self) {
        for task in self.tasks.get_mut().unwrap().drain(..) {
            task.abort();
        }
        self.pinger.get_mut().unwrap().take();
    }
}

/// 갱신 간격마다 리스를 갱신한다. 한 번은 간격의 두 배까지 기다린다.
async fn renew_loop(shared: Arc<Shared>, renewer: Arc<dyn LeaseRenewer>, interval: Duration) {
    let limit = interval * 2;
    let mut failing = false;
    loop {
        // 보내기 전에 잰다. 리더가 받은 시각은 이보다 늦다(설계 §3.2). 쓰다듬기 스레드의 시각을 먼저
        // 재어 그 마감이 `lease`의 마감보다 늦지 않게 한다(`from_std`는 시간을 멈춘 시험의 시계와 달라
        // 쓰지 않는다).
        let sent_clock = std::time::Instant::now();
        let sent = Instant::now();
        let result = tokio::time::timeout(limit, renewer.renew_lease(limit))
            .await
            .unwrap_or_else(|_| Err(ClusterError::Timeout));
        match result {
            Ok(()) => {
                {
                    let mut state = shared.state.lock().unwrap();
                    state.last_renewed = Some(state.last_renewed.map_or(sent, |s| s.max(sent)));
                }
                shared.record_renewal(sent_clock);
                shared.publish();
                shared.renewed.notify_one();
                if failing {
                    tracing::info!("HA lease renewed again");
                    failing = false;
                }
            }
            Err(e) => {
                if !failing {
                    tracing::warn!(error = %e, "cannot renew the HA lease");
                    failing = true;
                }
            }
        }
        tokio::time::sleep_until(sent + interval).await;
    }
}

/// 리스 상태가 바뀌는 때에 알린다.
async fn watch_loop(shared: Arc<Shared>) {
    loop {
        let next = shared.publish();
        tokio::select! {
            () = async {
                match next {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
            () = shared.renewed.notified() => {}
        }
    }
}

/// 쓰다듬기 스레드. `interval`마다 watchdog를 쓰다듬는다. 리스가 살아 있거나 도는 HA VM이 없을 때만
/// 쓰다듬는다. 잠금 없이 원자값만 읽는다. `rx`가 닫히면 쥔 장치를 돌려주고 끝난다.
fn ping_loop(
    shared: &Shared,
    rx: &mpsc::Receiver<Box<dyn Watchdog>>,
    interval: Duration,
) -> Option<Box<dyn Watchdog>> {
    let mut watchdog: Option<Box<dyn Watchdog>> = None;
    let mut holding = false;
    loop {
        match rx.recv_timeout(interval) {
            // 새 장치는 한 간격을 기다리지 않고 곧바로 한 번 쓰다듬는다(규칙은 같다).
            Ok(opened) => watchdog = Some(opened),
            Err(RecvTimeoutError::Disconnected) => return watchdog,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let Some(watchdog) = watchdog.as_mut() else {
            continue;
        };
        if shared.fresh_for_ping() || !shared.running_any.load(Ordering::Acquire) {
            holding = false;
            if let Err(e) = watchdog.ping() {
                tracing::warn!(error = %e, "cannot ping the watchdog");
            }
        } else if !holding {
            holding = true;
            tracing::warn!("HA lease lost with HA VMs running; no longer pinging the watchdog");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FENCE_AFTER: Duration = Duration::from_secs(10);
    const INTERVAL: Duration = Duration::from_secs(1);
    const MS: Duration = Duration::from_millis(1);

    /// 정해 둔 대로 갱신에 성공하거나 실패하고, 보낸 시각과 제한 시간을 남긴다.
    #[derive(Default)]
    struct FakeRenewer {
        ok: Mutex<bool>,
        /// 응답까지 걸리는 시간. None이면 끝나지 않는다.
        delay: Mutex<Option<Duration>>,
        /// 받은 갱신(보낸 시각, 제한 시간)
        calls: Mutex<Vec<(Instant, Duration)>>,
        /// 성공한 갱신을 보낸 시각
        oks: Mutex<Vec<Instant>>,
    }

    impl FakeRenewer {
        fn new(ok: bool) -> Arc<Self> {
            let r = Self::default();
            *r.ok.lock().unwrap() = ok;
            *r.delay.lock().unwrap() = Some(Duration::ZERO);
            Arc::new(r)
        }

        fn set_ok(&self, ok: bool) {
            *self.ok.lock().unwrap() = ok;
        }

        /// 마지막으로 성공한 갱신을 보낸 시각
        fn last_ok(&self) -> Instant {
            *self.oks.lock().unwrap().last().unwrap()
        }
    }

    #[async_trait]
    impl LeaseRenewer for FakeRenewer {
        async fn renew_lease(&self, timeout: Duration) -> Result<(), ClusterError> {
            let sent = Instant::now();
            self.calls.lock().unwrap().push((sent, timeout));
            let delay = *self.delay.lock().unwrap();
            match delay {
                Some(d) => tokio::time::sleep(d).await,
                None => std::future::pending().await,
            }
            if *self.ok.lock().unwrap() {
                self.oks.lock().unwrap().push(sent);
                Ok(())
            } else {
                Err(ClusterError::Timeout)
            }
        }
    }

    /// 장치 대신 한 일을 적는다.
    #[derive(Default)]
    struct FakeWatchdog {
        log: Arc<Mutex<Vec<&'static str>>>,
        fail_open: Mutex<bool>,
        /// 열린 장치가 제한 시간을 받아들이지 않은 이유
        reject: Mutex<Option<&'static str>>,
    }

    impl FakeWatchdog {
        fn log(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }

        fn count(&self, what: &str) -> usize {
            self.log().iter().filter(|e| **e == what).count()
        }
    }

    struct FakeDevice(Arc<Mutex<Vec<&'static str>>>, Option<&'static str>);

    impl Watchdog for FakeDevice {
        fn ping(&mut self) -> io::Result<()> {
            self.0.lock().unwrap().push("ping");
            Ok(())
        }

        fn magic_close(self: Box<Self>) -> io::Result<()> {
            self.0.lock().unwrap().push("magic_close");
            Ok(())
        }

        fn rejected(&self) -> Option<&str> {
            self.1
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("drop");
        }
    }

    impl WatchdogOpener for FakeWatchdog {
        fn open(&self) -> io::Result<Box<dyn Watchdog>> {
            if *self.fail_open.lock().unwrap() {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, "busy"));
            }
            self.log.lock().unwrap().push("open");
            let reject = *self.reject.lock().unwrap();
            Ok(Box::new(FakeDevice(self.log.clone(), reject)))
        }
    }

    fn guard(renewer: &Arc<FakeRenewer>, watchdog: Option<&Arc<FakeWatchdog>>) -> Arc<HaGuard> {
        HaGuard::spawn(
            renewer.clone(),
            HaConfig {
                fence_after: FENCE_AFTER,
                renew_interval: INTERVAL,
                watchdog: watchdog.map(|w| w.clone() as Arc<dyn WatchdogOpener>),
            },
        )
    }

    async fn sleep_until(at: Instant) {
        tokio::time::sleep_until(at).await;
    }

    /// 한 번도 갱신하지 못하면 fresh가 아니고, 시작에서 fence_after가 지나야 stale이다.
    #[tokio::test(start_paused = true)]
    async fn without_renewal_stale_starts_counting_from_the_start() {
        let start = Instant::now();
        let renewer = FakeRenewer::new(false);
        let g = guard(&renewer, None);
        let mut rx = g.subscribe();
        assert_eq!(g.state(), LeaseState::default());
        sleep_until(start + FENCE_AFTER - MS).await;
        assert!(!g.fresh());
        assert!(!g.stale());
        rx.changed().await.unwrap();
        assert_eq!(Instant::now(), start + FENCE_AFTER);
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: false,
                stale: true
            }
        );
        assert!(g.stale());
        g.close();
    }

    /// 갱신하면 fresh이고, 마지막으로 성공한 갱신을 보낸 시각에서 fence_after가 지나면 stale이다.
    #[tokio::test(start_paused = true)]
    async fn fresh_until_fence_after_from_the_last_renewal_sent() {
        let renewer = FakeRenewer::new(true);
        // 응답에 시간이 걸려도 보낸 시각부터 센다.
        *renewer.delay.lock().unwrap() = Some(Duration::from_millis(300));
        let g = guard(&renewer, None);
        let mut rx = g.subscribe();
        rx.changed().await.unwrap();
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: true,
                stale: false
            }
        );
        // 몇 번 더 갱신한 뒤 끊는다.
        tokio::time::sleep(Duration::from_millis(3500)).await;
        renewer.set_ok(false);
        tokio::time::sleep(INTERVAL).await;
        let last = renewer.last_ok();
        assert!(last > Instant::now() - INTERVAL * 3);
        sleep_until(last + FENCE_AFTER - MS).await;
        assert!(g.fresh());
        assert!(!g.stale());
        rx.changed().await.unwrap();
        assert_eq!(Instant::now(), last + FENCE_AFTER);
        assert_eq!(
            *rx.borrow_and_update(),
            LeaseState {
                fresh: false,
                stale: true
            }
        );
        // 다시 갱신되면 곧바로 fresh로 알린다.
        renewer.set_ok(true);
        rx.changed().await.unwrap();
        assert!(rx.borrow_and_update().fresh);
        assert!(!g.stale());
        g.close();
    }

    /// 한 번의 갱신은 간격의 두 배까지 기다리고, 끝나지 않으면 다음 갱신을 보낸다.
    #[tokio::test(start_paused = true)]
    async fn a_hung_renewal_times_out_after_twice_the_interval() {
        let renewer = FakeRenewer::new(true);
        *renewer.delay.lock().unwrap() = None;
        let start = Instant::now();
        let g = guard(&renewer, None);
        sleep_until(start + INTERVAL * 2 - MS).await;
        assert_eq!(renewer.calls.lock().unwrap().len(), 1);
        sleep_until(start + INTERVAL * 2 + MS).await;
        assert_eq!(
            *renewer.calls.lock().unwrap(),
            [(start, INTERVAL * 2), (start + INTERVAL * 2, INTERVAL * 2)]
        );
        assert!(!g.fresh());
        // 두 번째도 끝나지 않고, 세 번째는 성공한다.
        *renewer.delay.lock().unwrap() = Some(Duration::ZERO);
        sleep_until(start + INTERVAL * 4 + MS).await;
        assert!(g.fresh());
        assert_eq!(renewer.last_ok(), start + INTERVAL * 4);
        g.close();
    }

    /// 갱신하기 전에는 watchdog를 열지 않는다. 연 뒤에는 다시 열지 않고, 열지 못하면 다음에 다시 연다.
    #[tokio::test(start_paused = true)]
    async fn arm_needs_a_renewal_and_opens_the_watchdog_once() {
        let renewer = FakeRenewer::new(false);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(INTERVAL).await;
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        assert!(watchdog.log().is_empty());
        renewer.set_ok(true);
        tokio::time::sleep(INTERVAL).await;
        *watchdog.fail_open.lock().unwrap() = true;
        assert!(matches!(g.arm(), Err(ArmError::Open(_))));
        *watchdog.fail_open.lock().unwrap() = false;
        g.arm().unwrap();
        g.arm().unwrap();
        assert_eq!(watchdog.count("open"), 1);
        g.close();
    }

    /// 갱신한 적이 있어도 리스가 끝났으면 watchdog를 열지 않는다(리스 없이 타이머를 늘리지 않는다).
    #[tokio::test(start_paused = true)]
    async fn arm_needs_a_fresh_lease() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        let mut rx = g.subscribe();
        rx.wait_for(|s| s.fresh).await.unwrap();
        renewer.set_ok(false);
        rx.wait_for(|s| s.stale).await.unwrap();
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        assert!(watchdog.log().is_empty());
        renewer.set_ok(true);
        rx.wait_for(|s| s.fresh).await.unwrap();
        g.arm().unwrap();
        assert_eq!(watchdog.count("open"), 1);
        g.close();
    }

    /// 닫은 뒤에는 리스가 살아 있어도 watchdog를 열지 않는다.
    #[tokio::test(start_paused = true)]
    async fn arm_after_close_does_not_open_the_watchdog() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        g.subscribe().wait_for(|s| s.fresh).await.unwrap();
        g.close();
        assert!(g.fresh());
        assert!(matches!(g.arm(), Err(ArmError::Closed)));
        assert!(watchdog.log().is_empty());
    }

    /// watchdog가 없으면(process) 갱신했는지만 본다.
    #[tokio::test(start_paused = true)]
    async fn arm_without_a_watchdog_only_needs_a_renewal() {
        let renewer = FakeRenewer::new(false);
        let g = guard(&renewer, None);
        assert!(matches!(g.arm(), Err(ArmError::NotFresh)));
        renewer.set_ok(true);
        tokio::time::sleep(INTERVAL).await;
        g.arm().unwrap();
        g.close();
    }

    /// 쓰다듬기 스레드를 쓰는 시험의 짧은 시간들(실제 시간으로 돈다)
    const PING: Duration = Duration::from_millis(50);
    const SHORT_FENCE: Duration = Duration::from_millis(2000);

    /// 실제 시간으로 짧게 도는 `HaGuard`
    fn quick_guard(renewer: &Arc<FakeRenewer>, watchdog: Arc<dyn WatchdogOpener>) -> Arc<HaGuard> {
        HaGuard::start(
            renewer.clone(),
            HaConfig {
                fence_after: SHORT_FENCE,
                renew_interval: Duration::from_millis(100),
                watchdog: Some(watchdog),
            },
            PING,
        )
    }

    /// 리스가 살아 있거나 도는 HA VM이 없을 때만 쓰다듬는다.
    #[tokio::test]
    async fn pings_only_while_fresh_or_without_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = quick_guard(&renewer, watchdog.clone());
        let mut rx = g.subscribe();
        rx.wait_for(|s| s.fresh).await.unwrap();
        g.arm().unwrap();
        let vm = Uuid::new_v4();
        g.set_running(vm, true);
        // 리스가 살아 있으면 HA VM이 돌아도 쓰다듬는다.
        tokio::time::sleep(PING * 10).await;
        let pinged = watchdog.count("ping");
        assert!(pinged >= 3, "{pinged}");
        // 리스가 끝나면 HA VM이 도는 동안 쓰다듬지 않는다.
        renewer.set_ok(false);
        rx.wait_for(|s| !s.fresh).await.unwrap();
        tokio::time::sleep(PING * 2).await;
        let before = watchdog.count("ping");
        tokio::time::sleep(PING * 10).await;
        assert_eq!(watchdog.count("ping"), before);
        // 도는 HA VM이 없으면 리스가 없어도 쓰다듬는다.
        g.set_running(vm, false);
        tokio::time::sleep(PING * 10).await;
        assert!(watchdog.count("ping") >= before + 2);
        g.close();
    }

    /// 받은 장치는 쓰다듬기 간격을 기다리지 않고 곧바로 한 번 쓰다듬는다.
    #[tokio::test]
    async fn a_new_watchdog_is_pinged_at_once() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = HaGuard::start(
            renewer.clone(),
            HaConfig {
                fence_after: SHORT_FENCE,
                renew_interval: Duration::from_millis(100),
                watchdog: Some(watchdog.clone()),
            },
            Duration::from_secs(3600),
        );
        g.subscribe().wait_for(|s| s.fresh).await.unwrap();
        g.arm().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while watchdog.count("ping") == 0 {
            assert!(std::time::Instant::now() < deadline, "not pinged");
            tokio::time::sleep(PING).await;
        }
        assert_eq!(watchdog.log(), ["open", "ping"]);
        g.close();
    }

    /// 장치 대신 파일 끝에 쓰고, 쓴 스레드의 이름을 적는다.
    struct FileWatchdog {
        path: PathBuf,
        threads: Arc<Mutex<HashSet<String>>>,
    }

    struct FileDevice(std::fs::File, Arc<Mutex<HashSet<String>>>);

    impl Watchdog for FileDevice {
        fn ping(&mut self) -> io::Result<()> {
            let name = std::thread::current().name().unwrap_or_default().to_owned();
            self.1.lock().unwrap().insert(name);
            io::Write::write_all(&mut self.0, b"\0")
        }

        fn magic_close(mut self: Box<Self>) -> io::Result<()> {
            io::Write::write_all(&mut self.0, b"V")
        }
    }

    impl WatchdogOpener for FileWatchdog {
        fn open(&self) -> io::Result<Box<dyn Watchdog>> {
            let file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
            Ok(Box::new(FileDevice(file, self.threads.clone())))
        }
    }

    /// 실행기가 막혀 갱신 루프가 돌지 않아도 쓰다듬기 스레드는 계속 쓰다듬고, 스스로 시간을 재어
    /// 리스가 끝나면(HA VM이 도는 동안) 멈춘다.
    #[tokio::test]
    async fn the_watchdog_thread_pings_while_the_runtime_is_blocked() {
        let renewer = FakeRenewer::new(true);
        let file = tempfile::NamedTempFile::new().unwrap();
        let threads = Arc::new(Mutex::new(HashSet::new()));
        let g = quick_guard(
            &renewer,
            Arc::new(FileWatchdog {
                path: file.path().into(),
                threads: threads.clone(),
            }),
        );
        g.subscribe().wait_for(|s| s.fresh).await.unwrap();
        g.arm().unwrap();
        g.set_running(Uuid::new_v4(), true);
        let written = || std::fs::read(file.path()).unwrap();

        // 이 시험의 실행기(스레드 하나)를 막는다. 마지막 갱신은 막기 전이다.
        let blocked = std::time::Instant::now();
        // 느린 CI에서도 되게 정해진 시간 대신 세 번 쓰다듬을 때까지(리스가 끝나기 전까지만) 막은 채 기다린다.
        let early = loop {
            let w = written();
            if w.len() >= 3 || blocked.elapsed() >= SHORT_FENCE / 2 {
                break w;
            }
            std::thread::sleep(PING);
        };
        assert!(early.len() >= 3, "{early:?}");
        std::thread::sleep(
            (blocked + SHORT_FENCE + PING * 8).saturating_duration_since(std::time::Instant::now()),
        );
        let late = written();
        std::thread::sleep(PING * 6);
        assert_eq!(written(), late);
        assert!(late.iter().all(|b| *b == 0));
        assert_eq!(
            *threads.lock().unwrap(),
            HashSet::from(["orbivisor-watchdog".to_owned()])
        );
        // 도는 HA VM이 있으므로 닫아도 magic close를 하지 않는다.
        g.close();
        assert_eq!(written(), late);
    }

    /// 정상 종료 때 도는 HA VM이 없으면 magic close를 한다.
    #[tokio::test(start_paused = true)]
    async fn close_stops_the_watchdog_without_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        let vm = Uuid::new_v4();
        g.set_running(vm, true);
        g.set_running(vm, false);
        g.close();
        let log = watchdog.log();
        assert_eq!(&log[log.len() - 2..], ["magic_close", "drop"]);
        // 닫은 뒤에는 쓰다듬지 않는다.
        let pinged = watchdog.count("ping");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert_eq!(watchdog.count("ping"), pinged);
    }

    /// 도는 HA VM이 있으면 magic close 없이 닫아 타이머를 그대로 둔다.
    #[tokio::test(start_paused = true)]
    async fn close_leaves_the_watchdog_running_with_running_ha_vms() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        let g = guard(&renewer, Some(&watchdog));
        tokio::time::sleep(MS).await;
        g.arm().unwrap();
        g.set_running(Uuid::new_v4(), true);
        g.close();
        assert_eq!(watchdog.log().last(), Some(&"drop"));
        assert_eq!(watchdog.count("magic_close"), 0);
    }

    /// 제한 시간을 받아들이지 않은 장치도 닫지 않고 쥔다: 그냥 닫으면 커널 타이머가 남아 나중에 노드가
    /// 재시작된다. HA VM에는 쓰지 않고(`arm` 실패, 다시 열지 않는다) 평소 규칙대로 쓰다듬으며, 정상 종료
    /// 때 도는 HA VM이 없으면 magic close로 멈춘다.
    #[tokio::test]
    async fn a_rejected_watchdog_is_held_pinged_and_magic_closed() {
        let renewer = FakeRenewer::new(true);
        let watchdog = Arc::new(FakeWatchdog::default());
        *watchdog.reject.lock().unwrap() = Some("the device chose 60s");
        let g = quick_guard(&renewer, watchdog.clone());
        g.subscribe().wait_for(|s| s.fresh).await.unwrap();
        let Err(e) = g.arm() else {
            panic!("armed a rejected watchdog");
        };
        assert!(matches!(e, ArmError::Rejected(_)), "{e:?}");
        assert!(e.to_string().contains("the device chose 60s"), "{e}");
        assert!(matches!(g.arm(), Err(ArmError::Rejected(_))));
        assert_eq!(watchdog.count("open"), 1);
        tokio::time::sleep(PING * 10).await;
        assert!(watchdog.count("ping") >= 2);
        g.close();
        let log = watchdog.log();
        assert_eq!(&log[log.len() - 2..], ["magic_close", "drop"]);
    }

    /// 실제 장치 구현은 watchdog가 아닌 파일을 받아들이지 않는다: Linux는 ioctl이 실패해 쓸 수 없는
    /// 장치로 쥐고(`V`를 쓰지 않는다), 다른 OS는 열기가 실패한다. 진짜 `/dev/watchdog`는 열지 않는다.
    #[test]
    fn device_watchdog_rejects_a_regular_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let device = DeviceWatchdog {
            path: file.path().into(),
            timeout_secs: 30,
        };
        match (cfg!(target_os = "linux"), device.open()) {
            (true, Ok(watchdog)) => {
                let reason = watchdog.rejected().expect("accepted a regular file");
                assert!(
                    reason.starts_with("cannot set the watchdog timeout to 30s"),
                    "{reason}"
                );
            }
            (false, Err(e)) => assert_eq!(e.kind(), io::ErrorKind::Unsupported),
            (linux, opened) => panic!("linux: {linux}, opened: {}", opened.is_ok()),
        }
        assert_eq!(std::fs::read(file.path()).unwrap(), b"");
    }

    fn einval() -> io::Error {
        io::Error::from_raw_os_error(22)
    }

    /// 장치가 정한 제한 시간이 요청 이하일 때만 받아들인다. 정하지 못했으면 지금 값을 읽어 본다.
    #[test]
    fn device_timeout_must_not_exceed_the_request() {
        let unread = || -> io::Result<std::ffi::c_int> { panic!("read the timeout") };
        // 정한 대로이거나 더 짧으면 받아들인다.
        assert_eq!(accept_timeout(30, Ok(30), unread).unwrap(), 30);
        assert_eq!(accept_timeout(30, Ok(20), unread).unwrap(), 20);
        // 더 길게 골랐거나 0이면 안 된다.
        assert!(accept_timeout(30, Ok(60), unread).is_err());
        assert!(accept_timeout(30, Ok(0), unread).is_err());
        // 정하지 못했으면 지금 값이 요청 이하일 때만 받아들인다.
        assert_eq!(accept_timeout(30, Err(einval()), || Ok(10)).unwrap(), 10);
        assert_eq!(accept_timeout(30, Err(einval()), || Ok(30)).unwrap(), 30);
        let e = accept_timeout(30, Err(einval()), || Ok(60)).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(22));
        let e = accept_timeout(30, Err(einval()), || Err(io::Error::other("no"))).unwrap_err();
        assert_eq!(e.raw_os_error(), Some(22));
    }
}
```

`crates/opl-reconciler/tests/migration.rs`에서

```rust
    assert_eq!(got.spec.node, Some(env.node));
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(got.spec.node, Some(env.node));
    // 작업자는 표시를 지운 뒤에 이어서 실행한다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while status_of(&env, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(std::time::Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
```

`crates/opl-reconciler/tests/migration.rs`에서

```rust
    assert_eq!(got.status.pid, pid);
    assert_eq!(status_of(&env, vm.id).await, Some(RuntimeStatus::Running));
}
```

을 다음으로 바꾼다:

```rust
    assert_eq!(got.status.pid, pid);
    // 작업자는 표시를 지운 뒤에 이어서 실행한다.
    let deadline = std::time::Instant::now() + common::WAIT;
    while status_of(&env, vm.id).await != Some(RuntimeStatus::Running) {
        assert!(std::time::Instant::now() < deadline, "source not resumed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
```

`crates/opld/src/member.rs` 전체:

```rust
//! 클러스터 멤버로 도는 노드: 노드 간 서버, 클러스터 저장소, reconciler, Task 실행기, HTTPS API,
//! 노드 감시, 예약 스냅샷, 클러스터 정리, HA 복구, 노드별 풀 상태, 사용량 지표, VXLAN 장치, VM 방화벽, APT
//! 업데이트. 위트니스는 노드 간 서버와 클러스터 저장소, 승격, Node 등록만 띄운다.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::serve::ListenerExt;
use openraft::BasicNode;
use opl_api::cluster::{DEFAULT_JOIN_TTL, issue_join_token};
use opl_api::{
    AppState, AuditLog, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig,
    IncomingConfig, IncomingMigrations, RemotePoller, SnapshotScheduler, TaskRunner,
    TaskRunnerConfig, TelemetryReporter, UefiVarsFetcher, UpdateAgent, Updates,
};
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_cluster::{CATCH_UP_TIMEOUT, Cluster, ClusterConfig, ClusterError, Typed};
use opl_driver::{
    DirStorage, Hypervisor, LvmSharedStorage, LvmThinStorage, QemuHypervisor, RbdStorage, Storage,
    Storages, SystemLvm, SystemRbd, SystemZfs, ZfsStorage, migration_tls_dir, write_migration_tls,
};
use opl_peer::pki::{ClusterCa, NodeTls, PKI_DIR, fingerprint};
use opl_peer::tls::{TlsListener, api_server_config};
use opl_peer::{GrpcNetwork, NodeMonitor, NodeMonitorConfig, PeerService, Peers};
use opl_reconciler::{FirewallGate, FirmwareFetcher, HaGuard, Reconciler, ReconcilerConfig};
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::bootstrap;
use crate::config::Config;
use crate::firewall::{FirewallAgent, FirewallRules};
use crate::host::Host;
use crate::metrics::host as metrics_host;
use crate::metrics::sampler::MetricsSampler;
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::overlay::{OverlayAgent, cluster_tag};
use crate::pools::PoolMounter;
use crate::runtime::{Handles, on, run_on};

/// 데이터 디렉터리 아래 클러스터 저장소(Raft 로그와 상태)를 두는 디렉터리
pub const CLUSTER_DIR: &str = "cluster";
/// 데이터 디렉터리 아래 마이그레이션 디스크 기록 파일을 두는 디렉터리(로컬 디스크 마이그레이션 설계 §3.6)
const MIGRATION_DISKS_DIR: &str = "migration-disks";
/// 종료 신호를 받은 뒤 열린 연결(watch, 콘솔)이 끝나기를 기다리는 시간
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 커밋되기를 기다리는 시간. 리더를 잃으면 새 리더를 뽑는 데 보통 선거 제한 시간의 두 배쯤
/// 걸리므로(openraft는 리더 임기 동안 다른 노드에 투표하지 않는다) 그보다 넉넉히 둔다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// 다시 시작한 노드가 Node 리소스를 다시 기록하는 간격(과반수가 없으면 될 때까지)
const REGISTER_RETRY: Duration = Duration::from_secs(2);
/// 노드의 브리지 목록과 기능 표시(위트니스는 기능 표시만)를 다시 확인하는 간격. 관리자가 브리지를 만들면
/// 이만큼 뒤에 스케줄러가 알고, 옛 버전 리더가 지운 기능 표시는 이만큼 뒤에 되살아난다.
const BRIDGE_REFRESH: Duration = Duration::from_secs(60);
/// 다시 시작한 learner가 승격을 다시 요청하는 간격. 실패할 때마다 두 배로 늘려 최대 시간까지 둔다.
const PROMOTE_RETRY_MIN: Duration = Duration::from_secs(5);
const PROMOTE_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn pki_dir(config: &Config) -> PathBuf {
    config.data_dir.join(PKI_DIR)
}

pub fn cluster_dir(config: &Config) -> PathBuf {
    config.data_dir.join(CLUSTER_DIR)
}

/// 멤버가 되는 방법
pub enum Bootstrap {
    /// 이미 멤버다(다시 시작).
    Restart,
    /// 이 노드 하나로 새 클러스터를 만든다.
    Init { name: String, ca: ClusterCa },
    /// 합류가 받아들여졌다(이 노드는 이미 learner다). 인증서를 저장한 뒤 `via`(받아 준 노드의
    /// 노드 간 주소)로 투표 멤버 승격을 요청한다.
    Join { via: String },
}

/// 새 클러스터를 만든 결과. 사용자에게 한 번만 보여 준다.
pub struct Created {
    pub admin_password: String,
    pub join_url: String,
    pub ca_fingerprint: String,
}

/// 위트니스가 `oplctl cluster init`을 받았을 때의 답
pub const WITNESS_CANNOT_INIT: &str =
    "a witness cannot start a cluster; join an existing cluster instead";

/// 노드에서 노드 간 통신에 쓰는 Raft 설정. 스냅샷 조각은 JSON으로 가므로 작게 둔다.
fn raft_config(witness: bool) -> openraft::Config {
    openraft::Config {
        cluster_name: "opl".into(),
        // 위트니스는 투표는 하지만 후보가 되지 않는다. 리더 일은 늘 일반 노드가 한다.
        enable_elect: !witness,
        heartbeat_interval: 200,
        election_timeout_min: 1000,
        election_timeout_max: 2000,
        snapshot_max_chunk_size: 1 << 20,
        // 스냅샷 조각마다 걸리는 제한 시간(밀리초). 기본 200ms로는 JSON으로 부푼 1MiB 조각(3.5MiB쯤)이
        // 바쁜 노드에서 끝나지 못할 수 있다.
        install_snapshot_timeout: 10_000,
        ..Default::default()
    }
}

struct Server<E> {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<(), E>>,
}

/// 위트니스가 띄우지 않는 것: HTTPS API와 VM·풀을 다스리는 일, 리더의 일
struct Services {
    api: Server<std::io::Error>,
    reconciler: Reconciler,
    runner: TaskRunner,
    monitor: NodeMonitor,
    schedule: SnapshotScheduler,
    janitor: ClusterJanitor,
    /// 리더가 백업 작업을 실행한다.
    backups: BackupJobScheduler,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
    recovery: HaManager,
    pools: PoolMounter,
    /// 이 노드와 그 VM의 사용량 지표를 잰다.
    metrics: MetricsSampler,
    /// VXLAN 네트워크의 장치를 맞춘다(호스트 네트워크가 없으면 없다).
    overlay: Option<OverlayAgent>,
    /// VM 방화벽 규칙을 맞춘다(방화벽이 없으면 없다).
    firewall: Option<FirewallAgent>,
    /// HA 리스와 watchdog(`[ha] fencing = "off"`면 없다)
    ha: Option<Arc<HaGuard>>,
    /// APT 소스 파일과 주기 업데이트 확인(업데이트가 꺼졌으면 없다)
    updates: Option<UpdateAgent>,
    /// 리더가 텔레메트리 보고를 보낸다(설정에서 켰을 때).
    telemetry: TelemetryReporter,
    /// 리더가 원격 클러스터의 상태를 모은다.
    remotes: RemotePoller,
    /// API가 멈춘 뒤 남은 감사 항목을 쓰고 닫는다.
    audit: Arc<AuditLog>,
}

impl Services {
    /// `left`면 클러스터에서 빠진 것이다(VXLAN 장치와 방화벽 테이블을 지운다).
    async fn shutdown(mut self, left: bool) {
        let _ = self.api.stop.send(());
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.api.task)
            .await
            .is_err()
        {
            self.api.task.abort();
        }
        self.audit.shutdown().await;
        self.monitor.shutdown().await;
        self.schedule.shutdown().await;
        self.janitor.shutdown().await;
        self.backups.shutdown().await;
        self.telemetry.shutdown().await;
        self.remotes.shutdown().await;
        self.recovery.shutdown().await;
        self.pools.shutdown().await;
        self.metrics.shutdown().await;
        if let Some(updates) = self.updates {
            updates.shutdown().await;
        }
        if let Some(overlay) = self.overlay {
            overlay.shutdown(left).await;
        }
        self.runner.shutdown().await;
        self.reconciler.shutdown().await;
        // 작업자가 모두 멈춘 뒤에 멈춰야 테이블을 지운 뒤 시작 직전 확인이 다시 넣지 않는다.
        if let Some(firewall) = self.firewall {
            firewall.shutdown(left).await;
        }
        // 작업자가 모두 멈춘 뒤에 닫아야 도는 HA VM 목록이 맞다.
        if let Some(ha) = &self.ha {
            ha.close();
        }
    }
}

pub struct Member {
    config: Config,
    cluster: Arc<Cluster>,
    peer: Server<opl_peer::ServeError>,
    /// 위트니스면 없다.
    services: Option<Services>,
    register: JoinHandle<()>,
    promotion: JoinHandle<()>,
    /// Raft가 멈추면 `exited`로 알린다. 정상 종료는 저장소를 닫기 전에 이것부터 멈춘다.
    raft_watch: JoinHandle<()>,
    /// 서버 태스크가 끝나거나 Raft가 멈추면 그 이유가 온다.
    exited: mpsc::Receiver<String>,
}

impl Member {
    /// 멤버로서 시작한다. 인증서가 있으면 멤버로 보므로, 처음 멤버가 되는 경우 `Init`은 마지막에
    /// 노드 인증서를 저장해 도중에 실패하면 이 노드는 계속 미초기화 상태다. `Join`은 이미 learner로
    /// 더해졌으므로 승격을 요청하기 전에 저장한다. 그 뒤에 실패하거나 죽어도 클러스터가 투표 멤버로
    /// 세는 노드가 미초기화로 남지 않고, 다시 시작하면(`Restart`) 승격을 다시 요청한다.
    ///
    /// 서비스를 실행기 셋에 나눠 띄운다(설계 §3.1). Raft(openraft 작업은 `Raft::new`를 부른 실행기에
    /// 생긴다), 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시, 승격은 `rt.core`, API는 `rt.api`, VM·풀을
    /// 다스리는 일과 스케줄러는 `rt.work`에서 돈다. 노드 간 서버가 받은 Raft·리스가 아닌 요청(마이그레이션
    /// 받기, 콘솔 중계, UEFI 변수·감사 로그·지표 읽기)도 `rt.work`에서 돈다.
    pub async fn start(
        config: &Config,
        node: Uuid,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<(Self, Option<Created>)> {
        // `[telemetry] ca_file`을 읽지 못하면 아무것도 열기 전에 실패한다.
        let telemetry_config = config.telemetry.telemetry_config()?;
        let id = NodeId(node);
        // Raft·리스 전달·ping 채널의 연결 작업은 core, 콘솔·마이그레이션·감사·지표 채널의 것은 work에서
        // 돈다(처음 연 쪽 실행기와 관계없이).
        let peers = Arc::new(Peers::new(&tls).with_runtimes(rt.core.clone(), rt.work.clone()));
        let mut cluster_config = ClusterConfig::new(id, cluster_dir(config));
        cluster_config.raft = raft_config(config.witness);
        cluster_config.write_timeout = WRITE_TIMEOUT;
        let network = GrpcNetwork::new(peers.clone());
        let cluster = Arc::new(
            run_on(&rt.core, Cluster::open(cluster_config, network))
                .await?
                .with_forwarder(peers.clone()),
        );
        // 역할은 합류할 때 정해진다. 설정만 바꿔 다시 시작하면 아무것도 띄우지 않는다.
        if matches!(bootstrap, Bootstrap::Restart)
            && let Err(e) = bootstrap::check_role(&cluster, config)
        {
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        if config.witness {
            let member = Self::start_witness(config, cluster, peers, tls, bootstrap, rt).await?;
            return Ok((member, None));
        }
        // 포트가 이미 쓰이고 있으면 아무것도 띄우기 전에 실패한다.
        let ports = async {
            let api = TcpListener::bind(config.listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.listen))?;
            let peer = TcpListener::bind(config.cluster_listen)
                .await
                .with_context(|| format!("cannot listen on {}", config.cluster_listen))?;
            anyhow::Ok((api, peer))
        };
        let (api_listener, peer_listener) = match ports.await {
            Ok(listeners) => listeners,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 이 클러스터가 만든 VXLAN 장치의 표시. 남의 관리 이름 브리지는 브리지 목록에서 뺀다.
        let host = cluster_tag(&tls.ca_pem).and_then(|tag| {
            let host = Host::from_config(config, &tag)?;
            Ok((host, tag))
        });
        let (mut host, tag) = match host {
            Ok(host) => host,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        // 등록 때 방화벽 여부를 기록하므로 등록 전에 찾는다.
        host.detect_firewall(&tag).await;
        // 워커와 마이그레이션 받기가 VM을 켜기 직전에 규칙을 넣는다. 에이전트는 아래에서 띄운다.
        let firewall = FirewallRules::new(cluster.clone(), config, &host, tag.clone());
        let gate = firewall.clone().map(|f| f as Arc<dyn FirewallGate>);
        // 살아 있는 VM에 다시 붙으며 띄우는 QMP·모니터 작업이 `work`에 생긴다.
        let hv: Arc<dyn Hypervisor> =
            match run_on(&rt.work, QemuHypervisor::new(host.qemu_config(config))).await {
                Ok(hv) => Arc::new(hv),
                Err(e) => {
                    let _ = cluster.shutdown().await;
                    return Err(e.into());
                }
            };
        // QEMU가 마이그레이션 TLS에 쓰는 노드 인증서. 드라이버의 state_dir은 데이터 디렉터리다(host.rs).
        let tls_dir = migration_tls_dir(&config.data_dir);
        if let Err(e) = write_migration_tls(&tls_dir, &tls.ca_pem, &tls.cert_pem, &tls.key_pem) {
            let _ = cluster.shutdown().await;
            return Err(
                anyhow::Error::new(e).context(format!("cannot write {}", tls_dir.display()))
            );
        }
        // 계획 20 전에 받은 노드 인증서는 keyEncipherment가 없어 QEMU 8.2 이하의 마이그레이션 TLS가
        // 실패한다. 노드가 클러스터를 나갔다 다시 들어오면 새 인증서를 받는다.
        if let Ok(false) = opl_peer::pki::allows_key_encipherment(&tls.cert_pem) {
            tracing::warn!(
                "this node's certificate lacks the keyEncipherment key usage; live migration over TLS \
                 will fail with QEMU 8.2 or older until the node leaves and rejoins the cluster"
            );
        }
        // NFS 풀의 마운트 지점은 `{data_dir}/pools/{풀 id}`다.
        let mount_root = config.data_dir.join("pools");
        // 풀 백엔드 종류로 나눠 보내는 저장소 하나를 reconciler와 Task 실행기가 함께 쓴다.
        let files: Arc<dyn Storage> = Arc::new(DirStorage::new(&config.qemu_img, &mount_root));
        let lvm = LvmThinStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let zfs = ZfsStorage::new(&config.qemu_img, Arc::new(SystemZfs));
        let rbd = RbdStorage::new(&config.qemu_img, Arc::new(SystemRbd));
        let lvm_shared = LvmSharedStorage::new(&config.qemu_img, Arc::new(SystemLvm));
        let storage: Arc<dyn Storage> = Arc::new(
            Storages::new(&config.qemu_img, files)
                .with_lvm_thin(Arc::new(lvm))
                .with_zfs(Arc::new(zfs))
                .with_rbd(Arc::new(rbd))
                .with_lvm_shared(Arc::new(lvm_shared)),
        );
        // API 서버, 노드 간 서버, Raft 감시가 보낸다.
        let (exited_tx, exited) = mpsc::channel(3);
        // 받는 VM은 원본 노드에 배정된 채이므로 reconciler 없이 받는다.
        let incoming = IncomingMigrations::new(
            cluster.clone(),
            hv.clone(),
            IncomingConfig {
                node,
                mount_root: mount_root.clone(),
                storage: storage.clone(),
                listen_host: config.migration_listen_host(),
                ports: config.migration.ports.clone(),
                migration_disks: config.data_dir.join(MIGRATION_DISKS_DIR),
            },
        );
        // API가 적고, 다른 노드가 노드 간 서버로 읽는다.
        let audit = match config.audit.audit_config(&config.data_dir, &config.name) {
            Some(audit) => {
                let dir = audit.dir.clone();
                match AuditLog::open(audit) {
                    Ok(audit) => Arc::new(audit),
                    Err(e) => {
                        let _ = cluster.shutdown().await;
                        return Err(anyhow::Error::new(e)
                            .context(format!("cannot open the audit log in {}", dir.display())));
                    }
                }
            }
            None => Arc::new(AuditLog::disabled()),
        };
        // 측정기가 적고, API와 다른 노드(노드 간 서버)가 읽는다.
        let metrics_store = Arc::new(MetricsStore::default());
        let metrics_source = Arc::new(StoreSource::new(node, metrics_store.clone()));
        // UEFI 변수 파일은 드라이버의 state_dir(데이터 디렉터리)에 있다.
        let service = PeerService::new(cluster.clone(), Some(hv.clone()))
            .with_migrations(Arc::new(incoming.with_firewall(gate.clone())))
            .with_uefi_vars(config.data_dir.clone())
            .with_audit(audit.clone())
            .with_metrics(metrics_source.clone())
            .with_work(rt.work.clone());
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });

        // 여기서 실패하면 노드 간 서버와 저장소를 닫는다.
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let init = matches!(bootstrap, Bootstrap::Init { .. });
        let prepared = async {
            let mut password = None;
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { name, ca } => {
                    let me = BasicNode::new(config.cluster_address());
                    cluster.initialize(BTreeMap::from([(id, me)])).await?;
                    password =
                        Some(bootstrap::create_cluster(&cluster, config, &host, &name, &ca).await?);
                }
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_node(&cluster, config, &host).await?;
                }
            }
            let mut state = AppState::new(
                cluster.clone(),
                node,
                hv.clone(),
                config.data_dir.join("uploads"),
            );
            state.mount_root = mount_root.clone();
            state.peers = Some(peers.clone());
            state.secure_cookies = true;
            state.audit = audit;
            state.metrics = Some(metrics_source);
            let created = match password {
                Some(admin_password) => {
                    let token = issue_join_token(&state, DEFAULT_JOIN_TTL)
                        .await
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                    Some(Created {
                        admin_password,
                        join_url: token.url,
                        ca_fingerprint: fingerprint(&tls.ca_pem)?,
                    })
                }
                None => None,
            };
            let api_tls = api_server_config(&tls)?;
            if init {
                tls.save(&pki_dir(config))?;
            }
            anyhow::Ok((state, created, api_tls))
        };
        let (state, created, api_tls) = match prepared.await {
            Ok(prepared) => prepared,
            Err(e) => {
                let _ = peer.stop.send(());
                let _ = peer.task.await;
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };

        // 다시 시작한 노드는 과반수가 없어도 VM을 계속 다스려야 하므로, Node 리소스는 뒤에서 기록한다.
        let promotion = on(&rt.core, || spawn_promotion(first_time, &cluster, &peers));
        let raft_watch = on(&rt.core, || watch_raft(&cluster, exited_tx.clone()));
        // 풀 마운터가 QEMU의 rbd 블록 드라이버를 확인한다.
        let qemu_binary = host.profile.qemu_binary.clone();
        // 받은 UEFI 변수 파일의 크기를 이 노드의 템플릿과 견준다.
        let uefi_vars_template = host.profile.uefi_vars_template.clone();
        // 장치를 만들거나 지우면 곧바로 브리지 목록을 기록한다(아래 주기 갱신을 기다리지 않는다).
        let overlay = on(&rt.work, || {
            OverlayAgent::spawn(cluster.clone(), config, &host, tag)
        });
        let firewall = on(&rt.work, || firewall.map(FirewallAgent::spawn));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_node(&cluster, &config, &host).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_bridges(&cluster, &host).await {
                        tracing::warn!(error = %e, "cannot record bridges");
                    }
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        let mut reconciler_config = ReconcilerConfig::new(node, &mount_root, storage.clone());
        reconciler_config.acpi_timeout = config.reconciler.acpi_timeout();
        // 다른 노드에 있는 UEFI 변수 파일을 노드 사이 RPC로 받아 온다(시작, 스냅샷, 복제).
        let firmware: Arc<dyn FirmwareFetcher> = Arc::new(UefiVarsFetcher::new(
            cluster.clone(),
            peers.clone(),
            config.data_dir.clone(),
            uefi_vars_template,
        ));
        reconciler_config.firmware = Some(firmware.clone());
        // 펜싱이 켜져 있으면 지금부터 리스를 갱신한다(지금이 stale을 세는 기준점이다).
        let ha = config
            .ha
            .guard_config()
            .map(|ha| on(&rt.core, || HaGuard::spawn(cluster.clone(), ha)));
        reconciler_config.ha = ha.clone();
        reconciler_config.firewall = gate;
        let reconciler = on(&rt.work, || {
            Reconciler::spawn(cluster.clone(), hv.clone(), reconciler_config)
        });
        let pools = on(&rt.work, || {
            PoolMounter::spawn(
                cluster.clone(),
                mount_root.clone(),
                qemu_binary,
                config.pools.refresh(),
            )
        });
        // 지표는 메모리에만 둔다(Raft에 쓰지 않는다).
        let metrics = on(&rt.work, || {
            MetricsSampler::spawn(
                hv.clone(),
                metrics_host::native(),
                cluster.clone(),
                config.data_dir.clone(),
                metrics_store,
            )
        });
        // Task 실행기는 시작할 때 이 노드가 남긴 고아를 지운다. 그것이 끝난 뒤에 API를 연다.
        let mut runner_config = TaskRunnerConfig::new(node);
        runner_config.upload_dir = Some(config.data_dir.join("uploads"));
        // QEMU 드라이버의 state_dir과 같다(host.rs).
        runner_config.state_dir = Some(config.data_dir.clone());
        runner_config.mount_root = mount_root.clone();
        // 마이그레이션을 보낼 때 대상 노드를 부른다.
        runner_config.peers = Some(peers.clone());
        runner_config.firmware = Some(firmware);
        runner_config.migration_bandwidth =
            (config.migration.bandwidth_mib > 0).then_some(config.migration.bandwidth_mib << 20);
        runner_config.migration_timeout = config.migration.timeout;
        runner_config.migration_disk_timeout = config.migration.disk_timeout;
        // 받는 쪽(IncomingConfig)과 같은 디렉터리다.
        runner_config.migration_disks = Some(config.data_dir.join(MIGRATION_DISKS_DIR));
        // ESXi 가져오기의 자격 증명은 API가 받아 두고 가져오기 Task가 쓴다.
        runner_config.import_credentials = state.import_credentials.clone();
        // 노드 비우기 Task가 이 노드의 API와 같은 확인으로 마이그레이션 Task를 만든다.
        let state = Arc::new(state);
        runner_config.api = Some(state.clone());
        // 업데이트 확인은 주기 에이전트와 다시 확인 Task가 함께 쓴다(한 번에 하나만 돈다).
        let updates = config
            .updates
            .updates_config()
            .map(|c| Arc::new(Updates::new(c)));
        runner_config.updates = updates.clone();
        let runner = run_on(
            &rt.work,
            TaskRunner::spawn(
                cluster.clone(),
                storage,
                hv,
                reconciler.handle(),
                runner_config,
            ),
        )
        .await;
        let monitor = on(&rt.core, || {
            NodeMonitor::spawn(cluster.clone(), peers, NodeMonitorConfig::default())
        });
        let updates = on(&rt.work, || {
            updates.map(|u| UpdateAgent::spawn(cluster.clone(), node, u))
        });

        // TLS 핸드셰이크 작업이 `api`에 생긴다.
        let listener = on(&rt.api, || TlsListener::new(api_listener, api_tls))?;
        tracing::info!(
            node = %node,
            api = %config.listen,
            cluster = %config.cluster_listen,
            "orbivisord is ready"
        );
        let (stop, rx) = oneshot::channel::<()>();
        let (schedule, janitor, backups, telemetry, remotes) = on(&rt.work, || {
            (
                SnapshotScheduler::spawn(state.clone(), config.schedule.scheduler_config()),
                ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config()),
                BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config()),
                TelemetryReporter::spawn(state.clone(), telemetry_config),
                RemotePoller::spawn(state.clone(), config.remotes.poller_config()),
            )
        });
        let recovery = on(&rt.core, || {
            HaManager::spawn(state.clone(), HaManagerConfig::default())
        });
        let audit = state.audit.clone();
        let app = opl_api::router(state).merge(crate::web::router());
        let task = rt.api.spawn(async move {
            // 연결 주소(클라이언트 IP)를 시도 제한에 넘긴다. axum은 `TapIo`로 감싼 리스너에
            // `ConnectInfo<SocketAddr>`를 만들어 주므로 아무 일도 하지 않는 tap을 붙인다.
            let listener = listener.tap_io(|_| {});
            let r = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
            let _ = exited_tx.send(format!("API server stopped: {r:?}")).await;
            r
        });
        let member = Member {
            config: config.clone(),
            cluster,
            peer,
            services: Some(Services {
                api: Server { stop, task },
                reconciler,
                runner,
                monitor,
                schedule,
                janitor,
                backups,
                telemetry,
                remotes,
                audit,
                recovery,
                pools,
                metrics,
                overlay,
                firewall,
                ha,
                updates,
            }),
            register,
            promotion,
            raft_watch,
            exited,
        };
        Ok((member, created))
    }

    /// 위트니스로 시작한다. 노드 간 서버(하이퍼바이저 없음)와 승격, Node 등록만 띄우고 QEMU를 찾지
    /// 않는다. 위트니스는 클러스터를 만들 수 없다(`Init`은 main이 먼저 거부한다).
    async fn start_witness(
        config: &Config,
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        tls: NodeTls,
        bootstrap: Bootstrap,
        rt: &Handles,
    ) -> anyhow::Result<Self> {
        let peer_listener = match TcpListener::bind(config.cluster_listen)
            .await
            .with_context(|| format!("cannot listen on {}", config.cluster_listen))
        {
            Ok(listener) => listener,
            Err(e) => {
                let _ = cluster.shutdown().await;
                return Err(e);
            }
        };
        let (exited_tx, exited) = mpsc::channel(2);
        // 마이그레이션과 UEFI 변수는 받지 않는다.
        let service = PeerService::new(cluster.clone(), None);
        let peer = on(&rt.core, || {
            serve_peer(peer_listener, &tls, service, exited_tx.clone())
        });
        let first_time = !matches!(bootstrap, Bootstrap::Restart);
        let prepared = async {
            match bootstrap {
                Bootstrap::Restart => {}
                Bootstrap::Init { .. } => anyhow::bail!(WITNESS_CANNOT_INIT),
                Bootstrap::Join { via } => {
                    tls.save(&pki_dir(config))?;
                    promote(&cluster, &peers, Some(&via)).await?;
                    bootstrap::register_witness(&cluster, config).await?;
                }
            }
            anyhow::Ok(())
        };
        if let Err(e) = prepared.await {
            let _ = peer.stop.send(());
            let _ = peer.task.await;
            let _ = cluster.shutdown().await;
            return Err(e);
        }
        let promotion = on(&rt.core, || spawn_promotion(first_time, &cluster, &peers));
        let raft_watch = on(&rt.core, || watch_raft(&cluster, exited_tx));
        let register = {
            let (cluster, config) = (cluster.clone(), config.clone());
            rt.work.spawn(async move {
                if !first_time {
                    while let Err(e) = bootstrap::register_witness(&cluster, &config).await {
                        tracing::warn!(error = %e, "cannot register this node; will retry");
                        tokio::time::sleep(REGISTER_RETRY).await;
                    }
                }
                loop {
                    tokio::time::sleep(BRIDGE_REFRESH).await;
                    if let Err(e) = bootstrap::refresh_features(&cluster).await {
                        tracing::warn!(error = %e, "cannot record features");
                    }
                }
            })
        };
        tracing::info!(
            node = %cluster.id(),
            cluster = %config.cluster_listen,
            "orbivisord is ready as a witness"
        );
        Ok(Member {
            config: config.clone(),
            cluster,
            peer,
            services: None,
            register,
            promotion,
            raft_watch,
            exited,
        })
    }

    pub fn cluster(&self) -> &Cluster {
        &self.cluster
    }

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
        match self.exited.recv().await {
            Some(why) => anyhow::anyhow!(why),
            None => std::future::pending().await,
        }
    }

    /// 클러스터에서 빠진다. 이 노드에 배정된 VM이 있거나, 남는 투표 멤버가 없거나 모두 위트니스면
    /// 거부한다.
    /// 성공하면 `close(true)`로 닫는다.
    pub async fn leave(&self) -> anyhow::Result<()> {
        let me = self.cluster.id();
        let (_, nodes) = self.cluster.list::<NodeSpec>()?;
        check_leave(me, &self.cluster.members().voters, &nodes)?;
        let (_, vms) = self.cluster.list::<VmSpec>()?;
        if let Some(vm) = vms
            .iter()
            .find(|vm| vm.spec.node == Some(me.0) || vm.status.node == Some(me.0))
        {
            anyhow::bail!(
                "VM {:?} is still assigned to this node; delete it or move it to another node first",
                vm.name
            );
        }
        // 멤버십에서 빠지면 다른 노드가 이 노드의 요청을 받지 않으므로 Node 리소스부터 지운다.
        match self.cluster.delete::<NodeSpec>(me.0, None).await {
            Ok(_) | Err(ClusterError::Rejected(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.cluster
            .request(LeaderRequest::Remove { id: me })
            .await?;
        Ok(())
    }

    /// 모든 것을 멈춘다. `left`면 클러스터에서 빠진 것이므로 클러스터 저장소와 노드 인증서를 지운다.
    /// 아니면 오프라인으로 기록한다. 실행 중인 VM은 그대로 둔다.
    pub async fn close(mut self, left: bool) -> anyhow::Result<()> {
        // 아래에서 저장소를 닫아 Raft가 멈춰도 실패로 알리지 않는다.
        self.raft_watch.abort();
        if let Some(services) = self.services.take() {
            services.shutdown(left).await;
        }
        self.register.abort();
        self.promotion.abort();
        if !left {
            bootstrap::mark_offline(&self.cluster).await;
        }
        let _ = self.peer.stop.send(());
        // 오래 가는 콘솔 중계 스트림이 있으면 서버가 끝나지 않으므로 기다리는 시간을 제한한다.
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut self.peer.task)
            .await
            .is_err()
        {
            self.peer.task.abort();
        }
        let shutdown = self.cluster.shutdown().await;
        if left {
            // 이미 클러스터에서 빠졌으므로 저장소를 닫지 못했어도 지운다. 남기면 빠진 멤버로 다시 뜬다.
            if let Err(e) = &shutdown {
                tracing::warn!(error = %e, "cannot shut down the cluster store; removing it anyway");
            }
            match std::fs::remove_dir_all(pki_dir(&self.config)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                other => other?,
            }
            remove_cluster_data(&self.config)?;
        }
        shutdown?;
        Ok(())
    }
}

/// 이 노드가 빠진 뒤에도 리더가 될 수 있는 투표 멤버가 남는지 본다. 위트니스는 후보가 되지 않으므로
/// 위트니스만 남으면 클러스터에 리더가 영영 생기지 않는다. Node 리소스가 없는 투표 멤버는 일반 노드로
/// 본다(`get_cluster`처럼).
fn check_leave(
    me: NodeId,
    voters: &BTreeMap<NodeId, String>,
    nodes: &[Typed<NodeSpec>],
) -> anyhow::Result<()> {
    let mut rest = voters.keys().filter(|&&id| id != me).peekable();
    if rest.peek().is_none() {
        anyhow::bail!(
            "this node is the only voting member; stop orbivisord and remove its data directory instead"
        );
    }
    let witness = |id: &NodeId| nodes.iter().any(|n| n.id == id.0 && n.spec.witness);
    if rest.all(witness) {
        anyhow::bail!(
            "the remaining voting members are all witnesses, which cannot lead; add another \
             non-witness node first"
        );
    }
    Ok(())
}

/// 노드 간 서버를 띄운다. 서버가 스스로 멈추면 `exited`로 알린다.
fn serve_peer(
    listener: TcpListener,
    tls: &NodeTls,
    service: PeerService,
    exited: mpsc::Sender<String>,
) -> Server<opl_peer::ServeError> {
    let (stop, rx) = oneshot::channel::<()>();
    let tls = tls.clone();
    let task = tokio::spawn(async move {
        let r = opl_peer::serve(listener, &tls, service, async {
            let _ = rx.await;
        })
        .await;
        let _ = exited.send(format!("cluster server stopped: {r:?}")).await;
        r
    });
    Server { stop, task }
}

/// Raft가 멈추면(저장소 쓰기 실패나 저장소 스레드 멈춤으로 openraft가 멈춘 경우) 그 이유를 `exited`로
/// 보낸다. 주 루프가 0이 아닌 코드로 끝나고 systemd(`Restart=on-failure`)가 다시 띄운다. 그러지 않으면
/// HA VM이 없는 노드는 API만 답하는 채로 남는다.
fn watch_raft(cluster: &Cluster, exited: mpsc::Sender<String>) -> JoinHandle<()> {
    let stopped = cluster.stopped();
    tokio::spawn(async move {
        let why = stopped.await;
        let _ = exited.send(format!("Raft stopped: {why}")).await;
    })
}

/// 다시 시작한 노드는 투표 멤버가 될 때까지 승격을 다시 요청한다. 처음 멤버가 된 노드는 이미 했다.
fn spawn_promotion(first_time: bool, cluster: &Arc<Cluster>, peers: &Arc<Peers>) -> JoinHandle<()> {
    if first_time {
        return tokio::spawn(async {});
    }
    let (cluster, peers) = (cluster.clone(), peers.clone());
    tokio::spawn(async move { promote_until_voter(&cluster, &peers).await })
}

/// 클러스터 저장소(Raft 로그와 상태)를 지운다. 없으면 그대로 성공한다.
pub fn remove_cluster_data(config: &Config) -> std::io::Result<()> {
    let dir = cluster_dir(config);
    match std::fs::remove_dir_all(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// 합류하다 투표 멤버가 되기 전에 멈춘 노드는 learner로 남아 있다. 다시 시작하면 투표 멤버가 될
/// 때까지, 또는 클러스터에서 빠진 것을 알 때까지 승격을 다시 요청한다.
async fn promote_until_voter(cluster: &Cluster, peers: &Peers) {
    let id = cluster.id();
    // 저장한 멤버십이 지표에 반영되기를 기다린다. 합류 직후 죽었으면 리더가 보내 줄 때까지 모른다.
    while cluster
        .raft()
        .wait(Some(Duration::from_secs(10)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .nodes()
                    .any(|(n, _)| *n == id)
            },
            "this node is in the membership",
        )
        .await
        .is_err()
    {
        tracing::info!("this node is not in the cluster membership it knows yet; still waiting");
    }
    let mut retry = PROMOTE_RETRY_MIN;
    let mut asked = false;
    loop {
        let members = cluster.members();
        if members.voters.contains_key(&id) {
            if asked {
                tracing::info!("this node became a voting member");
            } else {
                tracing::info!("this node is a voting member");
            }
            return;
        }
        if !members.learners.contains_key(&id) {
            tracing::info!("this node was removed from the cluster; not asking to be promoted");
            return;
        }
        tracing::info!("this node is not a voting member yet; asking to be promoted");
        asked = true;
        match promote(cluster, peers, None).await {
            Ok(()) => {}
            Err(e) if matches!(e.downcast_ref(), Some(ClusterError::Rejected(_))) => {
                tracing::info!(
                    error = format!("{e:#}"),
                    "this node was removed from the cluster; not asking to be promoted"
                );
                return;
            }
            Err(e) => {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "cannot become a voting member; will retry"
                );
                tokio::time::sleep(retry).await;
                retry = (retry * 2).min(PROMOTE_RETRY_MAX);
            }
        }
    }
}

/// 합류한 노드가 로그를 따라잡으면 투표 멤버로 올려 달라고 리더에게 요청하고, 이 노드도 그것을
/// 적용할 때까지 기다린다. 요청을 받은 노드가 리더가 아니면 알려 준 리더로 다시 보낸다. `via`가
/// 없으면(다시 시작한 노드) 이 노드가 아는 리더나 투표 멤버에게 보낸다.
async fn promote(cluster: &Cluster, peers: &Peers, via: Option<&str>) -> anyhow::Result<()> {
    let id = cluster.id();
    let deadline = tokio::time::Instant::now() + CATCH_UP_TIMEOUT + Duration::from_secs(30);
    let mut redirect: Option<String> = None;
    loop {
        // 다시 시작한 직후에는 리더를 아직 모를 수 있다. 알 때까지 기다린다.
        let Some(addr) = redirect
            .take()
            .or_else(|| via.map(str::to_string))
            .or_else(|| voter_addr(cluster))
        else {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("cannot become a voting member: no voting member is known");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        };
        match peers
            .leader_request(&addr, LeaderRequest::AddVoter { id })
            .await
        {
            Ok(_) => break,
            Err(ClusterError::NotLeader {
                leader_addr: Some(leader),
                ..
            }) => redirect = Some(leader),
            // 리더의 멤버십에 learner로 없다(클러스터에서 빠졌다). 다시 시도해도 소용없다.
            Err(e @ ClusterError::Rejected(_)) => {
                return Err(e).context("this node is not in the cluster membership");
            }
            Err(e) if tokio::time::Instant::now() < deadline => {
                tracing::info!(error = %e, "waiting to become a voting member");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e).context("cannot become a voting member"),
        }
    }
    cluster
        .raft()
        .wait(Some(Duration::from_secs(30)))
        .metrics(
            |m| {
                m.membership_config
                    .membership()
                    .voter_ids()
                    .any(|v| v == id)
            },
            "this node is a voter",
        )
        .await?;
    Ok(())
}

/// 이 노드가 아는 리더의 주소, 모르면 아무 투표 멤버의 주소
fn voter_addr(cluster: &Cluster) -> Option<String> {
    let members = cluster.members();
    members
        .leader
        .and_then(|leader| members.voters.get(&leader))
        .or_else(|| members.voters.values().next())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtimes;
    use opl_types::resource::Resource;

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// `dir` 아래에 데이터를 두는 단일 노드 멤버의 설정
    fn member_config(dir: &std::path::Path) -> Config {
        let mut config: Config = toml::from_str(&format!(
            "name = \"n1\"\naddress = \"127.0.0.1\"\nlisten = \"127.0.0.1:{}\"\n\
             cluster_listen = \"127.0.0.1:{}\"\n[hypervisor]\naccel = \"tcg\"\n\
             systemd_scope = false\n[ha]\nfencing = \"process\"\n[updates]\nenabled = false",
            free_port(),
            free_port()
        ))
        .unwrap();
        config.data_dir = dir.join("data");
        config.run_dir = dir.join("run");
        config
    }

    /// 새 클러스터를 만든 멤버의 서비스가 실행기 셋에 나뉘어 돈다. 부른 쪽의 실행기에는 아무 작업도
    /// 남지 않고(고르지 않고 띄운 서비스가 없다), 닫으면 세 실행기의 작업이 모두 끝난다.
    #[test]
    fn services_run_on_the_three_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        let config = member_config(dir.path());
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let alive = |h: &tokio::runtime::Handle| h.metrics().num_alive_tasks();
        // 주 루프 대신 부르는 쪽
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (member, created) = Member::start(&config, node, tls, bootstrap, &rt)
                .await
                .unwrap();
            assert!(created.is_some());
            let here = tokio::runtime::Handle::current();
            assert_eq!(alive(&here), 0, "tasks left on the caller's runtime");
            for (name, h) in [("core", &rt.core), ("api", &rt.api), ("work", &rt.work)] {
                assert!(alive(h) > 0, "no task on {name}");
            }
            // 빠진 것처럼 닫아 root로 도는 Linux CI에서 만든 방화벽 테이블도 지운다.
            member.close(true).await.unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while [&rt.core, &rt.api, &rt.work].iter().any(|h| alive(h) > 0) {
            assert!(
                std::time::Instant::now() < deadline,
                "tasks left after close: core {} api {} work {}",
                alive(&rt.core),
                alive(&rt.api),
                alive(&rt.work)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Raft가 멈추면 API와 노드 간 서버가 돌아도 `failed`가 그 이유와 함께 끝난다(여기서는 저장소를
    /// 바깥에서 닫는다. 저장소 쓰기가 panic해 openraft가 멈추는 경우는 opl-cluster 시험이 본다).
    #[test]
    fn failed_ends_when_raft_stops() {
        let dir = tempfile::tempdir().unwrap();
        let config = member_config(dir.path());
        let runtimes = Runtimes::new(2, 2).unwrap();
        let rt = runtimes.handles();
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller.block_on(async {
            let node = Uuid::now_v7();
            let (ca, tls) = crate::new_cluster_tls(&config, node, "c").unwrap();
            let bootstrap = Bootstrap::Init {
                name: "c".into(),
                ca,
            };
            let (mut member, _) = Member::start(&config, node, tls, bootstrap, &rt)
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(300), member.failed())
                    .await
                    .is_err()
            );
            member.cluster().shutdown().await.unwrap();
            let e = tokio::time::timeout(Duration::from_secs(10), member.failed())
                .await
                .unwrap();
            assert!(e.to_string().starts_with("Raft stopped: "), "{e}");
            let _ = member.close(true).await;
        });
    }

    #[test]
    fn a_witness_does_not_stand_for_election() {
        assert!(raft_config(false).enable_elect);
        assert!(!raft_config(true).enable_elect);
    }

    /// 남는 투표 멤버가 없거나 모두 위트니스면(리더가 될 수 없다) 빠지지 않는다.
    #[test]
    fn leaving_keeps_a_voter_that_can_lead() {
        let node = |witness: bool| {
            let mut node = Resource::new(
                "n",
                NodeSpec {
                    address: "h:8006".into(),
                    maintenance: false,
                    witness,
                },
            );
            node.id = Uuid::now_v7();
            node
        };
        let (a, b, w) = (node(false), node(false), node(true));
        let id = |n: &Typed<NodeSpec>| NodeId(n.id);
        let voters = |nodes: &[&Typed<NodeSpec>]| {
            nodes
                .iter()
                .map(|n| (id(n), String::new()))
                .collect::<BTreeMap<_, _>>()
        };
        let nodes = [a.clone(), b.clone(), w.clone()];

        // 혼자 남은 투표 멤버
        let only = check_leave(id(&a), &voters(&[&a]), &nodes).unwrap_err();
        assert!(only.to_string().contains("only voting member"), "{only}");
        // 위트니스만 남는다.
        let err = check_leave(id(&a), &voters(&[&a, &w]), &nodes).unwrap_err();
        assert!(err.to_string().contains("all witnesses"), "{err}");
        // 일반 노드가 남으면 누구든 빠질 수 있다. 위트니스도.
        check_leave(id(&a), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &b, &w]), &nodes).unwrap();
        check_leave(id(&w), &voters(&[&a, &w]), &nodes).unwrap();
        // Node 리소스가 없는 투표 멤버는 일반 노드로 본다(get_cluster처럼).
        check_leave(id(&a), &voters(&[&a, &b, &w]), &[a.clone(), w.clone()]).unwrap();
    }
}
```

`crates/opld/src/runtime.rs`에서

```rust
//!
//! - `core`(2개, `orbivisor-core`): Raft, 노드 간 서버, `HaGuard`, `HaManager`, 노드 감시
//! - `api`(`orbivisor-api`): HTTPS API
//! - `work`(`orbivisor-work`): reconciler, Task 실행기, 스케줄러, 정리, 지표, 풀, 텔레메트리 등
//!
```

을 다음으로 바꾼다:

```rust
//!
//! - `core`(2개, `orbivisor-core`): Raft, 노드 간 서버(Raft·리스 요청), `HaGuard`, `HaManager`, 노드 감시
//! - `api`(`orbivisor-api`): HTTPS API
//! - `work`(`orbivisor-work`): reconciler, Task 실행기, 스케줄러, 정리, 지표, 풀, 텔레메트리, 노드 간
//!   서버가 받은 그 밖의 요청(마이그레이션 받기, 콘솔 중계 등)
//!
//! 이 밖에 OS 스레드 둘이 있다: watchdog 쓰다듬기(`orbivisor-watchdog`, `HaGuard`)와 redb 쓰기
//! (`orbivisor-store`, 클러스터 저장소).
//!
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `cargo check -p opl-peer`
Expected: `Cargo.lock`의 opl-peer 의존성 목록이 바뀐다(새 패키지는 없다, 이 Task의 커밋에 넣는다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1706개, 실패 0

Run: `cargo test -p opld --features ha-it --test ha`
Expected: PASS

Run: `cargo test -p opld --features migration-it --test migration`
Expected: PASS (실제 QEMU가 필요하다)

- [ ] **Step 4: 커밋**

```bash
git add Cargo.lock crates
git commit -m "feat(opld): watchdog 쓰다듬기 스레드(orbivisor-watchdog)와 redb 쓰기 스레드(orbivisor-store)"
```

---

### Task 3: systemd 알림

opld `notify.rs`: `Notifier`(`$NOTIFY_SOCKET`, `@`는 Linux 추상 소켓)와 `Liveness`(OS 스레드 `orbivisor-notify`, `WATCHDOG_USEC`가 있으면 그 3분의 1 간격 — 없으면 10초, 마감 2초, core·api가 답한 회차에만 `WATCHDOG=1`, work만 늦으면 경고). `READY=1`은 초기화 안 된 노드는 제어 소켓이 열리면, 멤버는 `TaskRunner::spawn` 직전과 API가 뜬 뒤, 위트니스는 피어 서버가 뜨면. 유닛 `Type=notify`·`NotifyAccess=main`·`WatchdogSec=30s`·`WatchdogSignal=SIGKILL`과 패키징 시험, 실제 프로세스 시험.

**Files:**
- Create: `crates/opld/src/notify.rs`
- Create: `crates/opld/tests/notify.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `crates/opld/src/runtime.rs`
- Modify: `crates/opld/tests/common/mod.rs`
- Modify: `packaging/deb/opld.service`
- Modify: `packaging/tests/build-deb-test.sh`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opld/src/main.rs`에서

```rust
mod metrics;
mod overlay;
```

을 다음으로 바꾼다:

```rust
mod metrics;
mod notify;
mod overlay;
```

`crates/opld/src/main.rs`에서

```rust
use crate::member::{Bootstrap, Member};
use crate::runtime::{Handles, Runtimes};
```

을 다음으로 바꾼다:

```rust
use crate::member::{Bootstrap, Member};
use crate::notify::{Liveness, Notifier};
use crate::runtime::{Handles, Runtimes};
```

`crates/opld/src/main.rs`에서

```rust
    let rt = runtimes.handles();
    runtimes.core.block_on(run(config, rt))
}

async fn run(config: Config, rt: Handles) -> anyhow::Result<()> {
    prepare_data_dir(&config.data_dir)?;
```

을 다음으로 바꾼다:

```rust
    let rt = runtimes.handles();
    // systemd 알림(설계 §3.3). 실행기 확인 스레드는 실행기보다 먼저 멈춘다(선언의 역순으로 놓임).
    let notifier = Notifier::from_env();
    let _liveness = Liveness::spawn(rt.clone(), notifier.clone());
    runtimes.core.block_on(run(config, rt, notifier))
}

async fn run(config: Config, rt: Handles, notifier: Notifier) -> anyhow::Result<()> {
    prepare_data_dir(&config.data_dir)?;
```

`crates/opld/src/main.rs`에서

```rust
                None => {
                    tracing::info!(
```

을 다음으로 바꾼다:

```rust
                None => {
                    // 미초기화 노드는 제어 소켓을 열었으면 시작이 끝난 것이다.
                    notifier.notify("READY=1");
                    tracing::info!(
```

`crates/opld/src/main.rs`에서

```rust
            };
            let mut cluster_name = read_cluster_name(&member);
```

을 다음으로 바꾼다:

```rust
            };
            // API가 듣기 시작했다(위트니스는 노드 간 서버).
            notifier.notify("READY=1");
            let mut cluster_name = read_cluster_name(&member);
```

`crates/opld/src/member.rs`에서

```rust
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::overlay::{OverlayAgent, cluster_tag};
```

을 다음으로 바꾼다:

```rust
use crate::metrics::store::{MetricsStore, StoreSource};
use crate::notify::Notifier;
use crate::overlay::{OverlayAgent, cluster_tag};
```

`crates/opld/src/member.rs`에서

```rust
        runner_config.updates = updates.clone();
        let runner = run_on(
```

을 다음으로 바꾼다:

```rust
        runner_config.updates = updates.clone();
        // Task 실행기의 고아 정리는 쓰기마다 10초까지 기다리므로 정족수 없이 다시 켤 때 systemd의 시작
        // 제한(90초)을 넘길 수 있다. 정리는 API보다 먼저 끝나야 하므로(API가 막 받는 업로드 파일을
        // 고아로 지우지 않게) 뒤로 미루지 않고 READY=1을 그 전에 보낸다(API가 뜬 뒤 다시 보낸다).
        Notifier::from_env().notify("READY=1");
        let runner = run_on(
```

`crates/opld/src/notify.rs`를 만든다:

```rust
//! systemd 알림(설계 §3.3). 유닛이 `Type=notify`·`WatchdogSec=30s`라 시작이 끝나면 `READY=1`을,
//! `core`·`api` 실행기가 빈 작업을 제때 돌리는 동안 `WATCHDOG=1`을 보낸다. 둘 중 하나가 20초 넘게
//! 막히면 쓰다듬기가 멈춰 systemd가 데몬을 끝내고(`WatchdogSignal=SIGKILL`) 다시 띄운다
//! (`KillMode=process`라 VM은 그대로 돈다). `work`가 늦으면 경고만 남긴다: 데몬을 다시 띄우는 동안
//! HA watchdog 장치의 제한 시간(30초)을 넘기면 노드가 재부팅되므로, 리스와 API가 도는 한 끄지 않는다.
//!
//! `$NOTIFY_SOCKET`(유닉스 데이터그램, `@`로 시작하면 Linux 추상 소켓)에 직접 쓴다. 변수가 없으면
//! (시험·macOS·systemd 밖) 아무것도 보내지 않는다.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::runtime::Handles;

/// 실행기 확인 간격(`WATCHDOG_USEC`이 없을 때)
const CHECK_INTERVAL: Duration = Duration::from_secs(10);
/// 빈 작업이 이 안에 돌아와야 그 회차에 쓰다듬는다(`WATCHDOG_USEC`이 없을 때).
const CHECK_DEADLINE: Duration = Duration::from_secs(2);

/// systemd가 준 `WATCHDOG_USEC`(마이크로초)에 맞춘 (확인 간격, 마감). 간격은 그 1/3(1초~10초),
/// 마감은 2초와 간격의 절반 중 짧은 것이다. 없거나 읽지 못하면 10초·2초다.
fn timing(watchdog_usec: Option<&str>) -> (Duration, Duration) {
    match watchdog_usec.and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(usec) => {
            let interval = (Duration::from_micros(usec) / 3)
                .clamp(Duration::from_secs(1), Duration::from_secs(10));
            (interval, CHECK_DEADLINE.min(interval / 2))
        }
        None => (CHECK_INTERVAL, CHECK_DEADLINE),
    }
}

/// `$NOTIFY_SOCKET`으로 상태를 보낸다.
#[derive(Clone)]
pub struct Notifier {
    addr: Option<SocketAddr>,
}

impl Notifier {
    pub fn from_env() -> Self {
        Self::new(std::env::var_os("NOTIFY_SOCKET").as_deref())
    }

    fn new(socket: Option<&OsStr>) -> Self {
        let addr = socket
            .filter(|s| !s.is_empty())
            .and_then(|s| match socket_addr(s) {
                Ok(addr) => Some(addr),
                Err(e) => {
                    tracing::warn!(error = %e, socket = ?s, "cannot use NOTIFY_SOCKET");
                    None
                }
            });
        Self { addr }
    }

    /// `state`(예: `READY=1`)를 보낸다. 실패하면 경고만 남긴다.
    pub fn notify(&self, state: &str) {
        let Some(addr) = &self.addr else { return };
        let sent = UnixDatagram::unbound().and_then(|s| s.send_to_addr(state.as_bytes(), addr));
        if let Err(e) = sent {
            tracing::warn!(error = %e, state, "cannot notify systemd");
        }
    }
}

fn socket_addr(socket: &OsStr) -> io::Result<SocketAddr> {
    match socket.as_bytes() {
        [b'@', name @ ..] => abstract_addr(name),
        _ => SocketAddr::from_pathname(socket),
    }
}

#[cfg(target_os = "linux")]
fn abstract_addr(name: &[u8]) -> io::Result<SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    SocketAddr::from_abstract_name(name)
}

#[cfg(not(target_os = "linux"))]
fn abstract_addr(_name: &[u8]) -> io::Result<SocketAddr> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "abstract sockets are Linux-only",
    ))
}

/// 실행기 확인 스레드(`orbivisor-notify`). 놓으면 멈춘다.
pub struct Liveness {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    /// 마지막 회차에 늦은 실행기
    slow: Arc<Mutex<Vec<&'static str>>>,
}

impl Liveness {
    /// 바로 한 번, 그 뒤 10초(`WATCHDOG_USEC`의 1/3)마다 세 실행기를 확인하고 `core`·`api`가 2초 안에
    /// 답한 회차에 `WATCHDOG=1`을 보낸다.
    pub fn spawn(rt: Handles, notifier: Notifier) -> Self {
        let (interval, deadline) = timing(std::env::var("WATCHDOG_USEC").ok().as_deref());
        Self::start(rt, notifier, interval, deadline)
    }

    fn start(rt: Handles, notifier: Notifier, interval: Duration, deadline: Duration) -> Self {
        let (stop, rx) = mpsc::channel();
        let last = Arc::new(Mutex::new(Vec::new()));
        let slow_out = last.clone();
        let thread = std::thread::Builder::new()
            .name("orbivisor-notify".into())
            .spawn(move || {
                loop {
                    let slow = check(&rt, deadline);
                    *last.lock().unwrap() = slow.clone();
                    if slow.iter().all(|name| *name == "work") {
                        notifier.notify("WATCHDOG=1");
                    }
                    if slow == ["work"] {
                        tracing::warn!(
                            runtimes = ?slow,
                            "runtimes did not run a task within {deadline:?}; still petting the systemd watchdog"
                        );
                    } else if !slow.is_empty() {
                        tracing::warn!(
                            runtimes = ?slow,
                            "runtimes did not run a task within {deadline:?}; not petting the systemd watchdog"
                        );
                    }
                    if !matches!(rx.recv_timeout(interval), Err(RecvTimeoutError::Timeout)) {
                        return;
                    }
                }
            })
            .expect("cannot start the notify thread");
        Self {
            stop: Some(stop),
            thread: Some(thread),
            slow: slow_out,
        }
    }

    /// 마지막 회차에 마감 안에 답하지 않은 실행기(`core`·`api`·`work`). 계획 46의 헬스가 읽는다.
    pub fn slow(&self) -> Vec<&'static str> {
        self.slow.lock().unwrap().clone()
    }
}

/// 마지막 회차에 늦은 실행기를 보인다.
impl std::fmt::Debug for Liveness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Liveness")
            .field("slow", &self.slow())
            .finish_non_exhaustive()
    }
}

impl Drop for Liveness {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 세 실행기에 빈 작업을 보내 `deadline` 안에 돌리지 못한 실행기의 이름
fn check(rt: &Handles, deadline: Duration) -> Vec<&'static str> {
    let (tx, rx) = mpsc::channel();
    let runtimes = [("core", &rt.core), ("api", &rt.api), ("work", &rt.work)];
    for (name, handle) in runtimes {
        let tx = tx.clone();
        // 끝난 실행기면 작업이 버려지고 `tx`가 닫힌다.
        handle.spawn(async move {
            let _ = tx.send(name);
        });
    }
    drop(tx);
    let mut waiting: Vec<_> = runtimes.iter().map(|(name, _)| *name).collect();
    let until = Instant::now() + deadline;
    while !waiting.is_empty() {
        match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(name) => waiting.retain(|n| *n != name),
            Err(_) => break,
        }
    }
    waiting
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;
    use crate::runtime::Runtimes;

    /// 받은 데이터그램 하나. 시간 안에 없으면 `None`.
    fn recv(socket: &UnixDatagram, timeout: Duration) -> Option<String> {
        socket.set_read_timeout(Some(timeout)).unwrap();
        let mut buf = [0u8; 256];
        let n = socket.recv(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// `want`이 올 때까지 받는다.
    fn wait_for(socket: &UnixDatagram, want: &str) {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if recv(socket, Duration::from_millis(500)).as_deref() == Some(want) {
                return;
            }
        }
        panic!("{want} did not arrive");
    }

    #[test]
    fn notifies_the_socket_path_and_does_nothing_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        Notifier::new(Some(path.as_os_str())).notify("READY=1");
        assert_eq!(
            recv(&socket, Duration::from_secs(1)).as_deref(),
            Some("READY=1")
        );
        // 변수가 없거나 비었거나 받는 쪽이 없으면 보내지 않는다(실패해도 돌아온다).
        for none in [None, Some(OsStr::new(""))] {
            assert!(Notifier::new(none).addr.is_none());
        }
        Notifier::new(Some(dir.path().join("missing").as_os_str())).notify("READY=1");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn notifies_an_abstract_socket() {
        use std::os::linux::net::SocketAddrExt;
        let name = format!("orbivisor-notify-test-{}", uuid::Uuid::now_v7());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let socket = UnixDatagram::bind_addr(&addr).unwrap();
        Notifier::new(Some(OsStr::new(&format!("@{name}")))).notify("READY=1");
        assert_eq!(
            recv(&socket, Duration::from_secs(1)).as_deref(),
            Some("READY=1")
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn abstract_sockets_are_ignored_off_linux() {
        assert!(Notifier::new(Some(OsStr::new("@x"))).addr.is_none());
    }

    /// `handle` 실행기의 작업자 `workers`개를 모두 `how_long` 동안 동기 sleep으로 막는다.
    fn block(handle: &tokio::runtime::Handle, workers: usize, how_long: Duration) {
        let barrier = Arc::new(Barrier::new(workers + 1));
        for _ in 0..workers {
            let barrier = barrier.clone();
            handle.spawn(async move {
                barrier.wait();
                std::thread::sleep(how_long);
            });
        }
        barrier.wait();
    }

    /// 실행기가 답하면 쓰다듬고, `api`나 `core` 실행기의 모든 스레드가 막히면 멈췄다가 풀리면 다시
    /// 쓰다듬는다.
    #[test]
    fn pets_only_while_core_and_api_run_tasks() {
        let rt = Runtimes::new(2, 2).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        let notifier = Notifier::new(Some(path.as_os_str()));
        let liveness = Liveness::start(
            rt.handles(),
            notifier,
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        wait_for(&socket, "WATCHDOG=1");

        for (name, handle) in [("api", rt.api.handle()), ("core", rt.core.handle())] {
            block(handle, 2, Duration::from_secs(3));
            // 막기 전에 시작한 회차가 끝나기를 기다리고 그때까지 온 것은 버린다.
            std::thread::sleep(Duration::from_millis(400));
            while recv(&socket, Duration::from_millis(1)).is_some() {}
            assert_eq!(recv(&socket, Duration::from_secs(1)), None, "{name}");
            assert_eq!(check(&rt.handles(), Duration::from_millis(500)), [name]);
            assert!(liveness.slow().contains(&name), "{name}");

            wait_for(&socket, "WATCHDOG=1");
        }
        drop(liveness);
    }

    /// `work`만 막히면 계속 쓰다듬고 마지막 회차에 늦은 실행기로 남긴다.
    #[test]
    fn keeps_petting_while_only_work_is_blocked() {
        let rt = Runtimes::new(2, 2).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let socket = UnixDatagram::bind(&path).unwrap();
        let notifier = Notifier::new(Some(path.as_os_str()));
        let liveness = Liveness::start(
            rt.handles(),
            notifier,
            Duration::from_millis(100),
            Duration::from_millis(300),
        );
        wait_for(&socket, "WATCHDOG=1");
        assert_eq!(liveness.slow(), Vec::<&str>::new());

        block(rt.work.handle(), 2, Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(400));
        while recv(&socket, Duration::from_millis(1)).is_some() {}
        wait_for(&socket, "WATCHDOG=1");
        assert_eq!(liveness.slow(), ["work"]);
        drop(liveness);
    }

    /// `WATCHDOG_USEC`의 1/3마다(1초~10초) 확인하고, 마감은 2초와 그 절반 중 짧은 것이다.
    #[test]
    fn timing_follows_watchdog_usec() {
        let secs = Duration::from_secs;
        let ms = Duration::from_millis;
        assert_eq!(timing(None), (secs(10), secs(2)));
        assert_eq!(timing(Some("x")), (secs(10), secs(2)));
        assert_eq!(timing(Some("30000000")), (secs(10), secs(2)));
        assert_eq!(timing(Some("120000000")), (secs(10), secs(2)));
        assert_eq!(timing(Some("6000000")), (secs(2), secs(1)));
        assert_eq!(timing(Some("4500000")), (ms(1500), ms(750)));
        assert_eq!(timing(Some("1000000")), (secs(1), ms(500)));
    }
}
```

`crates/opld/src/runtime.rs`에서

```rust
//!
//! 이 밖에 OS 스레드 둘이 있다: watchdog 쓰다듬기(`orbivisor-watchdog`, `HaGuard`)와 redb 쓰기
//! (`orbivisor-store`, 클러스터 저장소).
//!
```

을 다음으로 바꾼다:

```rust
//!
//! 이 밖에 OS 스레드 셋이 있다: watchdog 쓰다듬기(`orbivisor-watchdog`, `HaGuard`), redb 쓰기
//! (`orbivisor-store`, 클러스터 저장소)와 실행기 확인·systemd 알림(`orbivisor-notify`).
//!
```

`crates/opld/tests/common/mod.rs`에서

```rust
    pub join_url: Option<String>,
}
```

을 다음으로 바꾼다:

```rust
    pub join_url: Option<String>,
    /// 있으면 opld에 `NOTIFY_SOCKET`으로 준다(systemd 알림 시험).
    pub notify_socket: Option<PathBuf>,
}
```

`crates/opld/tests/common/mod.rs`에서

```rust
            join_url: None,
        }
```

을 다음으로 바꾼다:

```rust
            join_url: None,
            notify_socket: None,
        }
```

`crates/opld/tests/common/mod.rs`에서

```rust
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_orbivisord"))
            .arg("--config")
            .arg(self.path("opld.toml"))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
```

을 다음으로 바꾼다:

```rust
            .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_orbivisord"));
        command
            .arg("--config")
            .arg(self.path("opld.toml"))
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        match &self.notify_socket {
            Some(path) => command.env("NOTIFY_SOCKET", path),
            None => command.env_remove("NOTIFY_SOCKET"),
        };
        let child = command.spawn().unwrap();
        self.child = Some(child);
```

`crates/opld/tests/notify.rs`를 만든다:

```rust
//! systemd 알림: opld가 `NOTIFY_SOCKET`으로 시작이 끝나면 `READY=1`을, 실행기 확인이 되면 `WATCHDOG=1`을
//! 보내는지 본다(설계 §3.3).

mod common;

use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use common::Daemon;

/// `want`이 올 때까지 받는다.
fn wait_for(socket: &UnixDatagram, want: &str) {
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let until = Instant::now() + common::WAIT;
    let mut buf = [0u8; 256];
    while Instant::now() < until {
        if let Ok(n) = socket.recv(&mut buf)
            && &buf[..n] == want.as_bytes()
        {
            return;
        }
    }
    panic!("{want} did not arrive");
}

#[tokio::test]
async fn notifies_systemd_when_ready_and_while_runtimes_answer() {
    let mut d = Daemon::new("node1", "");
    let path = d.path("notify");
    let socket = UnixDatagram::bind(&path).unwrap();
    d.notify_socket = Some(path);
    // 미초기화 노드도 제어 소켓을 열면 시작이 끝난 것이다.
    d.start().await;
    wait_for(&socket, "READY=1");
    wait_for(&socket, "WATCHDOG=1");
    // 멤버가 되면(API가 듣기 시작하면) 다시 알린다.
    d.init("prod").await;
    wait_for(&socket, "READY=1");
    // 다시 띄운 멤버는 API가 들은 뒤 알린다.
    d.stop();
    d.start().await;
    wait_for(&socket, "READY=1");
}

/// 정족수 없이 다시 켠 노드는 고아 정리(지우기 쓰기마다 10초까지 기다림)가 끝나기 전에 `READY=1`을
/// 보낸다. 고아가 많아도 systemd의 시작 제한(90초)에 걸려 다시 띄워지지 않는다.
#[tokio::test]
async fn ready_comes_before_the_orphan_sweep_without_quorum() {
    let mut a = Daemon::new("node1", "");
    let mut b = Daemon::new("node2", "");
    a.start().await;
    b.start().await;
    a.init("prod").await;
    a.login().await;
    b.join(a.join_url.as_deref().unwrap()).await.unwrap();
    common::eventually("two voters", || async {
        let status = a.ctl_json(&["cluster", "status"]).await;
        status["members"]
            .as_array()
            .is_some_and(|m| m.iter().filter(|m| m["voter"] == true).count() == 2)
    })
    .await;

    // node1에 끝나지 않은 업로드(만드는 중인 볼륨)를 남기고 kill -9 한다.
    let pool = a.path("pool");
    std::fs::create_dir_all(&pool).unwrap();
    a.ctl(&["pool", "create", "pool1", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(a.path("oplctl.json")).unwrap()).unwrap();
    let token = config["token"].as_str().unwrap().to_string();
    let url = format!(
        "{}/api/v1/volumes/import?name=orphan&pool=pool1&source_format=raw",
        a.server()
    );
    let body = futures_util::stream::once(async { Ok::<_, std::io::Error>(vec![0u8; 1024]) })
        .chain(futures_util::stream::pending());
    let upload = tokio::spawn(
        reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .unwrap()
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_LENGTH, 1 << 20)
            .body(reqwest::Body::wrap_stream(body))
            .send(),
    );
    common::eventually("the uploading volume", || async {
        let volumes = a.ctl_json(&["volume", "list"]).await;
        volumes
            .as_array()
            .is_some_and(|v| v.iter().any(|v| v["name"] == "orphan"))
    })
    .await;
    a.kill();
    upload.abort();
    b.stop();

    let path = a.path("notify");
    let socket = UnixDatagram::bind(&path).unwrap();
    a.notify_socket = Some(path);
    let ready = std::thread::spawn(move || {
        wait_for(&socket, "READY=1");
        Instant::now()
    });
    a.start().await;
    let api = Instant::now();
    let ready = ready.join().unwrap();
    assert!(
        api.saturating_duration_since(ready) >= Duration::from_secs(5),
        "READY=1 came {:?} before the API",
        api.saturating_duration_since(ready)
    );
}
```

`packaging/deb/opld.service`에서

```ini
[Service]
Type=simple
ExecStart=/usr/bin/orbivisord --config /etc/opl/opld.toml
```

을 다음으로 바꾼다:

```ini
[Service]
# 시작이 끝나면 READY=1을, core·api 실행기가 답하는 동안 10초마다 WATCHDOG=1을 보낸다. 30초 넘게
# 없으면 systemd가 SIGKILL로 끝내고 다시 띄운다(코어 덤프로 늦지 않게, 진단은 늦은 회차의 경고 로그.
# KillMode=process라 VM은 그대로 돈다).
Type=notify
NotifyAccess=main
WatchdogSec=30s
WatchdogSignal=SIGKILL
ExecStart=/usr/bin/orbivisord --config /etc/opl/opld.toml
```

`packaging/tests/build-deb-test.sh`에서

```sh
	fail "유닛: $(ls -l "$UNIT_DIR" 2>&1)"
fi
```

을 다음으로 바꾼다:

```sh
	fail "유닛: $(ls -l "$UNIT_DIR" 2>&1)"
fi
# systemd 알림(설계 §3.3): 시작이 끝나면 READY=1, core·api가 답하는 동안 WATCHDOG=1(없으면 SIGKILL). 시작 제한은 기본(90초).
if grep -qx 'Type=notify' "$UNIT_DIR/opld.service" && grep -qx 'NotifyAccess=main' "$UNIT_DIR/opld.service" &&
	grep -qx 'WatchdogSec=30s' "$UNIT_DIR/opld.service" && grep -qx 'WatchdogSignal=SIGKILL' "$UNIT_DIR/opld.service" &&
	grep -qx 'Restart=on-failure' "$UNIT_DIR/opld.service" &&
	grep -qx 'KillMode=process' "$UNIT_DIR/opld.service" && ! grep -q '^TimeoutStartSec=' "$UNIT_DIR/opld.service"; then
	ok "유닛은 Type=notify·WatchdogSec=30s·WatchdogSignal=SIGKILL·NotifyAccess=main"
else
	fail "유닛 systemd 알림: $(cat "$UNIT_DIR/opld.service")"
fi
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1713개, 실패 0

Run: `bash packaging/tests/build-deb-test.sh`
Expected: ok

Run: `bash packaging/tests/name-test.sh`
Expected: ok

- [ ] **Step 3: 커밋**

```bash
git add crates packaging
git commit -m "feat(opld): systemd 알림 — READY=1과 실행기 확인 뒤 WATCHDOG=1, 유닛 Type=notify·WatchdogSec=30s"
```

---

### Task 4: 굶김 시험과 문서

시험 기능 `starve-it`(배포 빌드에는 없음): 설정 `[test] block_api_ms`, 제어 소켓의 `POST /v1/test/block-api`(api 실행기의 모든 작업자를 동기 sleep으로 막음)와 `GET /v1/test/raft`(리더·임기·리스 나이). 두 노드 실제 프로세스 굶김 시험(막힌 동안 리스 나이 < 3초, 리더·임기 그대로, 다른 노드로 보낸 쓰기 커밋, `WATCHDOG=1` 멈춤과 느린 api 경고). CI `check` 작업에 이 시험 단계. README "제어부 가용성" 절, `opld.toml.example`의 `[runtime]`, 설계 문서 §11 "구현(계획 45)".

**Files:**
- Create: `crates/opld/src/starve.rs`
- Create: `crates/opld/tests/starve.rs`
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `crates/opld/Cargo.toml`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/control.rs`
- Modify: `crates/opld/src/main.rs`
- Modify: `crates/opld/src/member.rs`
- Modify: `docs/superpowers/specs/2026-11-11-availability-scale-design.md`
- Modify: `packaging/deb/opld.toml.example`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
      - run: cargo test --workspace
      - name: Storage integration tests (qemu-img)
```

을 다음으로 바꾼다:

```yaml
      - run: cargo test --workspace
      # 시험 빌드에만 있는 고리로 리더의 api 실행기를 막는 굶김 시험(QEMU 없음)
      - run: cargo test -p opld --features starve-it --test starve
      - name: Storage integration tests (qemu-img)
```

`README.md`에서

```markdown
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
TOML
```

을 다음으로 바꾼다:

```markdown
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
# [runtime]
# api_threads = 4           # API 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
# work_threads = 4          # VM·작업 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
TOML
```

`README.md`에서

````markdown
sudo modprobe -r softdog                                        # 끝나면
```

## 노드 유지보수와 비우기
````

을 다음으로 바꾼다:

````markdown
sudo modprobe -r softdog                                        # 끝나면
```

## 제어부 가용성

API·작업·느린 디스크가 Raft 하트비트와 HA 리스 갱신을 늦춰 멀쩡한 노드가 펜싱되지 않게, orbivisord는 한
프로세스 안에서 일을 서로 다른 스레드에 나눈다([설계 문서](docs/superpowers/specs/2026-11-11-availability-scale-design.md) §3·§11).

| 실행기·스레드 | 스레드 이름 | 도는 것 |
|---|---|---|
| `core`(2개 고정) | `orbivisor-core` | Raft, 노드 간 서버(모든 연결의 TLS·HTTP/2 처리와 Raft·리스·전달 요청), 다른 노드로 가는 Raft·리스·전달 연결, HA 리스 갱신, HA 복구(리더), 노드 감시, 제어 소켓 |
| `api` | `orbivisor-api` | HTTPS API(웹 콘솔·watch 포함) |
| `work` | `orbivisor-work` | reconciler, 작업(Task) 실행기, 스케줄러, 정리, 지표, 풀, 텔레메트리, 원격 확인, 업데이트, 노드 간 서버가 받은 무거운 요청의 처리기(들어오는 마이그레이션, 콘솔 중계, UEFI 변수, 감사 로그·지표 읽기), 다른 노드로 가는 콘솔·마이그레이션·감사·지표 연결 |
| OS 스레드 | `orbivisor-watchdog` | HA watchdog 장치 쓰다듬기(1초마다, 실행기가 막혀도 리스 규칙대로) |
| OS 스레드 | `orbivisor-store` | 클러스터 저장소(redb)의 쓰기와 fsync, 차례대로 하나씩 |
| OS 스레드 | `orbivisor-notify` | 실행기 확인과 systemd 알림 |

```toml
# /etc/opl/opld.toml (노드마다, 생략하면 아래 기본값)
[runtime]
# api_threads = 4     # api 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
# work_threads = 4    # work 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
```

- **노드 간 연결.** 노드 간 서버의 연결마다의 TLS·HTTP/2 처리는 `core`에서 돈다. 그래서 콘솔의 VNC 바이트와
  UEFI 변수 조각을 틀에 넣고 암호화하는 일은 `core` 스레드가 하고, `work`에서는 그 요청의 처리기만 돈다. 팔로어가
  리더에게 전달하는 큰 API 쓰기도 `core`의 연결로 가므로 `core`에서 암호화된다. 다른
  노드로 가는 연결은 둘로 나눈다: Raft·리스 갱신·리더 전달·생존 확인은 `core`의 연결, 콘솔·마이그레이션 보내기·UEFI
  변수·감사 로그·지표 읽기는 `work`의 연결이다(처음 연 쪽 실행기와 관계없이). 대량 전송용 포트를 따로 두는 것은
  다중 링크(계획 49)에서 다룬다.
- **systemd watchdog.** 유닛(`opld.service`)은 `Type=notify`, `NotifyAccess=main`, `WatchdogSec=30s`,
  `WatchdogSignal=SIGKILL`이다. orbivisord는 시작이 끝나면 `READY=1`을 보낸다(클러스터에 속하지 않은 노드는 제어
  소켓을 열었을 때, 멤버는 작업 실행기가 이전 프로세스의 고아를 정리하기 직전과 API가 듣기 시작했을 때, 위트니스는
  노드 간 서버가 떴을 때). 고아 정리는 정족수가 없으면 지우기마다 10초까지 기다리므로, 클러스터 전체를 다시 켤 때
  시작 제한(90초)에 걸리지 않게 그 전에 보낸다. `orbivisor-notify` 스레드가 10초(`WATCHDOG_USEC`의 1/3, 1초~10초)마다
  세 실행기에 빈 작업을 보내 `core`와 `api`가 2초(간격의 절반이 더 짧으면 그만큼) 안에 돌리면 `WATCHDOG=1`을 보낸다.
  늦으면 보내지 않고 경고("runtimes did not run a task within 2s; not petting the systemd watchdog", 늦은 실행기
  이름)를 남긴다. `work`만 늦으면 경고("…; still petting the systemd watchdog")만 남기고 계속 보낸다(데몬을 다시
  띄우는 동안 HA VM이 있는 노드가 재부팅될 수 있으므로 리스와 API가 도는 한 끄지 않는다). `core`나 `api`가 20초 넘게
  막히면 쓰다듬기가 30초 넘게 끊겨 systemd가 orbivisord를 SIGKILL로 끝내고(코어 덤프로 늦어지지 않게, 진단은 경고
  로그로) 5초 뒤 다시 띄운다(`Restart=on-failure`). `KillMode=process`라 VM은 그대로 돌고, 다시 뜬 orbivisord가 VM에
  다시 붙는다. `systemctl status opld`가 `activating`에 머물면 `READY=1`이 아직 오지 않은 것이다(로그를 본다).
- **HA watchdog과의 관계.** HA watchdog 장치(노드 재시작)는 HA VM이 도는데 리스를 잃었을 때의 것이고, systemd
  watchdog(데몬 재시작)은 데몬이 멈췄을 때의 것이다. 데몬이 다시 시작되는 동안 HA VM이 있으면 장치 쓰다듬기도
  멈춘다(죽은 프로세스가 닫은 장치의 타이머는 그대로 돈다). 새 프로세스가 장치를 다시 열면(arm) Linux 드라이버가
  타이머를 다시 시작하므로, 마지막 쓰다듬기부터 다시 arm하기까지가 `watchdog_timeout_secs`(기본 30초)보다 짧아야
  노드가 재시작되지 않는다. 그 사이에 드는 것: `RestartSec`(5초), 프로세스 시작과 클러스터 저장소 열기, 도는 VM마다
  차례로 QMP에 다시 붙기(VM 수에 비례, 붙지 못하는 VM은 하나에 5초까지), 첫 리스 갱신(리더가 있어야 한다),
  reconciler가 도는 HA VM에 붙으며 하는 arm. VM이 많은 노드는 배포 뒤 이 시간을 재 본다.
- **Raft가 멈추면.** 저장소 쓰기가 실패하거나 저장소 스레드가 멈춰 Raft(openraft)가 멈추면 orbivisord는
  "Raft stopped: …"로 0이 아닌 코드로 끝나고 systemd가 다시 띄운다(API만 답하는 채로 남지 않는다).
- **느린 디스크.** 저장소 쓰기는 `orbivisor-store` 스레드에서 하므로 fsync가 실행기 스레드를 막지 않는다. 다만
  Raft(openraft)는 자기 로그 쓰기가 끝나기를 기다리므로, 디스크가 아주 느린 노드의 Raft 루프는 멈춘다. 그 노드가
  팔로어면 그 노드만 멈칫하고(선거를 시작할 수 있다) 다른 노드는 영향을 받지 않는다. 그 노드가 **리더**면 리더의
  Raft 루프가 멈춰 리스 갱신에 필요한 리더 확인(`confirm_leader`)이 실패하므로 **모든 노드의 리스 갱신이 실패**하고,
  다른 노드가 선거로 리더가 된 뒤에 회복한다.
- **끄기.** 장애를 조사하느라 systemd watchdog을 잠시 끄려면 `systemctl edit opld`로 `[Service]`에 `WatchdogSec=0`을
  둔다.
- **시험.** `cargo test -p opld --features starve-it --test starve`가 opld 셋의 리더의 `api` 실행기를 15초 동안 막고
  그동안 리더·임기가 그대로이고 모든 노드의 리스 나이가 3초 미만이며 `WATCHDOG=1`이 멈추는지 본다. `starve-it`
  기능은 시험 빌드에만 있는 고리(`[test] block_api_ms`, 제어 소켓의 `/v1/test/…`)를 넣으므로 배포 빌드에 켜지 않는다.

## 노드 유지보수와 비우기
````

`README.md`에서

```markdown
cargo test -p opld --test cluster                                   # E2E: orbivisord 프로세스 3개로 클러스터 구성
# Linux, root: 실제 tap과 브리지
```

을 다음으로 바꾼다:

```markdown
cargo test -p opld --test cluster                                   # E2E: orbivisord 프로세스 3개로 클러스터 구성
cargo test -p opld --features starve-it --test starve              # E2E: 리더의 API 실행기를 막아도 리스·Raft 리더 그대로
# Linux, root: 실제 tap과 브리지
```

`README.md`에서

```markdown
- [실시간 지표와 쉬운 화면 설계 (하위 프로젝트 14: 계획 44~47)](docs/superpowers/specs/2026-11-11-monitoring-ui-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

을 다음으로 바꾼다:

```markdown
- [실시간 지표와 쉬운 화면 설계 (하위 프로젝트 14: 계획 44~47)](docs/superpowers/specs/2026-11-11-monitoring-ui-design.md)
- [제어부 가용성과 대규모 클러스터 설계 (하위 프로젝트 15: 계획 45~56)](docs/superpowers/specs/2026-11-11-availability-scale-design.md)
- [계획 1: 워크스페이스, 리소스 모델, QEMU 제어](docs/superpowers/plans/2026-10-20-plan1-workspace-types-qemu.md)
```

`crates/opld/Cargo.toml`에서

```toml
ha-it = []
```

을 다음으로 바꾼다:

```toml
ha-it = []
# 시험 빌드에만 있는 굶김 시험 고리: 설정 `[test] block_api_ms`와 제어 소켓의 `POST /v1/test/block-api`
# (`api` 실행기의 작업자를 모두 막음)·`GET /v1/test/raft`(리더·임기·리스 나이). 배포 빌드에는 넣지 않는다.
# opld 셋으로 리더의 `api` 실행기를 막아도 HA 리스와 Raft 리더가 그대로인지 보는 시험(QEMU 없음).
starve-it = []
```

`crates/opld/src/config.rs`에서

```rust
    pub runtime: RuntimeSection,
}
```

을 다음으로 바꾼다:

```rust
    pub runtime: RuntimeSection,
    /// 시험 빌드(`starve-it`)에만 있다. 배포 빌드는 `[test]`를 모르는 키로 거부한다.
    #[cfg(feature = "starve-it")]
    #[serde(default)]
    pub test: TestSection,
}
```

`crates/opld/src/config.rs`에서

```rust
    (cpus / 2).max(2)
}
```

을 다음으로 바꾼다:

```rust
    (cpus / 2).max(2)
}

/// 굶김 시험의 고리(설계 §3.4, 시험 빌드에만)
#[cfg(feature = "starve-it")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestSection {
    /// 제어 소켓의 `POST /v1/test/block-api`가 `api` 실행기의 작업자를 모두 막는 시간(밀리초)
    #[serde(default)]
    pub block_api_ms: u64,
}

#[cfg(feature = "starve-it")]
impl TestSection {
    pub fn block_api(&self) -> Duration {
        Duration::from_millis(self.block_api_ms)
    }
}
```

`crates/opld/src/config.rs`에서

```rust
    }

    #[test]
    fn telemetry_section_defaults_and_overrides() {
```

을 다음으로 바꾼다:

```rust
    }

    /// `[test]`는 시험 빌드에만 있다.
    #[test]
    fn the_test_section_exists_only_in_starve_builds() {
        let parsed =
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[test]\nblock_api_ms = 15000");
        #[cfg(feature = "starve-it")]
        assert_eq!(parsed.unwrap().test.block_api(), Duration::from_secs(15));
        #[cfg(not(feature = "starve-it"))]
        assert!(parsed.is_err());
    }

    #[test]
    fn telemetry_section_defaults_and_overrides() {
```

`crates/opld/src/control.rs`에서

```rust
fn router(ctx: Ctx) -> Router {
    Router::new()
        .route(
```

을 다음으로 바꾼다:

```rust
fn router(ctx: Ctx) -> Router {
    let router = Router::new()
        .route(
```

`crates/opld/src/control.rs`에서

```rust
            }),
        )
        .with_state(ctx)
}
```

을 다음으로 바꾼다:

```rust
            }),
        );
    #[cfg(feature = "starve-it")]
    let router = router.merge(crate::starve::routes());
    router.with_state(ctx)
}
```

`crates/opld/src/main.rs`에서

```rust
mod runtime;
mod web;
```

을 다음으로 바꾼다:

```rust
mod runtime;
#[cfg(feature = "starve-it")]
mod starve;
mod web;
```

`crates/opld/src/main.rs`에서

```rust
    let rt = runtimes.handles();
    // systemd 알림(설계 §3.3). 실행기 확인 스레드는 실행기보다 먼저 멈춘다(선언의 역순으로 놓임).
```

을 다음으로 바꾼다:

```rust
    let rt = runtimes.handles();
    #[cfg(feature = "starve-it")]
    starve::install(rt.api.clone(), config.test.block_api());
    // systemd 알림(설계 §3.3). 실행기 확인 스레드는 실행기보다 먼저 멈춘다(선언의 역순으로 놓임).
```

`crates/opld/src/main.rs`에서

```rust
            notifier.notify("READY=1");
            let mut cluster_name = read_cluster_name(&member);
```

을 다음으로 바꾼다:

```rust
            notifier.notify("READY=1");
            #[cfg(feature = "starve-it")]
            starve::set_cluster(member.cluster_weak());
            let mut cluster_name = read_cluster_name(&member);
```

`crates/opld/src/member.rs`에서

```rust

    /// API 서버나 노드 간 서버가 스스로 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
```

을 다음으로 바꾼다:

```rust

    /// 시험 고리가 쥐는 클러스터 저장소(약한 참조라 닫힌 저장소를 붙잡지 않는다)
    #[cfg(feature = "starve-it")]
    pub fn cluster_weak(&self) -> std::sync::Weak<Cluster> {
        Arc::downgrade(&self.cluster)
    }

    /// API 서버나 노드 간 서버가 스스로 멈추거나 Raft가 멈추면 끝난다.
    pub async fn failed(&mut self) -> anyhow::Error {
```

`crates/opld/src/starve.rs`를 만든다:

```rust
//! 굶김 시험의 고리(설계 §3.4). 시험 빌드(`starve-it`)에만 있고 제어 소켓에 경로 둘을 더한다.
//!
//! - `POST /v1/test/block-api`: `api` 실행기의 작업자를 모두 `[test] block_api_ms` 동안 동기 sleep으로
//!   막는다. 모두 막힌 뒤에 답한다.
//! - `GET /v1/test/raft`: 이 노드가 아는 Raft 리더·임기와, 리더면 노드별 HA 리스 나이(밀리초)

use std::collections::BTreeMap;
use std::sync::{Arc, Barrier, Mutex, OnceLock, Weak};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::{get, post};
use opl_cluster::Cluster;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::time::Instant;
use uuid::Uuid;

struct Hooks {
    api: Handle,
    block: Duration,
    /// 지금 멤버의 클러스터 저장소. 닫힌 저장소를 붙잡지 않게 약한 참조로 둔다.
    cluster: Mutex<Weak<Cluster>>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// 실행기를 만든 뒤 한 번 부른다.
pub fn install(api: Handle, block: Duration) {
    let hooks = Hooks {
        api,
        block,
        cluster: Mutex::new(Weak::new()),
    };
    assert!(
        HOOKS.set(hooks).is_ok(),
        "the test hooks are already installed"
    );
}

/// 멤버가 되면(다시 시작 포함) 그 클러스터 저장소를 알린다.
pub fn set_cluster(cluster: Weak<Cluster>) {
    if let Some(hooks) = HOOKS.get() {
        *hooks.cluster.lock().unwrap() = cluster;
    }
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/v1/test/block-api", post(block_api))
        .route("/v1/test/raft", get(raft))
}

async fn block_api() -> StatusCode {
    let Some(hooks) = HOOKS.get() else {
        return StatusCode::NOT_FOUND;
    };
    let workers = hooks.api.metrics().num_workers();
    let started = Arc::new(Barrier::new(workers + 1));
    for _ in 0..workers {
        let started = started.clone();
        let block = hooks.block;
        hooks.api.spawn(async move {
            started.wait();
            std::thread::sleep(block);
        });
    }
    // 작업자가 모두 하나씩 받아 막힐 때까지 기다린다(막힌 작업자는 다음 작업을 받지 못한다).
    let _ = tokio::task::spawn_blocking(move || started.wait()).await;
    StatusCode::NO_CONTENT
}

async fn raft() -> Result<axum::Json<Value>, StatusCode> {
    let cluster = HOOKS
        .get()
        .and_then(|h| h.cluster.lock().unwrap().upgrade())
        .ok_or(StatusCode::NOT_FOUND)?;
    let leases = cluster.leases();
    let now = Instant::now();
    let ages: BTreeMap<Uuid, u128> = leases
        .renewed
        .iter()
        .map(|(id, at)| (id.0, now.saturating_duration_since(*at).as_millis()))
        .collect();
    Ok(axum::Json(json!({
        "node": cluster.id().0,
        "leader": cluster.members().leader.map(|id| id.0),
        "term": leases.term,
        "lease_age_ms": ages,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Runtimes;

    /// 답하면 `api`의 작업자가 모두 막혀 새 작업을 돌리지 못하고, 다른 실행기는 그대로 돈다.
    #[test]
    fn block_api_occupies_every_api_worker() {
        let rt = Runtimes::new(3, 2).unwrap();
        let h = rt.handles();
        install(h.api.clone(), Duration::from_secs(2));
        rt.core.block_on(async {
            assert_eq!(block_api().await, StatusCode::NO_CONTENT);
            let api = h.api.spawn(async {});
            tokio::time::sleep(Duration::from_secs(1)).await;
            assert!(!api.is_finished());
            h.work.spawn(async {}).await.unwrap();
            api.await.unwrap();
        });
    }
}
```

`crates/opld/tests/starve.rs`를 만든다:

```rust
//! 굶김 시험(설계 §3.4): opld 셋(`[ha] fencing = "process"`)으로 만든 클러스터에서 리더의 `api` 실행기의
//! 작업자를 모두 15초 동안 동기 sleep으로 막는다(시험 빌드의 제어 소켓 `POST /v1/test/block-api`). 그동안
//! 리더의 API는 답하지 않지만 `core`의 리더는 그대로이고 임기도 같으며, 세 노드의 HA 리스 나이가 3초를 넘지
//! 않고, 다른 노드로 한 쓰기가 커밋된다. 리더의 systemd 쓰다듬기(`WATCHDOG=1`)는 막힌 동안 멈췄다가 풀리면
//! 돌아온다. 막는 시간은 실행기 확인(10초 간격, 2초 기한)이 적어도 한 번 막힌 동안에 들도록 15초로 둔다.
//! `cargo test -p opld --features starve-it --test starve`
#![cfg(feature = "starve-it")]

mod common;

use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use common::{Daemon, eventually};
use serde_json::Value;

const BLOCK: Duration = Duration::from_secs(15);

/// 리스 갱신 간격 1초(`fence_after_secs`의 10분의 1)
const CONFIG: &str =
    "[ha]\nfencing = \"process\"\nfence_after_secs = 10\n[test]\nblock_api_ms = 15000\n";

/// 제어 소켓에 요청한다.
async fn control(d: &Daemon, post: bool, path: &str) -> reqwest::Response {
    let http = reqwest::Client::builder()
        .unix_socket(d.path("opld.sock"))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("http://opld{path}");
    let req = if post { http.post(url) } else { http.get(url) };
    req.send().await.unwrap()
}

/// 이 노드가 아는 Raft 리더·임기와 리스 나이(`GET /v1/test/raft`)
async fn raft(d: &Daemon) -> Value {
    control(d, false, "/v1/test/raft")
        .await
        .json()
        .await
        .unwrap()
}

/// 받은 데이터그램들(`within` 동안 기다린다)
fn received(socket: &UnixDatagram, within: Duration) -> Vec<String> {
    socket.set_read_timeout(Some(within)).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    while let Ok(n) = socket.recv(&mut buf) {
        got.push(String::from_utf8_lossy(&buf[..n]).into_owned());
        socket.set_nonblocking(true).unwrap();
    }
    socket.set_nonblocking(false).unwrap();
    got
}

#[tokio::test]
async fn a_blocked_api_runtime_does_not_starve_raft_or_ha_leases() {
    let mut nodes = vec![
        Daemon::new("node1", CONFIG),
        Daemon::new("node2", CONFIG),
        Daemon::new("node3", CONFIG),
    ];
    let notify = nodes[0].path("notify");
    let socket = UnixDatagram::bind(&notify).unwrap();
    nodes[0].notify_socket = Some(notify);
    for d in &mut nodes {
        d.start().await;
    }
    nodes[0].init("prod").await;
    nodes[0].login().await;
    let url = nodes[0].join_url.clone().unwrap();
    nodes[1].join(&url).await.unwrap();
    let url = nodes[0].ctl(&["cluster", "join-token"]).await.unwrap();
    nodes[2].join(url.trim()).await.unwrap();
    let (password, fp) = (
        nodes[0].admin_password.clone().unwrap(),
        nodes[0].ca_fingerprint.clone().unwrap(),
    );
    nodes[1].login_with(&password, &fp).await;

    // node1이 리더이고 세 노드의 리스를 받고 있다.
    eventually("node1 leads and holds three leases", || async {
        let r = raft(&nodes[0]).await;
        r["leader"] == r["node"] && r["lease_age_ms"].as_object().is_some_and(|l| l.len() == 3)
    })
    .await;
    let before = raft(&nodes[0]).await;
    assert!(
        received(&socket, Duration::from_secs(12))
            .iter()
            .any(|s| s == "WATCHDOG=1"),
        "no WATCHDOG=1 before blocking"
    );

    let resp = control(&nodes[0], true, "/v1/test/block-api").await;
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    let blocked = Instant::now();

    // 리더의 API는 답하지 않는다(정말 막혔다).
    let http = reqwest::Client::builder()
        .tls_danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let api = http
        .get(format!("{}/api/v1/auth/whoami", nodes[0].server()))
        .send()
        .await;
    assert!(api.is_err(), "the API answered while blocked: {api:?}");

    // 다른 노드의 API로 한 쓰기는 리더(core)를 거쳐 커밋된다.
    let pool = nodes[1].path("pool");
    std::fs::create_dir_all(&pool).unwrap();
    nodes[1]
        .ctl(&["pool", "create", "p", "--path", pool.to_str().unwrap()])
        .await
        .unwrap();

    // 막힌 동안(끝나기 1초 전까지) 리더·임기가 그대로이고 리스 나이가 3초 미만이다.
    // 막기 전에 시작한 실행기 확인(기한 2초)이 보낸 것은 버리고, 그 뒤로는 WATCHDOG=1이 오지 않는다.
    while blocked.elapsed() < BLOCK - Duration::from_secs(1) {
        let r = raft(&nodes[0]).await;
        assert_eq!(
            (&r["leader"], &r["term"]),
            (&before["leader"], &before["term"]),
            "{r}"
        );
        let ages = r["lease_age_ms"].as_object().unwrap();
        assert_eq!(ages.len(), 3, "{r}");
        assert!(ages.values().all(|a| a.as_u64().unwrap() < 3000), "{r}");
        let got = received(&socket, Duration::from_millis(250));
        if blocked.elapsed() > Duration::from_millis(2500) {
            assert!(
                !got.iter().any(|s| s == "WATCHDOG=1"),
                "petted while blocked: {got:?}"
            );
        }
    }

    // 풀리면 API와 쓰다듬기가 돌아온다.
    eventually("the API answers again", || async {
        http.get(format!("{}/api/v1/auth/whoami", nodes[0].server()))
            .send()
            .await
            .is_ok()
    })
    .await;
    assert!(
        received(&socket, Duration::from_secs(12))
            .iter()
            .any(|s| s == "WATCHDOG=1"),
        "no WATCHDOG=1 after unblocking"
    );
    let log = nodes[0].log();
    assert!(
        log.contains("not petting the systemd watchdog") && log.contains("\"api\""),
        "{log}"
    );
    assert_eq!(raft(&nodes[0]).await["term"], before["term"]);
}
```

`docs/superpowers/specs/2026-11-11-availability-scale-design.md`에서

```markdown

계획 45 구현 뒤에 채운다.
```

을 다음으로 바꾼다:

```markdown

§12의 네 단계를 커밋 넷으로 했다. 설계 §3과 다르거나 설계가 정하지 않아 정한 것을 적는다.

**실행기 셋(§3.1).**
- `opld/src/runtime.rs`: `Runtimes::new(api_threads, work_threads)`, 복제할 수 있는 `Handles { core, api, work }`,
  `runtime::on(&Handle, || …)`(그 실행기의 문맥에서 동기로 띄움), `runtime::run_on(&Handle, fut)`(그 실행기에서 돌리고
  기다림). `main`은 `#[tokio::main]` 대신 `runtimes.core.block_on(run(…))`이라 주 루프가 띄우는 작업과 **제어 소켓은
  `core`**다. 다만 주 루프 자신(close·leave·join을 기다리고 부르는 곳)은 `core` 작업자가 아닌 **주 스레드의
  `block_on`**에서 돌므로 `Liveness`의 실행기 확인이 보지 못한다(주 루프가 막혀도 `WATCHDOG=1`은 계속 간다).
- 설정 `[runtime] api_threads`·`work_threads`(기본 CPU 수의 절반, 최소 2, 0은 거부). `core`는 `CORE_THREADS` = 2.
- 배치는 표대로이고 설계가 적지 않은 것은 이렇게 두었다: TLS 받기·핸드셰이크는 `api`, `QemuHypervisor::new`(다시
  붙는 QMP·모니터 작업)·업데이트·VXLAN·방화벽·Node 등록 루프는 `work`, 승격은 `core`.
- **피어 gRPC 서버를 나눔(설계와 다름).** 설계는 피어 서버 전체를 `core`에 두었지만, 그러면 들어오는 라이브
  마이그레이션(받은 VM의 QMP·모니터 작업까지), 콘솔 VNC 중계, UEFI 변수 보내기가 `core`에서 돈다.
  `PeerService::with_work(Handle)`로 이것들과 감사 로그·지표 읽기를 `work`에서 돌리고, Raft·리스·전달·ping만
  `core`에 남겼다. 위트니스는 `work` 핸들을 넘기지 않는다(그런 요청을 받지 않는다). 처리기에서 panic하면 다시
  던지지 않고 오류 로그와 `internal`로 답한다(같은 HTTP/2 연결의 Raft RPC를 함께 끊지 않게).
- **남은 `core` 부담.** 나눈 것은 처리기뿐이다. 노드 간 서버의 연결마다의 TLS·HTTP/2 작업은 `core`에 있어서 콘솔
  VNC 바이트·UEFI 변수 조각을 틀에 넣고 암호화하는 일은 `core` 스레드가 한다. 팔로어가 리더에게 전달하는 큰 API
  쓰기도 control 연결(`core`)로 가므로 `core`에서 암호화된다. 대량 전송용 포트를 따로 두는 것은 다중 링크(계획
  49)에서 다룬다.
- **다른 노드로 가는 연결도 나눔(설계가 정하지 않음).** tonic 채널은 연결 작업(요청 버퍼, HTTP/2 연결)을 처음
  부른 쪽 실행기에 띄우므로, `work`의 Node 등록이나 `api`의 지표 모으기가 먼저 연 연결이면 Raft 하트비트와 리스
  갱신 전달이 그 실행기에 기댄다. `Peers::with_runtimes(control, bulk)`가 주소마다 채널을 둘 두고
  `Endpoint::executor`로 연결 작업을 정한 실행기에 띄운다: Raft(`GrpcNetwork`)·리더 전달(리스 갱신, 쓰기 전달)·ping은
  control(`core`), 콘솔·마이그레이션 보내기·UEFI 변수·감사 로그·지표 읽기는 bulk(`work`). 실행기를 주지 않으면(시험,
  oplctl) 채널 하나를 처음 부른 쪽에서 돌린다. 시험은 따로 만든 실행기가 먼저 연 control 채널이 그 실행기가 끝난
  뒤에도 되는지 본다.
- 배치 시험은 스레드 이름이 아니라 실행기별 남은 작업 수로 본다: 따로 만든 current-thread 실행기에서 단일 노드
  멤버를 띄우면 부른 쪽에 남은 작업이 0이고 세 실행기 모두 작업이 있으며, 닫으면 10초 안에 모두 0이다. 스레드
  이름은 `runtime.rs` 시험이 본다.

**watchdog 스레드와 저장소 스레드(§3.1·§3.2).**
- `HaGuard`: watchdog 장치를 쓸 때만 OS 스레드 `orbivisor-watchdog`을 띄운다. 스레드는 잠금 없이
  `AtomicU64`(마지막으로 성공한 갱신을 보내기 전 시각)와 `AtomicBool`(HA VM 있음)을 읽고 **스스로 시간을 재어**
  `fresh`를 계산한다(실행기가 멈춰 갱신 루프가 돌지 않아도 리스가 끝나면 규칙대로 쓰다듬기를 멈춤). 닫을 때
  스레드에서 장치를 돌려받아 잠금 밖에서 magic close한다.
- opl-cluster `writer.rs` `StoreWriter`: OS 스레드 `orbivisor-store` 하나, `run()`은 결과를 `oneshot`으로 기다리고
  `submit()`은 기다리지 않는다. 스레드는 데이터베이스를 약한 참조로 쥐어 저장소를 놓으면 닫힌다. 오류
  `StoreError::Thread`·`WriterStopped`·`WriterPanicked`. 쓰기가 panic하면 그 쓰기는 `WriterPanicked`이고 오류 로그를
  남긴 뒤 스레드를 멈춘다(메모리의 Raft 상태가 디스크와 어긋났을 수 있으므로 계속 쓰지 않는다. 뒤의 쓰기는
  `WriterStopped`이고 openraft가 그 노드의 Raft를 멈춘다).
- **Raft 멈춤 감시.** openraft가 Raft를 멈춰도(`metrics().running_state`가 `Err`) 아무도 보지 않으면 HA VM이 없는
  노드는 API만 답하는 좀비로 남는다. `Cluster::stopped()`(Raft가 멈추면 이유와 함께 끝나는 future)를
  `Member::start`·`start_witness`가 `core`의 작은 감시 작업으로 기다리다 `"Raft stopped: …"`를 서버가 멈출 때와 같은
  채널로 보낸다. 주 루프가 0이 아닌 코드로 끝나고 `Restart=on-failure`가 다시 띄운다. `close`는 저장소를 닫기 전에
  감시를 멈춰 정상 종료는 알리지 않는다. 시험: 저장소 쓰기를 panic시키면 `stopped`가 저장소 오류로 끝남(opl-cluster,
  `#[cfg(test)]` 훅), 멤버의 저장소를 닫으면 `failed`가 `"Raft stopped: "`로 끝남(opld).
- `orbivisor-watchdog` 스레드는 새 장치를 받으면 한 간격을 기다리지 않고 곧바로 한 번 쓰다듬는다(규칙은 같다).
- `HaGuard::close` 뒤의 `arm()`은 장치를 열지 않고 `ArmError::Closed`다(쓰다듬을 스레드가 없어 연 장치가 그냥 닫히면
  타이머가 돈다). 갱신 루프는 쓰다듬기 스레드가 쓰는 시각을 async 시각보다 먼저 재어 스레드의 마감이 `lease`의
  마감보다 늦지 않다.
- `append`는 **커밋 뒤에** 돌아오고 `LogFlushed` 콜백도 저장소 스레드가 커밋 뒤에 부른다. openraft 0.9.25는
  `append`가 돌아온 직후 그 엔트리를 읽을 수 있어야 하기 때문이다. `save_committed`는 기다리지 않고 큐에 넣는다.
  스냅샷을 만드는 큰 읽기는 `spawn_blocking`이다.
- **"쓰기가 늦으면 Raft 커밋이 늦어질 뿐"은 완전히 맞지 않는다.** openraft 0.9.25의 Raft 루프는 자기 로그 쓰기를
  기다린다. 그래서 한 노드의 디스크가 느리면 그 노드의 Raft 루프가 멈춘다. 저장소 스레드는 fsync가 실행기 스레드를
  막지 않게 할 뿐이다. 느린 노드가 팔로어면 그 노드만 멈칫한다(선거를 시작할 수 있다). **리더**면 리더의 Raft 루프가
  멈춰 리스 갱신의 리더 확인(`confirm_leader`)이 실패하므로 모든 노드의 리스 갱신이 실패하고, 하트비트가 끊긴
  팔로어가 선거로 새 리더를 뽑은 뒤 회복한다. 저장소 지연 시험은 느린 팔로워의 선거를 끄고(그 노드의 Raft 루프는
  멈추므로) 리더의 쓰기와 리스 갱신(`confirm_leader`)이 500 ms 안에 돌고 리더·임기가 그대로인지 본다.

**systemd 알림(§3.3).**
- `opld/src/notify.rs`: `Notifier::from_env()`·`notify(state)`(`$NOTIFY_SOCKET`, `@`는 Linux 추상 소켓, 없거나 비면
  아무것도 안 함, 보내기 실패는 경고), `Liveness::spawn(Handles, Notifier)`. 확인은 OS 스레드 **`orbivisor-notify`**
  (설계는 이름을 정하지 않았다)가 시작 때 한 번, 그 뒤 10초마다 한다. 늦은 실행기 이름을 경고로 남기고, 마지막
  회차의 결과는 `Liveness::slow()`로 읽는다(계획 46 헬스 체크가 쓴다). systemd가 `WATCHDOG_USEC`을 주면 간격은 그
  1/3(1초~10초), 기한은 2초와 간격의 절반 중 짧은 것이다.
- **`work`가 늦으면 쓰다듬기를 멈추지 않는다(설계와 다름).** `WATCHDOG=1`은 `core`와 `api`가 답한 회차에 보내고,
  `work`만 늦으면 경고만 남긴다. systemd가 데몬을 끝내면 새 프로세스가 HA watchdog 장치의 제한 시간 안에 다시
  arm해야 하는데, VM·작업이 바쁜 것만으로 그 위험을 지지 않는다. 유닛은 `WatchdogSignal=SIGKILL`이다(기본
  SIGABRT의 코어 덤프가 재시작을 늦추지 않게, 진단은 느린 회차의 경고 로그로).
- **여유 계산.** HA VM이 도는 노드에서 `api`가 막혀 systemd가 데몬을 끝내는 경우, 리스 갱신(`core`)은 돌고 있으므로
  `orbivisor-watchdog`이 죽기 직전까지 장치를 쓰다듬는다. 그 뒤 장치 제한 30초 안에 `RestartSec` 5초, 시작, 첫 리스
  갱신과 arm이 끝나야 한다(남는 시간 약 25초 − 시작·리스 갱신 시간). 죽은 프로세스가 닫은 장치의 타이머는 그대로
  돌고, 새 프로세스가 장치를 다시 열면 Linux watchdog 드라이버가 타이머를 다시 시작한다(열 때 `start`, 이미 돌면
  `ping`). 그래서 마감은 마지막 쓰다듬기에서 30초이고, 그 안에 차례로 `RestartSec`, 프로세스 시작, `Cluster::open`,
  `QemuHypervisor::new`(도는 VM마다 차례로 QMP에 다시 붙음, VM 수에 비례하고 붙지 못하는 VM은 하나에
  `ATTACH_TIMEOUT` 5초까지), `HaGuard::spawn`의 첫 리스 갱신(리더가 있어야 한다), reconciler 작업자의 `attach_ha`
  (`arm`)가 끝나야 한다. `core`가 막힌 경우는 리스 갱신도 멈추므로
  `fence_after` 뒤 쓰다듬기가 멈추고, systemd는 마지막 `WATCHDOG=1`에서 30초 뒤(막힌 때에서 최대 30초 + 확인 간격
  10초) 데몬을 끝낸다. 이때 장치 제한(30초)에서 `WatchdogSec`의 30초를 빼면 여유가 없으므로 노드 재시작은 리스를
  잃은 노드의 펜싱으로 본다. 그래서 `work`만 늦을 때는 끝내지 않고, 끝낼 때는 SIGKILL로 바로 끝낸다.
- **`READY=1`을 보내는 때(§3.3 본문의 "API가 듣기 시작하면"을 바로잡음).** 클러스터에 속하지 않은 노드는 API가 없으므로
  제어 소켓을 열었을 때, 멤버는 Task 실행기가 이전 프로세스의 고아를 정리하기 직전과 `Member::start`가 돌아온 뒤(API가
  들음, init·join·다시 시작마다 다시 보내도 해가 없다), 위트니스는 노드 간 서버가 떴을 때다. 고아 정리(`sweep_orphans`)는
  정족수가 없으면 지우기·상태 쓰기마다 10초까지 기다리므로, 클러스터 전체를 다시 켤 때 `Type=notify`의 시작 제한(90초)을
  넘겨 systemd가 다시 띄울 수 있다. 정리를 Task 실행기의 뒤 작업으로 옮기지 않은 것은 업로드 파일과 가져오기 디렉터리
  정리가 revision으로 거르지 않고 "API를 띄우기 전"에 기대기 때문이다(API가 막 받는 업로드를 고아로 지울 수 있다). `STOPPING=1`은 보내지 않는다. `NOTIFY_SOCKET`은 환경에서 지우지
  않는다(`NotifyAccess=main`이라 QEMU 같은 자식이 보내도 systemd가 무시한다).
- 유닛 `Type=notify`·`NotifyAccess=main`·`WatchdogSec=30s`, `TimeoutStartSec`은 기본 그대로. `build-deb-test.sh`가
  유닛 값을 확인한다.
- `runtime::run_on`은 작업이 취소되면(실행기가 끝날 때) `JoinError::into_panic`에서 panic한다. 대상 실행기가 먼저
  끝날 수 있는 곳에서는 쓰지 않는다(문서 주석). 실행기 확인은 `run_on`을 쓰지 않고 채널로 기다리므로 문제가 없다.

**굶김 시험(§3.4).**
- 시험 전용 기능 `starve-it`(opld). 켜면 설정 `[test] block_api_ms`와 제어 소켓(`core`)의 숨은 경로 둘이 생긴다:
  `POST /v1/test/block-api`(`api` 실행기의 작업자 수만큼 작업을 보내 모두 `block_api_ms` 동안 동기 sleep, 모두 막힌
  뒤 답함)와 `GET /v1/test/raft`(이 노드가 아는 리더·임기와, 리더면 노드별 리스 나이). 배포 빌드는 `[test]`를
  모르는 키로 거부하고 경로도 없다. 설계는 막기를 설정만으로 적었지만, 클러스터를 다 만든 뒤 막아야 하므로
  제어 소켓 요청으로 시작한다.
- **리스 나이는 `cluster status`에 없다(설계와 다름).** 리스 표는 리더의 메모리에만 있고 API로 내보내지 않는다.
  API를 바꾸지 않고 시험 경로로 읽는다(사용자에게 보이는 리스 나이는 계획 46의 헬스 체크·계획 47의 리스 기반
  노드 감시에서 정한다).
- 시험 `opld/tests/starve.rs`(CI `check` 작업의 Linux·macOS에서 돈다): opld 셋(`fencing = "process"`,
  `fence_after_secs = 10`이라 갱신 간격 1초), 리더 node1의 `api`를 **15초**(설계는 10초) 막는다. 10초 간격·2초
  기한의 실행기 확인이 적어도 한 번은 막힌 동안에 들게 하려는 것이다. 그동안 리더의 API는 답하지 않고, 리더·
  임기가 그대로이며, 세 노드의 리스 나이가 3초 미만이고, node2의 API로 한 쓰기가 커밋되고, `WATCHDOG=1`이 오지
  않는다. 풀리면 API와 `WATCHDOG=1`이 돌아오고 로그에 `api`가 늦었다는 경고가 있다.

**배포 뒤 확인(테스트 VM, UTM).** 실제 systemd로 띄우는 시험은 CI에 없다(§3.4). 패키지를 설치한 Debian 13
VM에서 손으로 확인하고 결과를 적는다.

| 확인 | 결과 |
|---|---|
| `systemctl start opld` 뒤 `active (running)`(미초기화 노드와 멤버 둘 다), `systemctl show opld -p WatchdogUSec`이 30s | 배포 뒤 확인 |
| `kill -STOP`으로 orbivisord를 세우면 약 30초 뒤 SIGKILL로 끝나고 5초 뒤 다시 뜸, 도는 VM은 그대로 | 배포 뒤 확인 |
| 위 재시작(SIGKILL부터 리스 갱신까지)이 HA watchdog 제한(30초)보다 짧아 HA VM이 있는 노드가 재부팅되지 않음 | 배포 뒤 확인 |
| 정족수가 없을 때(다른 노드를 모두 끈 채) `systemctl restart opld`가 `active (running)`이 되기까지의 시간(90초보다 짧음) | 배포 뒤 확인 |
| VM 수에 따른 다시 arm 시간(VM 수십 개를 띄운 노드에서 SIGKILL부터 `watchdog armed` 로그까지, HA watchdog 제한 30초보다 짧음) | 배포 뒤 확인 |
| `orbivisor-core`·`-api`·`-work`·`-store`·`-notify`(HA VM이 있으면 `-watchdog`) 스레드(`ps -T -p $(pidof orbivisord)`, 이름은 15자로 잘려 `orbivisor-watch`·`orbivisor-notif`) | 배포 뒤 확인 |
```

`packaging/deb/opld.toml.example`에서

```
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
```

을 다음으로 바꾼다:

```
# refresh_secs = 15         # 노드별 풀 상태를 확인하는 간격(초, NFS 마운트 포함)
# [runtime]
# api_threads = 4           # API 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
# work_threads = 4          # VM·작업 실행기의 작업자 수(기본: CPU 수의 절반, 최소 2)
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1714개, 실패 0

Run: `cargo test -p opld --features starve-it --test starve --test notify`
Expected: PASS

Run: `bash packaging/tests/build-deb-test.sh`
Expected: ok

Run: `bash packaging/tests/name-test.sh`
Expected: ok

Run: `git diff --check`
Expected: 출력 없음

- [ ] **Step 3: 커밋**

```bash
git add .github README.md crates docs packaging
git commit -m "test(opld): 굶김 시험과 제어부 가용성 문서"
```

---

## 완료 기준 (계획 45)

- `orbivisord`의 스레드 이름에 `orbivisor-core`·`orbivisor-api`·`orbivisor-work`·`orbivisor-store`·`orbivisor-notify`(HA 장치가 있으면 `orbivisor-watchdog`)가 보인다.
- api 실행기를 막아도 HA 리스 나이가 3초 아래로 유지되고 Raft 리더·임기가 그대로이며, 그동안 `WATCHDOG=1`이 멈춘다(`starve-it`).
- 유닛이 `Type=notify`·`WatchdogSec=30s`·`WatchdogSignal=SIGKILL`이고, 데몬이 시작을 마치면 `READY=1`을 보낸다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace`, `starve-it`·`ha-it`·`migration-it`, 패키징 시험이 통과한다. Linux CI가 통과한다.

## 알고 받아들인 제한

- 느린 디스크의 노드는 그 노드의 Raft가 멈칫하고, 리더라면 모든 노드의 리스 갱신이 늦어진다(openraft 0.9.25).
- 피어 서버의 연결별 h2·TLS와 리더로 전달되는 큰 쓰기의 암호화는 core에서 돈다.
- 주 루프(close·leave·join)는 실행기 확인이 보지 못한다.
- 실제 systemd에서의 재시작 시간·여유는 설계 §11 "배포 뒤 확인" 표로 테스트 VM에서 잰다.
