//! 다른 클러스터가 보내는 백업 사본 받기(엔터프라이즈 설계 §6.1). 받기가 `receiving` 백업과 그
//! 디렉터리를 만들고, 파일마다 `PUT`으로 받아 SHA-256을 돌려주고, 완료가 매니페스트를 써서 `ready`로
//! 바꾼다. 파일은 API를 받은 노드가 직접 쓰므로 백업 풀은 이 노드에서 쓸 수 있어야 한다. 모두 백업
//! 쓰기 권한이다.

use std::collections::HashSet;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_driver::check_pool_root;
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupOrigin, BackupPhase, BackupSpec, BackupStatus,
    MANIFEST_VERSION, MANIFEST_VERSION_INCREMENTAL, backup_dir, default_backup_name,
};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::storage::{StoragePool, is_shared};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{backup_tasks, check_not_deleting, find_pool, node_name, now};
use crate::cluster::{require_current, require_feature};
use crate::error::{ApiError, Json};
use crate::resources::{check_name, lookup};
use crate::state::AppState;
use crate::tasks::backup::{VARS_FILE, sync_path, write_manifest};
use crate::volumes::{check_pool_on, content_length};

/// 받는 동안 이만큼 쓸 때마다 디스크에 내려 써서 끝의 내려 쓰기가 짧게 한다.
const SYNC_EVERY: u64 = 128 << 20;
/// `vars.fd`의 최대 크기. 디스크 파일과 달리 매니페스트에 크기가 없다.
const MAX_VARS_BYTES: u64 = 64 << 20;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiveRequest {
    /// 받는 쪽 백업 이름. 없으면 `{VM 이름}-{만든 시각}`이고, 겹치면 뒤에 `-2`, `-3`…을 붙인다.
    pub name: Option<String>,
    /// 받을 백업 풀. 이 노드에서 쓸 수 있어야 한다.
    pub pool: String,
    /// 원본 백업의 매니페스트(`manifest.json`)
    pub manifest: BackupManifest,
    pub origin: BackupOrigin,
    /// 증분 백업이면 받는 쪽(이 클러스터)에 있는 부모 사본의 id. 전체 백업이면 뺀다.
    #[serde(default)]
    pub parent: Option<Uuid>,
}

/// 받기 시작한 백업
#[derive(Debug, Serialize, ToSchema)]
pub struct ReceivingBackup {
    pub id: Uuid,
    pub name: String,
}

/// 받은 파일
#[derive(Debug, Serialize, ToSchema)]
pub struct ReceivedFile {
    /// 받은 내용의 SHA-256(16진수 소문자)
    pub sha256: String,
    pub size: u64,
}

/// 매니페스트 검사: 이 노드가 읽는 버전, 부모 사본(`parent`)이 없으면 버전 1에 `parent`가 없고 있으면
/// 버전 2에 `parent`가 있음, 디스크 수가 VM 스펙과 같고 디스크 `i`가 `disk{i}` 이름과 `disk{i}.qcow2`
/// 파일, VM 이름과 스펙이 올바름. 아니면 400.
fn check_manifest(m: &BackupManifest, parent: Option<Uuid>) -> Result<(), ApiError> {
    if ![MANIFEST_VERSION, MANIFEST_VERSION_INCREMENTAL].contains(&m.version) {
        return Err(ApiError::bad_request(format!(
            "backup manifest version {} is not supported (this node reads versions {MANIFEST_VERSION} and {MANIFEST_VERSION_INCREMENTAL})",
            m.version
        )));
    }
    if parent.is_some() {
        if m.version != MANIFEST_VERSION_INCREMENTAL || m.parent.is_none() {
            return Err(ApiError::bad_request(format!(
                "an incremental backup (sent with a parent copy) needs manifest version {MANIFEST_VERSION_INCREMENTAL} with a parent"
            )));
        }
    } else if m.version != MANIFEST_VERSION || m.parent.is_some() {
        return Err(ApiError::bad_request(format!(
            "a full backup (sent without a parent copy) needs manifest version {MANIFEST_VERSION} without a parent"
        )));
    }
    validate_name(&m.vm_name)
        .map_err(|e| ApiError::bad_request(format!("invalid VM name in manifest: {e}")))?;
    m.vm_spec
        .validate()
        .map_err(|e| ApiError::bad_request(format!("invalid VM spec in manifest: {e}")))?;
    if m.disks.len() != m.vm_spec.disks.len() {
        return Err(ApiError::bad_request(
            "manifest lists a different number of disks than its VM spec",
        ));
    }
    for (i, disk) in m.disks.iter().enumerate() {
        if disk.name != format!("disk{i}") || disk.file != format!("disk{i}.qcow2") {
            return Err(ApiError::bad_request(format!(
                "disk {i} of the manifest must be disk{i} in file disk{i}.qcow2"
            )));
        }
    }
    Ok(())
}

/// `base`가 쓰이지 않았으면 그것, 아니면 `{base}-2`, `{base}-3`… 가운데 처음 빈 이름. 이름 길이(63자)를
/// 넘지 않게 `base`를 줄인다(끝의 `-`는 뺀다). `base`는 올바른 이름이다.
fn free_name(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let prefix = base[..base.len().min(63 - suffix.len())].trim_end_matches('-');
            format!("{prefix}{suffix}")
        })
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// 이 노드에서 백업 풀 `pool`의 루트. 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
fn pool_root(state: &AppState, pool: &StoragePool) -> Result<PathBuf, ApiError> {
    check_pool_on(state, pool, state.node, || {
        format!(
            "storage pool {:?} is not available on node {:?} that received the request; register \
             this cluster as a remote with the URL of a node holding the pool, or use a shared \
             (NFS) backup pool",
            pool.name,
            node_name(state, state.node).unwrap_or_else(|_| state.node.to_string())
        )
    })?;
    check_pool_root(pool, &state.mount_root).map_err(|e| {
        ApiError::conflict(
            "pool_unavailable",
            format!("storage pool {:?}: {e}", pool.name),
        )
    })
}

/// 증분 백업의 부모 사본 `parent` 확인: 같은 풀에 `ready`이고, 같은 CA 지문의 원본에서 왔고 그 원본이
/// 매니페스트의 `parent`이고, 공유가 아닌 풀이면 파일이 이 노드에 있고, 지우는 중이 아니어야 한다. 아니면
/// 409 `replica_parent_missing`(`details.parent`가 그 id).
fn check_parent_copy(
    state: &AppState,
    backups: &[Backup],
    pool: &StoragePool,
    parent: Uuid,
    origin: &BackupOrigin,
    manifest_parent: Option<Uuid>,
) -> Result<(), ApiError> {
    let copy = backups.iter().find(|b| b.id == parent);
    let matches = match copy {
        Some(p) => {
            p.spec.pool == pool.name
                && p.status.phase == BackupPhase::Ready
                && p.spec.origin.as_ref().is_some_and(|o| {
                    o.ca_fingerprint == origin.ca_fingerprint && Some(o.backup) == manifest_parent
                })
                && (is_shared(pool) || p.status.node == Some(state.node))
                && !backup_tasks(state, p.id)?.contains(&"being deleted")
        }
        None => false,
    };
    if matches {
        return Ok(());
    }
    Err(parent_missing(parent))
}

/// 409 `replica_parent_missing`: 증분 백업의 부모 사본 `parent`가 없거나 맞지 않는다(`details.parent`).
fn parent_missing(parent: Uuid) -> ApiError {
    let mut e = ApiError::conflict(
        "replica_parent_missing",
        format!(
            "the parent copy {parent} of this incremental backup is missing or does not match; send the parent first"
        ),
    );
    e.details = Some(serde_json::json!({ "parent": parent }));
    e
}

/// 풀에 매니페스트의 디스크 파일을 받을 자리가 없으면 507 `insufficient_storage`
fn check_space(root: &FsPath, manifest: &BackupManifest) -> Result<(), ApiError> {
    let need = manifest
        .disks
        .iter()
        .fold(0u64, |sum, d| sum.saturating_add(d.stored_bytes));
    let available = fs4::available_space(root).map_err(ApiError::internal)?;
    if need > available {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!("the backup pool has {available} bytes free; the backup needs {need} bytes"),
        ));
    }
    Ok(())
}

/// 다른 클러스터의 백업 사본을 받기 시작한다(201): 매니페스트의 VM·스펙·디스크로 `receiving` 백업을
/// 만들고 백업 풀에 그 디렉터리를 만든다. 풀이 없거나 백업 풀이 아니거나 매니페스트가 올바르지 않으면
/// 400, 풀을 이 노드에서 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`, 같은 원본(CA 지문과
/// 원본 id)의 사본이 같은 풀에 있으면 409 `replica_exists`(`details.backup`이 그 id), 풀에 디스크 파일을
/// 받을 자리가 없으면 507 `insufficient_storage`, 옛 노드가 있으면 409 `cluster_upgrading`. 증분 백업은
/// `parent`로 이 클러스터의 부모 사본을 함께 보낸다: 맞는 부모 사본이 없으면 409
/// `replica_parent_missing`, 증분 받기(`incremental-replication`)를 모르는 노드가 있으면 409
/// `cluster_upgrading`.
#[utoipa::path(post, path = "/backups/receive", tag = "backups",
    request_body = ReceiveRequest,
    responses((status = 201, body = ReceivingBackup), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn receive_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<ReceiveRequest>,
) -> Result<(StatusCode, Json<ReceivingBackup>), ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    if let Some(name) = &req.name {
        check_name(name)?;
    }
    // 옛 노드는 받는 중인 백업과 출처를 모른다.
    require_current(&state.cluster)?;
    // 계획 40 노드는 증분 백업을 알아도 `parent`가 있는 받기 요청을 모른다.
    if req.parent.is_some() {
        require_feature(&state.cluster, "incremental-replication")?;
    }
    let pool = find_pool(&state, &req.pool)?;
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    let root = pool_root(&state, &pool)?;
    let manifest = req.manifest;
    check_manifest(&manifest, req.parent)?;
    let (_, backups) = state.cluster.list::<BackupSpec>()?;
    if let Some(copy) = backups.iter().find(|b| {
        // 같은 원본이라도 다른 풀에 받은 것은 다른 사본이다.
        b.spec.pool == pool.name
            && b.spec.origin.as_ref().is_some_and(|o| {
                o.ca_fingerprint == req.origin.ca_fingerprint && o.backup == req.origin.backup
            })
    }) {
        let mut e = ApiError::conflict(
            "replica_exists",
            format!(
                "backup {:?} is already a copy of backup {} of cluster {:?}",
                copy.name, req.origin.backup, req.origin.cluster
            ),
        );
        e.details = Some(serde_json::json!({ "backup": copy.id }));
        return Err(e);
    }
    if let Some(parent) = req.parent {
        check_parent_copy(
            &state,
            &backups,
            &pool,
            parent,
            &req.origin,
            manifest.parent,
        )?;
    }
    check_space(&root, &manifest)?;
    let taken: HashSet<&str> = backups.iter().map(|b| b.name.as_str()).collect();
    let base = req
        .name
        .unwrap_or_else(|| default_backup_name(&manifest.vm_name, manifest.created_at));
    let name = free_name(&base, |n| taken.contains(n));

    let mut backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: manifest.vm,
            vm_name: manifest.vm_name,
            pool: pool.name.clone(),
            vm_spec: manifest.vm_spec,
            job: None,
            origin: Some(req.origin),
            incremental: false,
            max_chain: None,
        },
    );
    backup.status = BackupStatus {
        phase: BackupPhase::Receiving,
        created_at: Some(manifest.created_at),
        // 해시는 이 클러스터가 받은 파일의 것이다.
        disks: manifest
            .disks
            .into_iter()
            .map(|d| BackupDisk { sha256: None, ..d })
            .collect(),
        uefi_vars: manifest.uefi_vars,
        error: None,
        // 공유가 아닌 풀의 파일은 이 노드에만 있다.
        node: (!is_shared(&pool)).then_some(state.node),
        replicas: Vec::new(),
        received_at: None,
        parent: req.parent,
        full_reason: None,
    };
    // 그사이 같은 이름이 생겼으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&backup).await?;
    let dir = backup_dir(&root, backup.id);
    if let Err(e) = create_dir(&root, &dir).await {
        if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
            tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup without a directory");
        }
        return Err(ApiError::internal(format!(
            "cannot create backup directory: {e}"
        )));
    }
    Ok((
        StatusCode::CREATED,
        Json(ReceivingBackup {
            id: backup.id,
            name: backup.name,
        }),
    ))
}

/// 백업 디렉터리를 만들고, 새 이름이 남게 `backups`와 풀 루트도 내려 쓴다.
async fn create_dir(root: &FsPath, dir: &FsPath) -> std::io::Result<()> {
    tokio::fs::create_dir_all(dir).await?;
    for parent in [root.join("backups"), root.to_path_buf()] {
        sync_path(&parent).await?;
    }
    Ok(())
}

/// 받는 중인 백업의 디렉터리. 받는 중이 아니면 409 `backup_not_receiving`, 파일이 다른 노드에 있으면
/// 409 `backup_on_other_node`, 풀을 이 노드에서 쓸 수 없으면 409.
fn receiving_dir(state: &AppState, backup: &Backup) -> Result<PathBuf, ApiError> {
    if backup.status.phase != BackupPhase::Receiving {
        return Err(ApiError::conflict(
            "backup_not_receiving",
            format!("backup {:?} is not receiving files", backup.name),
        ));
    }
    if let Some(node) = backup.status.node.filter(|n| *n != state.node) {
        return Err(ApiError::conflict(
            "backup_on_other_node",
            format!(
                "backup {:?} is received by node {:?}; send its files there",
                backup.name,
                node_name(state, node)?
            ),
        ));
    }
    let pool = find_pool(state, &backup.spec.pool)?;
    Ok(backup_dir(&pool_root(state, &pool)?, backup.id))
}

/// 백업이 받는 파일 이름과 크기(UEFI 변수는 크기를 모른다)
fn expected_files(backup: &Backup) -> Vec<(&str, Option<u64>)> {
    let mut files: Vec<(&str, Option<u64>)> = backup
        .status
        .disks
        .iter()
        .map(|d| (d.file.as_str(), Some(d.stored_bytes)))
        .collect();
    if backup.status.uefi_vars {
        files.push((VARS_FILE, None));
    }
    files
}

/// 본문을 `part`에 `length`바이트까지 받으며 SHA-256을 계산하고 디스크에 내려 쓴다(긴 파일은
/// [`SYNC_EVERY`]마다). 그보다 길거나 짧으면 400, 조각이 `idle` 넘게 오지 않으면 400(멈춘 업로드).
async fn receive_file(
    part: &FsPath,
    body: Body,
    length: u64,
    idle: std::time::Duration,
) -> Result<String, ApiError> {
    let mut file = match tokio::fs::File::create(part).await {
        Ok(f) => f,
        // 디렉터리가 없다: 그사이 지웠다.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::conflict(
                "backup_not_receiving",
                "the backup directory is gone; the backup was deleted",
            ));
        }
        Err(e) => return Err(ApiError::internal(e)),
    };
    let mut hash = Sha256::new();
    let mut received = 0u64;
    let mut synced = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = tokio::time::timeout(idle, stream.next())
        .await
        .map_err(|_| ApiError::bad_request("the upload stalled"))?
    {
        let chunk = chunk.map_err(|e| ApiError::bad_request(format!("upload failed: {e}")))?;
        received += chunk.len() as u64;
        if received > length {
            return Err(ApiError::bad_request(
                "upload is longer than Content-Length",
            ));
        }
        hash.update(&chunk);
        file.write_all(&chunk).await.map_err(ApiError::internal)?;
        if received - synced >= SYNC_EVERY {
            file.sync_data().await.map_err(ApiError::internal)?;
            synced = received;
        }
    }
    if received != length {
        return Err(ApiError::bad_request("upload ended early"));
    }
    file.sync_all().await.map_err(ApiError::internal)?;
    Ok(hex::encode(hash.finalize()))
}

/// 업로드 본문의 문서용 스키마: 바이너리 파일
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct BackupFile(#[allow(dead_code)] Vec<u8>);

/// 받는 중인 백업의 파일 하나를 받는다(200): `{file}.part`에 쓰고 디스크에 내려 쓴 뒤 이름을 바꾸고,
/// 받은 내용의 SHA-256과 크기를 돌려주며, 디스크 파일의 해시와 받은 시각(`received_at`)을 백업 상태에
/// 적는다. 같은 파일을 다시 보내면 덮는다. `file`은 매니페스트의 디스크 파일이나(UEFI 변수가 있으면)
/// `vars.fd`여야 하고, `Content-Length`는 디스크 파일이면 매니페스트의 크기, `vars.fd`면 64 MiB까지다
/// (400). 본문 조각이 60초 넘게 오지 않으면 받은 것을 버리고 400(멈춘 업로드). `Content-Length`가 없으면
/// 411, 받는 중이 아니면 409 `backup_not_receiving`, 파일이 다른 노드에 있으면 409 `backup_on_other_node`,
/// 같은 파일을 받고 있으면 409 `backup_busy`.
#[utoipa::path(put, path = "/backups/{key}/files/{file}", tag = "backups",
    params(
        ("key" = String, Path, description = "id or name"),
        ("file" = String, Path, description = "disk file of the manifest or vars.fd"),
    ),
    request_body(content = BackupFile, content_type = "application/octet-stream"),
    responses((status = 200, body = ReceivedFile), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn put_backup_file(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((key, file)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<ReceivedFile>, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let length = content_length(&headers)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    let dir = receiving_dir(&state, &backup)?;
    // 이름은 정해진 몇 개뿐이므로 경로 문자를 따로 볼 필요가 없다.
    let Some((_, size)) = expected_files(&backup)
        .into_iter()
        .find(|(f, _)| *f == file)
    else {
        return Err(ApiError::bad_request(format!(
            "backup {:?} has no file {file:?}",
            backup.name
        )));
    };
    match size {
        Some(size) if length != size => {
            return Err(ApiError::bad_request(format!(
                "{file} of backup {:?} is {size} bytes, not {length}",
                backup.name
            )));
        }
        None if length > MAX_VARS_BYTES => {
            return Err(ApiError::bad_request(format!(
                "{file} must not be larger than {MAX_VARS_BYTES} bytes"
            )));
        }
        _ => {}
    }
    let _receiving = ReceivingFile::take(&state, &backup, &file)?;
    let part = dir.join(format!("{file}.part"));
    let sha256 = match receive_file(&part, body, length, state.receive_idle_timeout).await {
        Ok(hash) => hash,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    tokio::fs::rename(&part, dir.join(&file))
        .await
        .map_err(ApiError::internal)?;
    sync_path(&dir).await.map_err(ApiError::internal)?;
    record_file(&state, backup.id, &file, &sha256).await?;
    Ok(Json(ReceivedFile {
        sha256,
        size: length,
    }))
}

/// 이 노드가 받고 있는 (백업, 파일)의 표시. 놓으면 지운다.
struct ReceivingFile<'a> {
    state: &'a AppState,
    key: (Uuid, String),
}

impl<'a> ReceivingFile<'a> {
    /// 같은 파일을 받고 있으면 409 `backup_busy`
    fn take(state: &'a AppState, backup: &Backup, file: &str) -> Result<Self, ApiError> {
        let key = (backup.id, file.to_string());
        if !state
            .receiving_files
            .lock()
            .expect("receiving lock")
            .insert(key.clone())
        {
            return Err(ApiError::conflict(
                "backup_busy",
                format!(
                    "{file} of backup {:?} is being received; try again when that upload has finished",
                    backup.name
                ),
            ));
        }
        Ok(Self { state, key })
    }
}

impl Drop for ReceivingFile<'_> {
    fn drop(&mut self) {
        self.state
            .receiving_files
            .lock()
            .expect("receiving lock")
            .remove(&self.key);
    }
}

/// 받은 파일의 해시(디스크 파일이면)와 받은 시각을 백업 상태에 적는다. 다른 요청이 그사이 쓴 상태를
/// 덮지 않게 읽은 revision에서만 쓰고, 어긋나면 다시 한다. 받는 중이 아니게 됐으면 적지 않는다.
async fn record_file(state: &AppState, id: Uuid, file: &str, sha256: &str) -> Result<(), ApiError> {
    loop {
        let Some(backup) = state.cluster.get::<BackupSpec>(id)? else {
            return Ok(());
        };
        if backup.status.phase != BackupPhase::Receiving {
            return Ok(());
        }
        let mut status = backup.status.clone();
        for disk in status.disks.iter_mut().filter(|d| d.file == file) {
            disk.sha256 = Some(sha256.to_string());
        }
        status.received_at = Some(now());
        match state
            .cluster
            .update_status_checked::<BackupSpec>(id, backup.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// `path`가 일반 파일이면 그 크기
async fn file_size(path: &FsPath) -> Option<u64> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(m) if m.file_type().is_file() => Some(m.len()),
        _ => None,
    }
}

/// 받기를 끝낸다(200, 백업): 모든 파일이 있고 디스크 파일 크기가 매니페스트와 같으면 매니페스트를
/// (`backup`은 이 클러스터의 백업 id로, 증분이면 버전 2에 `parent`는 부모 사본 id로) 쓰고 `ready`로
/// 바꾼다. 빠지거나 크기가 다른 파일이 있으면 409 `backup_incomplete`(`details.files`가 그 파일들), 받는
/// 중이 아니면 409 `backup_not_receiving`, 지우는 Task가 걸려 있으면 409 `backup_busy`, 증분 사본의 부모
/// 사본이 없어졌거나 `ready`가 아니거나 지우는 중이면 409 `replica_parent_missing`.
#[utoipa::path(post, path = "/backups/{key}/actions/complete", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses(
        (status = 200, body = Resource<BackupSpec, BackupStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn complete_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Backup>, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    let dir = receiving_dir(&state, &backup)?;
    check_not_deleting(&state, &backup)?;
    let mut missing = vec![];
    for (file, size) in expected_files(&backup) {
        let found = file_size(&dir.join(file)).await;
        if found.is_none() || size.is_some_and(|s| found != Some(s)) {
            missing.push(file.to_string());
        }
    }
    if !missing.is_empty() {
        let mut e = ApiError::conflict(
            "backup_incomplete",
            format!(
                "backup {:?} is missing files or has files of the wrong size: {}",
                backup.name,
                missing.join(", ")
            ),
        );
        e.details = Some(serde_json::json!({ "files": missing }));
        return Err(e);
    }
    // 받는 사이 부모 사본이 바뀌었을 수 있다(받기의 확인 뒤).
    if let Some(parent) = backup.status.parent {
        let usable = match state.cluster.get::<BackupSpec>(parent)? {
            Some(p) => {
                p.status.phase == BackupPhase::Ready
                    && !backup_tasks(&state, p.id)?.contains(&"being deleted")
            }
            None => false,
        };
        if !usable {
            return Err(parent_missing(parent));
        }
    }
    // 그사이 받은 파일의 기록을 덮지 않게 읽은 revision에서만 쓰고, 어긋나면 다시 읽은 상태로
    // 매니페스트부터 다시 쓴다.
    let mut current = backup;
    loop {
        // 복원은 매니페스트의 백업 id(증분이면 부모 id도)가 이 클러스터의 것인지 확인한다.
        let manifest = BackupManifest {
            version: if current.status.parent.is_some() {
                MANIFEST_VERSION_INCREMENTAL
            } else {
                MANIFEST_VERSION
            },
            backup: current.id,
            parent: current.status.parent,
            vm: current.spec.vm,
            vm_name: current.spec.vm_name.clone(),
            vm_spec: current.spec.vm_spec.clone(),
            disks: current.status.disks.clone(),
            uefi_vars: current.status.uefi_vars,
            created_at: current.status.created_at.unwrap_or_default(),
        };
        write_manifest(&dir, &manifest)
            .await
            .map_err(|e| ApiError::internal(format!("cannot write manifest: {e}")))?;
        let status = BackupStatus {
            phase: BackupPhase::Ready,
            error: None,
            ..current.status.clone()
        };
        match state
            .cluster
            .update_status_checked::<BackupSpec>(current.id, current.revision, &status)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e.into()),
        }
        current = lookup::<BackupSpec>(&state.cluster, &current.id.to_string())?;
        receiving_dir(&state, &current)?;
        check_not_deleting(&state, &current)?;
    }
    Ok(Json(lookup::<BackupSpec>(
        &state.cluster,
        &current.id.to_string(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names_get_a_number_and_stay_short() {
        assert_eq!(free_name("web", |_| false), "web");
        assert_eq!(free_name("web", |n| n == "web"), "web-2");
        assert_eq!(free_name("web", |n| n == "web" || n == "web-2"), "web-3");
        let long = "a".repeat(63);
        let name = free_name(&long, |n| n == long);
        assert_eq!(name, format!("{}-2", "a".repeat(61)));
        assert_eq!(validate_name(&name), Ok(()));
        // 줄여서 끝이 `-`이면 뺀다.
        let dash = format!("{}-b", "a".repeat(60));
        assert_eq!(
            free_name(&dash, |n| n == dash),
            format!("{}-2", "a".repeat(60))
        );
    }
}
