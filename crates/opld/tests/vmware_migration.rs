//! 계획 72: 실제 opld(qemu-img·qemu-nbd 쓰기 대상)로 VMware 이전을 본다. 시험 안에 가짜 vSphere(opl-api `fake-vsphere` —
//! 스냅샷 사슬·CBT·게스트 쓰기의 의미를 흉내)를 띄우고 orbivisorctl로만 조작한다.
//! - 따뜻한 이전: 원본 `web01`(켜짐, 4 MiB 디스크 둘)을 전체 복사한 뒤 회차 사이에 게스트 쓰기를 섞고(델타 회차가 바뀐 구역만
//!   옮김), 전환하면 원본이 꺼지고 NIC가 "켤 때 연결"에서 빠지고 설명이 붙으며 대상 볼륨 바이트가 원본 마지막 바이트와 같다.
//!   revert는 원본을 다시 켜고 NIC·설명을 되돌린다(대상 VM은 꺼진 채 남음).
//! - 차가운 이전: 꺼진 `app01`을 통째로 옮기고 전환·마무리한 뒤, 그 VM을 OVA로 내보내 내려받아(ustar 머리) 계획 29 OVA
//!   가져오기로 다시 들이면 디스크 바이트가 같다.
//!
//! 모든 시험 끝에 원본 디스크 바이트(시작 + 게스트 쓰기)와 사용자 스냅샷이 그대로다. 만든 VM은 켜지 않는다(`--no-start` —
//! 게스트 OS가 없고 aarch64 macOS 노드는 x86 게스트를 돌리지 못한다).
//! `cargo test -p opld --features qemu-it,fake-vsphere --test vmware_migration`
//! QEMU(`qemu-it`와 같다)와 qemu-img·qemu-nbd가 필요하다.
#![cfg(all(feature = "qemu-it", feature = "fake-vsphere"))]

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use opl_api::vmware::fake::{BLOCK, FakeConfig, FakeVmConfig, FakeVsphere};
use opl_types::vmware::{SNAPSHOT_PREFIX, VmwarePower};
use serde_json::Value;

/// 원본 VM의 기대 내용(시작 바이트 + 게스트 쓰기)과 사용자 스냅샷
struct Source {
    vm: &'static str,
    disks: Vec<Vec<u8>>,
    user_snapshots: Vec<(String, String)>,
}

impl Source {
    fn new(fake: &FakeVsphere, vm: &'static str, disks: usize) -> Self {
        Source {
            vm,
            disks: (0..disks).map(|i| fake.disk_bytes(vm, i)).collect(),
            user_snapshots: user_snapshots(fake, vm),
        }
    }

    /// 게스트 쓰기(원본과 기대 내용 둘 다)
    fn write(&mut self, fake: &FakeVsphere, disk: usize, offset: u64, data: &[u8]) {
        fake.write(self.vm, disk, offset, data);
        let at = offset as usize;
        self.disks[disk][at..at + data.len()].copy_from_slice(data);
    }

    /// 원본이 우리 손을 타지 않았다: 디스크 바이트 = 시작 + 게스트 쓰기, 사용자 스냅샷이 모두 남음
    fn check(&self, fake: &FakeVsphere) {
        for (i, want) in self.disks.iter().enumerate() {
            assert!(
                fake.disk_bytes(self.vm, i) == *want,
                "source disk {i} of {} was changed",
                self.vm
            );
        }
        let now = user_snapshots(fake, self.vm);
        for s in &self.user_snapshots {
            assert!(now.contains(s), "user snapshot {s:?} was removed");
        }
    }
}

fn user_snapshots(fake: &FakeVsphere, vm: &str) -> Vec<(String, String)> {
    fake.snapshots(vm)
        .into_iter()
        .filter(|(_, name)| !name.starts_with(SNAPSHOT_PREFIX))
        .collect()
}

/// 기본 세계(`web01` 켜짐·디스크 둘, `db01`)에 꺼진 Linux VM `app01`(2 MiB 디스크 하나)을 더한다.
fn world() -> FakeConfig {
    let mut c = FakeConfig::default();
    let mut app = FakeVmConfig::linux("app01");
    app.folder = Some("Prod".into());
    app.disks = vec![2 << 20];
    app.power = VmwarePower::PoweredOff;
    c.vms.push(app);
    c
}

/// 클러스터·로그인·dir 풀 `local`, 가짜 vSphere와 원본 연결 `vc1`. 스케줄러 틱은 0.5초, 델타 주기는 3초.
async fn setup(name: &str) -> (Daemon, FakeVsphere) {
    let mut d = Daemon::new(
        "node1",
        "[test]\nvmware_tick_ms = 500\nvmware_sync_secs = 3\n",
    );
    d.start().await;
    d.init(name).await;
    d.login().await;
    let dir = d.path("local");
    std::fs::create_dir(&dir).unwrap();
    d.ctl(&["pool", "create", "local", "--path", dir.to_str().unwrap()])
        .await
        .unwrap();
    d.wait_pool_ready("local").await;
    let fake = FakeVsphere::start(world()).await;
    let spec = fake.source_spec();
    let password = d.path("vmware-password");
    std::fs::write(&password, spec.password.expose()).unwrap();
    let port = spec.port.to_string();
    d.ctl(&[
        "vmware",
        "source",
        "add",
        "vc1",
        "--host",
        &spec.host,
        "--port",
        &port,
        "--user",
        &spec.user,
        "--password-file",
        password.to_str().unwrap(),
        "--fingerprint",
        &spec.fingerprint,
    ])
    .await
    .unwrap();
    (d, fake)
}

/// 계획의 항목 하나(`vmware migration get -o json`)
async fn item(d: &Daemon, migration: &str) -> Value {
    let view = d.ctl_json(&["vmware", "migration", "get", migration]).await;
    view["vms"][0].clone()
}

/// 항목이 `ok`를 만족할 때까지 기다린다(`failed`면 바로 멈춤).
async fn wait_item(d: &Daemon, migration: &str, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let last = item(d, migration).await;
        assert_ne!(last["status"]["state"], "failed", "{what}: {last}");
        if ok(&last) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {last}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn qemu_img(args: &[&str]) {
    let out = Command::new("qemu-img").args(args).output().unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// 볼륨 `name`(풀 `local`)의 내용을 raw로 빼서 읽는다.
async fn volume_bytes(d: &Daemon, name: &str) -> Vec<u8> {
    let volume = d.ctl_json(&["volume", "get", name]).await;
    assert_eq!(volume["status"]["phase"], "ready", "{volume}");
    let format = volume["spec"]["format"].as_str().unwrap();
    let file = d
        .path("local")
        .join(format!("{}.{format}", volume["id"].as_str().unwrap()));
    raw_of(d, &file)
}

fn raw_of(d: &Daemon, file: &Path) -> Vec<u8> {
    let raw = d.path("out.raw");
    qemu_img(&[
        "convert",
        "-O",
        "raw",
        file.to_str().unwrap(),
        raw.to_str().unwrap(),
    ]);
    let bytes = std::fs::read(&raw).unwrap();
    std::fs::remove_file(&raw).unwrap();
    bytes
}

/// 바이트를 견준다(다르면 첫 차이 자리만 알린다 — 4 MiB를 찍지 않게).
fn assert_same(what: &str, got: &[u8], want: &[u8]) {
    assert_eq!(got.len(), want.len(), "{what}: size");
    if let Some(at) = got.iter().zip(want).position(|(a, b)| a != b) {
        panic!("{what}: first difference at byte {at}");
    }
}

/// 원본이 켜진 채 전체 복사 → 게스트 쓰기 → 델타 회차 → 다시 쓰기 → 전환. 대상 볼륨 = 원본 마지막 바이트, 원본은 꺼지고
/// 보호(NIC "켤 때 연결" 끔·설명)되며 내린 시간이 적힌다. revert는 원본을 다시 켜고 보호를 되돌린다.
#[tokio::test(flavor = "multi_thread")]
async fn warm_migration_syncs_deltas_cuts_over_and_reverts() {
    let (mut d, fake) = Box::pin(setup("vmware-warm")).await;
    let mut source = Source::new(&fake, "web01", 2);
    let note = fake.annotation("web01");
    assert_eq!(fake.nics_start_connected("web01"), vec![true, true]);

    d.ctl(&[
        "vmware",
        "migration",
        "create",
        "w1",
        "--source",
        "vc1",
        "--warm",
        "--vm",
        "DC1/vm/Prod/web01",
        "--net",
        "VM Network=user",
        "--net",
        "dvpg-web=user",
        "--pool",
        "local",
        "--no-start",
    ])
    .await
    .unwrap();
    let out = d
        .ctl(&["vmware", "migration", "preflight", "w1"])
        .await
        .unwrap();
    assert!(out.contains("web01"), "{out}");
    d.ctl(&["vmware", "migration", "start", "w1"])
        .await
        .unwrap();

    // 회차 1: 전체 복사(스케줄러가 Task를 만든다)
    let first = wait_item(&d, "w1", "full copy", |i| {
        i["status"]["state"] == "syncing" && i["status"]["rounds"].as_u64() >= Some(1)
    })
    .await;
    assert_eq!(first["status"]["cbt_enabled_by_us"], true, "{first}");
    assert!(fake.cbt_enabled("web01"));

    // 회차 사이 게스트 쓰기 → 다음 델타 회차가 그것만 옮긴다.
    let rounds = first["status"]["rounds"].as_u64().unwrap();
    source.write(&fake, 0, BLOCK * 3, &[0x5a; 4096]);
    source.write(&fake, 1, (2 << 20) + 100, b"warm delta between rounds");
    let after = wait_item(&d, "w1", "a delta round with the writes", |i| {
        i["status"]["rounds"].as_u64() > Some(rounds)
            && i["status"]["history"].as_array().unwrap().iter().any(|h| {
                let e = h["event"].as_str().unwrap_or_default();
                e.contains("(delta)") && !e.contains(": 0 B in")
            })
    })
    .await;
    // 델타는 바뀐 구역(CBT 64 KiB 조각)뿐이다 — 디스크 전체가 아니다.
    for disk in after["status"]["disks"].as_array().unwrap() {
        assert!(
            disk["last_delta_bytes"].as_u64().unwrap_or(0)
                < disk["capacity_bytes"].as_u64().unwrap(),
            "{after}"
        );
    }
    // 원본에는 우리 스냅샷 하나만(합치기로 앞 것을 지움)
    let ours: Vec<_> = fake
        .snapshots("web01")
        .into_iter()
        .filter(|(_, n)| n.starts_with(SNAPSHOT_PREFIX))
        .collect();
    assert_eq!(ours.len(), 1, "{ours:?}");

    // 전환 직전 쓰기(켜진 채 델타·마지막 델타가 옮긴다)
    source.write(&fake, 0, 0, b"last write before cutover");
    let done = d
        .ctl_json(&["vmware", "vm", "cutover", "w1-web01", "--wait"])
        .await;
    assert_eq!(done["status"]["state"], "done", "{done}");
    assert!(done["status"]["downtime_s"].is_u64(), "{done}");
    assert!(done["status"]["source_stopped_at"].is_u64(), "{done}");
    assert!(
        done["status"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["event"].as_str().unwrap_or_default().contains("downtime")),
        "{done}"
    );
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);
    assert_eq!(fake.nics_start_connected("web01"), vec![false, false]);
    assert!(
        fake.annotation("web01").contains("Migrated to Orbivisor"),
        "{}",
        fake.annotation("web01")
    );

    // 대상 VM은 꺼진 채 만들어졌고 볼륨 바이트 = 원본 마지막 바이트
    let vm = d.ctl_json(&["vm", "get", "web01"]).await;
    assert_eq!(vm["spec"]["power"], "stopped", "{vm}");
    assert_eq!(done["status"]["target_vm"], vm["id"], "{done}");
    let disks = vm["spec"]["disks"].as_array().unwrap();
    assert_eq!(disks.len(), 2, "{vm}");
    for (i, disk) in disks.iter().enumerate() {
        let volume = disk["volume"].as_str().unwrap();
        let got = volume_bytes(&d, volume).await;
        assert_same(&format!("disk {i}"), &got, &source.disks[i]);
    }
    source.check(&fake);

    // revert: 원본을 다시 켜고 NIC·설명을 되돌린다. 대상 VM은 남는다(꺼진 채).
    let reverted = d
        .ctl_json(&[
            "vmware",
            "vm",
            "revert",
            "w1-web01",
            "--confirm",
            "web01",
            "--wait",
        ])
        .await;
    assert_eq!(reverted["status"]["state"], "reverted", "{reverted}");
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOn);
    assert_eq!(fake.nics_start_connected("web01"), vec![true, true]);
    assert_eq!(fake.annotation("web01"), note);
    assert!(
        !fake
            .snapshots("web01")
            .iter()
            .any(|(_, n)| n.starts_with(SNAPSHOT_PREFIX)),
        "our snapshots left"
    );
    assert!(!fake.cbt_enabled("web01"), "CBT we enabled was left on");
    assert_eq!(
        d.ctl_json(&["vm", "get", "web01"]).await["spec"]["power"],
        "stopped"
    );
    source.check(&fake);
    d.stop();
}

/// 꺼진 VM의 차가운 이전 → 전환·마무리 → 그 VM을 OVA로 내보내 내려받고(ustar) OVA 가져오기로 다시 들이면 바이트가 같다.
#[tokio::test(flavor = "multi_thread")]
async fn cold_migration_then_export_round_trips_through_ova() {
    let (mut d, fake) = Box::pin(setup("vmware-cold")).await;
    let source = Source::new(&fake, "app01", 1);

    d.ctl(&[
        "vmware",
        "migration",
        "create",
        "c1",
        "--source",
        "vc1",
        "--cold",
        "--vm",
        "DC1/vm/Prod/app01",
        "--net",
        "VM Network=user",
        "--pool",
        "local",
        "--no-start",
    ])
    .await
    .unwrap();
    d.ctl(&["vmware", "migration", "start", "c1"])
        .await
        .unwrap();
    wait_item(&d, "c1", "cold copy", |i| i["status"]["state"] == "ready").await;
    let done = d
        .ctl_json(&["vmware", "vm", "cutover", "c1-app01", "--wait"])
        .await;
    assert_eq!(done["status"]["state"], "done", "{done}");
    // 차가운 이전은 원본을 켜지 않았고 끄지도 않았다(이미 꺼짐).
    assert_eq!(fake.power("app01"), VmwarePower::PoweredOff);
    assert_ne!(
        done["status"]["source_changes"]["powered_off_by_us"], true,
        "{done}"
    );
    let vm = d.ctl_json(&["vm", "get", "app01"]).await;
    let volume = vm["spec"]["disks"][0]["volume"]
        .as_str()
        .unwrap()
        .to_string();
    assert_same(
        "cold disk",
        &volume_bytes(&d, &volume).await,
        &source.disks[0],
    );

    let finalized = d
        .ctl_json(&["vmware", "vm", "finalize", "c1-app01", "--wait"])
        .await;
    assert_eq!(finalized["status"]["state"], "finalized", "{finalized}");
    source.check(&fake);

    // 내보내기 → 내려받은 OVA(ustar) → 계획 29 OVA 가져오기 → 같은 바이트
    let ova = d.path("app01.ova");
    d.ctl(&[
        "vm",
        "export",
        "app01",
        "--pool",
        "local",
        "--name",
        "app01-ova",
        "--download",
        ova.to_str().unwrap(),
    ])
    .await
    .unwrap();
    let data = std::fs::read(&ova).unwrap();
    assert!(data.len() > 1024, "{}", data.len());
    assert_eq!(&data[257..262], b"ustar", "not a ustar archive");
    let first = String::from_utf8_lossy(&data[..100]);
    assert!(first.trim_end_matches('\0').ends_with(".ovf"), "{first}");
    let export = d.ctl_json(&["export", "get", "app01-ova"]).await;
    assert_eq!(export["status"]["phase"], "ready", "{export}");

    d.ctl(&["import", "ova", ova.to_str().unwrap(), "--name", "back"])
        .await
        .unwrap();
    d.ctl(&[
        "import", "run", "back", "--name", "app01b", "--pool", "local", "--net", "user",
    ])
    .await
    .unwrap();
    let back = d.ctl_json(&["vm", "get", "app01b"]).await;
    let volume = back["spec"]["disks"][0]["volume"].as_str().unwrap();
    assert_same(
        "re-imported disk",
        &volume_bytes(&d, volume).await,
        &source.disks[0],
    );
    source.check(&fake);
    d.stop();
}
