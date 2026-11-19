//! 다른 노드에서 UEFI 변수·TPM 상태 파일 받아 오기와 펌웨어 사본 밀어 두기(설계 §5, 계획 59). 파일을 가진 노드
//! (`firmware_node`, 사본 노드)에 노드 사이 RPC `UefiVars`·`TpmState`로 받아 이 노드의 파일을 바꾸고, `PushFirmware`로
//! 사본 노드에 밀어 둔다. opld가 reconciler와 Task 실행기에 넣는다.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_driver::{KeepFirmware, adopt_firmware_epoch, tpm_state_path, uefi_vars_path};
use opl_peer::{Peers, PurgeFirmwareRequest, PushFirmwareRequest, read_chunks, sha256_hex};
use opl_reconciler::{FirmwareCopy, FirmwareError, FirmwareFetcher, FirmwarePurge};
use opl_types::firmware::tpm_is_current;
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use uuid::Uuid;

pub struct UefiVarsFetcher {
    cluster: Arc<Cluster>,
    peers: Arc<Peers>,
    /// 이 노드의 드라이버 `state_dir`(받은 파일을 여기 둔다)
    state_dir: PathBuf,
    /// 이 노드의 UEFI 변수 템플릿. 받은 파일의 크기가 이와 다르면(다른 펌웨어 빌드) 쓰지 않는다.
    template: PathBuf,
    /// 이 노드의 Secure Boot 변수 템플릿(계획 59). Secure Boot VM의 변수는 이것과 견준다. 없으면 받지 않는다.
    secure_boot_template: Option<PathBuf>,
}

impl UefiVarsFetcher {
    pub fn new(
        cluster: Arc<Cluster>,
        peers: Arc<Peers>,
        state_dir: impl Into<PathBuf>,
        template: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cluster,
            peers,
            state_dir: state_dir.into(),
            template: template.into(),
            secure_boot_template: None,
        }
    }

    /// Secure Boot VM의 변수 파일 크기를 `template`(Secure Boot 펌웨어의 변수 템플릿)과 견준다.
    pub fn with_secure_boot_template(mut self, template: Option<PathBuf>) -> Self {
        self.secure_boot_template = template;
        self
    }

    /// 노드의 첫 링크. 오프라인으로 기록된 노드에는 연결해 보지 않는다.
    fn addr(&self, node: Uuid) -> Result<String, FirmwareError> {
        match self.cluster.get::<NodeSpec>(node) {
            Ok(Some(n)) if !n.status.online => {
                return Err(FirmwareError(format!("node {:?} is offline", n.name)));
            }
            Ok(_) => {}
            Err(e) => return Err(FirmwareError(format!("cannot read node {node}: {e}"))),
        }
        self.cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string)
            .ok_or_else(|| FirmwareError(format!("node {node} is not a cluster member")))
    }

    /// 받은 파일을 켤 때 지우지 않도록 이 노드의 펌웨어 회차를 VM의 것으로 옮긴다. 회차가 바뀌면 받지 않은 낡은
    /// 파일(`keep`에 없는 것)을 먼저 지운다(리뷰 1 C2).
    fn record_epoch(&self, vm: Uuid, epoch: u32, keep: KeepFirmware) -> Result<(), FirmwareError> {
        adopt_firmware_epoch(&self.state_dir, vm, epoch, keep)
            .map_err(|e| FirmwareError(format!("cannot record the firmware epoch: {e}")))
    }

    /// 이 노드의 TPM 상태가 지금 것인지([`tpm_is_current`]: 이 노드가 `tpm_node`이거나 해시가 VM의 `tpm_sha256`과
    /// 같다). 회차를 옮길 때 지금 것인 TPM 상태는 지우지 않는다(리뷰 2).
    async fn tpm_current(&self, vm: &opl_types::vm::Vm) -> bool {
        let local = match tokio::fs::read(tpm_state_path(&self.state_dir, vm.id)).await {
            Ok(data) => Some(sha256_hex(&data)),
            Err(_) => None,
        };
        tpm_is_current(&vm.status, self.cluster.id().0, local.as_deref())
    }
}

#[async_trait]
impl FirmwareFetcher for UefiVarsFetcher {
    /// 노드마다 펌웨어 빌드가 달라 변수 파일의 크기가 이 노드의 템플릿(Secure Boot VM이면 Secure Boot 템플릿)과
    /// 다르면 받은 것을 버린다(이 노드의 펌웨어가 그 파일로 켜지지 않는다).
    async fn fetch(&self, vm: Uuid, node: Uuid) -> Result<(), FirmwareError> {
        let addr = self.addr(node)?;
        let current = self
            .cluster
            .get::<VmSpec>(vm)
            .map_err(|e| FirmwareError(format!("cannot read VM {vm}: {e}")))?;
        let secure_boot = current.as_ref().is_some_and(|v| v.spec.secure_boot);
        let template = if secure_boot {
            self.secure_boot_template
                .as_ref()
                .ok_or_else(|| FirmwareError("this node has no Secure Boot firmware".into()))?
        } else {
            &self.template
        };
        let len = tokio::fs::metadata(template)
            .await
            .map_err(|e| {
                FirmwareError(format!(
                    "cannot read the UEFI variables template {}: {e}",
                    template.display()
                ))
            })?
            .len();
        self.peers
            .uefi_vars(
                NodeId(node),
                &addr,
                vm,
                &uefi_vars_path(&self.state_dir, vm),
                Some(len),
                current.as_ref().map(|v| v.status.firmware_epoch),
            )
            .await
            .map_err(|e| FirmwareError(e.to_string()))?;
        match current {
            Some(v) => {
                let tpm = self.tpm_current(&v).await;
                let keep = KeepFirmware { vars: true, tpm };
                self.record_epoch(vm, v.status.firmware_epoch, keep)
            }
            None => Ok(()),
        }
    }

    async fn fetch_tpm(
        &self,
        vm: Uuid,
        node: Uuid,
        sha256: &str,
        epoch: u32,
    ) -> Result<(), FirmwareError> {
        let addr = self.addr(node)?;
        self.peers
            .tpm_state(
                NodeId(node),
                &addr,
                vm,
                &tpm_state_path(&self.state_dir, vm),
                sha256,
            )
            .await
            .map_err(|e| FirmwareError(e.to_string()))?;
        let keep = KeepFirmware {
            vars: false,
            tpm: true,
        };
        self.record_epoch(vm, epoch, keep)
    }

    async fn push(&self, copy: &FirmwareCopy) -> Result<(), FirmwareError> {
        let addr = self.addr(copy.to)?;
        let path = tpm_state_path(&self.state_dir, copy.vm);
        let tpm = tokio::fs::read(&path).await.map_err(|e| {
            FirmwareError(format!("cannot read the TPM state {}: {e}", path.display()))
        })?;
        if sha256_hex(&tpm) != copy.sha256 {
            return Err(FirmwareError(
                "the TPM state changed since it was recorded".into(),
            ));
        }
        let vars = read_chunks(&uefi_vars_path(&self.state_dir, copy.vm))
            .await
            .map_err(|e| FirmwareError(format!("cannot read the UEFI variables: {e}")))?;
        let req = PushFirmwareRequest {
            vm: copy.vm,
            epoch: copy.epoch,
            secure_boot: copy.secure_boot,
            sha256: copy.sha256.clone(),
            tpm: tpm.into(),
            vars,
        };
        self.peers
            .push_firmware(NodeId(copy.to), &addr, req)
            .await
            .map_err(|e| FirmwareError(e.to_string()))
    }

    async fn purge(&self, to: Uuid, purge: &FirmwarePurge) -> Result<(), FirmwareError> {
        let addr = self.addr(to)?;
        let req = PurgeFirmwareRequest {
            vm: purge.vm,
            epoch: purge.epoch,
            uefi_vars: purge.uefi_vars,
            tpm: purge.tpm,
            snapshot: purge.snapshot,
        };
        self.peers
            .purge_firmware(NodeId(to), &addr, req)
            .await
            .map_err(|e| FirmwareError(e.to_string()))
    }
}
