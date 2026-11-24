//! govmomi vcsim을 상대로 vSphere 클라이언트의 SOAP 꼴을 본다(계획 72 설계 §14 — GitHub `vmware-sim` 작업). govmomi가
//! 실제 WSDL 타입으로 만든 응답(봉투·네임스페이스·`xsi:type`·결함 모양)을 우리 손 XML이 읽는지, 그리고 같은 요청에
//! 가짜 vSphere가 같은 꼴(요소 이름·`xsi:type`)을 내는지 견준다. 디스크 내용·CBT 의미는 vcsim이 얕게 흉내 내므로
//! 가짜 vSphere 시험(`tests/vsphere.rs`)이 맡는다.
//!
//! vcsim은 미리 띄운다: `vcsim -l 127.0.0.1:8989 -username user -password pass`. 주소는 `OPL_TEST_VCSIM`
//! (`127.0.0.1:8989`), 사용자·비밀번호는 `OPL_TEST_VCSIM_USER`·`OPL_TEST_VCSIM_PASSWORD`(기본 user·pass)다.
//! `OPL_TEST_NBDKIT`(nbdkit 경로)가 있으면 nbdkit `file` 플러그인을 vddk 자리에 두고 [`NbdkitReader`] 배선도 본다.
//! `cargo test -p opl-api --features vcsim-it --test vcsim -- --nocapture`
#![cfg(feature = "vcsim-it")]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::Full;
use hyper::Request;
use hyper::body::Bytes;
use hyper::header::COOKIE;
use opl_api::vmware::VsphereError;
use opl_api::vmware::cbt;
use opl_api::vmware::client::Session;
use opl_api::vmware::fake::{FakeConfig, FakeVsphere};
use opl_api::vmware::http::{probe, split_host_port};
use opl_api::vmware::inventory::{self, VM_PATHS};
use opl_api::vmware::reader::{DiskReader, FolderReader, HostRoute, NbdkitReader, folder_url};
use opl_api::vmware::soap::{MoRef, Node};
use opl_types::vmware::{Secret, VmwareApiKind, VmwarePower};

fn vcsim() -> (String, u16, String, String) {
    let addr = std::env::var("OPL_TEST_VCSIM").unwrap_or_else(|_| "127.0.0.1:8989".into());
    let (host, port) = split_host_port(&addr, 8989);
    let user = std::env::var("OPL_TEST_VCSIM_USER").unwrap_or_else(|_| "user".into());
    let password = std::env::var("OPL_TEST_VCSIM_PASSWORD").unwrap_or_else(|_| "pass".into());
    (host, port, user, password)
}

async fn login() -> Session {
    let (host, port, user, password) = vcsim();
    let cert = probe(&host, port).await.expect("vcsim is running");
    Session::login_to(&host, port, &cert.sha256, &user, Secret::new(password))
        .await
        .unwrap()
        .with_poll(Duration::from_millis(50), Duration::from_millis(200))
}

/// 요소 이름과 `xsi:type`의 나무 꼴(값 없이) — `prefix/이름[형식]`
fn shape(node: &Node, prefix: &str, out: &mut BTreeSet<String>) {
    let here = match node.ty() {
        Some(t) => format!("{prefix}/{}[{t}]", node.name),
        None => format!("{prefix}/{}", node.name),
    };
    out.insert(here.clone());
    for c in &node.children {
        shape(c, &here, out);
    }
}

fn shape_of(node: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    shape(node, "", &mut out);
    out
}

#[tokio::test]
async fn login_status_and_faults() {
    let (host, port, user, _) = vcsim();
    let cert = probe(&host, port).await.unwrap();
    assert_eq!(cert.sha256.len(), 95);
    assert_eq!(cert.sha1.len(), 59);
    let s = login().await;
    assert_eq!(s.kind(), VmwareApiKind::Vcenter);
    let status = s.source_status(1);
    assert!(
        status.version.is_some() && status.instance_uuid.is_some(),
        "{status:?}"
    );
    println!("vcsim about: {:?}", s.about());

    // 틀린 비밀번호 → InvalidLogin
    let err = Session::login_to(&host, port, &cert.sha256, &user, Secret::new("wrong"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, VsphereError::Auth { kind, .. } if kind == "InvalidLogin"),
        "{err}"
    );
    // 틀린 지문 → 비밀번호를 보내기 전에 끊김
    let other = cert.sha256.replace(&cert.sha256[..2], "00");
    let err = Session::login_to(&host, port, &other, &user, Secret::new("x"))
        .await
        .unwrap_err();
    assert!(matches!(err, VsphereError::Fingerprint { .. }), "{err}");
    // 없는 객체 → ManagedObjectNotFound
    let err = s
        .retrieve_one(&MoRef::new("VirtualMachine", "vm-nonexistent"), &["name"])
        .await
        .unwrap_err();
    assert!(matches!(err, VsphereError::NotFound { .. }), "{err}");
    s.logout().await;
}

#[tokio::test]
async fn inventory_resolves_every_vm_to_a_source_vm() {
    let s = login().await;
    // 쪽 나눔을 보도록 쪽을 작게
    s.set_page_size(5);
    let inv = inventory::inventory(&s).await.unwrap();
    println!(
        "vcsim inventory: {} datacenters, {} hosts, {} datastores, {} networks, {} VMs",
        inv.datacenters.len(),
        inv.hosts.len(),
        inv.datastores.len(),
        inv.networks.len(),
        inv.vms.len()
    );
    assert!(!inv.vms.is_empty());
    for vm in &inv.vms {
        let src = vm.to_source();
        assert!(!src.vm.disks.is_empty(), "{}", vm.path);
        assert!(vm.path.contains("/vm/"), "{}", vm.path);
        assert!(vm.instance_uuid.is_some(), "{}", vm.path);
        for d in &vm.disks {
            assert!(d.datastore.is_some() && d.capacity > 0, "{d:?}");
        }
        for n in &vm.nics {
            assert!(!n.network.starts_with("dvs:"), "{n:?}");
        }
    }
    assert!(inv.hosts.iter().all(|h| h.certificate_sha256.is_some()));
}

#[tokio::test]
async fn cbt_snapshots_power_and_changed_areas() {
    let s = login().await;
    let inv = inventory::inventory(&s).await.unwrap();
    let vm = inv
        .vms
        .iter()
        .find(|v| v.name.ends_with("_VM1"))
        .unwrap_or(&inv.vms[0]);
    let r = MoRef::new("VirtualMachine", &vm.moref);

    s.set_change_tracking(&r, true).await.unwrap();
    let o = s
        .retrieve_one(&r, &["config.changeTrackingEnabled"])
        .await
        .unwrap();
    assert_eq!(o.bool("config.changeTrackingEnabled"), Some(true));
    s.set_annotation(&r, "orbivisor: migrating").await.unwrap();
    let nic_keys: Vec<i32> = vm.nics.iter().map(|n| n.key).collect();
    let done = s
        .set_nics_start_connected(&r, &nic_keys, false)
        .await
        .unwrap();
    assert_eq!(done, nic_keys);
    let after = inventory::inventory(&s).await.unwrap();
    let v = after.vm(&vm.moref).unwrap();
    assert_eq!(v.annotation.as_deref(), Some("orbivisor: migrating"));
    println!(
        "vcsim NIC startConnected after edit: {:?}",
        v.nics.iter().map(|n| n.start_connected).collect::<Vec<_>>()
    );

    let snap = s
        .create_snapshot(&r, "orbivisor-migration-test-1", "vcsim")
        .await
        .unwrap();
    let disks = s.snapshot_disks(&snap).await.unwrap();
    assert_eq!(disks.len(), vm.disks.len());
    println!("vcsim snapshot disks: {disks:?}");
    match cbt::changed_areas(
        &s,
        &r,
        &snap,
        disks[0].key,
        disks[0].capacity_bytes,
        cbt::ALL_AREAS,
    )
    .await
    {
        Ok(areas) => println!("vcsim QueryChangedDiskAreas(*): {} areas", areas.len()),
        Err(e) => {
            println!("vcsim QueryChangedDiskAreas: {e} ({:?})", e.fault_kind());
            assert!(matches!(e, VsphereError::NotSupported { .. }), "{e}");
        }
    }
    s.remove_snapshot(&snap).await.unwrap();

    // 전원: 끄기 → 다시 끄기는 InvalidPowerState(Task 결함) → 켜기
    if s.power_state(&r).await.unwrap() == VmwarePower::PoweredOn {
        s.power_off(&r).await.unwrap();
    }
    let err = s.power_off(&r).await.unwrap_err();
    assert!(
        matches!(&err, VsphereError::InvalidState { kind, .. } if kind == "InvalidPowerState"),
        "{err}"
    );
    s.power_on(&r).await.unwrap();
    assert_eq!(s.power_state(&r).await.unwrap(), VmwarePower::PoweredOn);
    match s.shutdown_guest(&r).await {
        Ok(()) => println!("vcsim ShutdownGuest: accepted"),
        Err(e) => println!("vcsim ShutdownGuest: {e}"),
    }
}

#[tokio::test]
async fn tickets_and_folder_range_reads() {
    let s = Arc::new(login().await);
    let inv = inventory::inventory(&s).await.unwrap();
    let vm = &inv.vms[0];
    let disk = &vm.disks[0];
    let dc = vm.datacenter.clone().unwrap();
    // 기반 flat 파일에 알려진 바이트를 올린다(vcsim의 flat은 비어 있다).
    let (ds, file) = opl_api::vmware::client::split_datastore_path(&disk.file).unwrap();
    let flat = file.replace(".vmdk", "-flat.vmdk");
    let data: Vec<u8> = (0..(1u32 << 20)).map(|i| (i % 251) as u8).collect();
    let put = Request::put(folder_url(&dc, ds, &flat))
        .header(COOKIE, s.cookie().unwrap())
        .body(Full::new(Bytes::from(data.clone())))
        .unwrap();
    let r = s.http().send(put, 1 << 20).await.unwrap();
    assert!(r.status.is_success(), "PUT {}", r.status);

    // 세션 쿠키로 vCenter /folder(ESXi와 같은 길)
    let mut reader = FolderReader::open(s.clone(), &dc, &disk.file, None)
        .await
        .unwrap();
    assert_eq!(reader.size(), disk.capacity);
    assert_eq!(reader.read(4096, 8192).await.unwrap(), data[4096..12288]);

    // 호스트 티켓으로 "호스트"(vcsim은 같은 서버)에 — vcsim이 티켓에 준 hostName과 호스트 인증서
    let (host, port, _, _) = vcsim();
    let ticket = s
        .acquire_ticket(&format!(
            "https://{host}:{port}{}",
            folder_url(&dc, ds, &flat)
        ))
        .await
        .unwrap();
    println!("vcsim ticket: {ticket:?}");
    let host_inv = inv.host(vm.host.as_deref().unwrap()).unwrap();
    let pin = host_inv.certificate_sha256.clone().unwrap();
    let server_pin = probe(&host, port).await.unwrap().sha256;
    println!(
        "vcsim host certificate matches the server: {}",
        pin == server_pin
    );
    let route = HostRoute {
        name: format!("{host}:{port}"),
        fingerprint: server_pin,
    };
    let mut direct = FolderReader::open(s.clone(), &dc, &disk.file, Some(route))
        .await
        .unwrap();
    assert_eq!(direct.read(0, 4096).await.unwrap(), data[..4096]);
    assert!(direct.direct(), "{:?}", direct.warnings());
}

#[tokio::test]
async fn fake_vsphere_answers_in_the_same_shape_as_vcsim() {
    let s = login().await;
    let inv = inventory::inventory(&s).await.unwrap();
    let vcsim_vm = s
        .retrieve_one(&MoRef::new("VirtualMachine", &inv.vms[0].moref), &VM_PATHS)
        .await
        .unwrap();

    let fake = FakeVsphere::start(FakeConfig::default()).await;
    let f = fake.login().await;
    let finv = inventory::inventory(&f).await.unwrap();
    let fake_vm = f
        .retrieve_one(&MoRef::new("VirtualMachine", &finv.vms[0].moref), &VM_PATHS)
        .await
        .unwrap();

    // ServiceContent의 about 꼴
    assert!(shape_of_about(s.about()).is_subset(&shape_of_about(f.about())));
    // VM 속성: 가짜가 주는 경로는 vcsim도 준다(설정 안 된 값은 둘 다 뺀다), 값의 xsi:type이 같다
    for (path, val) in &fake_vm.props {
        if let Some(v) = vcsim_vm.props.get(path) {
            assert_eq!(val.ty(), v.ty(), "xsi:type of {path}");
        }
    }
    // 디스크·NIC 장치의 요소 꼴: 가짜 것이 vcsim 것의 부분 집합이 아니면 우리 손 XML이 vcsim과 다르다
    let device = |o: &opl_api::vmware::client::ObjectContent, nic: bool| {
        o.get("config.hardware.device").and_then(|d| {
            d.children
                .iter()
                .find(|c| {
                    if nic {
                        c.child("macAddress").is_some()
                    } else {
                        c.ty() == Some("VirtualDisk")
                    }
                })
                .cloned()
        })
    };
    let mut compared = 0;
    for nic in [false, true] {
        let (Some(mut a), Some(mut b)) = (device(&fake_vm, nic), device(&vcsim_vm, nic)) else {
            continue;
        };
        if nic {
            // NIC 종류와 backing(네트워크 종류마다 다름)은 빼고 본다
            for d in [&mut a, &mut b] {
                d.xsi_type = None;
                d.children.retain(|c| c.name != "backing");
            }
        }
        let (fa, vb) = (shape_of(&a), shape_of(&b));
        let extra: Vec<_> = fa.difference(&vb).collect();
        // 가짜만 주는 것은 CBT changeId(vcsim은 CBT를 흉내 내지 않는다)와 부모 체인
        assert!(
            extra
                .iter()
                .all(|e| e.contains("changeId") || e.contains("/parent")),
            "nic={nic}: fake has {extra:?} that vcsim does not"
        );
        compared += 1;
    }
    assert_eq!(compared, 2, "both a disk and a NIC were compared");
}

fn shape_of_about(a: &opl_api::vmware::client::About) -> BTreeSet<&'static str> {
    let mut s = BTreeSet::new();
    for (k, v) in [
        ("name", &a.name),
        ("version", &a.version),
        ("build", &a.build),
        ("apiType", &a.api_type),
        ("apiVersion", &a.api_version),
    ] {
        if !v.is_empty() {
            s.insert(k);
        }
    }
    if a.instance_uuid.is_some() {
        s.insert("instanceUuid");
    }
    s
}

#[tokio::test]
async fn nbdkit_file_plugin_in_place_of_vddk() {
    let Ok(nbdkit) = std::env::var("OPL_TEST_NBDKIT") else {
        println!("OPL_TEST_NBDKIT is not set; skipping the nbdkit wiring test");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let disk = dir.path().join("disk.raw");
    let data: Vec<u8> = (0..(3u32 << 20)).map(|i| (i % 253) as u8).collect();
    std::fs::write(&disk, &data).unwrap();
    let mut r = NbdkitReader::spawn(
        std::path::Path::new(&nbdkit),
        &dir.path().join("run"),
        vec!["file".into(), format!("file={}", disk.display()).into()],
        Some(&Secret::new("unused")),
    )
    .await
    .unwrap();
    assert_eq!(r.size(), data.len() as u64);
    assert_eq!(
        r.read(1 << 20, 4096).await.unwrap(),
        data[1 << 20..(1 << 20) + 4096]
    );
    // 끝을 넘으면 잘라 준다
    let tail = r.read(data.len() as u64 - 10, 100).await.unwrap();
    assert_eq!(tail.len(), 10);
    drop(r);
    assert_eq!(
        std::fs::read_dir(dir.path().join("run")).unwrap().count(),
        0
    );
}
