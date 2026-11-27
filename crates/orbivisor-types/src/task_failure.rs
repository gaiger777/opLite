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
    ($($(#[doc = $doc:literal])* $name:ident = $code:literal, $retry:literal, [$($param:literal),*];)*) => {
        /// 원인 코드 상수
        pub mod code {
            $($(#[doc = $doc])* pub const $name: &str = $code;)*
        }
        /// 모든 원인 코드, 다시 하기 기본값, 서버가 다는 값 이름(웹 사전 시험이 읽는다 — 사전의 `{{이름}}`은 이 안에서만)
        pub const CODES: &[(&str, bool, &[&str])] = &[$(($code, $retry, &[$($param),*]),)*];
    };
}

// 한 줄에 하나: `상수 = "코드", 다시 하기, ["값", …];` (웹 시험이 이 모양을 읽고 줄 수를 센다)
codes! {
    // 공통
    /// 실행 시간 한도를 넘겼다
    TASK_TIMEOUT = "task_timeout", true, [];
    /// 도는 동안 orbivisord가 다시 시작했다
    TASK_INTERRUPTED = "task_interrupted", true, [];
    /// Task를 실행할 노드가 응답하지 않아 정리 작업이 실패로 끝냈다
    NODE_OFFLINE = "node_offline", true, ["node"];
    /// 마이그레이션 대상 노드가 클러스터 멤버가 아니다
    NODE_NOT_MEMBER = "node_not_member", false, ["node"];
    /// Task를 맡은 노드가 클러스터에서 제거되어 정리 작업이 실패로 끝냈다(이름을 모르면 id)
    NODE_REMOVED = "node_removed", false, ["node"];
    /// 이 노드가 다른 노드에 닿을 수 없다(노드 사이 연결 설정 없음)
    NODE_NO_PEERS = "node_no_peers", false, [];
    /// 저장 풀이 없다
    POOL_NOT_FOUND = "pool_not_found", false, ["pool"];
    /// 저장 풀의 노드 목록에 이 노드가 없다(그 노드에서 쓰도록 설정되지 않음)
    POOL_NOT_ON_NODE = "pool_not_on_node", false, ["pool", "node"];
    /// 저장 풀이 이 노드에서 준비되지 않았다(마운트 안 됨·응답 없음 등, 까닭은 원문)
    POOL_NOT_READY = "pool_not_ready", true, ["pool", "node"];
    /// 풀의 남은 공간이 모자란다(쓰기 전에 잰 값)
    POOL_FULL = "pool_full", false, ["pool", "node", "need_bytes", "free_bytes"];
    /// 풀에 쓰는 도중 공간이 바닥났다(ENOSPC — 볼륨 가져오기)
    POOL_OUT_OF_SPACE = "pool_out_of_space", false, ["pool", "node"];
    /// 대상(VM·볼륨)이 Task 도중 지워졌다
    TARGET_DELETED = "target_deleted", false, [];
    /// 대상이 다른 작업 중이다
    TARGET_BUSY = "target_busy", true, ["name"];
    /// VM에 연결된 볼륨이다
    VOLUME_IN_USE = "volume_in_use", false, ["volume", "vm"];
    // VM
    /// QEMU 프로세스가 켜자마자 끝났다(`failed to start: QEMU failed to start: <QEMU stderr>`)
    QEMU_START_FAILED = "qemu_start_failed", false, ["vm"];
    /// 보안 그룹을 건 NIC가 있는데 노드에 방화벽(nftables)이 없다
    VM_NO_FIREWALL = "vm_no_firewall", false, ["vm"];
    /// 켜기 전에 넣을 방화벽 규칙(nftables)을 넣지 못해 몇 번 시도한 뒤 포기했다
    VM_FIREWALL_FAILED = "vm_firewall_failed", false, ["vm"];
    /// VM이 켜져 있어야 하는데 꺼져 있다
    VM_NOT_RUNNING = "vm_not_running", false, ["vm"];
    // 마이그레이션
    /// 보내는 노드(VM이 도는 QEMU)가 마이그레이션 암호화(TLS)를 지원하지 않는다
    MIGRATION_TLS_MISSING = "migration_tls_missing", false, ["node"];
    /// 받는 노드의 QEMU가 마이그레이션 암호화(TLS)를 지원하지 않는다
    MIGRATION_TARGET_TLS_MISSING = "migration_target_tls_missing", false, ["node"];
    /// 대상 노드가 VM을 받을 준비를 하지 못했다(까닭은 원문)
    MIGRATION_TARGET_REFUSED = "migration_target_refused", false, ["vm", "node"];
    /// 대상 노드의 받는 포트가 모두 쓰는 중이다
    MIGRATION_NO_PORT = "migration_no_port", true, ["node", "ports"];
    /// 대상 노드가 받는 주소에서 포트를 열지 못했다(그 노드의 주소가 아님 등)
    MIGRATION_LISTEN_FAILED = "migration_listen_failed", false, ["node", "host"];
    /// 디스크를 대상 노드와 함께 열지 못했다(공유 스토리지 잠금)
    MIGRATION_STORAGE_NOT_SHARED = "migration_storage_not_shared", true, ["vm", "node"];
    /// 옮기는 동안 VM이 꺼졌다
    MIGRATION_VM_STOPPED = "migration_vm_stopped", false, ["vm"];
    // 백업·복원
    /// 백업을 담는 풀에 쓰는 도중 공간이 바닥났다(ENOSPC)
    BACKUP_POOL_FULL = "backup_pool_full", false, ["pool", "node"];
    /// 백업 저장소에 닿지 못했다
    BACKUP_STORE_UNAVAILABLE = "backup_store_unavailable", true, ["store"];
    /// 백업 저장소가 접근을 거절했다(자격 증명·버킷 정책)
    BACKUP_STORE_DENIED = "backup_store_denied", false, ["store"];
    /// 백업 저장소(디렉터리)에 공간이 없다(ENOSPC)
    BACKUP_STORE_FULL = "backup_store_full", false, ["store"];
    /// 백업 저장소의 데이터가 깨졌다
    BACKUP_CORRUPT = "backup_corrupt", false, ["store"];
    /// 암호 키가 저장소와 맞지 않는다(없음·다름·암호화하지 않은 저장소에 키, 까닭은 원문)
    BACKUP_KEY_MISMATCH = "backup_key_mismatch", false, ["store"];
    /// 복원할 VM 이름을 이미 쓴다
    RESTORE_NAME_TAKEN = "restore_name_taken", false, ["name"];
    // 받기·가져오기
    /// 받은 파일의 체크섬이 맞지 않는다
    UPLOAD_CHECKSUM = "upload_checksum", true, [];
    /// 주소에서 받지 못했다(HTTP 오류·연결 실패)
    DOWNLOAD_FAILED = "download_failed", true, ["url"];
    /// 노드의 업로드 디렉터리에 받을 공간이 없다
    UPLOAD_DIR_FULL = "upload_dir_full", false, ["node", "dir", "need_bytes"];
    /// 쓸 수 없는 이미지 형식이다(ISO가 아님, 기술자 + -flat VMDK·다른 파일을 가리키는 VMDK, 기반 파일이 있는 qcow2)
    IMAGE_UNSUPPORTED = "image_unsupported", false, [];
    // 선언형 적용
    /// 적용 한 단계가 실패했다(`cause`는 서버 오류 코드, `reason`은 원문)
    APPLY_STEP_FAILED = "apply_step_failed", false, ["step", "action", "kind", "name", "cause", "reason"];
    /// 계획을 만든 뒤 대상이 바뀌었다
    APPLY_CONFLICT = "apply_conflict", false, ["step", "action", "kind", "name", "cause", "reason"];
    /// 계획이 사라졌다(시간이 지났거나 노드가 다시 시작함)
    APPLY_PLAN_EXPIRED = "apply_plan_expired", false, [];
    // VMware
    /// vCenter·ESXi에 닿지 못했다
    VMWARE_UNREACHABLE = "vmware_unreachable", true, [];
    /// vCenter·ESXi가 로그인을 거절했다
    VMWARE_AUTH = "vmware_auth", false, [];
    // SDN·Ceph
    /// 꾸러미(dnf·apt) 설치가 실패했다
    PACKAGE_INSTALL_FAILED = "package_install_failed", true, ["node"];
    /// Ceph 모니터가 정족수를 이루지 못했다
    CEPH_NO_QUORUM = "ceph_no_quorum", true, ["node"];
    /// SDN(OVN)이 아직 준비되지 않았다
    SDN_NOT_READY = "sdn_not_ready", true, [];
    // HA
    /// 노드가 멈춰 HA가 VM을 다른 노드에서 다시 켰다
    HA_NODE_FAILED = "ha_node_failed", false, ["node"];
}

/// `code`의 다시 하기 기본값(모르는 코드는 `false`)
pub fn retryable(code: &str) -> bool {
    CODES.iter().any(|(c, r, _)| *c == code && *r)
}

/// 원문이 디스크 공간 없음(ENOSPC)인지. 어느 디스크인지는 원문이 말하지 않으므로 쓰는 곳이 대상을 안다.
pub fn is_no_space(error: &str) -> bool {
    error.contains("No space left on device") || error.contains("ENOSPC")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_snake_case() {
        let mut seen = std::collections::HashSet::new();
        for (c, _, _) in CODES {
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
    fn knows_no_space_messages() {
        assert!(is_no_space(
            "qemu-img: write failed: No space left on device"
        ));
        assert!(!is_no_space("something else"));
        assert!(!retryable("no_such_code"));
        assert!(retryable(code::TASK_TIMEOUT));
    }
}
