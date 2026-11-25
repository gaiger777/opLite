//! 로컬 디스크 복제(계획 55)의 하이퍼바이저 밖 준비물: 회차 요청, 받는 QEMU가 내보낼 복제본.

use std::path::PathBuf;
use std::time::Duration;

use orbivisor_types::runtime::ImageFormat;
use tokio::sync::watch;

use crate::migration::MirrorProgress;
use crate::storage::DiskLocation;

/// 복제 NBD(보내는 쪽 client, 받는 쪽 server)의 TLS 자격 증명 객체 id. 마이그레이션·미러 것과 나눈다.
pub const REPLICA_TLS_ID: &str = "opl-repl-tls";

/// 복제 회차의 디스크 하나: 받는 QEMU의 NBD 내보내기에 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplicaDiskTarget {
    /// 디스크 순서 번호(노드 `disk{i}`)
    pub disk: usize,
    /// 받는 노드의 주소
    pub host: String,
    /// 받는 QEMU의 NBD 포트([`Hypervisor::start_replica_receiver`](crate::Hypervisor::start_replica_receiver)의 결과)
    pub port: u16,
    /// 내보내기 이름(볼륨 id simple)
    pub export: String,
    /// 받는 노드 인증서에서 확인할 이름
    pub tls_hostname: String,
    /// 기준 비트맵. 있으면 그 뒤에 바뀐 블록만 보내고(증분), 없으면 디스크 전체를 보낸다.
    pub base: Option<String>,
}

/// 복제 회차 하나([`Hypervisor::replicate_disks`](crate::Hypervisor::replicate_disks))
#[derive(Debug)]
pub struct ReplicateRequest {
    pub targets: Vec<ReplicaDiskTarget>,
    /// 이 회차의 시점에 대상 디스크마다 새로 만들 비트맵(`opl-repl-{노드}-{회차}`)
    pub bitmap: String,
    /// 디스크 하나의 최대 속도(바이트/초). `None`이면 제한 없음
    pub speed: Option<u64>,
    /// 보낸 양의 합이 이만큼 늘지 않으면 실패한다(끊긴 링크).
    pub stall: Duration,
    /// 있으면 진행이 바뀔 때마다 보낸다.
    pub progress: Option<watch::Sender<MirrorProgress>>,
    /// 회차의 시점에 게스트 파일 시스템을 얼린다(계획 59, `guest_agent.freeze_fs`).
    pub quiesce: bool,
}

/// 받는 QEMU가 내보낼 복제본 하나([`Hypervisor::start_replica_receiver`](crate::Hypervisor::start_replica_receiver))
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverExport {
    /// 내보내기 이름(볼륨 id simple)
    pub name: String,
    /// 복제본 볼륨의 위치(파일 또는 블록 장치)
    pub source: DiskLocation,
    /// 복제본 볼륨의 형식
    pub format: ImageFormat,
    /// 있으면 복제본을 backing으로 둔 qcow2 겹층(이미 만든 파일, 절대 경로)을 내보낸다(증분). 없으면 복제본에
    /// 바로 쓴다(전체).
    pub overlay: Option<PathBuf>,
}
