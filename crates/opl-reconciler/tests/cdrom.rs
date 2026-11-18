//! CD-ROM 드라이브와 cloud-init 씨앗으로 VM을 켠다(계획 57).

mod common;

use std::path::PathBuf;

use common::{Env, vm};
use opl_types::cloud_init::CloudInit;
use opl_types::resource::Resource;
use opl_types::runtime::{ImageFormat, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::vm::{CdromSpec, PendingChange, PendingReason, Vm, VmPhase};
use uuid::Uuid;

fn running(v: &Vm) -> bool {
    v.status.phase == VmPhase::Running && v.status.hardware.is_some()
}

fn cd(iso: Option<&str>) -> CdromSpec {
    CdromSpec {
        iso: iso.map(String::from),
        boot_order: None,
    }
}

/// dir 풀 `isos`와 그 ISO 볼륨 `ubuntu`
async fn iso_pool(env: &Env) -> Volume {
    let pool: StoragePool = Resource::new(
        "isos",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/pools/isos".into(),
            },
            nodes: vec![env.node],
            shared: false,
            backups: false,
            folder: None,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    let iso: Volume = Resource::new(
        "ubuntu",
        VolumeSpec {
            pool: "isos".into(),
            size_bytes: 1 << 20,
            format: ImageFormat::Raw,
            content: opl_types::storage::VolumeContent::Iso,
            base: None,
        },
    );
    env.cluster.create(&iso).await.unwrap();
    iso
}

/// 이 VM을 마지막으로 띄운 런타임 스펙
fn last_launch(env: &Env, id: Uuid) -> VmRuntimeSpec {
    env.hv
        .launch_specs()
        .into_iter()
        .rfind(|s| s.id == id)
        .expect("VM was not launched")
}

/// 한 ISO를 두 VM이 함께 넣고 켠다(읽기 전용 매체). 빈 드라이브는 매체 없이, cloud-init VM은 VM 이름을 호스트 이름으로
/// 한 씨앗과 함께 켠다. 실행 중 하드웨어 기록에 드라이브와 씨앗의 instance-id가 남는다.
#[tokio::test]
async fn vms_share_an_iso_and_start_with_their_seed() {
    let env = Env::start().await;
    let iso = iso_pool(&env).await;
    let path = PathBuf::from(format!("/pools/isos/{}.raw", iso.id));
    let mut a = vm("web", env.node);
    a.spec.cdroms = vec![cd(Some("ubuntu")), cd(None)];
    a.spec.cloud_init = Some(CloudInit {
        user: Some("orbi".into()),
        ..Default::default()
    });
    let mut b = vm("db", env.node);
    b.spec.cdroms = vec![cd(Some("ubuntu"))];
    env.create(&a).await;
    env.create(&b).await;

    let got = env.wait(a.id, "running", running).await;
    env.wait(b.id, "running", running).await;
    assert_eq!(env.hv.media(a.id), [Some(path.clone()), None]);
    assert_eq!(env.hv.media(b.id), [Some(path)]);
    let launched = last_launch(&env, a.id);
    let order: Vec<_> = launched.cdroms.iter().map(|c| c.boot_order).collect();
    assert_eq!(order, [Some(1), Some(2)]);

    let seed = env.hv.seed(a.id).expect("no seed");
    let meta: serde_json::Value = serde_json::from_str(&seed.meta_data).unwrap();
    assert_eq!(meta["local-hostname"], "web");
    assert!(seed.user_data.contains("\"orbi\""), "{}", seed.user_data);
    assert_eq!(env.hv.seed(b.id), None);

    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.cdroms, a.spec.cdroms);
    assert_eq!(hw.cloud_init, Some(seed.instance_id));
    assert!(hw.pending.is_empty(), "{:?}", hw.pending);
}

/// 켜는 동안(QEMU를 띄운 뒤 기록하기 전) 바뀐 CD 매체·cloud-init은 기록의 보류로 남는다: 매체는 `vm hotplug`로,
/// cloud-init은 다시 켜야 적용된다.
#[tokio::test]
async fn cdrom_and_cloud_init_changes_made_while_starting_are_left_pending() {
    let env = Env::start().await;
    iso_pool(&env).await;
    let mut a = vm("web", env.node);
    a.spec.cdroms = vec![cd(Some("ubuntu"))];
    env.hv.hold_next_start();
    env.create(&a).await;
    tokio::time::timeout(common::WAIT, env.hv.start_held())
        .await
        .expect("start reached");
    loop {
        let now = env.get(a.id);
        let mut spec = now.spec.clone();
        spec.cdroms[0].iso = None;
        spec.cloud_init = Some(CloudInit::default());
        if env
            .cluster
            .update_spec(a.id, Some(now.revision), now.labels.clone(), &spec)
            .await
            .is_ok()
        {
            break;
        }
    }
    env.hv.release_start();
    let got = env.wait(a.id, "running", running).await;
    let hw = got.status.hardware.unwrap();
    assert_eq!(hw.cdroms, a.spec.cdroms);
    assert_eq!(hw.cloud_init, None);
    assert_eq!(hw.pending.len(), 2, "{:?}", hw.pending);
    assert_eq!(hw.pending[0].field, "cloud_init");
    assert_eq!(hw.pending[0].running, "none");
    assert_eq!(
        hw.pending[1],
        PendingChange {
            field: "cdroms[0]".into(),
            running: "ubuntu".into(),
            wanted: "empty".into(),
            reason: PendingReason::Restart,
            detail: Some("changed while the VM was starting; run `vm hotplug`".into()),
        }
    );
}
