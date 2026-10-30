//! 실제 공유 LVM 풀의 저장소 통합 테스트. 관리자가 만든 공유 VG(`OPL_TEST_LVM_SHARED=VG`, lvmlockd의
//! `vgcreate --shared`, 잠금을 시작한 것)에 볼륨 LV를 만들어 0으로 채우고, 배타 활성화하고 끄고,
//! 늘리고(늘린 부분을 0으로 채운다), 가져오고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_LVM_SHARED=oplshared cargo test -p opl-driver --features lvm-shared-it --test lvm_shared_storage`
//! Linux에서 root, lvm2-lockd와 sanlock(또는 dlm), qemu-img, qemu-io가 필요하다.
#![cfg(all(feature = "lvm-shared-it", target_os = "linux"))]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use opl_driver::{
    DirStorage, DiskLocation, DriverError, LvmSharedStorage, SourceFormat, Storage, Storages,
    SystemLvm,
};
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// 이 파일의 시험은 한 VG를 함께 쓰므로 하나씩 돈다. 늘린 부분의 0 채우기 시험은 지운 LV의 익스텐트에
/// 늘린 부분이 놓이는지 보는데, 다른 시험이 함께 돌면 그 자리를 먼저 차지할 수 있다.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `OPL_TEST_LVM_SHARED`의 공유 VG
fn vg() -> String {
    std::env::var("OPL_TEST_LVM_SHARED").expect("OPL_TEST_LVM_SHARED must name a shared VG")
}

fn shared_pool() -> StoragePool {
    Resource::new(
        "shared",
        StoragePoolSpec {
            backend: PoolBackend::LvmShared {
                vg: vg(),
                iscsi: None,
            },
            nodes: vec![],
            shared: true,
        },
    )
}

fn dir_pool(dir: &Path) -> StoragePool {
    Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: dir.to_path_buf(),
            },
            nodes: vec![],
            shared: false,
        },
    )
}

fn storage() -> LvmSharedStorage {
    LvmSharedStorage::new("qemu-img", Arc::new(SystemLvm))
}

fn lv(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 볼륨 LV의 장치 노드
fn device(id: Uuid) -> PathBuf {
    Path::new("/dev").join(vg()).join(lv(id))
}

/// 명령을 실행해 결과를 돌려준다(실패해도).
fn output(program: &str, args: &[&str]) -> std::process::Output {
    Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("LVM_SUPPRESS_FD_WARNINGS", "1")
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"))
}

/// 실패한 확인을 이해하도록 VG와 그 LV, lvmlockd의 상태를 보인다.
fn state() -> String {
    let vg = vg();
    let mut text = String::new();
    for (program, args) in [
        (
            "vgs",
            vec!["-o", "vg_name,vg_lock_type,vg_size,vg_free", &vg],
        ),
        (
            "lvs",
            vec![
                "-a",
                "-o",
                "lv_name,lv_attr,lv_size,lv_tags,seg_pe_ranges",
                &vg,
            ],
        ),
        ("lvmlockctl", vec!["--info"]),
    ] {
        let out = output(program, &args);
        text.push_str(&format!(
            "\n$ {program} {}\n{}{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    text
}

/// VG에 있는 LV 이름 중 `prefix`로 시작하는 것
fn lvs(prefix: &str) -> Vec<String> {
    let out = output("lvs", &["--noheadings", "-o", "lv_name", &vg()]);
    assert!(out.status.success(), "{out:?}{}", state());
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect();
    names.sort();
    names
}

/// 볼륨 LV의 `lvs` 필드 하나
fn lv_field(id: Uuid, field: &str) -> String {
    let target = format!("{}/{}", vg(), lv(id));
    let out = output("lvs", &["--noheadings", "-o", field, &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// 볼륨 LV의 물리 익스텐트 구간(PV, 처음, 끝)을 세그먼트 순서대로
fn pe_ranges(id: Uuid) -> Vec<(String, u64, u64)> {
    let target = format!("{}/{}", vg(), lv(id));
    let out = output("lvs", &["--noheadings", "-o", "seg_pe_ranges", &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .map(|range| {
            let (pv, pes) = range.rsplit_once(':').expect("PV:FIRST-LAST");
            let (first, last) = pes.split_once('-').expect("FIRST-LAST");
            (
                pv.to_string(),
                first.parse().unwrap(),
                last.parse().unwrap(),
            )
        })
        .collect()
}

/// 볼륨 LV가 이 노드에서 켜져 있는지(`lv_attr` 다섯째 글자 `a`)
fn is_active(id: Uuid) -> bool {
    lv_field(id, "lv_attr").chars().nth(4) == Some('a')
}

/// 볼륨 LV가 이 노드에서 배타로 켜져 있는지
fn is_exclusive(id: Uuid) -> bool {
    lv_field(id, "lv_active_exclusively") == "active exclusively"
}

fn qemu_img(args: &[&str]) {
    let status = Command::new("qemu-img").args(args).status().unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_zeroes_activates_exclusively_resizes_and_deletes() {
    let _serial = SERIAL.lock().await;
    let pool = shared_pool();
    let storage = storage();

    // 앞 볼륨이 쓴 자리에 새 볼륨이 놓여도 0으로 읽힌다: 만들 때 켜지 않으므로(`-Zn`) LVM은 채우지
    // 않고 드라이버가 모두 0으로 채운다. 이 노드가 켠 LV는 켜진 채로도 지운다.
    let old = Uuid::now_v7();
    storage
        .create_volume(&pool, old, 64 * MIB, RAW)
        .await
        .unwrap();
    storage.activate_volume(&pool, old, RAW).await.unwrap();
    assert!(qemu_io(&device(old), "raw", "write -P 0xab 0 64M"));
    storage.delete_volume(&pool, old, RAW).await.unwrap();
    assert!(lvs(&lv(old)).is_empty(), "{}", state());

    // 만든 뒤에는 꺼져 있다. 크기는 1 MiB 배수로 올린다.
    let id = Uuid::now_v7();
    let path = storage
        .create_volume(&pool, id, 64 * MIB - 1, RAW)
        .await
        .unwrap();
    assert_eq!(path, device(id));
    assert!(!path.exists(), "{}", state());
    assert!(!is_active(id), "{}", state());
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert_eq!(info.allocated_bytes, 64 * MIB);
    let err = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap_err();
    assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
    assert!(storage.target_reads_zero(&pool).await.unwrap());

    // 켜면 이 노드에서만(배타로) 켜지고 장치가 생긴다. 내용은 모두 0이다.
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(path.exists(), "{}", state());
    assert!(is_exclusive(id), "{}", state());
    assert!(qemu_io(&path, "raw", "read -P 0 0 64M"), "{}", state());
    let location = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, DiskLocation::Path(path.clone(), DiskKind::Block));
    assert!(storage.active_volumes(&pool).await.unwrap().contains(&id));
    // 이미 켜져 있어도 된다.
    storage.activate_volume(&pool, id, RAW).await.unwrap();

    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    assert!(
        capacity.available_bytes < capacity.total_bytes,
        "{capacity:?}"
    );

    let again = storage.create_volume(&pool, id, 64 * MIB, RAW).await;
    assert!(
        matches!(again, Err(DriverError::VolumeExists(_))),
        "{again:?}"
    );
    let qcow2 = storage
        .create_volume(&pool, Uuid::now_v7(), 64 * MIB, ImageFormat::Qcow2)
        .await;
    assert!(
        matches!(qcow2, Err(DriverError::InvalidSpec(_))),
        "{qcow2:?}"
    );
    assert!(!storage.supports_snapshots(&pool, RAW));
    let snap = storage.snapshot_create(&pool, id, RAW, "s1").await;
    assert!(matches!(snap, Err(DriverError::InvalidSpec(_))), "{snap:?}");

    // 켜진 볼륨을 늘리면 내용은 그대로다.
    assert!(qemu_io(&path, "raw", "write -P 0xcd 0 1M"));
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);
    assert!(qemu_io(&path, "raw", "read -P 0xcd 0 1M"));

    // 끄면 장치가 없어진다. 이미 꺼져 있어도 된다.
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    assert!(!path.exists(), "{}", state());
    assert!(!is_active(id), "{}", state());
    assert!(!storage.active_volumes(&pool).await.unwrap().contains(&id));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();

    // 꺼진 볼륨은 늘린 부분을 채우는 동안만 켠다. 이미 그 크기 이상이면 아무것도 하지 않는다(줄이지
    // 않는다).
    storage
        .resize_volume(&pool, id, RAW, 192 * MIB)
        .await
        .unwrap();
    assert!(!is_active(id), "{}", state());
    storage
        .resize_volume(&pool, id, RAW, 32 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 192 * MIB);

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(lvs(&lv(id)).is_empty(), "{}", state());
    let gone = storage.volume_info(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    let gone = storage.activate_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    let gone = storage.delete_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
}

/// 늘린 부분은 0으로 읽힌다: 바로 뒤에 놓였다가 지운 볼륨의 데이터가 보이지 않는다. 꺼진 볼륨은 늘린
/// 뒤에도 꺼져 있고, 켜진 볼륨(실행 중인 VM)은 켠 채로 채운다. 다 채우면 표시 태그를 뗀다. 크기
/// 늘리기가 남긴 태그는 켜기(VM 시작)가 마저 채운다.
#[tokio::test]
async fn resize_zeroes_the_new_part() {
    let _serial = SERIAL.lock().await;
    let pool = shared_pool();
    let storage = storage();

    // 작은 볼륨 바로 뒤에 놓인 볼륨을 패턴으로 채우고 지운다. 작은 볼륨을 늘리면 그 자리를 다시 쓴다.
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 4 * MIB, RAW)
        .await
        .unwrap();
    let old = Uuid::now_v7();
    storage
        .create_volume(&pool, old, 64 * MIB, RAW)
        .await
        .unwrap();
    // 패턴 볼륨이 바로 뒤 익스텐트에 놓여야 늘린 부분이 그 자리를 다시 쓴다.
    let (small, pattern) = (pe_ranges(id), pe_ranges(old));
    let (pv, _, last) = small.last().unwrap().clone();
    let (pattern_pv, pattern_first, _) = pattern[0].clone();
    assert!(
        pv == pattern_pv && pattern_first == last + 1,
        "the pattern LV is not right after the volume: {small:?} {pattern:?}{}",
        state()
    );
    storage.activate_volume(&pool, old, RAW).await.unwrap();
    assert!(qemu_io(&device(old), "raw", "write -P 0xab 0 64M"));
    storage.deactivate_volume(&pool, old, RAW).await.unwrap();
    storage.delete_volume(&pool, old, RAW).await.unwrap();

    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(qemu_io(&device(id), "raw", "write -P 0xcd 0 4M"));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    storage
        .resize_volume(&pool, id, RAW, 68 * MIB)
        .await
        .unwrap();
    assert!(!is_active(id), "{}", state());
    assert_eq!(lv_field(id, "lv_tags"), "", "{}", state());
    let grown = pe_ranges(id);
    assert!(
        grown
            .iter()
            .any(|(p, first, last)| *p == pattern_pv && (*first..=*last).contains(&pattern_first)),
        "the new part is not on the pattern LV's extents: {grown:?} {pattern:?}{}",
        state()
    );
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(qemu_io(&device(id), "raw", "read -P 0xcd 0 4M"));
    assert!(
        qemu_io(&device(id), "raw", "read -P 0 4M 64M"),
        "{}",
        state()
    );

    // 켜진 볼륨은 켠 채로 늘리고 채운다.
    storage
        .resize_volume(&pool, id, RAW, 72 * MIB)
        .await
        .unwrap();
    assert!(is_exclusive(id), "{}", state());
    assert_eq!(lv_field(id, "lv_tags"), "", "{}", state());
    assert!(
        qemu_io(&device(id), "raw", "read -P 0 4M 68M"),
        "{}",
        state()
    );

    // 늘린 부분을 다 채우지 못해 태그가 남았으면 켜기가 장치를 내주기 전에 마저 채우고 뗀다.
    assert!(qemu_io(&device(id), "raw", "write -P 0xef 68M 4M"));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    let tag = format!("opl-zero-from-{}", 68 * MIB);
    let target = format!("{}/{}", vg(), lv(id));
    let out = output("lvchange", &["-q", "--addtag", &tag, &target]);
    assert!(out.status.success(), "{out:?}{}", state());
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert_eq!(lv_field(id, "lv_tags"), "", "{}", state());
    assert!(qemu_io(&device(id), "raw", "read -P 0xcd 0 4M"));
    assert!(
        qemu_io(&device(id), "raw", "read -P 0 68M 4M"),
        "{}",
        state()
    );
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    storage.delete_volume(&pool, id, RAW).await.unwrap();
}

/// 장치를 연 채로는 끌 수 없다(실행 중인 VM의 디스크). 끄기는 실패하고 LV는 켜진 채 남는다.
#[tokio::test]
async fn an_open_volume_stays_active() {
    let _serial = SERIAL.lock().await;
    let pool = shared_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    let open = std::fs::File::open(device(id)).unwrap();
    let err = storage.deactivate_volume(&pool, id, RAW).await;
    assert!(err.is_err(), "{err:?}{}", state());
    assert!(is_active(id), "{}", state());
    drop(open);
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    assert!(!is_active(id), "{}", state());
    storage.delete_volume(&pool, id, RAW).await.unwrap();
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let _serial = SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let pool = shared_pool();
    let storage = storage();

    // 알려진 패턴의 raw → vmdk. 가상 크기는 LV의 extent 크기로 올라간다.
    let pattern: Vec<u8> = (0..MIB as usize).map(|i| (i % 251) as u8).collect();
    let raw = tmp.path().join("src.raw");
    std::fs::write(&raw, &pattern).unwrap();
    let vmdk = tmp.path().join("src.vmdk");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw.to_str().unwrap(),
        vmdk.to_str().unwrap(),
    ]);
    storage
        .check_import_source(&vmdk, SourceFormat::Vmdk)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, RAW)
        .await
        .unwrap();
    assert_eq!(path, device(id));
    // 써 넣은 뒤 끈다.
    assert!(!is_active(id), "{}", state());
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert!(info.virtual_bytes >= MIB, "{info:?}");
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    let mut head = vec![0u8; MIB as usize];
    std::fs::File::open(&path)
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert!(head == pattern, "imported contents differ");
    // 원본 뒤는 0이다.
    let rest = format!("read -P 0 1M {}", info.virtual_bytes - MIB);
    assert!(qemu_io(&path, "raw", &rest));
    storage.deactivate_volume(&pool, id, RAW).await.unwrap();
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 LV를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storage
        .import_volume(
            &pool,
            missing,
            &tmp.path().join("missing.vmdk"),
            SourceFormat::Vmdk,
            RAW,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(lvs(&lv(missing)).is_empty(), "{}", state());
}

#[tokio::test]
async fn copies_within_and_across_backends() {
    let _serial = SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let pool = shared_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_lvm_shared(Arc::new(storage()));
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    storages
        .create_volume(&pool, src, 64 * MIB, RAW)
        .await
        .unwrap();
    storages.activate_volume(&pool, src, RAW).await.unwrap();
    assert!(qemu_io(&device(src), "raw", "write -P 0xab 0 64k"));
    storages.deactivate_volume(&pool, src, RAW).await.unwrap();

    // 원본은 볼륨 LV를 켜서 읽게 하고, 놓으면 끈다. 스냅샷은 없다.
    let source = storages.volume_source(&pool, src, RAW, None).await.unwrap();
    assert_eq!(source.path, device(src));
    assert_eq!(source.size_bytes, 64 * MIB);
    assert!(is_exclusive(src), "{}", state());
    storages
        .release_source(&pool, src, RAW, None)
        .await
        .unwrap();
    assert!(!is_active(src), "{}", state());
    let snap = storages.volume_source(&pool, src, RAW, Some("s1")).await;
    assert!(matches!(snap, Err(DriverError::InvalidSpec(_))), "{snap:?}");
    assert!(!is_active(src), "{}", state());

    // 같은 풀 안: 새 LV로. 끝나면 둘 다 꺼져 있다.
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, None, false, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(!is_active(src) && !is_active(copy), "{}", state());
    let info = storages.volume_info(&pool, copy, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    storages.activate_volume(&pool, copy, RAW).await.unwrap();
    assert!(qemu_io(&device(copy), "raw", "read -P 0xab 0 64k"));
    assert!(qemu_io(&device(copy), "raw", "read -P 0 64k 1M"));
    storages.deactivate_volume(&pool, copy, RAW).await.unwrap();

    // 디렉터리 풀로: qcow2로
    let out = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, None, false, &local, out, qcow2)
        .await
        .unwrap();
    assert!(!is_active(src), "{}", state());
    let out_path = DirStorage::volume_path(&local, Path::new(MNT), out, qcow2);
    assert!(qemu_io(&out_path, "qcow2", "read -P 0xab 0 64k"));

    // 디렉터리 풀에서 돌아오기: qcow2 → raw LV
    assert!(qemu_io(&out_path, "qcow2", "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(!is_active(back), "{}", state());
    storages.activate_volume(&pool, back, RAW).await.unwrap();
    assert!(qemu_io(&device(back), "raw", "read -P 0x5a 0 64k"));
    storages.deactivate_volume(&pool, back, RAW).await.unwrap();

    // 스냅샷에서는 복사할 수 없고 대상 LV를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("s1"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(lvs(&lv(missing)).is_empty(), "{}", state());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(lvs(&lv(id)).is_empty(), "{}", state());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}
