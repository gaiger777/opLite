//! vSphere 클라이언트를 가짜 vSphere(기능 `fake-vsphere`)에 대고 본다(계획 72 설계 §14): 로그인·세션 만료 다시
//! 로그인·지문이 틀리면 비밀번호를 보내지 않음, 인벤토리(쪽 나눔·나무·DVS 이름·호스트 인증서), CBT 바뀐 구역과
//! "S(n+1) 만들기 → S(n) 지우기" 회차를 게스트 쓰기와 섞어 돌린 뒤 바이트가 같은지, 호스트 티켓 범위 읽기와
//! vCenter `/folder`로 내려가기, 끊긴 범위 읽기 다시 하기, CBT 초기화, 스냅샷 지우기 실패, 전원·`ShutdownGuest`,
//! 무료 ESXi, Task 기다리기, 데이터스토어 파일 정보, 결함 → API 코드. 시스템 도구는 쓰지 않는다.
//! `cargo test -p opl-api --features fake-vsphere --test vsphere`

use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_api::vmware::VsphereError;
use opl_api::vmware::cbt::{self, ALL_AREAS, CHUNK};
use opl_api::vmware::client::Session;
use opl_api::vmware::fake::{BLOCK, FakeConfig, FakeVsphere};
use opl_api::vmware::http::probe;
use opl_api::vmware::inventory::{self, InvVm, Inventory};
use opl_api::vmware::reader::{DiskReader, FolderReader, HostRoute};
use opl_api::vmware::soap::MoRef;
use opl_types::vm::Firmware;
use opl_types::vmware::{Secret, VmwareApiKind, VmwarePower};

fn vm_ref(v: &InvVm) -> MoRef {
    MoRef::new("VirtualMachine", &v.moref)
}

fn route(inv: &Inventory, vm: &InvVm) -> HostRoute {
    let host = inv.host(vm.host.as_deref().unwrap()).unwrap();
    HostRoute {
        name: host.name.clone(),
        fingerprint: host.certificate_sha256.clone().unwrap(),
    }
}

#[tokio::test]
async fn login_relogin_after_expiry_and_logout_on_drop() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let s = fake.login().await;
    assert_eq!(s.kind(), VmwareApiKind::Vcenter);
    let status = s.source_status(42);
    assert_eq!(status.version.as_deref(), Some("8.0.2"));
    assert_eq!(status.checked_at, Some(42));
    assert!(status.instance_uuid.is_some());

    fake.expire_sessions();
    let web = MoRef::new("VirtualMachine", &fake.moref("web01"));
    assert_eq!(s.power_state(&web).await.unwrap(), VmwarePower::PoweredOn);
    let logins = fake
        .requests()
        .iter()
        .filter(|r| r.method == "Login")
        .count();
    assert_eq!(logins, 2, "logged in again once after the session expired");
    // 비밀번호는 Login 본문에만
    assert!(
        fake.requests()
            .iter()
            .all(|r| r.has_password == (r.method == "Login"))
    );

    drop(s);
    for _ in 0..100 {
        if fake.requests().iter().any(|r| r.method == "Logout") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(fake.requests().iter().any(|r| r.method == "Logout"));

    let mut spec = fake.source_spec();
    spec.password = Secret::new("wrong");
    let err = Session::login(&spec).await.unwrap_err();
    assert!(
        matches!(&err, VsphereError::Auth { kind, .. } if kind == "InvalidLogin"),
        "{err}"
    );
    let api = err.api_error();
    assert_eq!((api.status.as_u16(), api.code), (400, "vmware_auth"));
}

#[tokio::test]
async fn a_wrong_fingerprint_never_sends_the_password() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let cert = probe("127.0.0.1", fake.addr.port()).await.unwrap();
    assert_eq!(cert.sha256, fake.fingerprint);
    assert_eq!(cert.sha1, fake.thumbprint_sha1);
    let mut spec = fake.source_spec();
    spec.fingerprint = fake.hosts[0].1.clone();
    let err = Session::login(&spec).await.unwrap_err();
    assert!(matches!(err, VsphereError::Fingerprint { .. }), "{err}");
    let api = err.api_error();
    assert_eq!(
        (api.status.as_u16(), api.code),
        (409, "fingerprint_mismatch")
    );
    assert!(fake.requests().is_empty(), "{:?}", fake.requests());

    // 닫힌 포트 → 502 vmware_unreachable
    let mut spec = fake.source_spec();
    spec.port = 1;
    let err = Session::login(&spec).await.unwrap_err();
    assert_eq!(err.api_error().code, "vmware_unreachable");
}

#[tokio::test]
async fn inventory_is_paged_and_resolved_into_a_tree() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let s = fake.login().await;
    s.set_page_size(3);
    let inv = inventory::inventory(&s).await.unwrap();
    assert!(
        fake.requests()
            .iter()
            .filter(|r| r.method == "ContinueRetrievePropertiesEx")
            .count()
            >= 3
    );
    assert!(fake.requests().iter().any(|r| r.method == "DestroyView"));
    assert_eq!(inv.datacenters.len(), 1);
    assert_eq!(inv.datacenters[0].path, "DC1");
    assert_eq!(inv.hosts.len(), 2);
    assert_eq!(inv.vms.len(), 2);
    let web = inv.vm_by_path("DC1/vm/Prod/web01").unwrap();
    assert_eq!(web.folder.as_deref(), Some("DC1/vm/Prod"));
    assert_eq!(web.cluster.as_deref(), Some("Cluster1"));
    assert_eq!(web.disks.len(), 2);
    assert_eq!(web.disks[0].file, "[ds1] web01/web01.vmdk");
    assert_eq!(web.disks[1].file, "[ds1] web01/web01_1.vmdk");
    let nets: Vec<&str> = web.nics.iter().map(|n| n.network.as_str()).collect();
    assert_eq!(nets, vec!["VM Network", "dvpg-web"]);
    assert!(web.tools_running());
    assert_eq!(web.guest_ips.len(), 2);
    assert!(inv.networks.iter().all(|n| !n.name.contains("Uplinks")));
    assert!(inv.datastore_named("vsanDatastore").unwrap().needs_vddk());
    // vCenter가 아는 호스트 인증서가 그 호스트 서버의 지문
    for (i, (_, fp)) in fake.hosts.iter().enumerate() {
        let h = inv.host(&format!("host-{}", 20 + i)).unwrap();
        assert_eq!(h.certificate_sha256.as_deref(), Some(fp.as_str()));
    }
    let db = inv.vm_by_path("DC1/vm/db01").unwrap();
    assert_eq!(db.firmware, Firmware::Uefi);
    assert_eq!(db.power, Some(VmwarePower::PoweredOff));
    assert_eq!(db.nics[0].adapter, "e1000");
    let src = db.to_source();
    assert_eq!(src.vm.guest_os, "windows2019srv_64Guest");
    assert_eq!(src.vm.disks[0].capacity_bytes, 2 << 20);
    assert_eq!(src.power, Some(VmwarePower::PoweredOff));
}

/// 디스크 하나의 구역을 읽어 `target`에 쓴다. 읽은 바이트 수를 돌려준다.
async fn copy_areas(reader: &mut FolderReader, areas: &[(u64, u64)], target: &mut [u8]) -> u64 {
    let mut n = 0;
    for (offset, len) in cbt::chunks(areas, CHUNK) {
        let data = reader.read(offset, len).await.unwrap();
        target[offset as usize..offset as usize + data.len()].copy_from_slice(&data);
        n += data.len() as u64;
    }
    n
}

#[tokio::test]
async fn warm_rounds_with_guest_writes_end_byte_identical() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    fake.set_change_window(BLOCK * 8);
    let s = Arc::new(fake.login().await);
    let inv = inventory::inventory(&s).await.unwrap();
    let web = inv.vm_by_path("DC1/vm/Prod/web01").unwrap().clone();
    let r = vm_ref(&web);
    let dc = web.datacenter.clone().unwrap();
    let mut targets: Vec<Vec<u8>> = web
        .disks
        .iter()
        .map(|d| vec![0; d.capacity as usize])
        .collect();

    // 첫 회차: CBT 켜기 → S1 → "*" 할당 구역 → 기반에서 읽기
    s.set_change_tracking(&r, true).await.unwrap();
    let s1 = s
        .create_snapshot(&r, "orbivisor-migration-t-1", "test")
        .await
        .unwrap();
    // 복사 중 게스트 쓰기(델타로 간다 — 기반은 S1 시점 그대로)
    fake.write("web01", 0, 100, b"written during the full copy");
    let disks1 = s.snapshot_disks(&s1).await.unwrap();
    let mut change_ids = Vec::new();
    for (i, d) in web.disks.iter().enumerate() {
        let areas = cbt::changed_areas(&s, &r, &s1, d.key, d.capacity, ALL_AREAS)
            .await
            .unwrap();
        // 블록 셋 중 둘만 할당되어 있다
        assert!(cbt::total(&areas) < d.capacity, "thin areas are skipped");
        let mut reader = FolderReader::open(s.clone(), &dc, &d.file, Some(route(&inv, &web)))
            .await
            .unwrap();
        assert_eq!(reader.size(), d.capacity);
        copy_areas(&mut reader, &areas, &mut targets[i]).await;
        assert!(reader.direct() && reader.warnings().is_empty());
        change_ids.push(disks1[i].change_id.clone().unwrap());
    }
    fake.write("web01", 1, 3 * BLOCK + 5, &[0xEE; 3000]);

    // 델타 회차 둘: S(n+1) 만들기 → S(n) 지우기 → 바뀐 구역만
    let mut current = s1;
    for round in 2..=3 {
        let next = s
            .create_snapshot(&r, &format!("orbivisor-migration-t-{round}"), "test")
            .await
            .unwrap();
        // 지우기 전 게스트 쓰기 — 다음 스냅샷 뒤라 이번 회차에 들지 않는다
        fake.write("web01", 0, 2 * BLOCK, &[round as u8; 10]);
        s.remove_snapshot(&current).await.unwrap();
        assert_eq!(fake.snapshots("web01").len(), 1);
        let disks = s.snapshot_disks(&next).await.unwrap();
        fake.clear_requests();
        let mut read = 0;
        for (i, d) in web.disks.iter().enumerate() {
            let areas = cbt::changed_areas(&s, &r, &next, d.key, d.capacity, &change_ids[i])
                .await
                .unwrap();
            let mut reader = FolderReader::open(s.clone(), &dc, &d.file, Some(route(&inv, &web)))
                .await
                .unwrap();
            read += copy_areas(&mut reader, &areas, &mut targets[i]).await;
            change_ids[i] = disks[i].change_id.clone().unwrap();
        }
        // 바뀐 블록만 읽었다(블록 하나~둘)
        assert!(
            read > 0 && read <= 2 * BLOCK,
            "round {round} read {read} bytes"
        );
        let gets: Vec<_> = fake
            .requests()
            .into_iter()
            .filter(|q| q.method == "GET")
            .collect();
        assert!(
            gets.iter().all(|g| g.server > 0 && g.ticket.is_some()),
            "{gets:?}"
        );
        current = next;
    }

    // 마지막: 끄고 → S4 → S3 지우기 → 델타 → 지금 디스크와 같음
    fake.write("web01", 1, 0, b"last write before shutdown");
    s.shutdown_guest(&r).await.unwrap();
    assert_eq!(s.power_state(&r).await.unwrap(), VmwarePower::PoweredOff);
    let last = s
        .create_snapshot(&r, "orbivisor-migration-t-4", "test")
        .await
        .unwrap();
    s.remove_snapshot(&current).await.unwrap();
    for (i, d) in web.disks.iter().enumerate() {
        let areas = cbt::changed_areas(&s, &r, &last, d.key, d.capacity, &change_ids[i])
            .await
            .unwrap();
        let mut reader = FolderReader::open(s.clone(), &dc, &d.file, Some(route(&inv, &web)))
            .await
            .unwrap();
        copy_areas(&mut reader, &areas, &mut targets[i]).await;
    }
    for (i, t) in targets.iter().enumerate() {
        assert!(*t == fake.disk_bytes("web01", i), "disk {i} differs");
    }
    // 마무리: 우리 스냅샷을 지우면 기반 = 마지막 상태, 원본 디스크 그대로
    s.remove_snapshot(&last).await.unwrap();
    assert!(fake.snapshots("web01").is_empty());
    assert!(fake.base_bytes("web01", 0) == fake.disk_bytes("web01", 0));
    s.set_change_tracking(&r, false).await.unwrap();
    assert!(!fake.cbt_enabled("web01"));
}

#[tokio::test]
async fn refused_tickets_fall_back_to_vcenter_and_cut_reads_are_retried() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let s = Arc::new(fake.login().await);
    let inv = inventory::inventory(&s).await.unwrap();
    let web = inv.vm_by_path("DC1/vm/Prod/web01").unwrap().clone();
    let dc = web.datacenter.clone().unwrap();
    let base = fake.base_bytes("web01", 0);

    let mut reader =
        FolderReader::open(s.clone(), &dc, &web.disks[0].file, Some(route(&inv, &web)))
            .await
            .unwrap();
    fake.reject_tickets(true);
    fake.clear_requests();
    let data = reader.read(BLOCK, 4096).await.unwrap();
    assert_eq!(data, base[BLOCK as usize..BLOCK as usize + 4096]);
    assert!(!reader.direct());
    let w = reader.warnings();
    assert!(
        w.len() == 1 && w[0].contains("refused the service ticket"),
        "{w:?}"
    );
    let gets: Vec<_> = fake
        .requests()
        .into_iter()
        .filter(|q| q.method == "GET")
        .collect();
    assert_eq!(gets.len(), 2);
    assert!(gets[0].server > 0 && gets[1].server == 0 && gets[1].session);
    fake.reject_tickets(false);

    // 끊긴 범위 읽기는 다시 한다
    fake.cut_reads(1);
    fake.clear_requests();
    let data = reader.read(0, 128 << 10).await.unwrap();
    assert_eq!(data, base[..128 << 10]);
    let gets = fake
        .requests()
        .into_iter()
        .filter(|q| q.method == "GET")
        .count();
    assert_eq!(gets, 2);
    // 디스크 끝을 넘으면 잘라 준다
    let size = reader.size();
    assert_eq!(reader.read(size - 10, 100).await.unwrap().len(), 10);
    assert!(reader.read(size, 100).await.unwrap().is_empty());
}

#[tokio::test]
async fn cbt_reset_and_snapshot_removal_failure() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let s = fake.login().await;
    let inv = inventory::inventory(&s).await.unwrap();
    let web = inv.vm_by_path("DC1/vm/Prod/web01").unwrap().clone();
    let r = vm_ref(&web);
    let d = &web.disks[0];

    // CBT를 켜지 않으면 바뀐 구역을 물을 수 없다
    let s0 = s
        .create_snapshot(&r, "orbivisor-migration-t-0", "")
        .await
        .unwrap();
    let err = cbt::changed_areas(&s, &r, &s0, d.key, d.capacity, ALL_AREAS)
        .await
        .unwrap_err();
    assert!(matches!(err, VsphereError::FileFault { .. }), "{err}");
    s.remove_snapshot(&s0).await.unwrap();

    s.set_change_tracking(&r, true).await.unwrap();
    let s1 = s
        .create_snapshot(&r, "orbivisor-migration-t-1", "")
        .await
        .unwrap();
    let cid = s.snapshot_disks(&s1).await.unwrap()[0]
        .change_id
        .clone()
        .unwrap();
    assert!(cid.starts_with("52 ") && cid.ends_with("/1"), "{cid}");
    fake.write("web01", 0, 0, b"x");
    fake.reset_cbt("web01");
    let s2 = s
        .create_snapshot(&r, "orbivisor-migration-t-2", "")
        .await
        .unwrap();
    let err = cbt::changed_areas(&s, &r, &s2, d.key, d.capacity, &cid)
        .await
        .unwrap_err();
    assert!(matches!(err, VsphereError::FileFault { .. }), "{err}");
    // "*"는 된다(그 디스크만 다시 전체 복사)
    assert!(
        !cbt::changed_areas(&s, &r, &s2, d.key, d.capacity, ALL_AREAS)
            .await
            .unwrap()
            .is_empty()
    );

    // 스냅샷 지우기 실패 → 둘이 남고, 다음 시도가 정리한다
    fake.fail_next("RemoveSnapshot_Task", "GenericVmConfigFault");
    let err = s.remove_snapshot(&s1).await.unwrap_err();
    assert!(
        matches!(&err, VsphereError::FileFault { kind, .. } if kind == "GenericVmConfigFault"),
        "{err}"
    );
    assert_eq!(fake.snapshots("web01").len(), 2);
    s.remove_snapshot(&s1).await.unwrap();
    assert_eq!(fake.snapshots("web01").len(), 1);

    // 사용자 스냅샷은 인벤토리에 보인다(우리 것은 moref로만 안다)
    fake.add_user_snapshot("web01", "nightly-backup");
    let inv = inventory::inventory(&s).await.unwrap();
    let names: Vec<&str> = inv
        .vm(&web.moref)
        .unwrap()
        .snapshots
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["orbivisor-migration-t-2", "nightly-backup"]);
    // 없는 스냅샷
    let err = s
        .remove_snapshot(&MoRef::new("VirtualMachineSnapshot", "snapshot-9"))
        .await
        .unwrap_err();
    assert!(matches!(err, VsphereError::NotFound { .. }), "{err}");
}

#[tokio::test]
async fn power_shutdown_and_reconfigure() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let s = fake.login().await;
    let web = MoRef::new("VirtualMachine", &fake.moref("web01"));
    let db = MoRef::new("VirtualMachine", &fake.moref("db01"));

    let err = s.power_off(&db).await.unwrap_err();
    assert!(
        matches!(&err, VsphereError::InvalidState { kind, .. } if kind == "InvalidPowerState"),
        "{err}"
    );
    s.power_on(&db).await.unwrap();
    assert_eq!(fake.power("db01"), VmwarePower::PoweredOn);

    fake.ignore_shutdown(true);
    s.shutdown_guest(&web).await.unwrap();
    assert_eq!(s.power_state(&web).await.unwrap(), VmwarePower::PoweredOn);
    fake.ignore_shutdown(false);
    fake.set_tools("web01", false);
    let err = s.shutdown_guest(&web).await.unwrap_err();
    assert!(
        matches!(&err, VsphereError::InvalidState { kind, .. } if kind == "ToolsUnavailable"),
        "{err}"
    );
    fake.set_tools("web01", true);
    s.shutdown_guest(&web).await.unwrap();
    assert_eq!(fake.power("web01"), VmwarePower::PoweredOff);

    // 원본 보호: NIC "켤 때 연결" 끄기·메모, 되돌리기
    let keys = s
        .set_nics_start_connected(&web, &[4000, 4001, 4999], false)
        .await
        .unwrap();
    assert_eq!(keys, vec![4000, 4001]);
    assert_eq!(fake.nics_start_connected("web01"), vec![false, false]);
    s.set_nics_start_connected(&web, &[4001], true)
        .await
        .unwrap();
    assert_eq!(fake.nics_start_connected("web01"), vec![false, true]);
    s.set_annotation(&web, "Migrated to Orbivisor <web01> & powered off")
        .await
        .unwrap();
    assert_eq!(
        fake.annotation("web01"),
        "Migrated to Orbivisor <web01> & powered off"
    );
    let inv = inventory::inventory(&s).await.unwrap();
    assert_eq!(
        inv.vm(&web.value).unwrap().annotation.as_deref(),
        Some("Migrated to Orbivisor <web01> & powered off")
    );

    // 주입한 결함(Task가 아닌 메서드) → 그 형식
    fake.fail_next("ShutdownGuest", "NoPermission");
    s.power_on(&web).await.unwrap();
    let err = s.shutdown_guest(&web).await.unwrap_err();
    assert!(matches!(err, VsphereError::NoPermission { .. }), "{err}");
    assert_eq!(err.api_error().code, "vmware_permission");
}

#[tokio::test]
async fn tasks_are_polled_until_done_and_datastores_report_flat_files() {
    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let s = fake.login().await;
    let inv = inventory::inventory(&s).await.unwrap();
    let db = inv.vm_by_path("DC1/vm/db01").unwrap().clone();
    fake.set_task_polls(3);
    let snap = s.create_snapshot(&vm_ref(&db), "t", "").await.unwrap();
    let polls = fake
        .requests()
        .iter()
        .filter(|r| r.method == "RetrievePropertiesEx")
        .count();
    assert!(polls >= 4, "{polls}");
    s.remove_snapshot(&snap).await.unwrap();
    fake.set_task_polls(0);

    let ds = inv.datastore_named("ds2").unwrap();
    let browser = MoRef::new("HostDatastoreBrowser", ds.browser.as_deref().unwrap());
    let files = s
        .search_datastore(&browser, "[ds2] db01", "*-flat.vmdk")
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "db01-flat.vmdk");
    assert_eq!(files[0].size, Some(2 << 20));
    // 꺼진 VM이 그새 켜져 쓰면(스냅샷 없이 기반에) 수정 시각이 바뀐다 — 차가운 이전의 "그새 켜졌나"
    fake.write("db01", 0, 0, b"booted");
    let again = s
        .search_datastore(&browser, "[ds2] db01", "*-flat.vmdk")
        .await
        .unwrap();
    assert_ne!(files[0].modification, again[0].modification);
    let all = s
        .search_datastore(&browser, "[ds2] db01", "*")
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn standalone_esxi_reads_with_the_session_and_free_esxi_refuses_writes() {
    let fake = FakeVsphere::start(FakeConfig {
        esxi: true,
        ..FakeConfig::default()
    })
    .await;
    let s = Arc::new(fake.login().await);
    assert_eq!(s.kind(), VmwareApiKind::Esxi);
    assert_eq!(s.source_status(1).instance_uuid, None);
    let inv = inventory::inventory(&s).await.unwrap();
    assert_eq!(inv.datacenters[0].name, "ha-datacenter");
    let web = inv.vms.iter().find(|v| v.name == "web01").unwrap().clone();
    assert_eq!(web.cluster, None);
    let mut reader = FolderReader::open(s.clone(), "ha-datacenter", &web.disks[0].file, None)
        .await
        .unwrap();
    let base = fake.base_bytes("web01", 0);
    assert_eq!(reader.read(0, 65536).await.unwrap(), base[..65536]);
    assert!(
        fake.requests()
            .iter()
            .filter(|r| r.method == "GET")
            .all(|r| r.server == 0 && r.session)
    );

    fake.set_restricted(true);
    let err = s.create_snapshot(&vm_ref(&web), "x", "").await.unwrap_err();
    assert!(matches!(err, VsphereError::Restricted(_)), "{err}");
    assert_eq!(err.api_error().code, "vmware_restricted");
    // 읽기는 된다
    assert!(inventory::inventory(&s).await.is_ok());
}

#[tokio::test]
async fn large_inventory_of_two_thousand_vms() {
    let fake = FakeVsphere::start(FakeConfig::many(2000)).await;
    let s = fake.login().await;
    let started = Instant::now();
    let inv = inventory::inventory(&s).await.unwrap();
    let took = started.elapsed();
    assert_eq!(inv.vms.len(), 2000);
    let pages = fake
        .requests()
        .iter()
        .filter(|r| r.method == "ContinueRetrievePropertiesEx")
        .count();
    // 객체 2 000여 개 ÷ 500 = 쪽 다섯
    assert_eq!(pages, 4);
    println!("fake vSphere inventory of 2000 VMs: {took:?}, {pages} continuation pages");
    assert!(inv.vms.iter().all(|v| {
        v.folder
            .as_deref()
            .is_some_and(|f| f.starts_with("DC1/vm/f"))
    }));
}
