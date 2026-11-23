//! 열린 저장소 하나(백엔드 + 키 + `store.json`). 청크·블롭·인덱스·매니페스트를 형식대로 쓰고 읽는다(설계 §7.4 쓰는
//! 차례: 청크 → 블롭 → 인덱스 → 매니페스트). 병렬·파이프라인은 부르는 쪽(백업 Task)이 한다.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::{StreamExt, TryStreamExt};
use opl_types::backup_store::{ChunkId, ZERO_CHUNK};
use uuid::Uuid;

use crate::backend::{Backend, ObjectInfo, StoreError, list_all};
use crate::crypto::{EscrowFile, MasterKey, StoreKeys, escrow_key, escrow_unwrap, escrow_wrap};
use crate::format::chunk::{self, EncodedChunk, sha256};
use crate::format::index::FixedIndex;
use crate::format::manifest::{MANIFEST_NAME, Manifest, ManifestBlob, ManifestDisk, VERSION};
use crate::format::{
    OWNER_FILE, OwnerFile, STORE_FILE, StoreInfo, chunk_key, parse_snapshot_key, snapshot_prefix,
};

/// 저장소 안 스냅샷 하나의 파일들(나열에서)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotFiles {
    pub vm: Uuid,
    pub backup: Uuid,
    /// 매니페스트가 있는가(커밋됨)
    pub complete: bool,
    /// `.fidx` 객체들(임시 파일 빼고)
    pub indexes: Vec<ObjectInfo>,
    /// 모든 객체 이름
    pub keys: Vec<String>,
}

/// 주인 표식을 쓰려 한 결과(설계 §12.6)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnerClaim {
    /// 새로 썼다
    Claimed,
    /// 이미 이 클러스터(같은 CA 지문·저장소 리소스)가 주인
    Ours,
    /// 다른 주인
    Other(OwnerFile),
}

pub struct Store {
    backend: Arc<dyn Backend>,
    keys: Option<Arc<StoreKeys>>,
    info: StoreInfo,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("backend", &self.backend.describe())
            .field("id", &self.info.id)
            .field("encrypted", &self.keys.is_some())
            .finish()
    }
}

/// `store.json`을 읽는다(없으면 `None`).
pub async fn read_info(backend: &dyn Backend) -> Result<Option<StoreInfo>, StoreError> {
    match backend.get(STORE_FILE).await {
        Ok(b) => Ok(Some(StoreInfo::from_bytes(&b)?)),
        Err(StoreError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// 저장소 안 escrow를 암호 문구로 풀어 주 키를 얻는다(클러스터를 잃었을 때 — 설계 §9.4).
pub async fn unlock_escrow(
    backend: &dyn Backend,
    passphrase: &str,
) -> Result<MasterKey, StoreError> {
    let info = read_info(backend)
        .await?
        .ok_or_else(|| StoreError::NotFound(STORE_FILE.into()))?;
    let fp = info
        .fingerprint()
        .ok_or_else(|| StoreError::Invalid("the store is not encrypted".into()))?;
    let raw = backend.get(&escrow_key(fp)).await?;
    let file: EscrowFile = serde_json::from_slice(&raw)
        .map_err(|e| StoreError::Corrupt(format!("escrowed key file: {e}")))?;
    Ok(escrow_unwrap(&file, passphrase)?)
}

impl Store {
    /// 새 저장소를 만든다: 백엔드 준비, `store.json`(없을 때만), 곧바로 다시 읽어 견줌(강한 읽은-뒤-쓰기 확인).
    /// 이미 같은 id의 저장소면 그대로 연다(이어 하기). 다른 저장소가 있으면 오류.
    pub async fn init(
        backend: Arc<dyn Backend>,
        id: Uuid,
        key: Option<&MasterKey>,
        now: u64,
    ) -> Result<Store, StoreError> {
        backend.prepare().await?;
        let keys = key.map(|k| Arc::new(StoreKeys::new(k)));
        let info = StoreInfo::new(id, keys.as_ref().map(|k| k.fingerprint()), now);
        let bytes = serde_json::to_vec_pretty(&info).expect("store.json serializes");
        backend.put_if_absent(STORE_FILE, bytes).await?;
        let found = read_info(backend.as_ref()).await?.ok_or_else(|| {
            StoreError::Invalid(
                "store.json is not readable right after writing it (the object store must be strongly consistent)"
                    .into(),
            )
        })?;
        if found.id != id {
            return Err(StoreError::Invalid(format!(
                "the location already holds another backup store ({})",
                found.id
            )));
        }
        Self::with_info(backend, found, keys)
    }

    /// 있는 저장소를 연다. 암호화 저장소면 키가 있어야 하고 지문이 같아야 한다.
    pub async fn open(
        backend: Arc<dyn Backend>,
        key: Option<&MasterKey>,
    ) -> Result<Store, StoreError> {
        let info = read_info(backend.as_ref()).await?.ok_or_else(|| {
            StoreError::NotFound(format!("{STORE_FILE} ({})", backend.describe()))
        })?;
        Self::with_info(backend, info, key.map(|k| Arc::new(StoreKeys::new(k))))
    }

    fn with_info(
        backend: Arc<dyn Backend>,
        info: StoreInfo,
        keys: Option<Arc<StoreKeys>>,
    ) -> Result<Store, StoreError> {
        match (info.fingerprint(), &keys) {
            (Some(fp), Some(k)) => k.check_fingerprint(fp)?,
            (Some(_), None) => {
                return Err(StoreError::KeyMismatch(
                    "the store is encrypted; its key is required".into(),
                ));
            }
            (None, Some(_)) => {
                return Err(StoreError::KeyMismatch("the store is not encrypted".into()));
            }
            (None, None) => {}
        }
        Ok(Store {
            backend,
            keys,
            info,
        })
    }

    pub fn info(&self) -> &StoreInfo {
        &self.info
    }

    pub fn id(&self) -> Uuid {
        self.info.id
    }

    pub fn encrypted(&self) -> bool {
        self.keys.is_some()
    }

    pub fn keys(&self) -> Option<&StoreKeys> {
        self.keys.as_deref()
    }

    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    pub fn backend_arc(&self) -> Arc<dyn Backend> {
        self.backend.clone()
    }

    pub fn max_streams(&self) -> usize {
        self.backend.max_streams().max(1)
    }

    // ---- 청크 ----

    pub fn chunk_id(&self, raw: &[u8]) -> ChunkId {
        chunk::chunk_id(raw, self.keys())
    }

    /// 청크를 인코딩한다(0 청크는 부르는 쪽이 [`chunk::is_zero`]로 걸러 저장하지 않는다).
    pub fn encode_chunk(&self, raw: &[u8]) -> EncodedChunk {
        chunk::encode(raw, self.keys())
    }

    pub async fn has_chunk(&self, id: &ChunkId) -> Result<bool, StoreError> {
        Ok(self.backend.head(&chunk_key(id)).await?.is_some())
    }

    pub async fn put_chunk(&self, c: &EncodedChunk) -> Result<(), StoreError> {
        self.backend.put(&chunk_key(&c.id), c.data.clone()).await
    }

    /// 저장된 청크 객체를 그대로(복제·복사 — 같은 키면 풀지 않고 옮긴다)
    pub async fn get_chunk_object(&self, id: &ChunkId) -> Result<Vec<u8>, StoreError> {
        self.backend.get(&chunk_key(id)).await
    }

    /// 청크를 읽어 풀고 id를 맞춰 본다. 0 청크면 `len` 바이트의 0.
    pub async fn get_chunk(&self, id: &ChunkId, len: usize) -> Result<Vec<u8>, StoreError> {
        if *id == ZERO_CHUNK {
            return Ok(vec![0; len]);
        }
        let obj = self.backend.get(&chunk_key(id)).await?;
        let raw = chunk::decode(&obj, id, self.keys())
            .map_err(|e| StoreError::Corrupt(format!("chunk {}: {e}", hex::encode(id))))?;
        if raw.len() != len {
            return Err(StoreError::Corrupt(format!(
                "chunk {}: length {} does not match the index ({len})",
                hex::encode(id),
                raw.len()
            )));
        }
        Ok(raw)
    }

    /// 주어진 id 가운데 저장소에 없는 것(커밋 확인 — 설계 §10.3 (다))
    pub async fn missing_chunks(&self, ids: &[ChunkId]) -> Result<Vec<ChunkId>, StoreError> {
        let found: Vec<(ChunkId, bool)> = futures_util::stream::iter(ids.iter().copied())
            .map(|id| async move { Ok::<_, StoreError>((id, self.has_chunk(&id).await?)) })
            .buffered(self.max_streams())
            .try_collect()
            .await?;
        Ok(found
            .into_iter()
            .filter(|(_, has)| !has)
            .map(|(id, _)| id)
            .collect())
    }

    // ---- 스냅샷 ----

    fn snap_key(vm: Uuid, backup: Uuid, name: &str) -> String {
        format!("{}{name}", snapshot_prefix(vm, backup))
    }

    /// 블롭을 쓴다(청크 다음, 인덱스 앞).
    pub async fn put_blob(
        &self,
        vm: Uuid,
        backup: Uuid,
        name: &str,
        raw: &[u8],
    ) -> Result<ManifestBlob, StoreError> {
        let obj = chunk::encode_blob(name, raw, self.keys());
        let sha = hex::encode(sha256(&obj));
        self.backend
            .put(&Self::snap_key(vm, backup, name), obj)
            .await?;
        Ok(ManifestBlob {
            name: name.into(),
            sha256: sha,
        })
    }

    /// 매니페스트에 적힌 블롭을 읽는다(SHA-256 확인 뒤 풂).
    pub async fn get_blob(&self, m: &Manifest, name: &str) -> Result<Vec<u8>, StoreError> {
        let entry = m
            .blob(name)
            .ok_or_else(|| StoreError::NotFound(format!("blob {name} of backup {}", m.backup)))?;
        let obj = self
            .backend
            .get(&Self::snap_key(m.vm, m.backup, name))
            .await?;
        if hex::encode(sha256(&obj)) != entry.sha256 {
            return Err(StoreError::Corrupt(format!(
                "blob {name}: checksum does not match"
            )));
        }
        chunk::decode_blob(name, &obj, self.keys())
            .map_err(|e| StoreError::Corrupt(format!("blob {name}: {e}")))
    }

    /// 인덱스를 쓰고 매니페스트의 디스크 칸을 돌려준다. `name`은 `disk{i}`.
    pub async fn put_index(
        &self,
        vm: Uuid,
        backup: Uuid,
        name: &str,
        index: &FixedIndex,
    ) -> Result<ManifestDisk, StoreError> {
        let bytes = index.to_bytes();
        let file = format!("{name}.fidx");
        let sha = hex::encode(sha256(&bytes));
        self.backend
            .put(&Self::snap_key(vm, backup, &file), bytes)
            .await?;
        let zero = index.ids.iter().filter(|id| **id == ZERO_CHUNK).count() as u64;
        Ok(ManifestDisk {
            name: name.into(),
            virtual_bytes: index.virtual_bytes,
            index: file,
            index_sha256: sha,
            chunks: index.ids.len() as u64,
            zero_chunks: zero,
        })
    }

    /// 매니페스트가 가리키는 인덱스를 읽는다(SHA-256·꼬리 해시·백업 id·크기 확인).
    pub async fn read_index(
        &self,
        m: &Manifest,
        disk: &ManifestDisk,
    ) -> Result<FixedIndex, StoreError> {
        let bytes = self
            .backend
            .get(&Self::snap_key(m.vm, m.backup, &disk.index))
            .await?;
        if hex::encode(sha256(&bytes)) != disk.index_sha256 {
            return Err(StoreError::Corrupt(format!(
                "index {}: checksum does not match",
                disk.index
            )));
        }
        let ix = FixedIndex::from_bytes(&bytes)?;
        if ix.backup != m.backup
            || ix.virtual_bytes != disk.virtual_bytes
            || ix.keyed != self.encrypted()
            || ix.chunk_bytes != self.info.chunk_bytes
        {
            return Err(StoreError::Corrupt(format!(
                "index {}: does not match the manifest",
                disk.index
            )));
        }
        Ok(ix)
    }

    /// 객체 이름으로 인덱스를 읽는다(매니페스트 없이 — GC 표시).
    pub async fn read_index_key(&self, key: &str) -> Result<FixedIndex, StoreError> {
        let bytes = self.backend.get(key).await?;
        FixedIndex::from_bytes(&bytes).map_err(|e| StoreError::Corrupt(format!("{key}: {e}")))
    }

    /// 새 매니페스트(블롭·디스크를 채운 뒤 [`Store::put_manifest`])
    pub fn new_manifest(
        &self,
        vm: Uuid,
        backup: Uuid,
        created_at: u64,
        disks: Vec<ManifestDisk>,
        blobs: Vec<ManifestBlob>,
    ) -> Manifest {
        Manifest {
            version: VERSION,
            store: self.info.id,
            backup,
            vm,
            created_at,
            disks,
            blobs,
            encryption: None,
            auth: None,
            extra: Default::default(),
        }
    }

    /// 매니페스트를 인증해 쓴다(커밋 지점).
    pub async fn put_manifest(&self, m: &mut Manifest) -> Result<(), StoreError> {
        m.seal(self.keys());
        self.backend
            .put(&Self::snap_key(m.vm, m.backup, MANIFEST_NAME), m.to_bytes())
            .await
    }

    /// 매니페스트를 읽고 인증한다. 다른 저장소에서 옮겨 온 매니페스트(저장소 id가 다름)는 사본으로 받으므로
    /// 저장소 id는 보지 않는다 — 백업·VM id와 경로가 맞는지만 본다.
    pub async fn read_manifest(&self, vm: Uuid, backup: Uuid) -> Result<Manifest, StoreError> {
        let bytes = self
            .backend
            .get(&Self::snap_key(vm, backup, MANIFEST_NAME))
            .await?;
        let m = Manifest::from_bytes(&bytes)?;
        m.verify(self.keys())?;
        if m.vm != vm || m.backup != backup {
            return Err(StoreError::Corrupt(format!(
                "manifest of {vm}/{backup} names another backup"
            )));
        }
        Ok(m)
    }

    /// `snapshots/` 아래 스냅샷들
    pub async fn list_snapshots(&self) -> Result<Vec<SnapshotFiles>, StoreError> {
        let mut by: BTreeMap<(Uuid, Uuid), SnapshotFiles> = BTreeMap::new();
        for o in list_all(self.backend.as_ref(), "snapshots/").await? {
            let Some((vm, backup, name)) = parse_snapshot_key(&o.key) else {
                continue;
            };
            let name = name.to_string();
            let s = by.entry((vm, backup)).or_insert_with(|| SnapshotFiles {
                vm,
                backup,
                complete: false,
                indexes: Vec::new(),
                keys: Vec::new(),
            });
            if name == MANIFEST_NAME {
                s.complete = true;
            }
            if name.ends_with(".fidx") && !name.contains(".tmp.") {
                s.indexes.push(o.clone());
            }
            s.keys.push(o.key);
        }
        Ok(by.into_values().collect())
    }

    /// 스냅샷을 지운다: 매니페스트 → 인덱스·블롭(설계 §10.1). 청크는 GC가 거둔다.
    pub async fn delete_snapshot(&self, vm: Uuid, backup: Uuid) -> Result<(), StoreError> {
        let prefix = snapshot_prefix(vm, backup);
        self.backend
            .delete(&format!("{prefix}{MANIFEST_NAME}"))
            .await?;
        for o in list_all(self.backend.as_ref(), &prefix).await? {
            self.backend.delete(&o.key).await?;
        }
        Ok(())
    }

    // ---- 주인·escrow ----

    pub async fn read_owner(&self) -> Result<Option<OwnerFile>, StoreError> {
        match self.backend.get(OWNER_FILE).await {
            Ok(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(|e| StoreError::Corrupt(format!("owner.json: {e}"))),
            Err(StoreError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 주인 표식을 없을 때만 쓰고 다시 읽어 견준다.
    pub async fn claim_owner(&self, owner: &OwnerFile) -> Result<OwnerClaim, StoreError> {
        let bytes = serde_json::to_vec_pretty(owner).expect("owner.json serializes");
        let wrote = self.backend.put_if_absent(OWNER_FILE, bytes).await?;
        let found = self.read_owner().await?.ok_or_else(|| {
            StoreError::Invalid("owner.json vanished right after writing it".into())
        })?;
        let same = found.ca_fingerprint == owner.ca_fingerprint
            && found.store_resource == owner.store_resource;
        Ok(match (wrote, same) {
            (true, true) => OwnerClaim::Claimed,
            (_, true) => OwnerClaim::Ours,
            (_, false) => OwnerClaim::Other(found),
        })
    }

    /// 주인 표식을 덮어쓴다(하트비트·넘겨받기).
    pub async fn write_owner(&self, owner: &OwnerFile) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(owner).expect("owner.json serializes");
        self.backend.put(OWNER_FILE, bytes).await
    }

    /// 주 키를 암호 문구로 감싸 `keys/<지문>.json`에 쓴다(덮어씀).
    pub async fn write_escrow(&self, mk: &MasterKey, passphrase: &str) -> Result<(), StoreError> {
        let fp = self
            .info
            .fingerprint()
            .ok_or_else(|| StoreError::Invalid("the store is not encrypted".into()))?;
        StoreKeys::new(mk).check_fingerprint(fp)?;
        let file = escrow_wrap(mk, passphrase)?;
        self.write_escrow_file(&file).await
    }

    /// 감싼 파일을 쓴다(비용을 고른 시험용).
    pub async fn write_escrow_file(&self, file: &EscrowFile) -> Result<(), StoreError> {
        if Some(file.fingerprint.as_str()) != self.info.fingerprint() {
            return Err(StoreError::KeyMismatch(
                "escrowed key is for another store".into(),
            ));
        }
        let bytes = serde_json::to_vec_pretty(file).expect("escrow serializes");
        self.backend
            .put(&escrow_key(&file.fingerprint), bytes)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mem::MemBackend;
    use crate::crypto::escrow_wrap_with;
    use crate::format::manifest::CONFIG_BLOB;
    use opl_types::backup_store::CHUNK_BYTES;

    fn mem() -> Arc<MemBackend> {
        Arc::new(MemBackend::new())
    }

    #[tokio::test]
    async fn init_open_and_keys() {
        let b = mem();
        let id = Uuid::from_u128(7);
        let mk = MasterKey::generate();
        let s = Store::init(b.clone(), id, Some(&mk), 100).await.unwrap();
        assert!(s.encrypted());
        assert_eq!(s.info().fingerprint(), s.keys().map(|k| k.fingerprint()));
        // 다시 init(이어 하기) 됨, 다른 id는 거절
        Store::init(b.clone(), id, Some(&mk), 200).await.unwrap();
        assert!(matches!(
            Store::init(b.clone(), Uuid::from_u128(8), Some(&mk), 1).await,
            Err(StoreError::Invalid(_))
        ));
        // 열기: 키 없음·다른 키 거절
        Store::open(b.clone(), Some(&mk)).await.unwrap();
        assert!(matches!(
            Store::open(b.clone(), None).await,
            Err(StoreError::KeyMismatch(_))
        ));
        assert!(matches!(
            Store::open(b.clone(), Some(&MasterKey::generate())).await,
            Err(StoreError::KeyMismatch(_))
        ));
        // 평문 저장소에 키를 주면 거절
        let p = mem();
        Store::init(p.clone(), id, None, 1).await.unwrap();
        assert!(matches!(
            Store::open(p.clone(), Some(&mk)).await,
            Err(StoreError::KeyMismatch(_))
        ));
        assert!(matches!(
            Store::open(mem(), None).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn snapshot_write_read_delete() {
        for enc in [false, true] {
            let b = mem();
            let mk = MasterKey::generate();
            let s = Store::init(b.clone(), Uuid::from_u128(1), enc.then_some(&mk), 1)
                .await
                .unwrap();
            let (vm, backup) = (Uuid::from_u128(10), Uuid::from_u128(11));
            let raw: Vec<u8> = (0..CHUNK_BYTES as usize).map(|i| (i % 251) as u8).collect();
            let c = s.encode_chunk(&raw);
            assert!(!s.has_chunk(&c.id).await.unwrap());
            s.put_chunk(&c).await.unwrap();
            assert!(s.has_chunk(&c.id).await.unwrap());
            assert_eq!(
                s.missing_chunks(&[c.id, [9; 32]]).await.unwrap(),
                vec![[9; 32]]
            );
            let blob = s
                .put_blob(vm, backup, CONFIG_BLOB, b"{\"x\":1}")
                .await
                .unwrap();
            let mut ix = FixedIndex::new(backup, CHUNK_BYTES * 2 + 10, CHUNK_BYTES as u32, enc);
            ix.ids[0] = c.id;
            let disk = s.put_index(vm, backup, "disk0", &ix).await.unwrap();
            assert_eq!((disk.chunks, disk.zero_chunks), (3, 2));
            // 매니페스트 전에는 완전하지 않다
            let snaps = s.list_snapshots().await.unwrap();
            assert_eq!(snaps.len(), 1);
            assert!(!snaps[0].complete);
            assert_eq!(snaps[0].indexes.len(), 1);
            let mut m = s.new_manifest(vm, backup, 5, vec![disk.clone()], vec![blob]);
            s.put_manifest(&mut m).await.unwrap();
            assert_eq!(m.auth.is_some(), enc);
            let back = s.read_manifest(vm, backup).await.unwrap();
            assert_eq!(back, m);
            assert!(s.list_snapshots().await.unwrap()[0].complete);
            assert_eq!(s.get_blob(&back, CONFIG_BLOB).await.unwrap(), b"{\"x\":1}");
            let ix2 = s.read_index(&back, &back.disks[0]).await.unwrap();
            assert_eq!(ix2, ix);
            assert_eq!(
                s.get_chunk(&ix2.ids[0], CHUNK_BYTES as usize)
                    .await
                    .unwrap(),
                raw
            );
            assert_eq!(s.get_chunk(&ix2.ids[2], 10).await.unwrap(), vec![0; 10]);
            assert!(matches!(
                s.get_chunk(&ix2.ids[0], 5).await,
                Err(StoreError::Corrupt(_))
            ));
            // 블롭 망가뜨림 → SHA-256이 잡는다
            let bk = format!("{}config", snapshot_prefix(vm, backup));
            b.tamper(&bk, |d| {
                let n = d.len();
                d[n - 1] ^= 1
            });
            assert!(matches!(
                s.get_blob(&back, CONFIG_BLOB).await,
                Err(StoreError::Corrupt(_))
            ));
            // 지우기: 매니페스트부터, 청크는 남는다
            s.delete_snapshot(vm, backup).await.unwrap();
            assert!(s.list_snapshots().await.unwrap().is_empty());
            assert!(s.has_chunk(&c.id).await.unwrap());
        }
    }

    #[tokio::test]
    async fn owner_claims() {
        let s = Store::init(mem(), Uuid::from_u128(1), None, 1)
            .await
            .unwrap();
        let me = OwnerFile {
            cluster: "prod".into(),
            ca_fingerprint: "sha256:aa".into(),
            store_resource: Uuid::from_u128(1),
            heartbeat: 10,
        };
        assert_eq!(s.claim_owner(&me).await.unwrap(), OwnerClaim::Claimed);
        assert_eq!(s.claim_owner(&me).await.unwrap(), OwnerClaim::Ours);
        let other = OwnerFile {
            cluster: "dr".into(),
            ca_fingerprint: "sha256:bb".into(),
            ..me.clone()
        };
        assert_eq!(
            s.claim_owner(&other).await.unwrap(),
            OwnerClaim::Other(me.clone())
        );
        s.write_owner(&other).await.unwrap();
        assert_eq!(s.read_owner().await.unwrap(), Some(other));
    }

    #[tokio::test]
    async fn escrow_in_store() {
        let b = mem();
        let mk = MasterKey::generate();
        let s = Store::init(b.clone(), Uuid::from_u128(1), Some(&mk), 1)
            .await
            .unwrap();
        let f = escrow_wrap_with(&mk, "pass phrase", 64, 1, 1).unwrap();
        s.write_escrow_file(&f).await.unwrap();
        assert_eq!(unlock_escrow(b.as_ref(), "pass phrase").await.unwrap(), mk);
        assert!(unlock_escrow(b.as_ref(), "nope").await.is_err());
        // 다른 키의 escrow는 거절
        let g = escrow_wrap_with(&MasterKey::generate(), "x", 64, 1, 1).unwrap();
        assert!(matches!(
            s.write_escrow_file(&g).await,
            Err(StoreError::KeyMismatch(_))
        ));
        let p = Store::init(mem(), Uuid::from_u128(2), None, 1)
            .await
            .unwrap();
        assert!(p.write_escrow(&mk, "x").await.is_err());
    }
}
