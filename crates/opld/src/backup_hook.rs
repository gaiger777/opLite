//! 백업 저장소 E2E의 고리(계획 68 스테이지 3). 시험 빌드(`backup-it`)에만 있고, 저장소 API(스테이지 6)가 생기기 전에
//! 제어 소켓으로 클러스터에 바로 쓴다. 스테이지 6이 지운다(계획 66·67의 같은 고리처럼).
//!
//! - `POST /v1/test/put` `{kind, name, spec}`: `backup-store`·`backup-job`은 스펙 검증만 하고 이름으로 만들거나 바꾼다
//!   (`spec`이 null이면 지움). 답은 리소스 id(지웠으면 null)다. API의 검사(기능·권한·풀·키·겹침)는 스테이지 4·6에서
//!   생긴다 — Raft의 겹침 키(`backup-store-target`)는 상태 기계가 본다.
//! - `POST /v1/test/task` `{op, target, node}`: Task를 곧바로 만든다(스테이지 4 — 저장소 초기화·GC·검증·옮기기). 답은 Task id.
//! - `POST /v1/test/backup` `{vm, store, name, incremental}`: VM의 저장소 백업(`pending` Backup과 `vm-backup` Task)을 만든다.
//!   Task는 `node`(없으면 VM이 도는 노드나 배치된 노드)에서 돈다. 답은 Task id.
//! - `POST /v1/test/live-restore-cancel` `{vm}`(스테이지 5): VM의 라이브 복원을 취소하라고 적는다(스테이지 6의
//!   `POST /vms/{key}/live-restore/cancel`과 같은 함수). 답은 취소를 하는 Task id. 라이브 복원은 `/v1/test/task`의
//!   `backup-live-restore`(대상 = 새 VM id)로 시작한다.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_types::backup_job::BackupJobSpec;
use opl_types::backup_store::BackupStoreSpec;
use opl_types::resource::{Resource, ResourceSpec};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

static MEMBER: OnceLock<Mutex<Option<Weak<Cluster>>>> = OnceLock::new();

/// 멤버가 되면(다시 시작 포함) 그 클러스터 저장소를 알린다.
pub fn set_member(cluster: Weak<Cluster>) {
    *MEMBER.get_or_init(Default::default).lock().unwrap() = Some(cluster);
}

type Failure = (StatusCode, String);

fn member() -> Result<Arc<Cluster>, Failure> {
    let gone = || (StatusCode::NOT_FOUND, "not a member".to_string());
    let m = MEMBER.get().ok_or_else(gone)?.lock().unwrap().clone();
    m.ok_or_else(gone)?.upgrade().ok_or_else(gone)
}

fn bad(e: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, e.to_string())
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/v1/test/put", post(put))
        .route("/v1/test/task", post(task))
        .route("/v1/test/backup", post(backup))
        .route("/v1/test/live-restore-cancel", post(live_restore_cancel))
}

#[derive(Deserialize)]
struct Put {
    kind: String,
    name: String,
    spec: Option<Value>,
}

async fn put(Json(b): Json<Put>) -> Result<Json<Option<uuid::Uuid>>, Failure> {
    let cluster = member()?;
    let id = match b.kind.as_str() {
        "backup-store" => {
            let spec: Option<BackupStoreSpec> = parse(b.spec)?;
            if let Some(s) = &spec {
                s.validate().map_err(bad)?;
            }
            put_one(&cluster, &b.name, spec).await?
        }
        "backup-job" => {
            let spec: Option<BackupJobSpec> = parse(b.spec)?;
            if let Some(s) = &spec {
                s.validate().map_err(bad)?;
            }
            put_one(&cluster, &b.name, spec).await?
        }
        other => return Err(bad(format!("unknown kind {other}"))),
    };
    Ok(Json(id))
}

fn parse<S: DeserializeOwned>(spec: Option<Value>) -> Result<Option<S>, Failure> {
    spec.map(serde_json::from_value).transpose().map_err(bad)
}

/// 이름 `name`의 리소스를 `spec`으로 만들거나 바꾸고(None이면 지우고) id를 돌려준다.
async fn put_one<S: ResourceSpec>(
    cluster: &Cluster,
    name: &str,
    spec: Option<S>,
) -> Result<Option<uuid::Uuid>, Failure> {
    let current = cluster.get_by_name::<S>(name).map_err(bad)?;
    match (current, spec) {
        (Some(r), Some(spec)) => {
            cluster
                .update_spec(r.id, Some(r.revision), r.labels, &spec)
                .await
                .map_err(bad)?;
            Ok(Some(r.id))
        }
        (None, Some(spec)) => {
            let r = Resource::new(name, spec);
            cluster.create(&r).await.map_err(bad)?;
            Ok(Some(r.id))
        }
        (Some(r), None) => {
            cluster
                .delete::<S>(r.id, Some(r.revision))
                .await
                .map_err(bad)?;
            Ok(None)
        }
        (None, None) => Ok(None),
    }
}

#[derive(Deserialize)]
struct NewTask {
    op: opl_types::task::TaskOp,
    target: uuid::Uuid,
    node: uuid::Uuid,
}

async fn create_task(
    cluster: &Cluster,
    op: opl_types::task::TaskOp,
    target: uuid::Uuid,
    node: uuid::Uuid,
) -> Result<uuid::Uuid, Failure> {
    use opl_types::task::TaskSpec;
    let mut t: opl_types::task::Task = Resource::new("", TaskSpec { op, target, node });
    t.name = t.id.to_string();
    cluster.create(&t).await.map_err(bad)?;
    Ok(t.id)
}

async fn task(Json(b): Json<NewTask>) -> Result<Json<uuid::Uuid>, Failure> {
    let cluster = member()?;
    Ok(Json(create_task(&cluster, b.op, b.target, b.node).await?))
}

#[derive(Deserialize)]
struct NewBackup {
    vm: String,
    store: String,
    name: String,
    #[serde(default)]
    incremental: bool,
    #[serde(default)]
    node: Option<uuid::Uuid>,
}

async fn backup(Json(b): Json<NewBackup>) -> Result<Json<uuid::Uuid>, Failure> {
    use opl_types::backup::BackupSpec;
    use opl_types::vm::VmSpec;
    let cluster = member()?;
    let vm = cluster
        .get_by_name::<VmSpec>(&b.vm)
        .map_err(bad)?
        .ok_or_else(|| bad("no such VM"))?;
    let node = b
        .node
        .or(vm.status.node)
        .or(vm.spec.node)
        .ok_or_else(|| bad("the VM has no node; give one"))?;
    let backup: opl_types::backup::Backup = Resource::new(
        b.name,
        BackupSpec {
            vm: vm.id,
            vm_name: vm.name.clone(),
            pool: String::new(),
            store: Some(b.store),
            source: None,
            vm_spec: vm.spec.clone(),
            job: None,
            origin: None,
            incremental: b.incremental,
            max_chain: None,
        },
    );
    cluster.create(&backup).await.map_err(bad)?;
    let op = opl_types::task::TaskOp::VmBackup { backup: backup.id };
    Ok(Json(create_task(&cluster, op, vm.id, node).await?))
}

#[derive(Deserialize)]
struct Cancel {
    vm: uuid::Uuid,
}

async fn live_restore_cancel(Json(b): Json<Cancel>) -> Result<Json<uuid::Uuid>, Failure> {
    let cluster = member()?;
    opl_api::request_live_restore_cancel(&cluster, b.vm)
        .await
        .map(Json)
        .map_err(bad)
}
