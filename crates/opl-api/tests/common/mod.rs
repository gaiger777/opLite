//! 노드 하나짜리 클러스터, Mock 하이퍼바이저, reconciler, Task 실행기 위에 API 서버를 띄우는 하네스.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use openraft::{BasicNode, ServerState};
use opl_api::{AppState, TaskRunner, TaskRunnerConfig, hash_password};
use opl_cluster::memnet::MemRouter;
use opl_cluster::types::NodeId;
use opl_cluster::{Cluster, ClusterConfig};
use opl_driver::{
    Capacity, DirStorage, DriverError, Hypervisor, MockHypervisor, Storage, VolumeInfo,
};
use opl_reconciler::{Reconciler, ReconcilerConfig, RestartBackoff};
use opl_types::auth::{Role, UserSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, StoragePoolSpec};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

pub const WAIT: Duration = Duration::from_secs(10);
pub const ADMIN_PASSWORD: &str = "admin-password";

pub struct Env {
    pub cluster: Arc<Cluster>,
    pub hv: Arc<MockHypervisor>,
    pub node: Uuid,
    pub base: String,
    pub dir: TempDir,
    /// API 서버의 상태. 예약 작업처럼 상태를 받는 반복 작업을 띄울 때 쓴다.
    pub state: Arc<AppState>,
    reconciler: Option<Reconciler>,
    runner: Option<TaskRunner>,
    server: tokio::task::JoinHandle<()>,
}

pub fn reconciler_config(node: Uuid) -> ReconcilerConfig {
    ReconcilerConfig {
        node,
        resync_interval: Duration::from_secs(3600),
        acpi_timeout: Duration::from_millis(300),
        retry_interval: Duration::from_millis(100),
        restart: RestartBackoff {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            max_failures: 3,
            window: Duration::from_secs(60),
        },
    }
}

/// API 서버와 같은 업로드 디렉터리를 쓰는 Task 실행기 설정. VM 상태 디렉터리는 `{dir}/state`다.
pub fn runner_config(node: Uuid, dir: &Path) -> TaskRunnerConfig {
    let mut config = TaskRunnerConfig::new(node);
    config.poll_interval = Duration::from_millis(20);
    config.upload_dir = Some(dir.join("uploads"));
    config.state_dir = Some(dir.join("state"));
    config
}

impl Env {
    pub async fn start() -> Self {
        Self::start_with(|_| {}, |_| async {}).await
    }

    /// `tweak`로 reconciler 설정을 바꾸고, `before_runner`로 Task 실행기가 뜨기 전에 저장소를 채운다.
    pub async fn start_with<F, Fut>(
        tweak: impl FnOnce(&mut ReconcilerConfig),
        before_runner: F,
    ) -> Self
    where
        F: FnOnce(Arc<Cluster>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let dir = TempDir::new().unwrap();
        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let mut config = ClusterConfig::new(id, dir.path().join("cluster"));
        config.raft = openraft::Config {
            heartbeat_interval: 50,
            election_timeout_min: 300,
            election_timeout_max: 600,
            ..Default::default()
        };
        let cluster = Arc::new(Cluster::open(config, router.network(id)).await.unwrap());
        router.register(id, cluster.raft().clone());
        cluster
            .initialize(BTreeMap::from([(id, BasicNode::new(id.to_string()))]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(WAIT))
            .metrics(|m| m.state == ServerState::Leader, "leader")
            .await
            .unwrap();
        let node = id.0;

        let mut me = Resource::new(
            "node1",
            NodeSpec {
                address: "127.0.0.1:0".into(),
                maintenance: false,
            },
        );
        me.id = node;
        me.status = NodeStatus {
            online: true,
            cpus: 4,
            memory_mib: 8192,
            hypervisor: "mock".into(),
            bridges: vec![],
        };
        cluster.create(&me).await.unwrap();
        let admin = Resource::new(
            "admin",
            UserSpec {
                password_hash: hash_password(ADMIN_PASSWORD.into()).await.unwrap(),
                role: Role::Admin,
            },
        );
        cluster.create(&admin).await.unwrap();
        seed_cluster_info(&cluster).await;
        before_runner(cluster.clone()).await;

        let hv = Arc::new(MockHypervisor::default());
        let mut rc = reconciler_config(node);
        tweak(&mut rc);
        let hv_dyn: Arc<dyn Hypervisor> = hv.clone();
        let reconciler = Reconciler::spawn(cluster.clone(), hv_dyn.clone(), rc);
        let storage: Arc<dyn Storage> = Arc::new(DirStorage::new("qemu-img"));
        let runner = TaskRunner::spawn(
            cluster.clone(),
            storage,
            hv_dyn.clone(),
            reconciler.handle(),
            runner_config(node, dir.path()),
        )
        .await;
        let state = Arc::new(AppState::new(
            cluster.clone(),
            node,
            hv_dyn,
            dir.path().join("uploads"),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = opl_api::router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Env {
            cluster,
            hv,
            node,
            base,
            dir,
            state,
            reconciler: Some(reconciler),
            runner: Some(runner),
            server,
        }
    }

    /// 인증 없는 클라이언트
    pub fn anonymous(&self) -> Client {
        Client {
            base: self.base.clone(),
            http: reqwest::Client::new(),
            token: None,
        }
    }

    /// 로그인해 API 토큰을 만들고, 그 토큰을 쓰는 클라이언트를 돌려준다.
    pub async fn login(&self, user: &str, password: &str) -> Client {
        login_at(&self.base, user, password).await
    }

    pub async fn admin(&self) -> Client {
        self.login("admin", ADMIN_PASSWORD).await
    }

    /// 관리자로 사용자를 만들고 그 사용자로 로그인한다.
    pub async fn user(&self, name: &str, role: &str) -> Client {
        let admin = self.admin().await;
        let (status, body) = admin
            .post(
                "/users",
                json!({ "name": name, "password": "password-1", "role": role }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        self.login(name, "password-1").await
    }

    pub async fn stop_runner(&mut self) {
        self.runner.take().unwrap().shutdown().await;
    }

    pub async fn start_runner(&mut self) {
        self.start_runner_with(Arc::new(DirStorage::new("qemu-img")))
            .await;
    }

    /// `storage`를 쓰는 Task 실행기를 띄운다.
    pub async fn start_runner_with(&mut self, storage: Arc<dyn Storage>) {
        self.start_runner_config(storage, |_| {}).await;
    }

    /// `storage`를 쓰고 `tweak`으로 설정을 바꾼 Task 실행기를 띄운다.
    pub async fn start_runner_config(
        &mut self,
        storage: Arc<dyn Storage>,
        tweak: impl FnOnce(&mut TaskRunnerConfig),
    ) {
        let mut config = runner_config(self.node, self.dir.path());
        tweak(&mut config);
        self.runner = Some(
            TaskRunner::spawn(
                self.cluster.clone(),
                storage,
                self.hv.clone(),
                self.reconciler.as_ref().unwrap().handle(),
                config,
            )
            .await,
        );
    }

    pub async fn pid(&self, id: Uuid) -> Option<u32> {
        self.hv.inspect(id).await.unwrap().map(|s| s.pid)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.server.abort();
    }
}

#[derive(Clone)]
pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
    pub token: Option<String>,
}

impl Client {
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self
            .http
            .request(method, format!("{}/api/v1{path}", self.base));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    pub async fn send(&self, r: reqwest::RequestBuilder) -> (StatusCode, Value) {
        let resp = r.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, body)
    }

    pub async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::GET, path)).await
    }

    pub async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::POST, path).json(&body))
            .await
    }

    pub async fn put(&self, path: &str, body: Value, if_match: Option<u64>) -> (StatusCode, Value) {
        let mut r = self.req(reqwest::Method::PUT, path).json(&body);
        if let Some(rev) = if_match {
            r = r.header("if-match", rev.to_string());
        }
        self.send(r).await
    }

    pub async fn delete(&self, path: &str) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::DELETE, path)).await
    }

    pub async fn upload(&self, path: &str, body: Vec<u8>) -> (StatusCode, Value) {
        self.send(self.req(reqwest::Method::POST, path).body(body))
            .await
    }

    /// Task가 끝날 때까지 기다려 마지막 모습을 돌려준다.
    pub async fn wait_task(&self, task: &Value) -> Value {
        let id = task["id"].as_str().expect("task id");
        let deadline = Instant::now() + WAIT;
        loop {
            let (status, task) = self.get(&format!("/tasks/{id}")).await;
            assert_eq!(status, StatusCode::OK, "{task}");
            let state = task["status"]["state"].as_str().unwrap_or("pending");
            if state == "succeeded" || state == "failed" {
                return task;
            }
            assert!(Instant::now() < deadline, "task did not finish: {task}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 동작을 요청하고 Task가 끝날 때까지 기다린다.
    pub async fn act(&self, vm: &str, action: &str) -> Value {
        let (status, task) = self
            .post(&format!("/vms/{vm}/actions/{action}"), Value::Null)
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        self.wait_task(&task).await
    }
}

/// `base`의 API에 로그인해 API 토큰을 만들고, 그 토큰을 쓰는 클라이언트를 돌려준다.
pub async fn login_at(base: &str, user: &str, password: &str) -> Client {
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": user, "password": password }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "login as {user}");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let resp = http
        .post(format!("{base}/api/v1/tokens"))
        .header("cookie", cookie)
        .json(&json!({ "name": format!("{user}-{}", Uuid::new_v4().simple()) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body: Value = resp.json().await.unwrap();
    Client {
        base: base.to_string(),
        http,
        token: Some(body["secret"].as_str().unwrap().to_string()),
    }
}

/// 클러스터 정보(이름 `test`, 새 CA)를 저장소에 만든다. 클러스터를 만들 때 opld가 하는 일이다.
pub async fn seed_cluster_info(cluster: &Cluster) {
    let ca = opl_peer::pki::ClusterCa::generate("test").unwrap();
    let info = Resource::new(
        "test",
        opl_types::cluster::ClusterSpec {
            ca_cert_pem: ca.cert_pem,
            ca_key_pem: ca.key_pem,
        },
    );
    cluster.create(&info).await.unwrap();
}

/// 디스크 없는 작은 VM 생성 요청
pub fn vm_request(name: &str) -> Value {
    json!({
        "name": name,
        "spec": {
            "cpu": { "cores": 1 },
            "memory_mib": 256,
            "machine": "virt",
            "nics": [ { "network": "user", "model": "virtio" } ]
        }
    })
}

/// 파일 없이 볼륨 호출을 받아 기록하는 스토리지. `pause()` 뒤 처음 오는 `snapshot_revert`나
/// `resize_volume`은 `resume()`까지 멈춘다. 그사이 테스트가 다른 요청을 끼워 넣는다.
#[derive(Default)]
pub struct FakeStorage {
    calls: std::sync::Mutex<Vec<(&'static str, Uuid)>>,
    sizes: std::sync::Mutex<std::collections::HashMap<Uuid, u64>>,
    paused: std::sync::atomic::AtomicBool,
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl FakeStorage {
    pub fn calls(&self) -> Vec<(&'static str, Uuid)> {
        self.calls.lock().unwrap().clone()
    }

    /// `volume_info`가 알릴 크기
    pub fn set_size(&self, id: Uuid, size_bytes: u64) {
        self.sizes.lock().unwrap().insert(id, size_bytes);
    }

    pub fn pause(&self) {
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// 멈춘 호출이 들어올 때까지 기다린다.
    pub async fn reached(&self) {
        tokio::time::timeout(WAIT, self.reached.notified())
            .await
            .expect("paused call");
    }

    pub fn resume(&self) {
        self.release.notify_one();
    }

    async fn record(&self, call: &'static str, id: Uuid) {
        self.calls.lock().unwrap().push((call, id));
        if self.paused.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait::async_trait]
impl Storage for FakeStorage {
    async fn create_volume(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.set_size(id, size_bytes);
        Ok(PathBuf::new())
    }
    async fn import_volume(
        &self,
        _: &StoragePoolSpec,
        _: Uuid,
        _: &Path,
        _: SourceFormat,
        _: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        Err(DriverError::InvalidSpec("not supported".into()))
    }
    async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
        Err(DriverError::InvalidSpec("not supported".into()))
    }
    async fn delete_volume(
        &self,
        _: &StoragePoolSpec,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        Ok(())
    }
    async fn volume_info(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let size = self.sizes.lock().unwrap().get(&id).copied();
        let virtual_bytes = size.ok_or(DriverError::VolumeNotFound(id))?;
        Ok(VolumeInfo {
            virtual_bytes,
            allocated_bytes: 0,
        })
    }
    async fn capacity(&self, _: &StoragePoolSpec) -> Result<Capacity, DriverError> {
        Ok(Capacity {
            total_bytes: 1 << 40,
            available_bytes: 1 << 40,
        })
    }
    fn supports_snapshots(&self, format: ImageFormat) -> bool {
        format == ImageFormat::Qcow2
    }
    async fn snapshot_create(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("create", id).await;
        Ok(())
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("revert", id).await;
        Ok(())
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        self.record("delete", id).await;
        Ok(())
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePoolSpec,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Ok(vec![])
    }
    async fn resize_volume(
        &self,
        _: &StoragePoolSpec,
        id: Uuid,
        _: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.record("resize", id).await;
        self.set_size(id, size_bytes);
        Ok(())
    }
    async fn copy_volume(
        &self,
        _: &StoragePoolSpec,
        src_id: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePoolSpec,
        dst_id: Uuid,
    ) -> Result<(), DriverError> {
        self.record("copy", dst_id).await;
        let size = self.sizes.lock().unwrap().get(&src_id).copied();
        self.set_size(dst_id, size.ok_or(DriverError::VolumeNotFound(src_id))?);
        Ok(())
    }
}
