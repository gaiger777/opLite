//! 파생 객체의 주인(계획 62 설계 §7): 볼륨은 붙은 VM 또는 풀, 스냅샷은 VM, 백업은 VM(지워졌으면 `vm_spec.folder`),
//! Task는 op의 대상, 가져오기는 루트에 둔 가져오기 객체다. 목록 거르기는 상태를 한 번 떠 쓰고, `/watch`는 연결마다
//! 하나를 두고 이벤트로 고친다(이벤트 순서대로라 그 리비전의 주인이다).

use std::collections::HashMap;

use opl_cluster::Cluster;
use opl_cluster::watch::WatchEvent;
use opl_types::authz::Privilege;
use opl_types::authz_eval::AuthzTarget;
use opl_types::backup::BackupSpec;
use opl_types::network::NetworkSpec;
use opl_types::resource::{Kind, ResourceSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::task::{TaskOp, TaskSpec};
use opl_types::vm::VmSpec;
use uuid::Uuid;

use crate::error::ApiError;

/// 확인할 대상과, 그것을 "볼 수 있다"고 하는 권한(대상이 없는 Task는 루트 `cluster.read`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owned {
    pub target: AuthzTarget,
    pub read: Privilege,
}

impl Owned {
    pub fn object(target: AuthzTarget) -> Self {
        Owned {
            target,
            read: Privilege::SystemRead,
        }
    }

    /// 클러스터 객체(노드·Ceph 등)에 딸린 것: 루트 `cluster.read`로 본다.
    pub fn cluster() -> Self {
        Owned {
            target: AuthzTarget::Root,
            read: Privilege::ClusterRead,
        }
    }
}

/// 주인을 찾는 표. 상태 하나(또는 이벤트를 따라 고친 것)를 담는다.
#[derive(Debug, Default, Clone)]
pub struct Owners {
    /// VM id → 폴더
    vms: HashMap<Uuid, Option<Uuid>>,
    /// 볼륨 이름 → 그 볼륨을 디스크로 쓰는 VM id
    vm_of_volume: HashMap<String, Uuid>,
    /// VM id → 그 VM 디스크의 볼륨 이름(VM이 바뀔 때 옛 연결을 지우려고)
    disks: HashMap<Uuid, Vec<String>>,
    /// 풀 이름 → (id, 폴더)
    pools: HashMap<String, (Uuid, Option<Uuid>)>,
    /// 볼륨 id → (이름, 풀 이름)
    volumes: HashMap<Uuid, (String, String)>,
    /// 백업 id → (VM id, 만든 때 VM의 폴더)
    backups: HashMap<Uuid, (Uuid, Option<Uuid>)>,
    /// 가져오기 id
    imports: std::collections::HashSet<Uuid>,
    /// 네트워크 id → 폴더
    networks: HashMap<Uuid, Option<Uuid>>,
    /// 라우터·유동 IP·부하 분산기·네트워크 옮기기 id → (종류, 폴더) (계획 66·67 — 네트워크 폴더의 객체)
    routing: HashMap<Uuid, (Kind, Option<Uuid>)>,
}

impl Owners {
    /// 이 노드의 지금 상태에서 뜬다.
    pub fn load(cluster: &Cluster) -> Result<Self, ApiError> {
        let mut o = Owners::default();
        for vm in cluster.list::<VmSpec>()?.1 {
            o.put_vm(vm.id, &vm.spec);
        }
        for p in cluster.list::<StoragePoolSpec>()?.1 {
            o.pools.insert(p.name, (p.id, p.spec.folder));
        }
        for n in cluster.list::<NetworkSpec>()?.1 {
            o.networks.insert(n.id, n.spec.folder);
        }
        for r in cluster.list::<opl_types::router::RouterSpec>()?.1 {
            o.routing.insert(r.id, (Kind::Router, r.spec.folder));
        }
        for f in cluster.list::<opl_types::floating_ip::FloatingIpSpec>()?.1 {
            o.routing.insert(f.id, (Kind::FloatingIp, f.spec.folder));
        }
        for l in cluster
            .list::<opl_types::load_balancer::LoadBalancerSpec>()?
            .1
        {
            o.routing.insert(l.id, (Kind::LoadBalancer, l.spec.folder));
        }
        for m in cluster
            .list::<opl_types::network_migration::NetworkMigrationSpec>()?
            .1
        {
            o.routing
                .insert(m.id, (Kind::NetworkMigration, m.spec.folder));
        }
        for v in cluster.list::<VolumeSpec>()?.1 {
            o.volumes.insert(v.id, (v.name, v.spec.pool));
        }
        for b in cluster.list::<BackupSpec>()?.1 {
            o.backups.insert(b.id, (b.spec.vm, b.spec.vm_spec.folder));
        }
        for i in cluster.list::<opl_types::vm_import::VmImportSpec>()?.1 {
            o.imports.insert(i.id);
        }
        Ok(o)
    }

    /// 볼륨 하나의 주인(붙은 VM을 찾으려고 VM을 훑는다)
    pub fn around_volume(
        cluster: &Cluster,
        name: &str,
        pool: &str,
    ) -> Result<AuthzTarget, ApiError> {
        let mut o = Owners::default();
        if let Some(vm) = cluster
            .list::<VmSpec>()?
            .1
            .into_iter()
            .find(|vm| vm.spec.disks.iter().any(|d| d.volume == name))
        {
            o.put_vm(vm.id, &vm.spec);
        }
        if let Some(p) = cluster.get_by_name::<StoragePoolSpec>(pool)? {
            o.pools.insert(p.name, (p.id, p.spec.folder));
        }
        Ok(o.volume(name, pool))
    }

    /// Task 하나의 주인을 찾는 데 드는 것만 뜬다.
    pub fn around_task(cluster: &Cluster, task: &TaskSpec) -> Result<Self, ApiError> {
        let mut o = Owners::default();
        let t = task.target;
        let mut vms = vec![t];
        if let Some(v) = cluster.get::<VolumeSpec>(t)? {
            if let Some(vm) = cluster
                .list::<VmSpec>()?
                .1
                .into_iter()
                .find(|vm| vm.spec.disks.iter().any(|d| d.volume == v.name))
            {
                o.put_vm(vm.id, &vm.spec);
            }
            if let Some(p) = cluster.get_by_name::<StoragePoolSpec>(&v.spec.pool)? {
                o.pools.insert(p.name, (p.id, p.spec.folder));
            }
            o.volumes.insert(v.id, (v.name, v.spec.pool));
        }
        let mut backups = vec![t];
        if let TaskOp::BackupRestore { backup, .. } = &task.op {
            backups.push(*backup);
        }
        for id in backups {
            if let Some(b) = cluster.get::<BackupSpec>(id)? {
                vms.push(b.spec.vm);
                o.backups.insert(b.id, (b.spec.vm, b.spec.vm_spec.folder));
            }
        }
        for id in vms {
            if let Some(vm) = cluster.get::<VmSpec>(id)? {
                o.put_vm(vm.id, &vm.spec);
            }
        }
        Ok(o)
    }

    fn put_vm(&mut self, id: Uuid, spec: &VmSpec) {
        self.remove_vm(id);
        self.vms.insert(id, spec.folder);
        let names: Vec<String> = spec.disks.iter().map(|d| d.volume.clone()).collect();
        for n in &names {
            self.vm_of_volume.insert(n.clone(), id);
        }
        self.disks.insert(id, names);
    }

    fn remove_vm(&mut self, id: Uuid) {
        self.vms.remove(&id);
        for n in self.disks.remove(&id).unwrap_or_default() {
            if self.vm_of_volume.get(&n) == Some(&id) {
                self.vm_of_volume.remove(&n);
            }
        }
    }

    /// VM `id`(지워졌으면 루트에 있는 VM으로 본다)
    pub fn vm(&self, id: Uuid) -> AuthzTarget {
        AuthzTarget::Object {
            kind: Kind::Vm,
            id,
            folder: self.vms.get(&id).copied().flatten(),
        }
    }

    pub fn pool(&self, name: &str) -> AuthzTarget {
        match self.pools.get(name) {
            Some((id, folder)) => AuthzTarget::Object {
                kind: Kind::StoragePool,
                id: *id,
                folder: *folder,
            },
            None => AuthzTarget::Root,
        }
    }

    /// 볼륨: 디스크로 붙은 VM, 아니면 풀
    pub fn volume(&self, name: &str, pool: &str) -> AuthzTarget {
        match self.vm_of_volume.get(name) {
            Some(vm) => self.vm(*vm),
            None => self.pool(pool),
        }
    }

    /// 백업: VM이 있으면 지금 VM, 지워졌으면 만든 때 VM의 폴더
    pub fn backup(&self, vm: Uuid, folder: Option<Uuid>) -> AuthzTarget {
        AuthzTarget::Object {
            kind: Kind::Vm,
            id: vm,
            folder: match self.vms.get(&vm) {
                Some(f) => *f,
                None => folder,
            },
        }
    }

    fn backup_id(&self, id: Uuid) -> Option<AuthzTarget> {
        self.backups.get(&id).map(|(vm, f)| self.backup(*vm, *f))
    }

    fn volume_id(&self, id: Uuid) -> Option<AuthzTarget> {
        self.volumes
            .get(&id)
            .map(|(name, pool)| self.volume(name, pool))
    }

    /// 가져오기 객체(폴더 없음 — 루트의 `vm.create`·`system.read`로 다룬다)
    pub fn import(id: Uuid) -> AuthzTarget {
        AuthzTarget::Object {
            kind: Kind::VmImport,
            id,
            folder: None,
        }
    }

    /// Task: op의 대상. 노드·Ceph·HA 기록처럼 객체가 없는 Task는 루트 `cluster.read`다.
    pub fn task(&self, spec: &TaskSpec) -> Owned {
        let t = spec.target;
        let object = match &spec.op {
            TaskOp::VmStart
            | TaskOp::VmStop
            | TaskOp::VmForceStop
            | TaskOp::VmReboot
            | TaskOp::SnapshotCreate { .. }
            | TaskOp::SnapshotRevert { .. }
            | TaskOp::SnapshotDelete { .. }
            | TaskOp::VmClone { .. }
            | TaskOp::VmMigrate { .. }
            | TaskOp::VmHaRecover { .. }
            | TaskOp::VmBackup { .. }
            | TaskOp::VmHotplug
            | TaskOp::VmReplicate { .. }
            | TaskOp::VmUntemplate
            | TaskOp::VmFirmwareReset { .. } => Some(self.vm(t)),
            TaskOp::VolumeCreate
            | TaskOp::VolumeImport { .. }
            | TaskOp::VolumeDownload { .. }
            | TaskOp::VolumeDelete
            | TaskOp::VolumeFlatten
            | TaskOp::VolumeResize { .. } => {
                Some(self.volume_id(t).unwrap_or(AuthzTarget::Object {
                    kind: Kind::Volume,
                    id: t,
                    folder: None,
                }))
            }
            TaskOp::BackupDelete | TaskOp::BackupReplicate { .. } => self.backup_id(t),
            // 새 VM이 아직 없으면 되살리는 백업이 주인이다.
            TaskOp::BackupRestore { backup, .. } => Some(if self.vms.contains_key(&t) {
                self.vm(t)
            } else {
                self.backup_id(*backup).unwrap_or_else(|| self.vm(t))
            }),
            TaskOp::VmImport { import, .. } => Some(if self.vms.contains_key(&t) {
                self.vm(t)
            } else {
                Self::import(*import)
            }),
            TaskOp::VmImportDelete => Some(Self::import(t)),
            _ => None,
        };
        match object {
            Some(target) => Owned::object(target),
            None => Owned::cluster(),
        }
    }

    /// 이 표가 아는 VM·네트워크·풀·라우터·유동 IP 자체의 대상(이벤트 앞의 폴더). 모르면 None
    pub fn stored(&self, kind: Kind, id: Uuid) -> Option<AuthzTarget> {
        let folder = match kind {
            Kind::Vm => *self.vms.get(&id)?,
            Kind::Network => *self.networks.get(&id)?,
            Kind::StoragePool => self.pools.values().find(|(p, _)| *p == id)?.1,
            Kind::Router | Kind::FloatingIp | Kind::LoadBalancer | Kind::NetworkMigration => {
                self.routing.get(&id).filter(|(k, _)| *k == kind)?.1
            }
            _ => return None,
        };
        Some(AuthzTarget::Object { kind, id, folder })
    }

    /// 커밋 이벤트로 표를 고친다(그 리비전의 주인). 다른 객체의 주인이 바뀔 수 있으면(VM의 폴더·디스크, 풀의 폴더,
    /// VM·풀 지우기) 참이다.
    pub fn apply(&mut self, event: &WatchEvent) -> bool {
        match event {
            WatchEvent::Put { kind, resource } => {
                let id = resource.id;
                match kind {
                    Kind::Vm => {
                        if let Ok(spec) = serde_json::from_value::<VmSpec>(resource.spec.clone()) {
                            let disks: Vec<String> =
                                spec.disks.iter().map(|d| d.volume.clone()).collect();
                            let changed = self.vms.get(&id) != Some(&spec.folder)
                                || self.disks.get(&id) != Some(&disks);
                            self.put_vm(id, &spec);
                            return changed;
                        }
                    }
                    Kind::Network => {
                        self.networks.insert(id, folder_of(&resource.spec));
                    }
                    Kind::Router
                    | Kind::FloatingIp
                    | Kind::LoadBalancer
                    | Kind::NetworkMigration => {
                        self.routing.insert(id, (*kind, folder_of(&resource.spec)));
                    }
                    Kind::StoragePool => {
                        let folder = folder_of(&resource.spec);
                        let old = self.pools.insert(resource.name.clone(), (id, folder));
                        return old.map(|(_, f)| f) != Some(folder);
                    }
                    Kind::Volume => {
                        if let Some(pool) = resource.spec.get("pool").and_then(|p| p.as_str()) {
                            self.volumes
                                .insert(id, (resource.name.clone(), pool.to_string()));
                        }
                    }
                    Kind::Backup => {
                        if let Some(vm) = resource
                            .spec
                            .get("vm")
                            .and_then(|v| v.as_str())
                            .and_then(|v| Uuid::try_parse(v).ok())
                        {
                            let folder = resource.spec.get("vm_spec").and_then(folder_of);
                            self.backups.insert(id, (vm, folder));
                        }
                    }
                    Kind::VmImport => {
                        self.imports.insert(id);
                    }
                    _ => {}
                }
            }
            WatchEvent::Deleted { kind, resource, .. } => {
                let id = resource.id;
                match kind {
                    Kind::Vm => {
                        self.remove_vm(id);
                        return true;
                    }
                    Kind::Network => {
                        self.networks.remove(&id);
                    }
                    Kind::Router
                    | Kind::FloatingIp
                    | Kind::LoadBalancer
                    | Kind::NetworkMigration => {
                        self.routing.remove(&id);
                    }
                    Kind::StoragePool => {
                        self.pools.remove(&resource.name);
                        return true;
                    }
                    Kind::Volume => {
                        self.volumes.remove(&id);
                    }
                    Kind::Backup => {
                        self.backups.remove(&id);
                    }
                    Kind::VmImport => {
                        self.imports.remove(&id);
                    }
                    _ => {}
                }
            }
            WatchEvent::Reset { .. } => {}
        }
        false
    }

    /// 커밋 이벤트의 리소스(JSON)가 무엇에 딸렸는지. 폴더를 따르지 않는 종류는 None이다(종류 단위로 본다).
    pub fn of_event(
        &self,
        kind: Kind,
        resource: &opl_cluster::types::RawResource,
    ) -> Option<Owned> {
        let spec = &resource.spec;
        let target = match kind {
            Kind::Vm
            | Kind::Network
            | Kind::StoragePool
            | Kind::Router
            | Kind::FloatingIp
            | Kind::LoadBalancer
            | Kind::NetworkMigration => AuthzTarget::Object {
                kind,
                id: resource.id,
                folder: folder_of(spec),
            },
            Kind::Volume => {
                let pool = spec
                    .get("pool")
                    .and_then(|p| p.as_str())
                    .unwrap_or_default();
                self.volume(&resource.name, pool)
            }
            Kind::Snapshot => match uuid_at(spec, "vm") {
                Some(vm) => self.vm(vm),
                None => AuthzTarget::Root,
            },
            Kind::Backup => match uuid_at(spec, "vm") {
                Some(vm) => self.backup(vm, spec.get("vm_spec").and_then(folder_of)),
                None => AuthzTarget::Root,
            },
            Kind::VmImport => Self::import(resource.id),
            Kind::Task => {
                return Some(match serde_json::from_value::<TaskSpec>(spec.clone()) {
                    Ok(task) => self.task(&task),
                    Err(_) => Owned::cluster(),
                });
            }
            Kind::Folder => AuthzTarget::Folder(resource.id),
            _ => return None,
        };
        Some(Owned::object(target))
    }
}

fn uuid_at(spec: &serde_json::Value, field: &str) -> Option<Uuid> {
    spec.get(field)
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::try_parse(v).ok())
}

fn folder_of(spec: &serde_json::Value) -> Option<Uuid> {
    uuid_at(spec, "folder")
}

/// 네트워크 이름의 대상(없는 네트워크는 루트)
pub fn network_target(cluster: &Cluster, name: &str) -> Result<AuthzTarget, ApiError> {
    Ok(match cluster.get_by_name::<NetworkSpec>(name)? {
        Some(n) => AuthzTarget::Object {
            kind: Kind::Network,
            id: n.id,
            folder: n.spec.folder,
        },
        None => AuthzTarget::Root,
    })
}

/// 풀 이름의 대상(없는 풀은 루트)
pub fn pool_target(cluster: &Cluster, name: &str) -> Result<AuthzTarget, ApiError> {
    Ok(match cluster.get_by_name::<StoragePoolSpec>(name)? {
        Some(p) => AuthzTarget::Object {
            kind: StoragePoolSpec::KIND,
            id: p.id,
            folder: p.spec.folder,
        },
        None => AuthzTarget::Root,
    })
}
