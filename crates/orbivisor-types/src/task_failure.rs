//! Task 실패의 원인 코드(계획 77-2, 진단 §3.3). Task가 실패하면 `TaskStatus.error`(영어 원문)와 함께
//! `TaskStatus.failure`(코드·값)를 남기고, 웹은 코드마다 "무엇이 / 왜 / 이렇게 하세요" 문구를 보인다.
//! 코드 목록은 아래 `codes!` 한 곳이다. 웹 시험(`web/src/tasks/failures/failures.test.ts`)이 이 파일을 읽어 문구 사전과
//! 짝이 맞는지 본다 — 코드를 더하면 웹 사전(ko·en)에도 더한다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 실패한 Task의 구조 오류. 옛 노드는 모르고 `error` 문자열만 남긴다(웹은 그때 원문만 보인다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct TaskFailure {
    /// 원인 코드(`code::*`, 예: `migration_tls_missing`). 웹은 모르는 코드면 원문만 보인다.
    pub code: String,
    /// 사람에게 보일 값(노드·풀·VM·네트워크 이름, 필요한·남은 바이트 …). 문구의 `{{이름}}` 자리에 들어간다.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub params: BTreeMap<String, String>,
    /// 실패한 단계(예: 적용의 "step 1/3 (create AlertRule \"x\")")
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub step: Option<String>,
    /// 같은 요청을 다시 내면 될 수 있는 실패(잠깐의 바쁨·끊김·시간 초과). 아니면 먼저 원인을 고쳐야 한다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub retryable: bool,
}

impl TaskFailure {
    /// `code`의 실패(다시 하기는 코드 목록의 기본값)
    pub fn new(code: &str) -> Self {
        Self {
            code: code.to_string(),
            retryable: retryable(code),
            ..Self::default()
        }
    }

    /// 사람에게 보일 값 하나
    pub fn param(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.params.insert(key.to_string(), value.to_string());
        self
    }

    pub fn step(mut self, step: impl Into<String>) -> Self {
        self.step = Some(step.into());
        self
    }
}

macro_rules! codes {
    ($($(#[doc = $doc:literal])* $name:ident = $code:literal, $retry:literal;)*) => {
        /// 원인 코드 상수
        pub mod code {
            $($(#[doc = $doc])* pub const $name: &str = $code;)*
        }
        /// 모든 원인 코드와 다시 하기 기본값(웹 사전 시험이 읽는다)
        pub const CODES: &[(&str, bool)] = &[$(($code, $retry),)*];
    };
}

codes! {
    // 공통
    /// 실행 시간 한도를 넘겼다
    TASK_TIMEOUT = "task_timeout", true;
    /// 도는 동안 orbivisord가 다시 시작했다
    TASK_INTERRUPTED = "task_interrupted", true;
    /// Task를 실행할 노드가 응답하지 않아 정리 작업이 실패로 끝냈다(`node`)
    NODE_OFFLINE = "node_offline", true;
    /// 노드가 클러스터에 없다(제거됨·멤버 아님, `node`)
    NODE_NOT_MEMBER = "node_not_member", false;
    /// 이 노드가 다른 노드에 닿을 수 없다(노드 사이 연결 설정 없음)
    NODE_NO_PEERS = "node_no_peers", false;
    /// 디스크(파일 시스템)에 남은 공간이 없다
    STORAGE_FULL = "storage_full", false;
    /// 저장 풀이 없다(`pool`)
    POOL_NOT_FOUND = "pool_not_found", false;
    /// 저장 풀이 이 노드에서 준비되지 않았다(마운트 안 됨·목록에 없음, `pool`, `node`)
    POOL_NOT_READY = "pool_not_ready", true;
    /// 풀의 남은 공간이 모자란다(`pool`, `node`, `need_bytes`, `free_bytes`)
    POOL_FULL = "pool_full", false;
    /// 대상(VM·볼륨)이 Task 도중 지워졌다
    TARGET_DELETED = "target_deleted", false;
    /// 대상이 다른 작업 중이다(`name`)
    TARGET_BUSY = "target_busy", true;
    /// VM에 연결된 볼륨이다(`volume`, `vm`)
    VOLUME_IN_USE = "volume_in_use", false;
    // VM
    /// QEMU를 켜지 못했다(`vm`)
    QEMU_START_FAILED = "qemu_start_failed", false;
    /// VM이 켜져 있어야 하는데 꺼져 있다(`vm`)
    VM_NOT_RUNNING = "vm_not_running", false;
    // 마이그레이션
    /// 이 노드의 QEMU가 마이그레이션 암호화(TLS)를 지원하지 않는다
    MIGRATION_TLS_MISSING = "migration_tls_missing", false;
    /// 대상 노드가 VM을 받을 준비를 하지 못했다(`vm`, `node`)
    MIGRATION_TARGET_REFUSED = "migration_target_refused", true;
    /// 대상 노드에 받는 데 쓸 빈 포트가 없다
    MIGRATION_NO_PORT = "migration_no_port", true;
    /// 디스크를 대상 노드와 함께 열지 못했다(공유 스토리지 잠금, `vm`, `node`)
    MIGRATION_STORAGE_NOT_SHARED = "migration_storage_not_shared", true;
    /// 옮기는 동안 VM이 꺼졌다(`vm`)
    MIGRATION_VM_STOPPED = "migration_vm_stopped", false;
    // 백업·복원
    /// 백업 저장소에 닿지 못했다(`store`)
    BACKUP_STORE_UNAVAILABLE = "backup_store_unavailable", true;
    /// 백업 저장소가 접근을 거절했다(자격 증명·버킷 정책, `store`)
    BACKUP_STORE_DENIED = "backup_store_denied", false;
    /// 백업 저장소의 데이터가 깨졌다(`store`)
    BACKUP_CORRUPT = "backup_corrupt", false;
    /// 암호 키가 저장소와 맞지 않는다(`store`)
    BACKUP_KEY_MISMATCH = "backup_key_mismatch", false;
    /// 복원할 VM 이름을 이미 쓴다(`name`)
    RESTORE_NAME_TAKEN = "restore_name_taken", false;
    // 받기·가져오기
    /// 받은 파일의 체크섬이 맞지 않는다
    UPLOAD_CHECKSUM = "upload_checksum", true;
    /// 주소에서 받지 못했다(HTTP 오류·연결 실패, `url`)
    DOWNLOAD_FAILED = "download_failed", true;
    /// 쓸 수 없는 이미지 형식이다(ISO가 아님, 기술자만 있는 VMDK, 기반 파일이 있는 qcow2)
    IMAGE_UNSUPPORTED = "image_unsupported", false;
    // 선언형 적용
    /// 적용 한 단계가 실패했다(`step`, 원인은 원문)
    APPLY_STEP_FAILED = "apply_step_failed", false;
    /// 계획을 만든 뒤 대상이 바뀌었다(`step`)
    APPLY_CONFLICT = "apply_conflict", false;
    /// 계획이 사라졌다(시간이 지났거나 노드가 다시 시작함)
    APPLY_PLAN_EXPIRED = "apply_plan_expired", false;
    // VMware
    /// vCenter·ESXi에 닿지 못했다(`host`)
    VMWARE_UNREACHABLE = "vmware_unreachable", true;
    /// vCenter·ESXi가 로그인을 거절했다(`host`)
    VMWARE_AUTH = "vmware_auth", false;
    // SDN·Ceph
    /// 꾸러미(dnf·apt) 설치가 실패했다(`node`)
    PACKAGE_INSTALL_FAILED = "package_install_failed", true;
    /// Ceph 모니터가 정족수를 이루지 못했다(`node`)
    CEPH_NO_QUORUM = "ceph_no_quorum", true;
    /// SDN(OVN)이 아직 준비되지 않았다
    SDN_NOT_READY = "sdn_not_ready", true;
    // HA
    /// 노드가 멈춰 HA가 VM을 다른 노드에서 다시 켰다(`node`)
    HA_NODE_FAILED = "ha_node_failed", false;
}

/// `code`의 다시 하기 기본값(모르는 코드는 `false`)
pub fn retryable(code: &str) -> bool {
    CODES.iter().any(|(c, r)| *c == code && *r)
}

/// 다른 프로그램(QEMU·커널)이 낸 원문에서 원인을 알아본다. 우리 코드가 붙인 코드보다 구체적이므로 먼저 본다.
pub fn classify(error: &str) -> Option<TaskFailure> {
    if error.contains("TLS credentials support requires") {
        return Some(TaskFailure::new(code::MIGRATION_TLS_MISSING));
    }
    if error.contains("no free migration port") {
        return Some(TaskFailure::new(code::MIGRATION_NO_PORT));
    }
    if error.contains("No space left on device") || error.contains("ENOSPC") {
        return Some(TaskFailure::new(code::STORAGE_FULL));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case() {
        let mut seen = std::collections::HashSet::new();
        for (c, _) in CODES {
            assert!(seen.insert(*c), "duplicate {c}");
            assert!(
                c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{c}"
            );
        }
        assert!(CODES.len() >= 20);
    }

    #[test]
    fn failure_omits_empty_fields_and_reads_back() {
        let f = TaskFailure::new(code::POOL_FULL)
            .param("pool", "local")
            .param("need", "20 GiB");
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"code": "pool_full", "params": {"need": "20 GiB", "pool": "local"}})
        );
        assert_eq!(serde_json::from_value::<TaskFailure>(json).unwrap(), f);
        let busy = TaskFailure::new(code::TARGET_BUSY).step("copying-disks");
        assert_eq!(
            serde_json::to_value(&busy).unwrap(),
            serde_json::json!({"code": "target_busy", "step": "copying-disks", "retryable": true})
        );
    }

    #[test]
    fn classifies_foreign_messages() {
        let tls = "the target node cannot receive the VM: QMP command failed: GenericError: TLS credentials support requires GNUTLS";
        assert_eq!(classify(tls).unwrap().code, code::MIGRATION_TLS_MISSING);
        assert_eq!(
            classify("qemu-img: write failed: No space left on device")
                .unwrap()
                .code,
            code::STORAGE_FULL
        );
        assert!(classify("something else").is_none());
        assert!(!retryable("no_such_code"));
    }
}
