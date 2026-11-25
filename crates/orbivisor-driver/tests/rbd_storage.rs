//! 실제 Ceph 풀의 저장소 통합 테스트. 관리자가 만든 Ceph 풀(`ORBIVISOR_TEST_RBD=POOL`, `rbd pool init`을
//! 마친 것)에 볼륨 이미지를 만들고 가져오고, RBD 스냅샷을 찍어 되돌리고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" ORBIVISOR_TEST_RBD=rbd cargo test -p orbivisor-driver --features rbd-it --test rbd_storage`
//! Linux에서 root, ceph-common(`rbd`, `ceph`), qemu-block-extra(rbd 블록 드라이버), qemu-img, qemu-io와
//! `client.admin`으로 접속할 수 있는 `/etc/ceph/ceph.conf`·키링이 필요하다. 제한된 사용자 시험은
//! `ORBIVISOR_TEST_RBD_USER`(caps `profile rbd`), `ORBIVISOR_TEST_RBD_NAMESPACE`(그 풀의 네임스페이스),
//! `ORBIVISOR_TEST_RBD_CONF`(그 사용자의 키링을 가리키는 ceph.conf)가 있을 때만 돈다.
#![cfg(all(feature = "rbd-it", target_os = "linux"))]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use orbivisor_driver::{
    DirStorage, DiskLocation, DriverError, RbdStorage, SourceFormat, Storage, Storages, SystemRbd,
};
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::{ImageFormat, RbdTarget};
use orbivisor_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use serde_json::Value;
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// `ORBIVISOR_TEST_RBD`의 Ceph 풀
fn ceph_pool() -> String {
    std::env::var("ORBIVISOR_TEST_RBD").expect("ORBIVISOR_TEST_RBD must name a Ceph pool")
}

fn rbd_pool() -> StoragePool {
    Resource::new(
        "rbd",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: ceph_pool(),
                namespace: None,
                user: None,
                conf: None,
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
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
            backups: false,
            folder: None,
        },
    )
}

fn storage() -> RbdStorage {
    RbdStorage::new("qemu-img", Arc::new(SystemRbd))
}

fn image(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 볼륨 이미지의 RBD 대상(기본 사용자 `admin`, Ceph 기본 conf)
fn target(id: Uuid) -> RbdTarget {
    RbdTarget {
        pool: ceph_pool(),
        namespace: None,
        image: image(id),
        user: "admin".into(),
        conf: None,
    }
}

/// `rbd` 명령을 실행하고 표준 출력을 돌려준다.
fn rbd(args: &[&str]) -> Vec<u8> {
    let out = Command::new("rbd").args(args).output().expect("run rbd");
    assert!(out.status.success(), "rbd {args:?}: {out:?}");
    out.stdout
}

/// 풀의 이미지와 스냅샷(`image@tag`) 이름 중 `prefix`로 시작하는 것
fn rbd_names(prefix: &str) -> Vec<String> {
    let pool = ceph_pool();
    let images: Vec<String> =
        serde_json::from_slice(&rbd(&["ls", "--format", "json", "--pool", &pool])).unwrap();
    let mut names = vec![];
    for name in images.into_iter().filter(|n| n.starts_with(prefix)) {
        let spec = format!("{pool}/{name}");
        let snaps: Value =
            serde_json::from_slice(&rbd(&["snap", "ls", "--format", "json", &spec])).unwrap();
        for s in snaps.as_array().unwrap() {
            names.push(format!("{name}@{}", s["name"].as_str().unwrap()));
        }
        names.push(name);
    }
    names.sort();
    names
}

fn qemu_img(args: &[&str]) {
    let status = Command::new("qemu-img").args(args).status().unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

/// 볼륨 이미지(`snapshot`이 있으면 그 스냅샷)에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다.
/// `read -P`는 패턴이 다르면 실패한다. 읽기는 읽기 전용으로 연다(스냅샷은 쓸 수 없다).
fn qemu_io_rbd(id: Uuid, snapshot: Option<&str>, cmd: &str) -> bool {
    qemu_io_target(&target(id), snapshot, cmd)
}

/// [`qemu_io_rbd`]를 RBD 대상(네임스페이스·사용자·conf)에
fn qemu_io_target(target: &RbdTarget, snapshot: Option<&str>, cmd: &str) -> bool {
    let mut opts = format!("driver=raw,file.driver=rbd,file.pool={}", target.pool);
    if let Some(ns) = &target.namespace {
        opts.push_str(&format!(",file.namespace={ns}"));
    }
    opts.push_str(&format!(
        ",file.image={},file.user={}",
        target.image, target.user
    ));
    if let Some(conf) = &target.conf {
        opts.push_str(&format!(",file.conf={conf}"));
    }
    if let Some(tag) = snapshot {
        opts.push_str(&format!(",file.snapshot={tag}"));
    }
    let mut qemu_io = Command::new("qemu-io");
    if cmd.starts_with("read") {
        qemu_io.arg("-r");
    }
    qemu_io
        .args(["--image-opts", "-c", cmd, &opts])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

/// 파일 볼륨에 `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다.
fn qemu_io_file(path: &Path, format: &str, cmd: &str) -> bool {
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_inspect_resize_and_delete_volume() {
    let pool = rbd_pool();
    let storage = storage();
    let id = Uuid::now_v7();

    // 이미지에는 경로가 없다. 크기는 1 MiB 배수로 올린다.
    let path = storage
        .create_volume(&pool, id, 64 * MIB - 1, RAW)
        .await
        .unwrap();
    assert_eq!(path, Path::new(""));
    assert_eq!(rbd_names(&image(id)), vec![image(id)]);
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(info.allocated_bytes < info.virtual_bytes, "{info:?}");
    assert!(qemu_io_rbd(id, None, "write -P 0xab 0 1M"));
    let location = orbivisor_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, DiskLocation::Rbd(target(id)));
    storage.activate_volume(&pool, id, RAW).await.unwrap();

    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    assert!(
        capacity.available_bytes <= capacity.total_bytes,
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

    // 늘리면 내용은 그대로다. 이미 그 크기 이상이면 아무것도 하지 않는다(줄이지 않는다).
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);
    assert!(qemu_io_rbd(id, None, "read -P 0xab 0 1M"));
    storage
        .resize_volume(&pool, id, RAW, 32 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(rbd_names(&image(id)).is_empty());
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
    let gone = storage.delete_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = rbd_pool();
    let storage = storage();

    // 알려진 패턴의 raw → vmdk
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
    storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, RAW)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, MIB, "{info:?}");
    // 이미지 내용을 파일로 꺼내 비교한다.
    let exported = tmp.path().join("exported.raw");
    rbd(&[
        "export",
        "--no-progress",
        &format!("{}/{}", ceph_pool(), image(id)),
        exported.to_str().unwrap(),
    ]);
    assert!(
        std::fs::read(&exported).unwrap() == pattern,
        "imported contents differ"
    );
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 이미지를 남기지 않는다.
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
    assert!(rbd_names(&image(missing)).is_empty());
}

#[tokio::test]
async fn snapshots_revert_to_any_and_go_with_the_volume() {
    let pool = rbd_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(&pool, RAW));
    assert!(!storage.supports_snapshots(&pool, ImageFormat::Qcow2));
    let name = image(id);

    assert!(qemu_io_rbd(id, None, "write -P 0xab 0 64k"));
    let s1 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s1).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone()]
    );
    assert_eq!(rbd_names(&name), vec![name.clone(), format!("{name}@{s1}")]);
    let dup = storage.snapshot_create(&pool, id, RAW, &s1).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    // 스냅샷은 그때 내용으로 읽힌다.
    assert!(qemu_io_rbd(id, None, "write -P 0xcd 0 64k"));
    assert!(qemu_io_rbd(id, Some(&s1), "read -P 0xab 0 64k"));
    let s2 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s2).await.unwrap();
    assert!(qemu_io_rbd(id, None, "write -P 0x5a 0 64k"));
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone(), s2.clone()]
    );

    // 더 최근 스냅샷이 있어도 앞 스냅샷으로 되돌릴 수 있고, 스냅샷은 모두 남는다.
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io_rbd(id, None, "read -P 0xab 0 64k"));
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone(), s2.clone()]
    );
    storage.snapshot_revert(&pool, id, RAW, &s2).await.unwrap();
    assert!(qemu_io_rbd(id, None, "read -P 0xcd 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, RAW, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s2.clone()]
    );
    // 이미 없어도 성공한다.
    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io_rbd(id, None, "read -P 0xcd 0 64k"));

    // 여러 볼륨에 같은 태그를 찍는다(하나씩). 볼륨을 지우면 남은 스냅샷도 함께 지운다.
    let other = Uuid::now_v7();
    storage
        .create_volume(&pool, other, 64 * MIB, RAW)
        .await
        .unwrap();
    storage
        .snapshot_many(&pool, &[id, other], RAW, "both")
        .await
        .unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s2.clone(), "both".to_string()]
    );
    assert_eq!(
        storage.snapshot_tags(&pool, other, RAW).await.unwrap(),
        vec!["both".to_string()]
    );
    for v in [id, other] {
        storage.delete_volume(&pool, v, RAW).await.unwrap();
        assert!(rbd_names(&image(v)).is_empty());
    }
}

#[tokio::test]
async fn copies_snapshots_within_and_across_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = rbd_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_rbd(Arc::new(storage()));
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    storages
        .create_volume(&pool, src, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(src, None, "write -P 0xab 0 64k"));
    storages
        .snapshot_create(&pool, src, RAW, "s1")
        .await
        .unwrap();
    assert!(qemu_io_rbd(src, None, "write -P 0xcd 0 64k"));
    // 스냅샷 원본은 경로가 아니라 이미지와 스냅샷 이름이다.
    let snap = storages
        .volume_source(&pool, src, RAW, Some("s1"))
        .await
        .unwrap();
    assert_eq!(snap.rbd, Some(target(src)));
    assert_eq!(snap.snapshot.as_deref(), Some("s1"));
    assert_eq!(snap.size_bytes, 64 * MIB);
    storages
        .release_source(&pool, src, RAW, Some("s1"))
        .await
        .unwrap();

    // 같은 풀 안: 스냅샷 때의 내용을 새 이미지로
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(copy, None, "read -P 0xab 0 64k"));
    let info = storages.volume_info(&pool, copy, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    // 사본에는 스냅샷이 따라오지 않는다.
    assert!(
        storages
            .snapshot_tags(&pool, copy, RAW)
            .await
            .unwrap()
            .is_empty()
    );

    // 디렉터리 풀로: 스냅샷 때의 내용을 qcow2로
    let out = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &local, out, qcow2)
        .await
        .unwrap();
    let out_path = DirStorage::volume_path(&local, Path::new(MNT), out, qcow2);
    assert!(qemu_io_file(&out_path, "qcow2", "read -P 0xab 0 64k"));

    // 디렉터리 풀에서 돌아오기: qcow2 → raw 이미지, 지금 내용
    assert!(qemu_io_file(&out_path, "qcow2", "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(back, None, "read -P 0x5a 0 64k"));

    // 없는 스냅샷은 실패하고 대상 이미지를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("gone"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(rbd_names(&image(missing)).is_empty());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(rbd_names(&image(id)).is_empty());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}

/// 제한된 사용자(`profile rbd`)가 네임스페이스 안에서 명시적 conf(키링은 그 conf만 가리킨다)로
/// 접속한다: 만들기·용량, QEMU로 쓰기, 스냅샷과 되돌리기, 복사, 지우기.
#[tokio::test]
async fn restricted_user_in_a_namespace_with_its_conf() {
    let var = |name| std::env::var(name).ok();
    let (Some(user), Some(namespace), Some(conf)) = (
        var("ORBIVISOR_TEST_RBD_USER"),
        var("ORBIVISOR_TEST_RBD_NAMESPACE"),
        var("ORBIVISOR_TEST_RBD_CONF"),
    ) else {
        eprintln!(
            "skipped: ORBIVISOR_TEST_RBD_USER, ORBIVISOR_TEST_RBD_NAMESPACE and ORBIVISOR_TEST_RBD_CONF are not set"
        );
        return;
    };
    let pool: StoragePool = Resource::new(
        "limited",
        StoragePoolSpec {
            backend: PoolBackend::Rbd {
                pool: ceph_pool(),
                namespace: Some(namespace.clone()),
                user: Some(user.clone()),
                conf: Some(conf.clone()),
            },
            nodes: vec![],
            shared: true,
            backups: false,
            folder: None,
        },
    );
    let storage = storage();
    let id = Uuid::now_v7();
    storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    let image = match orbivisor_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap() {
        DiskLocation::Rbd(image) => image,
        other => panic!("not an rbd image: {other:?}"),
    };
    assert_eq!(
        (image.namespace.as_deref(), image.user.as_str()),
        (Some(namespace.as_str()), user.as_str())
    );
    assert_eq!(image.conf.as_deref(), Some(conf.as_str()));
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(qemu_io_target(&image, None, "write -P 0xab 0 64k"));

    storage.snapshot_create(&pool, id, RAW, "s1").await.unwrap();
    assert!(qemu_io_target(&image, None, "write -P 0xcd 0 64k"));
    assert!(qemu_io_target(&image, Some("s1"), "read -P 0xab 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, "s1").await.unwrap();
    assert!(qemu_io_target(&image, None, "read -P 0xab 0 64k"));
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();

    let copy = Uuid::now_v7();
    storage
        .copy_volume(&pool, id, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    let copied = RbdTarget {
        image: format!("opl-{copy}"),
        ..image.clone()
    };
    assert!(qemu_io_target(&copied, None, "read -P 0xab 0 64k"));

    for v in [id, copy] {
        storage.delete_volume(&pool, v, RAW).await.unwrap();
        let gone = storage.volume_info(&pool, v, RAW).await;
        assert!(
            matches!(gone, Err(DriverError::VolumeNotFound(_))),
            "{gone:?}"
        );
    }
}

/// 연결 복제: 보호한 기반 스냅샷 `@opl-base`의 clone이다. 복제는 기반의 내용을 읽고, 복제에 써도 기반은 그대로다.
/// clone이 있는 동안 기반 이미지도 기반 스냅샷도 지워지지 않는다(보호를 풀지 못한다). 평탄화(`rbd flatten`)하거나
/// clone을 지우면 기반 스냅샷을 풀고 기반을 지울 수 있고, 평탄화한 복제는 그대로 읽힌다.
#[tokio::test]
async fn linked_clones_are_clones_of_the_protected_base_snapshot() {
    let pool = rbd_pool();
    let storage = storage();
    let (base, a, b) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    storage
        .create_volume(&pool, base, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(qemu_io_rbd(base, None, "write -P 0x5a 0 1M"));

    for clone in [a, b] {
        let (path, format) = storage.link_clone(&pool, base, RAW, clone).await.unwrap();
        assert_eq!((path.as_path(), format), (Path::new(""), RAW));
    }
    assert_eq!(
        rbd_names(&image(base)),
        vec![image(base), format!("{}@opl-base", image(base))]
    );
    assert!(qemu_io_rbd(a, None, "read -P 0x5a 0 1M"));
    assert!(qemu_io_rbd(a, None, "write -P 0x77 0 1M"));
    assert!(qemu_io_rbd(base, None, "read -P 0x5a 0 1M"));
    assert!(qemu_io_rbd(b, None, "read -P 0x5a 0 1M"));
    let err = storage.link_clone(&pool, base, RAW, a).await.unwrap_err();
    assert!(
        matches!(err, DriverError::VolumeExists(v) if v == a),
        "{err:?}"
    );

    // 백엔드 막이
    let err = storage.delete_volume(&pool, base, RAW).await.unwrap_err();
    assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    let err = storage.release_base(&pool, base, RAW).await.unwrap_err();
    assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    assert!(rbd_names(&image(base)).contains(&image(base)));

    storage.flatten_volume(&pool, a, RAW).await.unwrap();
    storage.flatten_volume(&pool, a, RAW).await.unwrap();
    storage.delete_volume(&pool, b, RAW).await.unwrap();
    storage.release_base(&pool, base, RAW).await.unwrap();
    storage.release_base(&pool, base, RAW).await.unwrap();
    assert_eq!(rbd_names(&image(base)), vec![image(base)]);
    storage.delete_volume(&pool, base, RAW).await.unwrap();
    assert!(qemu_io_rbd(a, None, "read -P 0x77 0 1M"));
    assert!(qemu_io_rbd(a, None, "read -P 0 1M 1M"));
    storage.delete_volume(&pool, a, RAW).await.unwrap();
    assert!(rbd_names(&image(a)).is_empty());

    // clone이 없으면 기반 이미지의 지우기가 기반 스냅샷의 보호를 풀고 지운다.
    let (base, c) = (Uuid::now_v7(), Uuid::now_v7());
    storage
        .create_volume(&pool, base, 16 * MIB, RAW)
        .await
        .unwrap();
    storage.link_clone(&pool, base, RAW, c).await.unwrap();
    storage.delete_volume(&pool, c, RAW).await.unwrap();
    storage.delete_volume(&pool, base, RAW).await.unwrap();
    assert!(rbd_names(&image(base)).is_empty());
}
