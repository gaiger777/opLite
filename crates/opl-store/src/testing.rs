//! 시험 도움(기능 `test-util`): 디스크 이미지 바이트를 저장소 백업으로 쓴다(4 MiB 청크, 0 청크는 저장하지 않음).
//! 백업 Task의 파이프라인(스테이지 4) 없이 헬퍼·복원 시험이 백업을 만든다.

use opl_types::backup_store::CHUNK_BYTES;
use uuid::Uuid;

use crate::backend::StoreError;
use crate::format::chunk::is_zero;
use crate::format::index::FixedIndex;
use crate::format::manifest::{CONFIG_BLOB, ConfigBlob, Manifest};
use crate::store::Store;

/// 디스크마다 `disks[i]`(가상 크기 = 길이)를 `disk{i}`로 쓰고 매니페스트까지 커밋한다.
pub async fn write_backup(
    store: &Store,
    vm: Uuid,
    backup: Uuid,
    disks: &[Vec<u8>],
) -> Result<Manifest, StoreError> {
    let config = ConfigBlob {
        vm_name: "test".into(),
        vm_spec: serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt"
        }))
        .expect("vm spec"),
        uefi_vars: false,
        tpm_state: false,
    };
    write_backup_with(store, vm, backup, disks, &config, 1).await
}

/// [`write_backup`]에 `config` 블롭과 만든 시각을 준다(API 시험이 백업 리소스와 맞춘다).
pub async fn write_backup_with(
    store: &Store,
    vm: Uuid,
    backup: Uuid,
    disks: &[Vec<u8>],
    config: &ConfigBlob,
    created_at: u64,
) -> Result<Manifest, StoreError> {
    let cb = CHUNK_BYTES as usize;
    let mut entries = Vec::new();
    for (i, image) in disks.iter().enumerate() {
        let mut ix = FixedIndex::new(backup, image.len() as u64, cb as u32, store.encrypted());
        for (n, raw) in image.chunks(cb).enumerate() {
            if is_zero(raw) {
                continue;
            }
            let c = store.encode_chunk(raw);
            if !store.has_chunk(&c.id).await? {
                store.put_chunk(&c).await?;
            }
            ix.ids[n] = c.id;
        }
        entries.push(
            store
                .put_index(vm, backup, &format!("disk{i}"), &ix)
                .await?,
        );
    }
    let blob = store
        .put_blob(
            vm,
            backup,
            CONFIG_BLOB,
            &serde_json::to_vec(config).expect("config"),
        )
        .await?;
    let mut m = store.new_manifest(vm, backup, created_at, entries, vec![blob]);
    store.put_manifest(&mut m).await?;
    Ok(m)
}
