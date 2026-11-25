//! 주체 하나의 유효 권한 색인(계획 62 설계 §7). 참조 구현 `orbivisor_types::authz_eval::effective`와 같은 답을 내되, 그
//! 주체의 grant를 대상별로 모은 뒤 폴더마다 조상 쪽 propagate grant를 미리 합쳐 두어 확인이 해시 두 번 찾기다.

use std::collections::HashMap;

use orbivisor_types::authz::{
    GrantSpec, GrantTarget, PrincipalRef, PrivSet, Privilege, RoleRef, TokenScope,
    builtin_privileges,
};
use orbivisor_types::authz_eval::{AuthzSnapshot, AuthzTarget, ancestors, groups_of};
use orbivisor_types::resource::Kind;
use uuid::Uuid;

/// 같은 주체의 grant(대상, propagate, 권한)를 대상별로 모은 것
#[derive(Debug, Default)]
struct Grants {
    /// 루트 대상(클러스터 객체)에 닿는 것: 루트 grant 전부
    root_self: PrivSet,
    /// 루트 아래 모두에 닿는 것: propagate 루트 grant(전역 역할 포함)
    root_down: PrivSet,
    /// 폴더 자체에 건 grant 전부
    folder_self: HashMap<Uuid, PrivSet>,
    /// 폴더에 건 propagate grant(자손 폴더와 그 안의 객체에 닿는다)
    folder_down: HashMap<Uuid, PrivSet>,
    /// 객체에 직접 건 grant
    objects: HashMap<(Kind, Uuid), PrivSet>,
}

impl Grants {
    fn add(&mut self, target: &GrantTarget, propagate: bool, set: PrivSet) {
        if set.is_empty() {
            return;
        }
        match *target {
            GrantTarget::Root => {
                self.root_self |= set;
                if propagate {
                    self.root_down |= set;
                }
            }
            GrantTarget::Folder { id } => {
                *self.folder_self.entry(id).or_default() |= set;
                if propagate {
                    *self.folder_down.entry(id).or_default() |= set;
                }
            }
            GrantTarget::Object { kind, id } => *self.objects.entry((kind, id)).or_default() |= set,
        }
    }

    /// 폴더 트리를 한 번 훑어 대상별 합을 만든다(폴더 수 × 깊이).
    fn resolve(self, snap: &AuthzSnapshot) -> Layer {
        let mut contents = HashMap::new();
        let mut folders = HashMap::new();
        if !self.folder_down.is_empty() || !self.folder_self.is_empty() {
            for &f in snap.folders.keys() {
                let mut down = PrivSet::EMPTY;
                if !self.folder_down.is_empty() {
                    for a in ancestors(snap, Some(f)) {
                        if let Some(set) = self.folder_down.get(&a) {
                            down |= *set;
                        }
                    }
                }
                let here = down | self.folder_self.get(&f).copied().unwrap_or_default();
                if !down.is_empty() {
                    contents.insert(f, self.root_down | down);
                }
                if !here.is_empty() {
                    folders.insert(f, self.root_down | here);
                }
            }
            // 트리에 없는 폴더에 건 grant도 그 폴더 대상 자체에는 닿는다(참조 구현과 같다).
            for (f, set) in &self.folder_self {
                if !snap.folders.contains_key(f) {
                    folders.insert(*f, self.root_down | *set);
                }
            }
        }
        Layer {
            root: self.root_self,
            down_root: self.root_down,
            contents,
            folders,
            objects: self.objects,
        }
    }
}

/// 대상별로 합친 권한(`system.read` 품기 전)
#[derive(Debug, Default, PartialEq, Eq)]
struct Layer {
    /// 루트 대상
    root: PrivSet,
    /// 루트에 있는 객체(폴더 없음·없는 폴더)
    down_root: PrivSet,
    /// 폴더 안의 객체. 없으면 `down_root`
    contents: HashMap<Uuid, PrivSet>,
    /// 폴더 자체. 없으면 `down_root`
    folders: HashMap<Uuid, PrivSet>,
    objects: HashMap<(Kind, Uuid), PrivSet>,
}

impl Layer {
    /// 어느 대상에서든 `privilege`가 있다(루트·폴더·폴더 안·객체).
    fn anywhere(&self, privilege: Privilege) -> bool {
        self.root.contains(privilege)
            || self.down_root.contains(privilege)
            || self.contents.values().any(|s| s.contains(privilege))
            || self.folders.values().any(|s| s.contains(privilege))
            || self.objects.values().any(|s| s.contains(privilege))
    }

    fn get(&self, target: &AuthzTarget) -> PrivSet {
        match *target {
            AuthzTarget::Root => self.root,
            AuthzTarget::Folder(f) => self.folders.get(&f).copied().unwrap_or(self.down_root),
            AuthzTarget::Object { kind, id, folder } => {
                let inside = folder
                    .and_then(|f| self.contents.get(&f))
                    .copied()
                    .unwrap_or(self.down_root);
                inside | self.objects.get(&(kind, id)).copied().unwrap_or_default()
            }
        }
    }
}

/// 주체(사용자, 범위 있는 토큰이면 그 범위) 하나의 유효 권한 색인. 만든 때의 세대를 담는다.
#[derive(Debug)]
pub struct EffectiveIndex {
    pub generation: u64,
    /// 루트에 propagate로 모든 권한이 있고 범위가 없다(전역 admin): 색인 없이 전부 참
    all: bool,
    owner: Layer,
    /// 토큰 범위의 천장(범위 대상마다 범위 권한을 건 propagate grant). 범위 없는 주체는 None
    ceiling: Option<Layer>,
}

impl EffectiveIndex {
    /// 사용자 `user`의 색인. `scope`가 있으면 그 천장과의 교집합이다. 없는 사용자는 아무 권한도 없다.
    pub fn build(
        snap: &AuthzSnapshot,
        user: Uuid,
        scope: Option<&TokenScope>,
        generation: u64,
    ) -> Self {
        let Some(entry) = snap.users.get(&user) else {
            return Self::empty(generation);
        };
        let mut grants = Grants::default();
        grants.add(&GrantTarget::Root, true, builtin_privileges(entry.role));
        if scope.is_none() && grants.root_down == PrivSet::all() {
            return EffectiveIndex {
                generation,
                all: true,
                owner: Layer::default(),
                ceiling: None,
            };
        }
        let groups = groups_of(snap, user);
        for g in &snap.grants {
            let mine = match g.principal {
                PrincipalRef::User { id } => id == user,
                PrincipalRef::Group { id } => groups.contains(&id),
            };
            if mine {
                grants.add(&g.target, g.propagate, role_privileges(snap, g));
            }
        }
        if scope.is_none() && grants.root_down == PrivSet::all() {
            return EffectiveIndex {
                generation,
                all: true,
                owner: Layer::default(),
                ceiling: None,
            };
        }
        let ceiling = scope.map(|s| {
            let mut c = Grants::default();
            let set = s.privset();
            for t in &s.targets {
                c.add(t, true, set);
            }
            c.resolve(snap)
        });
        EffectiveIndex {
            generation,
            all: false,
            owner: grants.resolve(snap),
            ceiling,
        }
    }

    /// 아무 권한도 없는 색인(없는 사용자·지워진 토큰)
    pub fn empty(generation: u64) -> Self {
        EffectiveIndex {
            generation,
            all: false,
            owner: Layer::default(),
            ceiling: None,
        }
    }

    /// 대상에서의 유효 권한. 하나라도 있으면 `system.read`를 품는다.
    pub fn get(&self, target: &AuthzTarget) -> PrivSet {
        if self.all {
            return PrivSet::all();
        }
        let owner = self.owner.get(target).with_implied();
        match &self.ceiling {
            Some(c) => owner & c.get(target).with_implied(),
            None => owner,
        }
    }

    /// 루트에 모든 권한이 있다(색인 없이 전부 참).
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// 세대만 다르고 권한은 같다(`/watch`가 세대가 바뀌어도 닫지 않아도 되는지 본다).
    pub fn same_rights(&self, other: &EffectiveIndex) -> bool {
        self.all == other.all && self.owner == other.owner && self.ceiling == other.ceiling
    }

    /// 어딘가에서 `privilege`가 있다(범위 토큰이면 범위도). 만들기 요청에 폴더가 빠졌을 때 400 `folder_required`와
    /// 403을 가른다.
    pub fn anywhere(&self, privilege: Privilege) -> bool {
        self.all
            || (self.owner.anywhere(privilege)
                && self.ceiling.as_ref().is_none_or(|c| c.anywhere(privilege)))
    }

    /// 대상별 권한 그대로(`/permissions/mine`): (루트, 폴더 자체, 폴더 안의 객체, 객체). 전부 `system.read`를 품는다.
    pub fn layers(&self, folders: &[Uuid]) -> Layers {
        let root = self.get(&AuthzTarget::Root);
        let mut out = Layers {
            all: self.all,
            root,
            folders: Default::default(),
            contents: Default::default(),
            objects: Default::default(),
        };
        if self.all {
            return out;
        }
        for f in folders {
            let here = self.get(&AuthzTarget::Folder(*f));
            if !here.is_empty() {
                out.folders.insert(*f, here);
            }
            let inside = self.get(&AuthzTarget::Object {
                kind: Kind::Vm,
                id: Uuid::nil(),
                folder: Some(*f),
            });
            if !inside.is_empty() {
                out.contents.insert(*f, inside);
            }
        }
        for (kind, id) in self.owner.objects.keys() {
            // 객체 grant는 그 객체가 어느 폴더에 있든 더해지므로 루트에 있는 것으로 셈한 값을 준다(폴더 권한과 합쳐 본다).
            let set = self.get(&AuthzTarget::Object {
                kind: *kind,
                id: *id,
                folder: None,
            });
            out.objects.insert((*kind, *id), set);
        }
        out
    }
}

/// 주체 색인을 대상별로 펼친 것(`GET /permissions/mine`)
#[derive(Debug, Default)]
pub struct Layers {
    pub all: bool,
    pub root: PrivSet,
    pub folders: HashMap<Uuid, PrivSet>,
    pub contents: HashMap<Uuid, PrivSet>,
    pub objects: HashMap<(Kind, Uuid), PrivSet>,
}

fn role_privileges(snap: &AuthzSnapshot, g: &GrantSpec) -> PrivSet {
    match g.role {
        RoleRef::Builtin { role } => builtin_privileges(role),
        RoleRef::Custom { id } => snap.roles.get(&id).copied().unwrap_or_default(),
    }
}

/// 주체별 색인의 LRU(시계 바늘). 꽉 차면 최근에 쓰이지 않은 것부터 바꾼다.
pub(crate) struct Lru<K, V> {
    cap: usize,
    map: HashMap<K, usize>,
    slots: Vec<Slot<K, V>>,
    hand: usize,
}

struct Slot<K, V> {
    key: K,
    value: V,
    used: bool,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Lru<K, V> {
    pub(crate) fn new(cap: usize) -> Self {
        assert!(cap > 0);
        Lru {
            cap,
            map: HashMap::new(),
            slots: Vec::new(),
            hand: 0,
        }
    }

    pub(crate) fn get(&mut self, key: &K) -> Option<V> {
        let i = *self.map.get(key)?;
        self.slots[i].used = true;
        Some(self.slots[i].value.clone())
    }

    pub(crate) fn insert(&mut self, key: K, value: V) {
        if let Some(&i) = self.map.get(&key) {
            self.slots[i].value = value;
            self.slots[i].used = true;
            return;
        }
        if self.slots.len() < self.cap {
            self.map.insert(key.clone(), self.slots.len());
            self.slots.push(Slot {
                key,
                value,
                used: true,
            });
            return;
        }
        loop {
            let i = self.hand;
            self.hand = (self.hand + 1) % self.cap;
            if self.slots[i].used {
                self.slots[i].used = false;
                continue;
            }
            self.map.remove(&self.slots[i].key);
            self.map.insert(key.clone(), i);
            self.slots[i] = Slot {
                key,
                value,
                used: true,
            };
            return;
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }
}
