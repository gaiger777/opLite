//! CD-ROM 드라이브와 cloud-init 씨앗 드라이브를 단 실제 QEMU(OS 없음)를 띄워 매체·부팅 순서·넣기·빼기를 보는 통합
//! 테스트(계획 57). `cargo test -p orbivisor-qemu --features qemu-it --test cdrom`
//! 이 호스트의 기본 머신(macOS·aarch64는 `virt` + UEFI, x86_64는 q35 + UEFI·BIOS)과, x86_64가 아닌 호스트에
//! `qemu-system-x86_64`가 있으면 q35(TCG)의 BIOS·UEFI도 본다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use orbivisor_qemu::iso9660::{build, cidata_files};
use orbivisor_qemu::{Accel, HostProfile, QmpClient, RuntimePaths, build_cmdline, change_medium};
use orbivisor_types::cloud_init::CloudInitSeed;
use orbivisor_types::runtime::{
    DiskKind, ImageFormat, NicBackend, RuntimeCdrom, RuntimeDisk, RuntimeMedium, RuntimeNic,
    VmRuntimeSpec,
};
use orbivisor_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware, NicModel, effective_boot};
use serde_json::{Value, json};
use uuid::Uuid;

/// 펌웨어가 CD를 읽기까지 기다리는 상한. svc01(aarch64 TCG)과 q35 TCG의 OVMF가 느리다.
const READ_WAIT: Duration = Duration::from_secs(120);

fn host() -> HostProfile {
    let share = orbivisor_types::legacy::env("QEMU_SHARE")
        .unwrap_or_else(|| "/opt/homebrew/share/qemu".into());
    HostProfile::native(Path::new(&share))
}

/// 볼 머신들: (이름, 호스트 프로필, 펌웨어)
fn machines() -> Vec<(&'static str, HostProfile, Firmware)> {
    let native = host();
    let mut out = vec![("native-uefi", native.clone(), Firmware::Uefi)];
    if native.arch == Arch::X86_64 {
        out.push(("native-bios", native, Firmware::Bios));
        return out;
    }
    let found = std::process::Command::new("qemu-system-x86_64")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if found {
        // Homebrew의 x86_64 펌웨어(변수 원본은 i386과 함께 쓴다)
        let share = native.uefi_code.parent().unwrap_or(Path::new("/"));
        let q35 = HostProfile {
            arch: Arch::X86_64,
            accel: Accel::Tcg,
            qemu_binary: PathBuf::from("qemu-system-x86_64"),
            uefi_code: share.join("edk2-x86_64-code.fd"),
            uefi_vars_template: share.join("edk2-i386-vars.fd"),
            secure_boot: None,
        };
        out.push(("q35-tcg-bios", q35.clone(), Firmware::Bios));
        if q35.uefi_code.exists() && q35.uefi_vars_template.exists() {
            out.push(("q35-tcg-uefi", q35, Firmware::Uefi));
        }
    } else {
        eprintln!("qemu-system-x86_64 not found; checking only the native machine");
    }
    out
}

fn seed() -> CloudInitSeed {
    CloudInitSeed {
        instance_id: "iid-0123456789abcdef".into(),
        meta_data: r#"{"instance-id": "iid-0123456789abcdef", "local-hostname": "it-cdrom"}"#
            .into(),
        user_data: "#cloud-config\n{}".into(),
        vendor_data: None,
        network_config: None,
    }
}

/// 빈 qcow2 디스크 하나(부팅 1), CD 둘(첫째에 `iso`, 둘째는 빈 드라이브 — 부팅 순서는 기본 규칙), 씨앗 드라이브
fn spec(
    host: &HostProfile,
    firmware: Firmware,
    id: Uuid,
    disk: PathBuf,
    iso: PathBuf,
) -> VmRuntimeSpec {
    let (disks, cds) = effective_boot(&[None], &[None, None]);
    VmRuntimeSpec {
        id,
        name: "it-cdrom".into(),
        cpu: CpuSpec {
            emulator: None,
            policy: Default::default(),
            thread_policy: None,
            flags: Vec::new(),
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
            max_vcpus: None,
        },
        memory_mib: 256,
        firmware,
        machine: host.arch.machine(),
        disks: vec![RuntimeDisk {
            path: disk,
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: disks[0],
        }],
        nics: vec![RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::User,
        }],
        guest_agent: false,
        hotplug: None,
        cdroms: vec![
            RuntimeCdrom {
                medium: Some(RuntimeMedium { path: iso }),
                boot_order: cds[0],
            },
            RuntimeCdrom {
                medium: None,
                boot_order: cds[1],
            },
        ],
        cloud_init: Some(seed()),
        tpm: None,
        secure_boot: false,
        freeze_fs: false,
        firmware_epoch: 0,
        keep_tpm: false,
        cpu_resolved: None,
        numa: None,
        host_placement: None,
        pinning: None,
    }
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{path:?} did not appear within 10s");
}

/// 실행 중인 QEMU 하나. 테스트가 끝나면(`kill_on_drop`) 그 프로세스만 멈춘다.
struct Vm {
    tmp: tempfile::TempDir,
    paths: RuntimePaths,
    child: tokio::process::Child,
    qmp: QmpClient,
}

impl Vm {
    async fn quit(mut self) {
        self.qmp.quit().await.unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(10), self.child.wait())
            .await
            .expect("qemu did not exit within 10s")
            .unwrap();
        assert!(exit.success(), "qemu exited with {exit}");
    }

    fn iso(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }
}

/// 시험 ISO 둘(`a.iso`·`b.iso`)과 씨앗 `cidata.iso`를 만들고 [`spec`]으로 QEMU를 `-S`로 띄운다.
async fn launch(host: &HostProfile, firmware: Firmware) -> Vm {
    // macOS의 기본 TMPDIR은 경로가 길어 소켓 경로 한도(103바이트)를 넘기므로 /tmp를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let id = Uuid::now_v7();
    let paths = RuntimePaths::new(tmp.path(), id).unwrap();
    std::fs::create_dir_all(&paths.dir).unwrap();
    let disk = tmp.path().join("root.qcow2");
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&disk)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());
    for (name, label) in [("a.iso", "ISO_A"), ("b.iso", "ISO_B")] {
        let image = build(label, &[("readme.txt", label.as_bytes())]).unwrap();
        std::fs::write(tmp.path().join(name), image).unwrap();
    }
    std::fs::write(
        &paths.cidata,
        build("cidata", &cidata_files(&seed())).unwrap(),
    )
    .unwrap();
    let spec = spec(host, firmware, id, disk, tmp.path().join("a.iso"));
    if firmware == Firmware::Uefi {
        std::fs::copy(&host.uefi_vars_template, &paths.uefi_vars).unwrap();
    }
    let args = build_cmdline(&spec, host, &paths).unwrap();
    let child = tokio::process::Command::new(&host.qemu_binary)
        .args(&args)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for(&paths.qmp_socket).await;
    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    Vm {
        tmp,
        paths,
        child,
        qmp,
    }
}

/// 드라이브 `dev`의 매체 파일(빈 드라이브는 None)
async fn medium(qmp: &QmpClient, dev: &str) -> Option<PathBuf> {
    let media = qmp.query_block_media().await.unwrap();
    media
        .iter()
        .find(|(id, _)| id == dev)
        .unwrap_or_else(|| panic!("no {dev} in {media:?}"))
        .1
        .clone()
}

/// 드라이브 `dev`의 `query-block` 항목
async fn block(qmp: &QmpClient, dev: &str) -> Value {
    let ret = qmp.execute("query-block", None).await.unwrap();
    ret.as_array()
        .unwrap()
        .iter()
        .find(|b| {
            let q = b["qdev"].as_str().unwrap_or("");
            q == dev || q == format!("/machine/peripheral/{dev}")
        })
        .unwrap_or_else(|| panic!("no {dev} in {ret}"))
        .clone()
}

/// 드라이브 `dev`가 읽은 바이트(`query-blockstats`)
async fn read_bytes(qmp: &QmpClient, dev: &str) -> u64 {
    let stats = qmp.query_blockstats().await.unwrap();
    stats
        .iter()
        .find(|s| {
            s.qdev
                .as_deref()
                .is_some_and(|q| q == dev || q.strip_prefix("/machine/peripheral/") == Some(dev))
        })
        .unwrap_or_else(|| panic!("no {dev} in {stats:?}"))
        .rd_bytes
}

async fn bootindex(qmp: &QmpClient, dev: &str) -> Value {
    qmp.qom_get(&format!("/machine/peripheral/{dev}"), "bootindex")
        .await
        .unwrap()
}

async fn block_node_names(qmp: &QmpClient) -> Vec<String> {
    qmp.query_block_nodes()
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.node_name)
        .collect()
}

/// 매체가 읽기 전용으로 들어가 있고, 부팅 순서는 디스크 1 다음 CD 2·3이며 씨앗에는 없다. 실행하면 펌웨어가 빈
/// 디스크 다음 CD를 읽는다. UEFI(OVMF·ArmVirtQemu)는 `bootindex`가 있는 장치만 보므로 씨앗을 읽지 않는다. SeaBIOS는
/// 순서에 든 장치 다음에 나머지도 시도하므로 씨앗의 첫 섹터를 읽고 넘어간다(부팅 기록이 없는 ISO).
#[tokio::test]
async fn firmware_reads_the_cdrom_after_the_disk() {
    for (label, host, firmware) in machines() {
        let vm = launch(&host, firmware).await;
        let qmp = &vm.qmp;
        assert_eq!(
            medium(qmp, "cd0-dev").await,
            Some(vm.iso("a.iso")),
            "{label}"
        );
        assert_eq!(medium(qmp, "cd1-dev").await, None, "{label}");
        assert_eq!(
            medium(qmp, "cidata-dev").await,
            Some(vm.paths.cidata.clone()),
            "{label}"
        );
        for dev in ["cd0-dev", "cd1-dev", "cidata-dev"] {
            let b = block(qmp, dev).await;
            assert_eq!(b["removable"], true, "{label} {dev}: {b}");
            if dev != "cd1-dev" {
                assert_eq!(b["inserted"]["ro"], true, "{label} {dev}: {b}");
            } else {
                assert!(b.get("inserted").is_none(), "{label} {dev}: {b}");
            }
        }
        assert_eq!(bootindex(qmp, "disk0-dev").await, json!(1), "{label}");
        assert_eq!(bootindex(qmp, "cd0-dev").await, json!(2), "{label}");
        assert_eq!(bootindex(qmp, "cd1-dev").await, json!(3), "{label}");
        assert_eq!(bootindex(qmp, "cidata-dev").await, json!(-1), "{label}");

        let start = Instant::now();
        qmp.cont().await.unwrap();
        let read = loop {
            let n = read_bytes(qmp, "cd0-dev").await;
            if n > 0 {
                break n;
            }
            assert!(
                start.elapsed() < READ_WAIT,
                "{label}: the firmware did not read the CD within {READ_WAIT:?}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        // 펌웨어가 CD를 다 본 뒤의 씨앗
        tokio::time::sleep(Duration::from_secs(5)).await;
        let seed = read_bytes(qmp, "cidata-dev").await;
        eprintln!(
            "{label}: cd0 read {read} bytes after {:?}; seed read {seed} bytes",
            start.elapsed()
        );
        if firmware == Firmware::Uefi {
            assert_eq!(seed, 0, "{label}: the firmware read the seed drive");
        }
        vm.quit().await;
    }
}

/// 드라이브 `dev`의 트레이 이벤트(`tray-open` 값)를 `want`만큼 기다린다.
async fn tray_moves(
    events: &mut tokio::sync::broadcast::Receiver<orbivisor_qemu::QmpEvent>,
    dev: &str,
    want: usize,
) -> Vec<bool> {
    let mut seen = Vec::new();
    let wait = async {
        while seen.len() < want {
            let ev = events.recv().await.unwrap();
            if ev.event == "DEVICE_TRAY_MOVED" && ev.data["id"] == dev {
                seen.push(ev.data["tray-open"].as_bool().unwrap());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .unwrap_or_else(|_| panic!("tray events of {dev}: {seen:?}"));
    seen
}

/// 실행 중 넣기·바꾸기·빼기와 실패 뒤 정리. 트레이는 열렸다 닫히고(`DEVICE_TRAY_MOVED`), 뺀 매체의 노드는 남지 않는다.
#[tokio::test]
async fn media_change_while_running() {
    let (label, host, firmware) = machines().remove(0);
    let vm = launch(&host, firmware).await;
    let qmp = &vm.qmp;
    qmp.cont().await.unwrap();
    let (a, b) = (vm.iso("a.iso"), vm.iso("b.iso"));
    let mut events = qmp.subscribe();

    // 빈 드라이브에 넣기
    change_medium(qmp, 1, Some(&b)).await.unwrap();
    assert_eq!(tray_moves(&mut events, "cd1-dev", 2).await, [true, false]);
    assert_eq!(medium(qmp, "cd1-dev").await, Some(b.clone()), "{label}");
    assert_eq!(block(qmp, "cd1-dev").await["inserted"]["ro"], true);

    // 바꾸기: 같은 파일을 두 드라이브가 함께 읽기 전용으로 연다.
    change_medium(qmp, 0, Some(&b)).await.unwrap();
    assert_eq!(tray_moves(&mut events, "cd0-dev", 2).await, [true, false]);
    assert_eq!(medium(qmp, "cd0-dev").await, Some(b.clone()), "{label}");

    // 빼기: 빈 드라이브가 되고 노드가 닫힌다.
    change_medium(qmp, 0, None).await.unwrap();
    assert_eq!(tray_moves(&mut events, "cd0-dev", 2).await, [true, false]);
    assert_eq!(medium(qmp, "cd0-dev").await, None, "{label}");
    let nodes = block_node_names(qmp).await;
    assert!(!nodes.iter().any(|n| n.starts_with("cd0")), "{nodes:?}");
    // 빈 드라이브를 다시 빼도 된다.
    change_medium(qmp, 0, None).await.unwrap();

    // 없는 파일: 오류이고 드라이브는 빈 채, 노드도 남지 않는다. 그 뒤 넣기는 된다.
    let missing = vm.tmp.path().join("missing.iso");
    change_medium(qmp, 0, Some(&missing)).await.unwrap_err();
    assert_eq!(medium(qmp, "cd0-dev").await, None, "{label}");
    let nodes = block_node_names(qmp).await;
    assert!(!nodes.iter().any(|n| n.starts_with("cd0")), "{nodes:?}");
    change_medium(qmp, 0, Some(&a)).await.unwrap();
    assert_eq!(medium(qmp, "cd0-dev").await, Some(a.clone()), "{label}");
    // 매체가 든 드라이브에 없는 파일: 새 노드를 먼저 열다 실패하므로 넣은 매체가 그대로이고 트레이도 닫혀 있다.
    change_medium(qmp, 0, Some(&missing)).await.unwrap_err();
    assert_eq!(medium(qmp, "cd0-dev").await, Some(a.clone()), "{label}");
    assert_eq!(block(qmp, "cd0-dev").await["tray_open"], false);
    // 다시 바꾸면 옛 노드는 닫힌다(바꿀 때마다 쓰지 않은 노드 이름).
    change_medium(qmp, 0, Some(&b)).await.unwrap();
    change_medium(qmp, 0, Some(&a)).await.unwrap();
    assert_eq!(medium(qmp, "cd0-dev").await, Some(a), "{label}");
    let nodes = block_node_names(qmp).await;
    let cd0: Vec<_> = nodes.iter().filter(|n| n.starts_with("cd0")).collect();
    assert_eq!(cd0.len(), 2, "{nodes:?}");
    // 씨앗과 다른 드라이브는 그대로다.
    assert_eq!(medium(qmp, "cd1-dev").await, Some(b));
    assert_eq!(
        medium(qmp, "cidata-dev").await,
        Some(vm.paths.cidata.clone())
    );
    vm.quit().await;
}
