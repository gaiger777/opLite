//! 폴더·객체 권한의 평가(계획 62 설계 §7). 모든 API 노드가 자기 저장소에서 색인을 만들고 리더에 아무것도 묻지 않는다.
//!
//! - **세대**: 폴더·grant·역할·그룹·사용자·토큰이 커밋되면(또는 스냅샷 설치·구독 끊김) `generation`이 오른다. 확인
//!   때마다 이 노드의 변경 구독에서 쌓인 것을 기다리지 않고 꺼내 보므로, 이 노드에 적용된 커밋은 다음 확인부터 반영된다.
//!   VM 이동은 세대를 올리지 않는다(객체의 폴더는 확인할 때 읽는다).
//! - **색인**: 세대마다 한 번 상태를 떠([`AuthzSnapshot`]) 주체별 [`EffectiveIndex`]를 처음 요청에서 만든다. 주체 1024개
//!   LRU이고, 만들기는 잠금 밖에서 한다.
//! - **시행**: 경로표(`crate::authz_routes`)의 미들웨어가 경로의 객체를 찾아 [`Authz::require`]를 부르고, 목록·watch·
//!   지표·경보·Task는 [`Authz::filter`]·[`Owners`]로 거른다. 루트 종류는 `AppState::authorize`(루트 확인)다.

mod index;
mod owners;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use opl_cluster::Cluster;
use opl_cluster::watch::{Watch, WatchEvent};
use opl_types::auth::{ApiTokenSpec, UserSpec};
use opl_types::authz::{AccessRoleSpec, GrantSpec, GroupSpec, PrivSet, Privilege, TokenScope};
use opl_types::authz_eval::{AuthzSnapshot, AuthzTarget, FolderNode, UserEntry};
use opl_types::folder::{Folder, FolderSpec};
use opl_types::network::Network;
use opl_types::resource::{Kind, ResourceSpec};
use opl_types::storage::StoragePool;
use opl_types::vm::Vm;
use uuid::Uuid;

use index::Lru;
pub use index::{EffectiveIndex, Layers};
pub use owners::{Owned, Owners, network_target, pool_target};

use crate::auth::Principal;
use crate::error::ApiError;

/// 주체별 색인 캐시의 크기
pub const INDEX_CACHE: usize = 1024;

/// 권한을 묻는 주체: 사용자와, 토큰 요청이면 그 토큰(범위를 찾는다)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Subject {
    pub user: Uuid,
    pub token: Option<Uuid>,
}

impl From<&Principal> for Subject {
    /// 토큰 요청이면 토큰의 범위까지 본다.
    fn from(p: &Principal) -> Self {
        Subject {
            user: p.user,
            token: p.token,
        }
    }
}

/// 권한을 확인할 수 있는 것. 폴더를 따르는 객체는 자기 폴더를, 폴더는 자기를 대상으로 한다.
pub trait AuthzObject {
    fn authz_target(&self) -> AuthzTarget;

    /// 읽을 수 없을 때의 404. 없는 객체와 같은 답이어야 존재를 드러내지 않는다.
    fn not_found(&self) -> ApiError {
        ApiError::not_found("not found")
    }

    /// "볼 수 있다"의 권한. 객체는 `system.read`, 객체가 없는 Task 같은 클러스터 것은 `cluster.read`다.
    fn read_privilege(&self) -> Privilege {
        Privilege::SystemRead
    }
}

impl AuthzObject for Owned {
    fn authz_target(&self) -> AuthzTarget {
        self.target
    }

    fn read_privilege(&self) -> Privilege {
        self.read
    }
}

/// 찾은 객체와 그 404 문구(경로표 미들웨어가 쓴다)
pub struct Found {
    pub owned: Owned,
    pub missing: String,
}

impl AuthzObject for Found {
    fn authz_target(&self) -> AuthzTarget {
        self.owned.target
    }

    fn not_found(&self) -> ApiError {
        ApiError::not_found(self.missing.clone())
    }

    fn read_privilege(&self) -> Privilege {
        self.owned.read
    }
}

impl AuthzObject for AuthzTarget {
    fn authz_target(&self) -> AuthzTarget {
        *self
    }
}

impl<T: AuthzObject + ?Sized> AuthzObject for &T {
    fn authz_target(&self) -> AuthzTarget {
        (**self).authz_target()
    }

    fn not_found(&self) -> ApiError {
        (**self).not_found()
    }

    fn read_privilege(&self) -> Privilege {
        (**self).read_privilege()
    }
}

/// 리소스 이름으로 찾지 못한 때와 같은 문구(`resources::found`)
fn missing<S: ResourceSpec>(name: &str) -> ApiError {
    ApiError::not_found(format!("{} {name:?} not found", S::KIND.as_str()))
}

macro_rules! object_in_folder {
    ($($ty:ty),*) => {$(
        impl AuthzObject for $ty {
            fn authz_target(&self) -> AuthzTarget {
                AuthzTarget::Object {
                    kind: <<$ty as HasSpec>::Spec as ResourceSpec>::KIND,
                    id: self.id,
                    folder: self.spec.folder,
                }
            }

            fn not_found(&self) -> ApiError {
                missing::<<$ty as HasSpec>::Spec>(&self.name)
            }
        }
    )*};
}

/// 리소스 별칭에서 스펙 타입을 꺼내는 도우미
trait HasSpec {
    type Spec: ResourceSpec;
}

impl<S: ResourceSpec> HasSpec for opl_types::resource::Resource<S, S::Status> {
    type Spec = S;
}

object_in_folder!(Vm, Network, StoragePool);

impl AuthzObject for Folder {
    fn authz_target(&self) -> AuthzTarget {
        AuthzTarget::Folder(self.id)
    }

    fn not_found(&self) -> ApiError {
        missing::<FolderSpec>(&self.name)
    }
}

/// 세대 하나의 상태: 평가 스냅샷과 토큰(주인, 범위)
struct Shared {
    generation: u64,
    snap: AuthzSnapshot,
    tokens: HashMap<Uuid, (Uuid, Option<TokenScope>)>,
}

/// 이 노드의 권한 평가기
pub struct Authz {
    cluster: Arc<Cluster>,
    generation: AtomicU64,
    /// 세대를 올릴 커밋을 보는 구독. 없으면 다음 확인이 새로 구독하고 세대를 올린다.
    feed: Mutex<Option<Watch>>,
    shared: Mutex<Option<Arc<Shared>>>,
    cache: Mutex<Lru<Subject, Arc<EffectiveIndex>>>,
}

/// 세대를 올리는 종류
fn bumps(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Folder | Kind::Grant | Kind::AccessRole | Kind::Group | Kind::User | Kind::ApiToken
    )
}

impl Authz {
    pub fn new(cluster: Arc<Cluster>) -> Self {
        Authz {
            cluster,
            generation: AtomicU64::new(0),
            feed: Mutex::new(None),
            shared: Mutex::new(None),
            cache: Mutex::new(Lru::new(INDEX_CACHE)),
        }
    }

    /// 지금 세대. 이 노드의 구독에 쌓인 변경을 기다리지 않고 꺼내 보고, 권한에 닿는 것이 있으면 올린다.
    pub fn generation(&self) -> u64 {
        let mut feed = self.feed.lock().unwrap();
        loop {
            let Some(watch) = feed.as_mut() else {
                // 처음이거나 구독이 끊겼다(밀림·스냅샷 기록 밖). 놓친 변경이 있을 수 있으니 세대를 올린다.
                self.generation.fetch_add(1, Ordering::SeqCst);
                *feed = self.cluster.watch(self.cluster.revision()).ok();
                if feed.is_none() {
                    break;
                }
                continue;
            };
            match watch.next().now_or_never() {
                None => break,
                Some(Ok(WatchEvent::Put { kind, .. } | WatchEvent::Deleted { kind, .. })) => {
                    if bumps(kind) {
                        self.generation.fetch_add(1, Ordering::SeqCst);
                    }
                }
                Some(Ok(WatchEvent::Reset { .. })) => {
                    self.generation.fetch_add(1, Ordering::SeqCst);
                }
                Some(Err(_)) => *feed = None,
            }
        }
        self.generation.load(Ordering::SeqCst)
    }

    /// 세대 `generation`의 상태. 그보다 옛것이면 다시 뜬다(잠금 밖에서).
    fn shared(&self, generation: u64) -> Result<Arc<Shared>, ApiError> {
        if let Some(s) = self.shared.lock().unwrap().as_ref()
            && s.generation >= generation
        {
            return Ok(s.clone());
        }
        let s = Arc::new(self.load(generation)?);
        let mut slot = self.shared.lock().unwrap();
        if slot
            .as_ref()
            .is_none_or(|old| old.generation < s.generation)
        {
            *slot = Some(s.clone());
        }
        Ok(s)
    }

    fn load(&self, generation: u64) -> Result<Shared, ApiError> {
        let c = &self.cluster;
        let mut snap = AuthzSnapshot::default();
        for f in c.list::<FolderSpec>()?.1 {
            snap.folders.insert(
                f.id,
                FolderNode {
                    parent: f.spec.parent,
                    kind: f.spec.kind,
                },
            );
        }
        for u in c.list::<UserSpec>()?.1 {
            snap.users.insert(
                u.id,
                UserEntry {
                    role: u.spec.role,
                    realm: u.spec.realm,
                    external_groups: u.spec.external_groups,
                },
            );
        }
        for g in c.list::<GroupSpec>()?.1 {
            snap.groups.insert(g.id, g.spec);
        }
        for r in c.list::<AccessRoleSpec>()?.1 {
            snap.roles.insert(r.id, r.spec.privset().0);
        }
        snap.grants = c
            .list::<GrantSpec>()?
            .1
            .into_iter()
            .map(|g| g.spec)
            .collect();
        let tokens = c
            .list::<ApiTokenSpec>()?
            .1
            .into_iter()
            .map(|t| (t.id, (t.spec.user, t.spec.scope)))
            .collect();
        Ok(Shared {
            generation,
            snap,
            tokens,
        })
    }

    /// 지금 세대의 평가 스냅샷(폴더 트리·사용자·그룹·역할·grant). 유효 권한의 근거를 찾을 때 쓴다.
    pub fn with_snapshot<R>(&self, f: impl FnOnce(&AuthzSnapshot) -> R) -> Result<R, ApiError> {
        let shared = self.shared(self.generation())?;
        Ok(f(&shared.snap))
    }

    /// 주체의 색인. 세대가 같으면 캐시에서 꺼낸다.
    pub fn index(&self, subject: Subject) -> Result<Arc<EffectiveIndex>, ApiError> {
        let generation = self.generation();
        if let Some(hit) = self.cache.lock().unwrap().get(&subject)
            && hit.generation == generation
        {
            return Ok(hit);
        }
        let mut shared = self.shared(generation)?;
        // 이 노드에 막 적용된 사용자·토큰을 구독이 아직 알리지 않았을 수 있다(인증은 저장소를 바로 읽는다). 스냅샷에
        // 없는데 저장소에 있으면 세대를 올려 다시 뜬다.
        let stale = !shared.snap.users.contains_key(&subject.user)
            && self.cluster.get::<UserSpec>(subject.user)?.is_some()
            || subject.token.is_some_and(|t| {
                !shared.tokens.contains_key(&t)
                    && self
                        .cluster
                        .get::<ApiTokenSpec>(t)
                        .is_ok_and(|found| found.is_some())
            });
        if stale {
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            shared = self.shared(generation)?;
        }
        let built = Arc::new(match subject.token {
            None => EffectiveIndex::build(&shared.snap, subject.user, None, shared.generation),
            // 지워졌거나 남의 토큰이면 아무 권한도 없다.
            Some(token) => match shared.tokens.get(&token) {
                Some((owner, scope)) if *owner == subject.user => EffectiveIndex::build(
                    &shared.snap,
                    subject.user,
                    scope.as_ref(),
                    shared.generation,
                ),
                _ => EffectiveIndex::empty(shared.generation),
            },
        });
        self.cache.lock().unwrap().insert(subject, built.clone());
        Ok(built)
    }

    /// 대상에서의 유효 권한
    pub fn effective(
        &self,
        principal: &Principal,
        object: &impl AuthzObject,
    ) -> Result<PrivSet, ApiError> {
        Ok(self
            .index(Subject::from(principal))?
            .get(&object.authz_target()))
    }

    /// `privilege`가 없으면 오류: 그 객체를 읽을 수도 없으면 404(없는 것과 같게), 읽을 수 있으면 403.
    pub fn require(
        &self,
        principal: &Principal,
        privilege: Privilege,
        object: &impl AuthzObject,
    ) -> Result<(), ApiError> {
        let set = self.effective(principal, object)?;
        if set.contains(privilege) {
            Ok(())
        } else if set.contains(object.read_privilege()) {
            Err(ApiError::forbidden())
        } else {
            Err(object.not_found())
        }
    }

    /// 루트(클러스터 객체)에서 `privilege`가 없으면 403
    pub fn require_root(
        &self,
        principal: &Principal,
        privilege: Privilege,
    ) -> Result<(), ApiError> {
        if self
            .effective(principal, &AuthzTarget::Root)?
            .contains(privilege)
        {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }

    /// 어디에서도 `privilege`가 없으면 403(객체를 찾기 전에 막는다)
    pub fn require_anywhere(
        &self,
        principal: &Principal,
        privilege: Privilege,
    ) -> Result<(), ApiError> {
        if self.index(Subject::from(principal))?.anywhere(privilege) {
            Ok(())
        } else {
            Err(ApiError::forbidden())
        }
    }

    /// 읽을 수 있는(`system.read`) 것만 남긴다. 순서는 그대로다.
    pub fn filter<T: AuthzObject>(
        &self,
        principal: &Principal,
        items: impl IntoIterator<Item = T>,
    ) -> Result<Vec<T>, ApiError> {
        self.filter_by(principal, items, |o| Owned {
            target: o.authz_target(),
            read: o.read_privilege(),
        })
    }

    /// `owner`가 정한 대상에서 볼 수 있는 것만 남긴다(볼륨·백업·Task처럼 주인을 따르는 것). 순서는 그대로다.
    pub fn filter_by<T>(
        &self,
        principal: &Principal,
        items: impl IntoIterator<Item = T>,
        owner: impl Fn(&T) -> Owned,
    ) -> Result<Vec<T>, ApiError> {
        let index = self.index(Subject::from(principal))?;
        if index.is_all() {
            return Ok(items.into_iter().collect());
        }
        Ok(items
            .into_iter()
            .filter(|o| {
                let owned = owner(o);
                index.get(&owned.target).contains(owned.read)
            })
            .collect())
    }

    /// 대상에서 `privilege`가 있는지(오류 없이). 응답에서 가릴 것을 정할 때 쓴다.
    pub fn allows(&self, principal: &Principal, privilege: Privilege, target: AuthzTarget) -> bool {
        self.effective(principal, &target)
            .is_ok_and(|set| set.contains(privilege))
    }

    /// NIC를 붙일 네트워크마다 `network.assign`. 사용자 모드 NAT(`user`)와 없는 네트워크는 보지 않는다(검증이 400을 낸다).
    pub fn require_networks<'a>(
        &self,
        principal: &Principal,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), ApiError> {
        let mut seen = std::collections::BTreeSet::new();
        for name in names {
            if name == opl_types::network::USER_NETWORK || !seen.insert(name) {
                continue;
            }
            let target = network_target(&self.cluster, name)?;
            if target != AuthzTarget::Root {
                self.require(
                    principal,
                    Privilege::NetworkAssign,
                    &Found {
                        owned: Owned::object(target),
                        missing: format!("network {name:?} not found"),
                    },
                )?;
            }
        }
        Ok(())
    }

    /// 볼륨을 만들 풀의 `storage.allocate`. 없는 풀은 보지 않는다(검증이 400을 낸다).
    pub fn require_pool(&self, principal: &Principal, name: &str) -> Result<(), ApiError> {
        let target = pool_target(&self.cluster, name)?;
        if target == AuthzTarget::Root {
            return Ok(());
        }
        self.require(
            principal,
            Privilege::StorageAllocate,
            &Found {
                owned: Owned::object(target),
                missing: format!("storage pool {name:?} not found"),
            },
        )
    }

    /// VM 디스크로 붙일 볼륨마다 `storage.allocate`(볼륨의 주인 — 붙은 VM 또는 풀). 없는 볼륨은 보지 않는다.
    pub fn require_volumes<'a>(
        &self,
        principal: &Principal,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), ApiError> {
        for name in names.into_iter().collect::<std::collections::BTreeSet<_>>() {
            let Some(v) = self
                .cluster
                .get_by_name::<opl_types::storage::VolumeSpec>(name)?
            else {
                continue;
            };
            let target = Owners::around_volume(&self.cluster, &v.name, &v.spec.pool)?;
            self.require(
                principal,
                Privilege::StorageAllocate,
                &Found {
                    owned: Owned::object(target),
                    missing: format!("volume {name:?} not found"),
                },
            )?;
        }
        Ok(())
    }

    /// VM CD-ROM에 넣을 ISO 볼륨마다 그 주인(보통 풀)의 `system.read`. 없는 볼륨은 보지 않는다(검증이 400을 낸다).
    pub fn require_isos<'a>(
        &self,
        principal: &Principal,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), ApiError> {
        for name in names.into_iter().collect::<std::collections::BTreeSet<_>>() {
            let Some(v) = self
                .cluster
                .get_by_name::<opl_types::storage::VolumeSpec>(name)?
            else {
                continue;
            };
            let target = Owners::around_volume(&self.cluster, &v.name, &v.spec.pool)?;
            self.require(
                principal,
                Privilege::SystemRead,
                &Found {
                    owned: Owned::object(target),
                    missing: format!("volume {name:?} not found"),
                },
            )?;
        }
        Ok(())
    }

    /// 새 객체(VM·네트워크·풀)를 `folder`에 만들 권한. 폴더를 주지 않았는데 루트에는 권한이 없고 다른 어딘가(폴더)에는
    /// 있으면 400 `folder_required`다(설계 §5).
    pub fn require_new(
        &self,
        principal: &Principal,
        privilege: Privilege,
        kind: Kind,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        if folder.is_none() {
            let index = self.index(Subject::from(principal))?;
            if !index.get(&AuthzTarget::Root).contains(privilege) && index.anywhere(privilege) {
                return Err(ApiError::bad_request(format!(
                    "give a folder: you may not create a {} at the root",
                    kind.as_str()
                ))
                .with_code("folder_required"));
            }
        }
        self.require_in_folder(principal, privilege, kind, folder)
    }

    /// 폴더 `folder`(없으면 그 트리의 루트)에 새 객체를 둘 권한. 루트면 루트 대상에서, 폴더면 그 폴더 안의 객체가
    /// 받는 권한(propagate)으로 본다. 폴더를 읽을 수도 없으면 그 폴더가 없는 것과 같은 404다.
    pub fn require_in_folder(
        &self,
        principal: &Principal,
        privilege: Privilege,
        kind: Kind,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        let Some(folder) = folder else {
            return self.require_root(principal, privilege);
        };
        // 폴더 안에 새로 둘 객체(id 없음)가 받는 권한
        let inside = AuthzTarget::Object {
            kind,
            id: Uuid::nil(),
            folder: Some(folder),
        };
        let set = self.effective(principal, &inside)?;
        if set.contains(privilege) {
            Ok(())
        } else if set.contains(Privilege::SystemRead)
            || self
                .effective(principal, &AuthzTarget::Folder(folder))?
                .contains(Privilege::SystemRead)
        {
            Err(ApiError::forbidden())
        } else {
            Err(ApiError::not_found(format!("folder {folder} not found")))
        }
    }
}
