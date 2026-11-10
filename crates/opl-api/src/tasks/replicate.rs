//! 백업 복제 Task(엔터프라이즈 설계 §6.2): `ready` 백업을 원격 클러스터의 받기 API(§6.1)로 보내 그 백업
//! 풀에 사본을 만든다. 받기를 시작하고(적어 둔 사본이 원격에 아직 있거나 같은 원본의 사본이 있으면, 이
//! 백업의 사본인지 확인하고 그것으로 이어 간다. ready면 받은 파일의 해시가 원본에 적힌 해시와 같아야 끝난
//! 것이다), 파일마다 읽으면서 SHA-256을 계산해 보내고 원격이 받은 해시·크기와 비교해 원본 디스크에 그
//! 해시를 적은 뒤, 완료를 요청한다. 원본 `status.replicas`의 (원격, 풀) 항목을 copying으로 시작해 ready나
//! failed(실행기의 정리)로 바꾼다. 이미 ready인 사본의 확인은 기록을 ready로 둔 채 원격에 묻고, 다시
//! 보내야 할 때만 copying으로 바꾼다. 증분 백업은 체인의 앞 백업부터 차례로 보내고(증분 백업 설계 §4),
//! 받는 쪽에 부모 사본의 id를 함께 보낸다.

use std::collections::HashSet;
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
/// 원격 사본 지우기가 409 `backup_in_use`(자식 사본의 원격 지우기 Task가 아직 끝나지 않음)일 때 다시
/// 요청하며 기다리는 시간
const IN_USE_TIMEOUT: Duration = Duration::from_secs(60);

/// 원본 백업 `id`의 (`remote`, `pool`) 사본 기록을 `change`로 바꾼다(없으면 새로 만든다). 다른 쪽이 그사이
/// 쓴 상태를 덮지 않게 읽은 revision에서만 쓰고, 기록될 때까지 다시 시도한다. 백업이 없어졌으면 그만둔다.
async fn record(
    ctx: &Ctx,
    id: Uuid,
    remote: &str,
    pool: &str,
    change: impl Fn(&mut BackupReplica),
) {
    update_replicas(ctx, id, |replicas| {
        let at = replicas
            .iter()
            .position(|r| r.remote == remote && r.pool == pool);
        let i = at.unwrap_or_else(|| {
            replicas.push(BackupReplica {
                remote: remote.to_string(),
                pool: pool.to_string(),
                backup: None,
                state: ReplicaState::Copying,
                error: None,
                at: 0,
            });
            replicas.len() - 1
        });
        change(&mut replicas[i]);
        replicas[i].at = now();
    })
    .await;
}

/// 원본 백업 `id`의 (`remote`, `pool`) 사본 기록을 failed와 `error`로 바꾼다(원격에 그 사본이 없다). 사본
/// id는 남겨 다음에 보낼 때 원격에 물어 보고(없으면 새로 받는다) 원본을 지울 때도 지운다.
async fn fail_missing(ctx: &Ctx, id: Uuid, remote: &str, pool: &str, error: &str) {
    record(ctx, id, remote, pool, |r| {
        r.state = ReplicaState::Failed;
        r.error = Some(error.to_string());
    })
    .await;
}

/// 원본 백업 `id`의 사본 기록들을 `change`로 바꿔 읽은 revision에서만 쓴다. 기록될 때까지 다시 시도하고,
/// 백업이 없어졌으면 그만둔다.
async fn update_replicas(ctx: &Ctx, id: Uuid, change: impl Fn(&mut Vec<BackupReplica>)) {
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
        change(&mut status.replicas);
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

/// 원격이 받기를 거절한 것: 증분 백업의 부모 사본이 원격에 없다(409 `replica_parent_missing`).
struct ParentMissing(String);

/// 백업 `id`를 원격 `remote`의 백업 풀 `pool`에 사본으로 보낸다. 증분 백업은 체인(전체 백업부터 `id`까지)을
/// 앞에서부터 보고, 앞 백업 가운데 사본이 ready로 적힌 것은 건너뛰고(확인하지 않는다) 나머지를 차례로
/// 보낸다. 원격에 부모 사본이 없다고 하면 부모의 기록을 failed로 바꾸고 체인을 처음부터 한 번 더 본다. 이번에는
/// ready로 적힌 앞 백업도 확인한다(원격에 없으면 다시 보낸다). 실패하면 실행기의 정리([`discard_chain`])가
/// `id`의 사본 기록을 failed로 바꾼다(앞 백업이 실패하면 그 기록도 여기서). 기록이 ready인 사본은 확인만
/// 하므로 기록을 ready로 둔다(확인이 실패해도 이유만 적힌다). 원격에 없거나 다른 사본이면 그때 copying이나
/// failed로 바꾼다.
pub(super) async fn replicate(ctx: &Ctx, id: Uuid, remote: &str, pool: &str) -> Result<(), String> {
    let mut retried = false;
    loop {
        match replicate_chain(ctx, id, remote, pool, retried).await? {
            Ok(()) => return Ok(()),
            Err(ParentMissing(e)) if retried => return Err(e),
            Err(ParentMissing(_)) => retried = true,
        }
    }
}

/// 체인을 한 번 본다. `check_all`이면 ready로 적힌 앞 백업도 건너뛰지 않고 확인한다. 원격에 부모 사본이
/// 없으면 그 부모의 기록을 failed로 바꾸고 `Ok(Err(..))`다(그 백업이 체인 중간이면 그 기록도 failed).
async fn replicate_chain(
    ctx: &Ctx,
    id: Uuid,
    remote: &str,
    pool: &str,
    check_all: bool,
) -> Result<Result<(), ParentMissing>, String> {
    let chain = read_chain(ctx, id)?;
    if chain.len() > 1 {
        check_remote_features(ctx, remote).await?;
    }
    // 체인에서 앞 백업의 원격 사본 id(받는 쪽에 보낼 부모)
    let mut parent: Option<Uuid> = None;
    for (i, backup) in chain.iter().enumerate() {
        let last = i + 1 == chain.len();
        let recorded = backup
            .status
            .replicas
            .iter()
            .find(|r| r.remote == remote && r.pool == pool);
        if !last
            && !check_all
            && let Some(copy) = recorded
                .filter(|r| r.state == ReplicaState::Ready)
                .and_then(|r| r.backup)
        {
            parent = Some(copy);
            continue;
        }
        tracing::info!(backup = %backup.name, remote, step = i + 1, of = chain.len(), "replicating backup of the chain");
        match replicate_one(ctx, backup.id, remote, pool, parent).await {
            Ok(Ok(copy)) => parent = Some(copy),
            Ok(Err(missing)) => {
                // 부모 사본이 원격에서 지워졌다: 기록을 failed로 바꿔 다음에 부모부터 보내게 한다.
                if i > 0 {
                    fail_missing(ctx, chain[i - 1].id, remote, pool, &missing.0).await;
                }
                // 체인 중간의 기록이 copying으로 남지 않게 한다(마지막 백업은 실행기가 정리한다).
                if !last {
                    discard(ctx, backup.id, remote, pool, &missing.0).await;
                }
                return Ok(Err(missing));
            }
            Err(e) if last => return Err(e),
            Err(e) => {
                discard(ctx, backup.id, remote, pool, &e).await;
                return Err(format!(
                    "cannot replicate backup {:?} of the chain: {e}",
                    backup.name
                ));
            }
        }
    }
    Ok(Ok(()))
}

/// 백업 `id`의 체인: 전체 백업부터 `id`까지(`status.parent`를 따라 이 클러스터의 백업을 읽는다). 모두
/// `ready`여야 한다. 아니면 "backup chain is broken: …"이다.
fn read_chain(ctx: &Ctx, id: Uuid) -> Result<Vec<Backup>, String> {
    let broken = |e: String| format!("backup chain is broken: {e}");
    let backup = get_backup(ctx, id)?.ok_or("backup was deleted")?;
    if backup.status.phase != BackupPhase::Ready {
        return Err("backup is not ready".into());
    }
    let mut chain = vec![backup];
    while let Some(parent) = chain[chain.len() - 1].status.parent {
        let child = &chain[chain.len() - 1].name;
        if chain.iter().any(|b| b.id == parent) {
            return Err(broken(format!("backup {child:?} is its own ancestor")));
        }
        let b = get_backup(ctx, parent)?
            .ok_or_else(|| broken(format!("the parent of backup {child:?} was deleted")))?;
        if b.status.phase != BackupPhase::Ready {
            return Err(broken(format!("backup {:?} is not ready", b.name)));
        }
        chain.push(b);
    }
    chain.reverse();
    Ok(chain)
}

/// 원격의 모든 노드(위트니스 포함)가 증분 받기(`incremental-replication`)를 아는지 본다(`GET /nodes`의
/// `status.features`). 노드 목록이 비었으면 모르는 것으로 본다.
async fn check_remote_features(ctx: &Ctx, remote: &str) -> Result<(), String> {
    let client = client(ctx, remote)?;
    let nodes: Value = client
        .get_json("/nodes")
        .await
        .map_err(|e| remote_error(remote, "list nodes", e))?;
    let items = nodes["items"].as_array().cloned().unwrap_or_default();
    let knows = |n: &Value| {
        n["status"]["features"]
            .as_array()
            .is_some_and(|f| f.iter().any(|f| f == "incremental-replication"))
    };
    if !items.is_empty() && items.iter().all(knows) {
        Ok(())
    } else {
        Err(format!(
            "remote cluster {remote:?} does not support incremental backups; upgrade it"
        ))
    }
}

/// 체인의 백업 하나(`id`)를 보내고 원격 사본 id를 돌려준다. `parent`는 받는 쪽의 부모 사본(전체 백업이면
/// 없다). 원격에 부모 사본이 없으면 `Ok(Err(..))`다.
async fn replicate_one(
    ctx: &Ctx,
    id: Uuid,
    remote: &str,
    pool: &str,
    parent: Option<Uuid>,
) -> Result<Result<Uuid, ParentMissing>, String> {
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
    let copy = match send(ctx, &backup, remote, pool, parent, checking).await? {
        Ok(copy) => copy,
        Err(missing) => return Ok(Err(missing)),
    };
    record(ctx, id, remote, pool, |r| {
        r.backup = Some(copy);
        r.state = ReplicaState::Ready;
        r.error = None;
    })
    .await;
    Ok(Ok(copy))
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

/// 실패하거나 중단된 복제 Task의 정리: 대상 백업 `id`의 기록은 [`discard`]로 바꾸고, 체인의 앞 백업
/// (`status.parent`를 따라감) 가운데 (`remote`, `pool`) 기록이 copying인 것도 failed와 `error`로 바꾼다(opld가
/// 다시 시작하면 보내던 중간 백업의 기록이 copying으로 남는다). ready·failed 기록은 그대로다. 같은 체인
/// 뿌리의 복제 Task는 원격마다 하나뿐이므로 다른 Task가 보내는 기록을 바꾸지 않는다.
pub(super) async fn discard_chain(ctx: &Ctx, id: Uuid, remote: &str, pool: &str, error: &str) {
    discard(ctx, id, remote, pool, error).await;
    let mut seen = HashSet::from([id]);
    let mut next = get_backup(ctx, id)
        .ok()
        .flatten()
        .and_then(|b| b.status.parent);
    while let Some(parent) = next
        && seen.insert(parent)
    {
        let Ok(Some(backup)) = get_backup(ctx, parent) else {
            return;
        };
        let copying = |r: &BackupReplica| {
            r.remote == remote && r.pool == pool && r.state == ReplicaState::Copying
        };
        if backup.status.replicas.iter().any(copying) {
            update_replicas(ctx, parent, |replicas| {
                for r in replicas.iter_mut().filter(|r| copying(r)) {
                    r.state = ReplicaState::Failed;
                    r.error = Some(error.to_string());
                    r.at = now();
                }
            })
            .await;
        }
        next = backup.status.parent;
    }
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

/// 원격 사본 `copy`를 지운다. 409 `backup_in_use`(체인을 자식부터 지울 때 자식 사본의 원격 지우기 Task가
/// 아직 끝나지 않았다)면 [`IN_USE_TIMEOUT`]까지 다시 요청한다.
async fn delete_copy(ctx: &Ctx, remote: &str, copy: Uuid) -> Result<(), String> {
    let client = client(ctx, remote)?;
    let deleting = |e| remote_error(remote, &format!("delete copy {copy}"), e);
    let deadline = tokio::time::Instant::now() + IN_USE_TIMEOUT;
    loop {
        let (status, body) = call(&client, Method::DELETE, &format!("/backups/{copy}"), None)
            .await
            .map_err(deleting)?;
        match status {
            200..=299 | 404 => return Ok(()),
            409 if body["code"] == "backup_in_use" && tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(ctx.config.poll_interval).await;
            }
            _ => return Err(deleting(status_error(status, &body))),
        }
    }
}

/// 원격의 오류(사람이 읽을 말)
fn remote_error(remote: &str, doing: &str, e: RemoteError) -> String {
    format!("cannot {doing} on remote {remote:?}: {e}")
}

/// 파일을 보내고 원격 사본의 id를 돌려준다. `parent`는 받는 쪽의 부모 사본이다(증분 백업). `checking`이면
/// 기록이 ready인 사본을 확인하는 것이고, 파일을 다시 보내야 할 때 기록을 copying으로 바꾼다. 원격에 부모
/// 사본이 없으면 `Ok(Err(..))`다.
async fn send(
    ctx: &Ctx,
    backup: &Backup,
    remote: &str,
    pool: &str,
    parent: Option<Uuid>,
    checking: bool,
) -> Result<Result<Uuid, ParentMissing>, String> {
    let id = backup.id;
    let client = client(ctx, remote)?;
    if let Some(node) = backup.status.node.filter(|n| *n != ctx.config.node) {
        return Err(format!("backup is stored on node {node}"));
    }
    let (_, root) = backup_root(ctx, &backup.spec.pool)?;
    let dir = backup_dir(&root, id);
    let manifest = read_manifest(&dir).await?;
    if manifest.backup != id
        || manifest.parent != backup.status.parent
        || manifest.vm_spec != backup.spec.vm_spec
    {
        return Err("backup manifest does not match the backup record".into());
    }
    let origin = origin(ctx, backup)?;

    let mut request = json!({
        "name": backup.name,
        "pool": pool,
        "manifest": manifest,
        "origin": origin,
    });
    // 전체 백업은 `parent`를 빼서 옛 받는 쪽도 받게 한다.
    if let Some(parent) = parent {
        request["parent"] = json!(parent);
    }
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
                409 if body["code"] == "replica_parent_missing" => {
                    return Ok(Err(ParentMissing(start(status_error(status, &body)))));
                }
                _ => return Err(start(status_error(status, &body))),
            }
        }
    };
    if let Some(got) = &existing {
        if let Err(e) = check_copy(got, backup, &manifest, parent, remote, pool) {
            return Err(mismatch(ctx, id, remote, pool, e).await);
        }
        // 이미 ready면 받은 파일의 해시가 원본과 같을 때만 다 보낸 것이다. ready 사본은 파일을 다시
        // 받지 않으므로 다르면 실패로 알린다.
        if got.status.phase == BackupPhase::Ready {
            if same_files(ctx, backup, got, &dir).await? {
                return Ok(Ok(copy));
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
    // 받는 동안 부모 사본이 없어졌다(받는 사본이 있으면 원격이 부모 사본 지우기를 막으므로 방어용이다).
    if status == 409 && body["code"] == "replica_parent_missing" {
        let e = remote_error(remote, "complete the copy", status_error(status, &body));
        return Ok(Err(ParentMissing(e)));
    }
    expect(200, status, body).map_err(|e| remote_error(remote, "complete the copy", e))?;
    Ok(Ok(copy))
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

/// 원격 백업 `got`이 이 백업(`manifest`)을 풀 `pool`에 받는 사본인지 본다: 출처의 원본 id, 풀, 부모
/// 사본(`parent`, 전체 백업이면 없음), VM 스펙, 디스크 이름과 크기, UEFI 변수, 받는 중이거나 ready. 다르면
/// 원격에서 지우라는 오류다.
fn check_copy(
    got: &Backup,
    backup: &Backup,
    manifest: &BackupManifest,
    parent: Option<Uuid>,
    remote: &str,
    pool: &str,
) -> Result<(), String> {
    let disks = |d: &[BackupDisk]| -> Vec<(String, u64)> {
        d.iter().map(|d| (d.name.clone(), d.stored_bytes)).collect()
    };
    let same = got.spec.origin.as_ref().map(|o| o.backup) == Some(backup.id)
        && got.spec.pool == pool
        && got.status.parent == parent
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
