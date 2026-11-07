//! 백업 복제 Task(엔터프라이즈 설계 §6.2): `ready` 백업을 원격 클러스터의 받기 API(§6.1)로 보내 그 백업
//! 풀에 사본을 만든다. 받기를 시작하고(적어 둔 사본이 원격에 아직 있거나 같은 원본의 사본이 있으면, 이
//! 백업의 사본인지 확인하고 그것으로 이어 간다. ready면 받은 파일의 해시가 원본에 적힌 해시와 같아야 끝난
//! 것이다), 파일마다 읽으면서 SHA-256을 계산해 보내고 원격이 받은 해시·크기와 비교해 원본 디스크에 그
//! 해시를 적은 뒤, 완료를 요청한다. 원본 `status.replicas`의 (원격, 풀) 항목을 copying으로 시작해 ready나
//! failed(실행기의 정리)로 바꾼다. 이미 ready인 사본의 확인은 기록을 ready로 둔 채 원격에 묻고, 다시
//! 보내야 할 때만 copying으로 바꾼다.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::Method;
use hyper::body::{Body, Bytes, Frame};
use hyper::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderValue};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::backup::{
    Backup, BackupDisk, BackupManifest, BackupOrigin, BackupPhase, BackupReplica, BackupSpec,
    ReplicaState, backup_dir,
};
use opl_types::backup_job::BackupJobSpec;
use opl_types::cluster::ClusterSpec;
use opl_types::remote::RemoteSpec;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::Ctx;
use super::backup::{VARS_FILE, backup_root, get_backup, read_manifest};
use crate::backups::now;
use crate::remote_client::{RemoteClient, RemoteError};

/// 파일을 읽어 보내는 조각의 크기
const CHUNK: usize = 256 << 10;
/// 원격 JSON 응답 본문의 최대 크기
const MAX_JSON_BYTES: usize = 4 << 20;
/// 파일을 다 보낸 뒤 응답을 기다리는 시간. 받는 쪽은 큰 파일을 디스크에 내려 쓰고서 답한다.
const FILE_ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// 원본 백업 `id`의 (`remote`, `pool`) 사본 기록을 `change`로 바꾼다(없으면 새로 만든다). 다른 쪽이 그사이
/// 쓴 상태를 덮지 않게 읽은 revision에서만 쓰고, 기록될 때까지 다시 시도한다. 백업이 없어졌으면 그만둔다.
async fn record(
    ctx: &Ctx,
    id: Uuid,
    remote: &str,
    pool: &str,
    change: impl Fn(&mut BackupReplica),
) {
    loop {
        let backup = match get_backup(ctx, id) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot read backup; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        let mut status = backup.status.clone();
        let at = status
            .replicas
            .iter()
            .position(|r| r.remote == remote && r.pool == pool);
        let i = at.unwrap_or_else(|| {
            status.replicas.push(BackupReplica {
                remote: remote.to_string(),
                pool: pool.to_string(),
                backup: None,
                state: ReplicaState::Copying,
                error: None,
                at: 0,
            });
            status.replicas.len() - 1
        });
        change(&mut status.replicas[i]);
        status.replicas[i].at = now();
        match ctx
            .cluster
            .update_status_checked::<BackupSpec>(id, backup.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot record backup replica; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 사본을 보내기 시작한 기록
fn copying(r: &mut BackupReplica) {
    r.state = ReplicaState::Copying;
    r.error = None;
}

/// 원격 사본이 이 백업의 사본이 아니다: 기록을 failed로 바꾸고 `error`를 돌려준다(ready 기록이라도).
async fn mismatch(ctx: &Ctx, id: Uuid, remote: &str, pool: &str, error: String) -> String {
    record(ctx, id, remote, pool, |r| r.state = ReplicaState::Failed).await;
    error
}

/// 백업 `id`를 원격 `remote`의 백업 풀 `pool`에 사본으로 보낸다. 실패하면 실행기의 정리([`discard`])가
/// 사본 기록을 failed로 바꾼다. 기록이 ready인 사본은 확인만 하므로 기록을 ready로 둔다(확인이 실패해도
/// 이유만 적힌다). 원격에 없거나 다른 사본이면 그때 copying이나 failed로 바꾼다.
pub(super) async fn replicate(ctx: &Ctx, id: Uuid, remote: &str, pool: &str) -> Result<(), String> {
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    let checking = backup.status.replicas.iter().any(|r| {
        r.remote == remote && r.pool == pool && r.state == ReplicaState::Ready && r.backup.is_some()
    });
    if !checking {
        record(ctx, id, remote, pool, copying).await;
    }
    let copy = send(ctx, &backup, remote, pool, checking).await?;
    record(ctx, id, remote, pool, |r| {
        r.backup = Some(copy);
        r.state = ReplicaState::Ready;
        r.error = None;
    })
    .await;
    Ok(())
}

/// 실패한 복제 Task의 사본 기록을 failed와 `error`로 바꾼다. ready 기록은 확인하다 실패한 것이므로(원격에
/// 닿지 못함 등) ready로 두고 `error`만 적는다. 다음 실행이 다시 확인한다.
pub(super) async fn discard(ctx: &Ctx, id: Uuid, remote: &str, pool: &str, error: &str) {
    record(ctx, id, remote, pool, |r| {
        if r.state != ReplicaState::Ready {
            r.state = ReplicaState::Failed;
        }
        r.error = Some(error.to_string());
    })
    .await;
}

/// 원격 `remote`(등록한 이름)의 클라이언트
fn client(ctx: &Ctx, remote: &str) -> Result<RemoteClient, String> {
    let spec = ctx
        .cluster
        .get_by_name::<RemoteSpec>(remote)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("remote {remote:?} not found"))?
        .spec;
    RemoteClient::new(&spec.url, &spec.ca_pem, &spec.token)
        .map_err(|e| remote_error(remote, "connect", e))
}

/// 원본 백업 `backup`의 원격 사본(id가 있는 것 — 실패로 끝났어도 원격에 받다 만 사본이 있다)을 원격에서
/// 지운다(원격의 지우기 Task를 요청한다). 404는 이미 지운 것이다. 지우지 못한 사본마다 이유 한 줄을
/// 돌려준다.
pub(super) async fn delete_copies(ctx: &Ctx, backup: &Backup) -> Vec<String> {
    let mut left = Vec::new();
    for replica in &backup.status.replicas {
        let Some(copy) = replica.backup else {
            continue;
        };
        if let Err(e) = delete_copy(ctx, &replica.remote, copy).await {
            tracing::warn!(backup = %backup.name, remote = %replica.remote, copy = %copy, error = %e, "cannot delete remote copy of backup");
            left.push(e);
        }
    }
    left
}

async fn delete_copy(ctx: &Ctx, remote: &str, copy: Uuid) -> Result<(), String> {
    let client = client(ctx, remote)?;
    let deleting = |e| remote_error(remote, &format!("delete copy {copy}"), e);
    let (status, body) = call(&client, Method::DELETE, &format!("/backups/{copy}"), None)
        .await
        .map_err(deleting)?;
    match status {
        200..=299 | 404 => Ok(()),
        _ => Err(deleting(status_error(status, &body))),
    }
}

/// 원격의 오류(사람이 읽을 말)
fn remote_error(remote: &str, doing: &str, e: RemoteError) -> String {
    format!("cannot {doing} on remote {remote:?}: {e}")
}

/// 파일을 보내고 원격 사본의 id를 돌려준다. `checking`이면 기록이 ready인 사본을 확인하는 것이고, 파일을
/// 다시 보내야 할 때 기록을 copying으로 바꾼다.
async fn send(
    ctx: &Ctx,
    backup: &Backup,
    remote: &str,
    pool: &str,
    checking: bool,
) -> Result<Uuid, String> {
    let id = backup.id;
    let client = client(ctx, remote)?;
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (_, root) = backup_root(ctx, &backup.spec.pool)?;
    let dir = backup_dir(&root, id);
    let manifest = read_manifest(&dir).await?;
    if manifest.backup != id || manifest.vm_spec != backup.spec.vm_spec {
        return Err("backup manifest does not match the backup record".into());
    }
    let origin = origin(ctx, backup)?;

    let request = json!({
        "name": backup.name,
        "pool": pool,
        "manifest": manifest,
        "origin": origin,
    });
    let start = |e| remote_error(remote, "start the copy", e);
    // 앞의 복제가 적은 사본이 원격에 아직 있으면 그것을 이어 간다. 원격에서 지웠으면(404) 새로 받는다.
    let recorded = backup
        .status
        .replicas
        .iter()
        .find(|r| r.remote == remote && r.pool == pool)
        .and_then(|r| r.backup);
    let existing = match recorded {
        Some(copy) => get_copy(&client, copy).await.map_err(start)?,
        None => None,
    };
    if checking && existing.is_none() {
        record(ctx, id, remote, pool, copying).await;
    }
    let (copy, existing) = match existing {
        Some(got) => (got.id, Some(got)),
        None => {
            let (status, body) = call(&client, Method::POST, "/backups/receive", Some(&request))
                .await
                .map_err(start)?;
            match status {
                201 => (body_id(&body, "id").map_err(start)?, None),
                // 앞의 복제가 만든 사본이다(기록하기 전에 멈췄다).
                409 if body["code"] == "replica_exists" => {
                    let copy = body_id(&body["details"], "backup").map_err(start)?;
                    let got = get_copy(&client, copy)
                        .await
                        .map_err(start)?
                        .ok_or_else(|| {
                            format!("the copy {copy} on remote {remote:?} was deleted; try again")
                        })?;
                    (copy, Some(got))
                }
                _ => return Err(start(status_error(status, &body))),
            }
        }
    };
    if let Some(got) = &existing {
        if let Err(e) = check_copy(got, backup, &manifest, remote, pool) {
            return Err(mismatch(ctx, id, remote, pool, e).await);
        }
        // 이미 ready면 받은 파일의 해시가 원본과 같을 때만 다 보낸 것이다. ready 사본은 파일을 다시
        // 받지 않으므로 다르면 실패로 알린다.
        if got.status.phase == BackupPhase::Ready {
            if same_files(ctx, backup, got, &dir).await? {
                return Ok(copy);
            }
            let e = format!(
                "the copy {copy} on remote {remote:?} is ready but its files do not match backup \
                 {:?} (or were not hashed); delete it there and replicate again",
                backup.name
            );
            return Err(mismatch(ctx, id, remote, pool, e).await);
        }
    }
    record(ctx, id, remote, pool, |r| {
        r.backup = Some(copy);
        copying(r);
    })
    .await;

    let mut files: Vec<&str> = manifest.disks.iter().map(|d| d.file.as_str()).collect();
    if manifest.uefi_vars {
        files.push(VARS_FILE);
    }
    for file in files {
        let sha256 = send_file(&client, remote, copy, &dir.join(file), file).await?;
        record_hash(ctx, id, file, &sha256).await;
    }
    let (status, body) = call(
        &client,
        Method::POST,
        &format!("/backups/{copy}/actions/complete"),
        None,
    )
    .await
    .map_err(|e| remote_error(remote, "complete the copy", e))?;
    expect(200, status, body).map_err(|e| remote_error(remote, "complete the copy", e))?;
    Ok(copy)
}

/// 원격 사본 `copy`(없으면 `None`)
async fn get_copy(client: &RemoteClient, copy: Uuid) -> Result<Option<Backup>, RemoteError> {
    let (status, body) = call(client, Method::GET, &format!("/backups/{copy}"), None).await?;
    match status {
        404 => Ok(None),
        _ => {
            let body = expect(200, status, body)?;
            serde_json::from_value(body).map(Some).map_err(|e| {
                RemoteError::Invalid(format!("the remote answered an unexpected backup: {e}"))
            })
        }
    }
}

/// 원격 백업 `got`이 이 백업(`manifest`)을 풀 `pool`에 받는 사본인지 본다: 출처의 원본 id, 풀, VM 스펙,
/// 디스크 이름과 크기, UEFI 변수, 받는 중이거나 ready. 다르면 원격에서 지우라는 오류다.
fn check_copy(
    got: &Backup,
    backup: &Backup,
    manifest: &BackupManifest,
    remote: &str,
    pool: &str,
) -> Result<(), String> {
    let disks = |d: &[BackupDisk]| -> Vec<(String, u64)> {
        d.iter().map(|d| (d.name.clone(), d.stored_bytes)).collect()
    };
    let same = got.spec.origin.as_ref().map(|o| o.backup) == Some(backup.id)
        && got.spec.pool == pool
        && got.spec.vm_spec == backup.spec.vm_spec
        && disks(&got.status.disks) == disks(&manifest.disks)
        && got.status.uefi_vars == manifest.uefi_vars
        && matches!(
            got.status.phase,
            BackupPhase::Receiving | BackupPhase::Ready
        );
    if same {
        Ok(())
    } else {
        Err(format!(
            "a different backup claims to be the copy of {} on remote {remote:?}; delete it there",
            backup.id
        ))
    }
}

/// ready 사본 `got`이 받은 디스크 파일의 해시가 원본 `backup`에 적힌 해시와 모두 같은지. 원본에 해시가
/// 없는 디스크(해시를 적기 전의 백업)만 디렉터리 `dir`의 파일을 읽어 계산하고 원본에 적는다.
async fn same_files(
    ctx: &Ctx,
    backup: &Backup,
    got: &Backup,
    dir: &std::path::Path,
) -> Result<bool, String> {
    for (disk, copied) in backup.status.disks.iter().zip(&got.status.disks) {
        let Some(copied) = &copied.sha256 else {
            return Ok(false);
        };
        let sha256 = match &disk.sha256 {
            Some(sha256) => sha256.clone(),
            None => {
                let sha256 = file_sha256(&dir.join(&disk.file)).await?;
                record_hash(ctx, backup.id, &disk.file, &sha256).await;
                sha256
            }
        };
        if *copied != sha256 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// 원본 백업 `id`의 디스크 파일 `file`에 SHA-256을 적는다(이미 같으면 쓰지 않는다. 디스크 파일이 아니면
/// 할 일이 없다). 읽은 revision에서만 쓰고, 기록될 때까지 다시 시도한다. 백업이 없어졌으면 그만둔다.
async fn record_hash(ctx: &Ctx, id: Uuid, file: &str, sha256: &str) {
    loop {
        let backup = match get_backup(ctx, id) {
            Ok(Some(b)) => b,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot read backup; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
                continue;
            }
        };
        let mut status = backup.status.clone();
        let Some(disk) = status.disks.iter_mut().find(|d| d.file == file) else {
            return;
        };
        if disk.sha256.as_deref() == Some(sha256) {
            return;
        }
        disk.sha256 = Some(sha256.to_string());
        match ctx
            .cluster
            .update_status_checked::<BackupSpec>(id, backup.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::NotFound { .. })) => return,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(backup = %id, error = %e, "cannot record backup hash; will retry");
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
        }
    }
}

/// 파일 `path`의 SHA-256(16진수 소문자)
async fn file_sha256(path: &std::path::Path) -> Result<String, String> {
    let reading = |e: std::io::Error| format!("cannot read {}: {e}", path.display());
    let mut file = tokio::fs::File::open(path).await.map_err(reading)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; CHUNK];
    loop {
        let n = file.read(&mut buf).await.map_err(reading)?;
        if n == 0 {
            return Ok(hex::encode(hash.finalize()));
        }
        hash.update(&buf[..n]);
    }
}

/// 원격 사본의 출처: 이 클러스터 이름과 CA 지문, 원본 id, 원본 작업 이름(작업이 아직 있으면)
fn origin(ctx: &Ctx, backup: &Backup) -> Result<BackupOrigin, String> {
    let info = ctx
        .cluster
        .list::<ClusterSpec>()
        .map_err(|e| e.to_string())?
        .1
        .into_iter()
        .next()
        .ok_or("the cluster has not been initialized")?;
    let ca_fingerprint =
        opl_peer::pki::fingerprint(&info.spec.ca_cert_pem).map_err(|e| e.to_string())?;
    let job = match backup.spec.job {
        Some(job) => ctx
            .cluster
            .get::<BackupJobSpec>(job)
            .map_err(|e| e.to_string())?
            .map(|j| j.name),
        None => None,
    };
    Ok(BackupOrigin {
        cluster: info.name,
        ca_fingerprint,
        backup: backup.id,
        job,
    })
}

/// 파일 `path`를 원격 사본 `copy`의 `file`로 보내고 그 SHA-256을 돌려준다. 읽으면서 계산한 SHA-256·크기가
/// 원격이 받은 것과 다르면 오류다.
async fn send_file(
    client: &RemoteClient,
    remote: &str,
    copy: Uuid,
    path: &std::path::Path,
    file: &str,
) -> Result<String, String> {
    let opened = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("cannot read {file}: {e}"))?;
    let size = opened
        .metadata()
        .await
        .map_err(|e| format!("cannot read {file}: {e}"))?
        .len();
    let hash = Arc::new(Mutex::new(Sha256::new()));
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_LENGTH, HeaderValue::from(size));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let sending = |e| remote_error(remote, &format!("send {file}"), e);
    let response = client
        .request_with_timeout(
            Method::PUT,
            &format!("/backups/{copy}/files/{file}"),
            headers,
            hashing_body(opened, hash.clone()),
            FILE_ANSWER_TIMEOUT,
        )
        .await
        .map_err(sending)?;
    let (status, body) = read_json(response).await.map_err(sending)?;
    let got = expect(200, status, body).map_err(sending)?;
    let sent = hex::encode(hash.lock().expect("hash lock").clone().finalize());
    if got["sha256"].as_str() != Some(sent.as_str()) || got["size"].as_u64() != Some(size) {
        return Err(format!(
            "remote {remote:?} received {file} with SHA-256 {} and {} bytes; sent SHA-256 {sent} and {size} bytes",
            got["sha256"], got["size"]
        ));
    }
    Ok(sent)
}

/// 파일을 조각으로 읽어 보내며 `hash`에 더하는 본문
fn hashing_body(
    file: tokio::fs::File,
    hash: Arc<Mutex<Sha256>>,
) -> impl Body<Data = Bytes, Error = std::io::Error> + Send + Unpin + 'static {
    let chunks = stream::unfold(Some(file), move |file| {
        let hash = hash.clone();
        async move {
            let mut file = file?;
            let mut buf = vec![0; CHUNK];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    hash.lock().expect("hash lock").update(&buf);
                    Some((Ok(Frame::data(Bytes::from(buf))), Some(file)))
                }
                // 읽기 오류는 보내기를 끊는다.
                Err(e) => Some((Err(e), None)),
            }
        }
    });
    StreamBody::new(Box::pin(chunks))
}

/// 요청 하나를 보내고 상태와 JSON 본문(없거나 JSON이 아니면 null)을 돌려준다.
async fn call(
    client: &RemoteClient,
    method: Method,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), RemoteError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let body = match body {
        Some(body) => {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            Bytes::from(serde_json::to_vec(body).map_err(|e| RemoteError::Invalid(e.to_string()))?)
        }
        None => Bytes::new(),
    };
    let response = client
        .request(method, path, headers, Full::new(body))
        .await?;
    read_json(response).await
}

async fn read_json(
    response: hyper::Response<crate::remote_client::RemoteBody>,
) -> Result<(u16, Value), RemoteError> {
    let status = response.status().as_u16();
    let bytes = Limited::new(response.into_body(), MAX_JSON_BYTES)
        .collect()
        .await
        .map_err(|e| RemoteError::Unreachable(e.to_string()))?
        .to_bytes();
    Ok((
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    ))
}

/// 상태가 `want`면 본문, 아니면 [`RemoteError::Status`]
fn expect(want: u16, status: u16, body: Value) -> Result<Value, RemoteError> {
    if status == want {
        Ok(body)
    } else {
        Err(status_error(status, &body))
    }
}

/// 원격 오류 응답의 `message`(없으면 상태 번호만)
fn status_error(status: u16, body: &Value) -> RemoteError {
    let message = body["message"].as_str().unwrap_or("unexpected response");
    RemoteError::Status(status, message.to_string())
}

/// 본문 `body`의 `field`(UUID)
fn body_id(body: &Value, field: &str) -> Result<Uuid, RemoteError> {
    body[field]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| RemoteError::Invalid(format!("the remote answered without {field}")))
}
