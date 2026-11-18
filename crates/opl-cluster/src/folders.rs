//! 폴더 트리 검사(계획 62 설계 §5). 상태 머신이 커밋 순서대로 보므로, 여러 노드가 동시에 폴더를 옮기거나 지우고
//! 리소스를 넣어도 하나만 이긴다. 폴더(`Kind::Folder`)가 하나도 없으면 리소스 쓰기에는 아무 일도 하지 않아,
//! 업그레이드 중 같은 로그를 적용한 옛 노드와 결과가 같다. 형제 이름 중복은 `exclusive.rs`가 막는다.

use std::collections::{HashMap, HashSet};

use opl_types::folder::{FolderKind, FolderSpec, FolderUsage, MAX_FOLDER_DEPTH};
use opl_types::quota::{
    DirectUsage, QuotaNode, QuotaTree, disk_names, quota_ancestors, quota_exceeded,
};
use opl_types::resource::Kind;
use redb::{ReadableTable, Table};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::store::StoreError;
use crate::types::{RawResource, Rejection};

type Objects<'t> = Table<'t, (&'static str, u128), &'static [u8]>;

/// 폴더 하나의 트리 정보
#[derive(Debug, Clone, Copy)]
struct Node {
    parent: Option<Uuid>,
    kind: FolderKind,
}

fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, StoreError> {
    Ok(serde_json::from_slice(bytes)?)
}

/// 풀 수 있는 폴더 전부. 풀 수 없는 스펙(API가 쓰지 않는다)은 트리에 없는 것으로 본다.
fn load_folders(objects: &Objects) -> Result<HashMap<Uuid, Node>, StoreError> {
    let mut out = HashMap::new();
    let kind = Kind::Folder.as_str();
    for item in objects.range((kind, 0u128)..=(kind, u128::MAX))? {
        let (key, v) = item?;
        let raw: RawResource = decode(v.value())?;
        if let Ok(f) = FolderSpec::deserialize(&raw.spec) {
            out.insert(
                Uuid::from_u128(key.value().1),
                Node {
                    parent: f.parent,
                    kind: f.kind,
                },
            );
        }
    }
    Ok(out)
}

fn any_folder(objects: &Objects) -> Result<bool, StoreError> {
    let kind = Kind::Folder.as_str();
    Ok(objects
        .range((kind, 0u128)..=(kind, u128::MAX))?
        .next()
        .is_some())
}

/// 리소스 스펙의 `folder` 칸
fn folder_of(spec: &Value) -> Option<Uuid> {
    spec.get("folder")?.as_str()?.parse().ok()
}

/// `from`에서 루트까지의 단계 수(자기 포함). 있을 수 없는 순환이 있어도 끝난다.
fn depth(folders: &HashMap<Uuid, Node>, from: Option<Uuid>) -> usize {
    let mut seen = HashSet::new();
    let mut cur = from;
    while let Some(id) = cur {
        let Some(node) = folders.get(&id) else { break };
        if !seen.insert(id) {
            break;
        }
        cur = node.parent;
    }
    seen.len()
}

/// `id` 아래 가장 깊은 자손까지의 단계 수(자손이 없으면 0)
fn height(folders: &HashMap<Uuid, Node>, id: Uuid) -> usize {
    let mut children: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (child, node) in folders {
        if let Some(p) = node.parent {
            children.entry(p).or_default().push(*child);
        }
    }
    let mut seen = HashSet::from([id]);
    let mut level = vec![id];
    let mut h = 0;
    loop {
        let next: Vec<Uuid> = level
            .iter()
            .flat_map(|p| children.get(p).into_iter().flatten())
            .copied()
            .filter(|c| seen.insert(*c))
            .collect();
        if next.is_empty() {
            return h;
        }
        h += 1;
        level = next;
    }
}

/// 폴더 `id`(종류 `kind`)에 자식 폴더나 든 리소스가 있다.
fn has_contents(
    objects: &Objects,
    folders: &HashMap<Uuid, Node>,
    id: Uuid,
    kind: Option<FolderKind>,
) -> Result<bool, StoreError> {
    if folders.values().any(|n| n.parent == Some(id)) {
        return Ok(true);
    }
    // 종류를 모르면(풀 수 없는 스펙) 세 트리를 다 본다.
    let member_kinds: Vec<Kind> = match kind {
        Some(k) => vec![k.member_kind()],
        None => FolderKind::ALL.iter().map(|k| k.member_kind()).collect(),
    };
    for member in member_kinds {
        let name = member.as_str();
        for item in objects.range((name, 0u128)..=(name, u128::MAX))? {
            let (_, v) = item?;
            let raw: RawResource = decode(v.value())?;
            if folder_of(&raw.spec) == Some(id) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// 만들기·스펙 바꾸기의 폴더 검사. `old`는 바꾸기 전 스펙이다(만들기는 None).
pub(crate) fn check_write(
    objects: &Objects,
    kind: Kind,
    id: Uuid,
    spec: &Value,
    old: Option<&Value>,
) -> Result<Result<(), Rejection>, StoreError> {
    if kind == Kind::Folder {
        return check_folder_write(objects, id, spec, old);
    }
    let Some(tree) = FolderKind::of(kind) else {
        return Ok(Ok(()));
    };
    let Some(folder) = folder_of(spec) else {
        return Ok(Ok(()));
    };
    // 폴더를 바꾸지 않는 쓰기는 보지 않는다(폴더가 없던 때 들어간 값이어도 다른 칸은 바꿀 수 있다).
    if old.and_then(folder_of) == Some(folder) {
        return Ok(Ok(()));
    }
    let found = match objects.get((Kind::Folder.as_str(), folder.as_u128()))? {
        Some(v) => {
            let raw: RawResource = decode(v.value())?;
            FolderSpec::deserialize(&raw.spec).is_ok_and(|f| f.kind == tree)
        }
        None => false,
    };
    if found || !any_folder(objects)? {
        return Ok(Ok(()));
    }
    Ok(Err(Rejection::NotFound {
        kind: Kind::Folder,
        id: folder,
    }))
}

fn check_folder_write(
    objects: &Objects,
    id: Uuid,
    spec: &Value,
    old: Option<&Value>,
) -> Result<Result<(), Rejection>, StoreError> {
    // 풀 수 없는 폴더 스펙은 API가 쓰지 않는다. 트리 검사에서는 없는 것으로 본다.
    let Ok(new) = FolderSpec::deserialize(spec) else {
        return Ok(Ok(()));
    };
    let folders = load_folders(objects)?;
    let old_kind = old.and_then(|o| FolderSpec::deserialize(o).ok().map(|f| f.kind));
    if old.is_some() && old_kind != Some(new.kind) && has_contents(objects, &folders, id, old_kind)?
    {
        return Ok(Err(Rejection::FolderNotEmpty { id }));
    }
    if let Some(parent) = new.parent {
        if folders.get(&parent).is_none_or(|p| p.kind != new.kind) && parent != id {
            return Ok(Err(Rejection::NotFound {
                kind: Kind::Folder,
                id: parent,
            }));
        }
        // 새 부모에서 걸어 올라가 자기를 만나면 순환이다.
        let mut seen = HashSet::new();
        let mut cur = Some(parent);
        while let Some(f) = cur {
            if f == id {
                return Ok(Err(Rejection::FolderCycle { id, parent }));
            }
            if !seen.insert(f) {
                break;
            }
            cur = folders.get(&f).and_then(|n| n.parent);
        }
    }
    let deepest = depth(&folders, new.parent) + 1 + height(&folders, id);
    if deepest > MAX_FOLDER_DEPTH {
        return Ok(Err(Rejection::FolderTooDeep { id, depth: deepest }));
    }
    Ok(Ok(()))
}

/// 지우기의 폴더 검사: 자식 폴더나 든 리소스가 있는 폴더는 지우지 않는다.
pub(crate) fn check_delete(
    objects: &Objects,
    kind: Kind,
    id: Uuid,
    spec: &Value,
) -> Result<Result<(), Rejection>, StoreError> {
    if kind != Kind::Folder {
        return Ok(Ok(()));
    }
    let folders = load_folders(objects)?;
    let tree = FolderSpec::deserialize(spec).ok().map(|f| f.kind);
    if has_contents(objects, &folders, id, tree)? {
        return Ok(Err(Rejection::FolderNotEmpty { id }));
    }
    Ok(Ok(()))
}

type Names<'t> = Table<'t, (&'static str, &'static str), u128>;

/// 폴더 전부의 트리와 할당량(풀 수 없는 스펙은 없는 것으로 본다)
fn load_quota_tree(objects: &Objects) -> Result<QuotaTree, StoreError> {
    let mut out = QuotaTree::new();
    let kind = Kind::Folder.as_str();
    for item in objects.range((kind, 0u128)..=(kind, u128::MAX))? {
        let (key, v) = item?;
        let raw: RawResource = decode(v.value())?;
        if let Ok(f) = FolderSpec::deserialize(&raw.spec) {
            out.insert(
                Uuid::from_u128(key.value().1),
                QuotaNode {
                    parent: f.parent,
                    quota: f.quota,
                },
            );
        }
    }
    Ok(out)
}

/// 이름 `name`인 볼륨의 `size_bytes`(없으면 0)
fn volume_size(objects: &Objects, names: &Names, name: &str) -> Result<u64, StoreError> {
    let kind = Kind::Volume.as_str();
    let Some(id) = names.get((kind, name))?.map(|v| v.value()) else {
        return Ok(0);
    };
    let Some(v) = objects.get((kind, id))? else {
        return Ok(0);
    };
    let raw: RawResource = decode(v.value())?;
    Ok(raw.spec["size_bytes"].as_u64().unwrap_or(0))
}

/// VM 스펙(JSON)의 사용량. 볼륨 크기는 저장된 값이다.
fn vm_usage(objects: &Objects, names: &Names, spec: &Value) -> Result<FolderUsage, StoreError> {
    let mut usage = FolderUsage::of_vm_value(spec, |_| 0);
    for name in disk_names(spec) {
        usage.disk_bytes = usage
            .disk_bytes
            .saturating_add(volume_size(objects, names, name)?);
    }
    Ok(usage)
}

/// 할당량 검사(설계 §9) 하나. [`quota_check`]가 만들고, VM을 한 번 훑으며 [`QuotaCheck::add`]로 사용량을 모은 뒤
/// [`QuotaCheck::finish`]가 판정한다. VM 쓰기는 `check_exclusive`의 VM 훑기에 붙여 한 번만 푼다.
pub(crate) struct QuotaCheck {
    before: QuotaTree,
    after: QuotaTree,
    /// 할당량 있는 조상이 있는 폴더(쓰기 전이나 뒤). 그 밖의 VM은 세지 않는다.
    relevant: HashSet<Uuid>,
    /// 쓰는 VM(훑기에서 빼고 전·후 몫을 따로 더한다)
    vm: Option<Uuid>,
    old_vm: Option<(Option<Uuid>, FolderUsage)>,
    new_vm: Option<(Option<Uuid>, FolderUsage)>,
    /// 늘리는 볼륨의 이름과 새 크기
    resized: Option<(String, u64)>,
    before_direct: DirectUsage,
    after_direct: DirectUsage,
    /// VM을 이미 다 훑었다.
    pub(crate) scanned: bool,
}

/// 이 쓰기가 할당량을 넘길 수 있으면 검사를 준비한다. 할당량이 걸린 조상이 없거나 아무것도 늘리지 않는 쓰기(HA·
/// 마이그레이션·끄기 같은 노드·전원 바꾸기, 이름 바꾸기, 줄이기)는 None이라 VM을 훑지 않는다. `name`은 쓰는 리소스의
/// 이름, `old`는 바꾸기 전 스펙이다(만들기는 None).
pub(crate) fn quota_check(
    objects: &Objects,
    names: &Names,
    kind: Kind,
    id: Uuid,
    name: &str,
    spec: &Value,
    old: Option<&Value>,
) -> Result<Option<QuotaCheck>, StoreError> {
    if !matches!(kind, Kind::Vm | Kind::Volume | Kind::Folder) {
        return Ok(None);
    }
    // 볼륨은 늘릴 때만 본다(붙지 않은 볼륨은 세지 않는다).
    if kind == Kind::Volume {
        let size = |v: &Value| v["size_bytes"].as_u64().unwrap_or(0);
        if old.is_none_or(|o| size(spec) <= size(o)) {
            return Ok(None);
        }
    }
    let tree = load_quota_tree(objects)?;
    if !tree
        .values()
        .any(|n| n.quota.is_some_and(|q| !q.is_empty()))
    {
        return Ok(None);
    }
    let mut after = tree.clone();
    let mut check = QuotaCheck {
        before: tree,
        after: QuotaTree::new(),
        relevant: HashSet::new(),
        vm: None,
        old_vm: None,
        new_vm: None,
        resized: None,
        before_direct: DirectUsage::new(),
        after_direct: DirectUsage::new(),
        scanned: false,
    };
    let subset = |a: &[Uuid], b: &[Uuid]| a.iter().all(|x| b.contains(x));
    match kind {
        Kind::Vm => {
            let (to, from) = (folder_of(spec), old.and_then(folder_of));
            let bound = quota_ancestors(&check.before, to);
            if bound.is_empty() {
                return Ok(None);
            }
            let new = vm_usage(objects, names, spec)?;
            if let Some(old) = old {
                let was = vm_usage(objects, names, old)?;
                if new.within(&was) && subset(&bound, &quota_ancestors(&check.before, from)) {
                    return Ok(None);
                }
                check.old_vm = Some((from, was));
            }
            check.vm = Some(id);
            check.new_vm = Some((to, new));
        }
        Kind::Volume => {
            check.resized = Some((name.to_string(), spec["size_bytes"].as_u64().unwrap_or(0)));
        }
        Kind::Folder => {
            // 옮길 때만 하위 트리가 새 조상에 더해진다. 할당량 바꾸기(줄이기 포함)는 막지 않는다.
            let (Some(old), Ok(new)) = (old, FolderSpec::deserialize(spec)) else {
                return Ok(None);
            };
            let from = FolderSpec::deserialize(old).ok().and_then(|f| f.parent);
            if from == new.parent {
                return Ok(None);
            }
            after.insert(
                id,
                QuotaNode {
                    parent: new.parent,
                    quota: new.quota,
                },
            );
            let bound = quota_ancestors(&after, new.parent);
            if subset(&bound, &quota_ancestors(&check.before, from)) {
                return Ok(None);
            }
        }
        _ => unreachable!(),
    }
    check.relevant = after
        .keys()
        .chain(check.before.keys())
        .filter(|f| {
            !quota_ancestors(&after, Some(**f)).is_empty()
                || !quota_ancestors(&check.before, Some(**f)).is_empty()
        })
        .copied()
        .collect();
    check.after = after;
    Ok(Some(check))
}

impl QuotaCheck {
    /// 훑기에서 만난 VM 하나(`id`, 저장된 스펙)를 센다.
    pub(crate) fn add(
        &mut self,
        objects: &Objects,
        names: &Names,
        id: Uuid,
        spec: &Value,
    ) -> Result<(), StoreError> {
        if self.vm == Some(id) {
            return Ok(());
        }
        let Some(folder) = folder_of(spec).filter(|f| self.relevant.contains(f)) else {
            return Ok(());
        };
        let before = vm_usage(objects, names, spec)?;
        let mut after = before;
        if let Some((name, size)) = &self.resized {
            let grown = disk_names(spec).filter(|n| n == name).count() as u64;
            if grown > 0 {
                let old = volume_size(objects, names, name)?;
                after.disk_bytes = after
                    .disk_bytes
                    .saturating_add(size.saturating_sub(old).saturating_mul(grown));
            }
        }
        self.before_direct.entry(folder).or_default().add(&before);
        self.after_direct.entry(folder).or_default().add(&after);
        Ok(())
    }

    /// 아직 훑지 않았으면 VM을 훑고, 할당량을 넘으면 `QuotaExceeded`.
    pub(crate) fn finish(
        mut self,
        objects: &Objects,
        names: &Names,
    ) -> Result<Result<(), Rejection>, StoreError> {
        if !self.scanned {
            let kind = Kind::Vm.as_str();
            for item in objects.range((kind, 0u128)..=(kind, u128::MAX))? {
                let (key, v) = item?;
                let raw: RawResource = decode(v.value())?;
                self.add(objects, names, Uuid::from_u128(key.value().1), &raw.spec)?;
            }
        }
        if let Some((Some(f), usage)) = self.old_vm {
            self.before_direct.entry(f).or_default().add(&usage);
        }
        if let Some((Some(f), usage)) = self.new_vm {
            self.after_direct.entry(f).or_default().add(&usage);
        }
        Ok(
            match quota_exceeded(
                (&self.before, &self.before_direct),
                (&self.after, &self.after_direct),
            ) {
                Some(e) => Err(Rejection::QuotaExceeded {
                    folder: e.folder,
                    dimension: e.dimension,
                    limit: e.limit,
                    used: e.used,
                }),
                None => Ok(()),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use opl_types::resource::Resource;
    use redb::Database;
    use redb::backends::InMemoryBackend;
    use serde_json::json;

    use super::*;
    use crate::state_machine::apply_command;
    use crate::store::{NAMES, OBJECTS};
    use crate::types::Command;

    /// 상태 머신의 명령 적용만 하는 메모리 저장소
    struct Sm {
        db: Database,
        index: u64,
    }

    impl Sm {
        fn new() -> Self {
            let db = Database::builder()
                .create_with_backend(InMemoryBackend::new())
                .unwrap();
            Sm { db, index: 0 }
        }

        fn apply(&mut self, cmd: Command) -> Result<(), Rejection> {
            self.index += 1;
            let txn = self.db.begin_write().unwrap();
            let r = {
                let mut objects = txn.open_table(OBJECTS).unwrap();
                let mut names = txn.open_table(NAMES).unwrap();
                apply_command(&mut objects, &mut names, cmd, self.index)
                    .unwrap()
                    .map(|_| ())
            };
            txn.commit().unwrap();
            r
        }

        fn create(&mut self, kind: Kind, id: Uuid, spec: Value) -> Result<(), Rejection> {
            let mut resource: RawResource = Resource::new(id.to_string(), spec);
            resource.id = id;
            self.apply(Command::Create { kind, resource })
        }

        fn update(&mut self, kind: Kind, id: Uuid, spec: Value) -> Result<(), Rejection> {
            self.apply(Command::UpdateSpec {
                kind,
                id,
                expected_revision: None,
                labels: BTreeMap::new(),
                spec,
            })
        }

        fn delete(&mut self, kind: Kind, id: Uuid) -> Result<(), Rejection> {
            self.apply(Command::Delete {
                kind,
                id,
                expected_revision: None,
            })
        }

        fn folder(&mut self, id: Uuid, name: &str, parent: Option<Uuid>) -> Result<(), Rejection> {
            self.create(Kind::Folder, id, folder(name, "vm", parent))
        }
    }

    fn folder(name: &str, kind: &str, parent: Option<Uuid>) -> Value {
        let mut v = json!({"name": name, "kind": kind});
        if let Some(p) = parent {
            v["parent"] = json!(p);
        }
        v
    }

    fn vm(folder: Option<Uuid>) -> Value {
        let mut v =
            json!({"cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256, "machine": "virt"});
        if let Some(f) = folder {
            v["folder"] = json!(f);
        }
        v
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn not_found(f: Uuid) -> Result<(), Rejection> {
        Err(Rejection::NotFound {
            kind: Kind::Folder,
            id: f,
        })
    }

    /// 폴더가 하나도 없는 동안에는 `folder` 칸을 보지 않는다(옛 노드와 같은 결과).
    #[test]
    fn without_folders_nothing_is_checked() {
        let mut sm = Sm::new();
        assert_eq!(sm.create(Kind::Vm, id(1), vm(Some(id(99)))), Ok(()));
        assert_eq!(sm.update(Kind::Vm, id(1), vm(Some(id(98)))), Ok(()));
        assert_eq!(
            sm.create(Kind::Network, id(2), json!({"folder": id(97)})),
            Ok(())
        );
        assert_eq!(sm.delete(Kind::Vm, id(1)), Ok(()));
    }

    #[test]
    fn a_resource_must_point_to_a_folder_of_its_tree() {
        let mut sm = Sm::new();
        sm.folder(id(10), "a", None).unwrap();
        sm.create(Kind::Folder, id(11), folder("n", "network", None))
            .unwrap();
        assert_eq!(
            sm.create(Kind::Vm, id(1), vm(Some(id(99)))),
            not_found(id(99))
        );
        // 네트워크 폴더에 VM은 들지 못한다.
        assert_eq!(
            sm.create(Kind::Vm, id(1), vm(Some(id(11)))),
            not_found(id(11))
        );
        assert_eq!(sm.create(Kind::Vm, id(1), vm(Some(id(10)))), Ok(()));
        assert_eq!(sm.create(Kind::Vm, id(2), vm(None)), Ok(()));
        assert_eq!(
            sm.create(Kind::StoragePool, id(3), json!({"folder": id(10)})),
            not_found(id(10))
        );
        assert_eq!(
            sm.create(Kind::Network, id(4), json!({"folder": id(11)})),
            Ok(())
        );
        // 폴더에 들지 않는 종류는 보지 않는다.
        assert_eq!(
            sm.create(Kind::Volume, id(5), json!({"folder": id(99)})),
            Ok(())
        );
        // 옮기기도 같다.
        assert_eq!(
            sm.update(Kind::Vm, id(2), vm(Some(id(99)))),
            not_found(id(99))
        );
        assert_eq!(sm.update(Kind::Vm, id(2), vm(Some(id(10)))), Ok(()));
    }

    /// 폴더가 없던 때 들어간 `folder` 값은 그대로 두면 다른 칸을 바꿀 수 있다.
    #[test]
    fn an_unchanged_folder_reference_is_not_checked() {
        let mut sm = Sm::new();
        sm.create(Kind::Vm, id(1), vm(Some(id(99)))).unwrap();
        sm.folder(id(10), "a", None).unwrap();
        let mut spec = vm(Some(id(99)));
        spec["memory_mib"] = json!(512);
        assert_eq!(sm.update(Kind::Vm, id(1), spec), Ok(()));
    }

    #[test]
    fn the_parent_must_exist_in_the_same_tree() {
        let mut sm = Sm::new();
        assert_eq!(sm.folder(id(10), "a", Some(id(99))), not_found(id(99)));
        sm.create(Kind::Folder, id(11), folder("n", "network", None))
            .unwrap();
        assert_eq!(sm.folder(id(10), "a", Some(id(11))), not_found(id(11)));
        assert_eq!(sm.folder(id(10), "a", None), Ok(()));
        assert_eq!(sm.folder(id(12), "b", Some(id(10))), Ok(()));
    }

    /// A→B와 B→A를 동시에 내도 커밋 순서로 앞의 것만 이긴다.
    #[test]
    fn a_folder_cannot_move_under_its_descendant() {
        let mut sm = Sm::new();
        sm.folder(id(1), "a", None).unwrap();
        sm.folder(id(2), "b", None).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(1), folder("a", "vm", Some(id(2)))),
            Ok(())
        );
        assert_eq!(
            sm.update(Kind::Folder, id(2), folder("b", "vm", Some(id(1)))),
            Err(Rejection::FolderCycle {
                id: id(2),
                parent: id(1)
            })
        );
        // 자기 자신 아래로도 못 간다.
        assert_eq!(
            sm.update(Kind::Folder, id(2), folder("b", "vm", Some(id(2)))),
            Err(Rejection::FolderCycle {
                id: id(2),
                parent: id(2)
            })
        );
        // 손자 아래로도 못 간다.
        sm.folder(id(3), "c", Some(id(1))).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(2), folder("b", "vm", Some(id(3)))),
            Err(Rejection::FolderCycle {
                id: id(2),
                parent: id(3)
            })
        );
    }

    #[test]
    fn the_tree_is_at_most_16_deep() {
        let mut sm = Sm::new();
        let mut parent = None;
        for n in 1..=MAX_FOLDER_DEPTH as u128 {
            sm.folder(id(n), "f", parent).unwrap();
            parent = Some(id(n));
        }
        assert_eq!(
            sm.folder(id(100), "f", parent),
            Err(Rejection::FolderTooDeep {
                id: id(100),
                depth: 17
            })
        );
        // 자손이 있는 폴더를 옮길 때는 하위 트리 높이까지 센다: 루트 아래 x/y를 깊이 15 아래로 옮기면 17이다.
        sm.folder(id(200), "x", None).unwrap();
        sm.folder(id(201), "y", Some(id(200))).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(200), folder("x", "vm", Some(id(15)))),
            Err(Rejection::FolderTooDeep {
                id: id(200),
                depth: 17
            })
        );
        assert_eq!(
            sm.update(Kind::Folder, id(200), folder("x", "vm", Some(id(14)))),
            Ok(())
        );
    }

    #[test]
    fn only_empty_folders_are_deleted() {
        let mut sm = Sm::new();
        sm.folder(id(1), "a", None).unwrap();
        sm.folder(id(2), "b", Some(id(1))).unwrap();
        let not_empty = |f| Err(Rejection::FolderNotEmpty { id: id(f) });
        assert_eq!(sm.delete(Kind::Folder, id(1)), not_empty(1));
        sm.create(Kind::Vm, id(10), vm(Some(id(2)))).unwrap();
        assert_eq!(sm.delete(Kind::Folder, id(2)), not_empty(2));
        sm.update(Kind::Vm, id(10), vm(None)).unwrap();
        // 지우기와 넣기가 엇갈리면 먼저 커밋된 것이 이긴다.
        assert_eq!(sm.delete(Kind::Folder, id(2)), Ok(()));
        assert_eq!(
            sm.update(Kind::Vm, id(10), vm(Some(id(2)))),
            not_found(id(2))
        );
        assert_eq!(sm.delete(Kind::Folder, id(1)), Ok(()));
    }

    #[test]
    fn a_non_empty_folder_keeps_its_kind() {
        let mut sm = Sm::new();
        sm.folder(id(1), "a", None).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(1), folder("a", "network", None)),
            Ok(())
        );
        sm.create(Kind::Network, id(10), json!({"folder": id(1)}))
            .unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(1), folder("a", "vm", None)),
            Err(Rejection::FolderNotEmpty { id: id(1) })
        );
    }

    #[test]
    fn sibling_names_are_unique_ignoring_case() {
        let mut sm = Sm::new();
        sm.folder(id(1), "teamA", None).unwrap();
        let taken = |holder| {
            matches!(
                holder,
                Err(Rejection::InUse { kind: Kind::Folder, ref field, .. }) if field == "name"
            )
        };
        assert!(taken(sm.folder(id(2), "TEAMA", None)));
        // 다른 부모·다른 트리에서는 된다.
        sm.folder(id(3), "teamB", None).unwrap();
        assert_eq!(sm.folder(id(4), "teamA", Some(id(3))), Ok(()));
        assert_eq!(
            sm.create(Kind::Folder, id(5), folder("teamA", "network", None)),
            Ok(())
        );
        // 이름 바꾸기·옮기기도 같다. 자기 이름을 다시 쓰는 것은 된다.
        assert!(taken(sm.update(
            Kind::Folder,
            id(3),
            folder("TeamA", "vm", None)
        )));
        assert!(taken(sm.update(
            Kind::Folder,
            id(4),
            folder("teamA", "vm", None)
        )));
        assert_eq!(
            sm.update(Kind::Folder, id(1), folder("TeamA", "vm", None)),
            Ok(())
        );
    }

    fn quota_folder(parent: Option<Uuid>, quota: Value) -> Value {
        let mut v = folder("q", "vm", parent);
        v["quota"] = quota;
        v
    }

    fn sized_vm(folder: Option<Uuid>, cores: u64, memory: u64, disks: &[Uuid]) -> Value {
        let mut v = vm(folder);
        v["cpu"]["cores"] = json!(cores);
        v["memory_mib"] = json!(memory);
        v["disks"] = json!(
            disks
                .iter()
                .map(|d| json!({"volume": d.to_string(), "bus": "virtio-blk"}))
                .collect::<Vec<_>>()
        );
        v
    }

    fn volume(size: u64) -> Value {
        json!({"pool": "p", "size_bytes": size, "format": "qcow2"})
    }

    fn exceeded(r: Result<(), Rejection>) -> Option<(Uuid, QuotaDimension, u64, u64)> {
        match r {
            Err(Rejection::QuotaExceeded {
                folder,
                dimension,
                limit,
                used,
            }) => Some((folder, dimension, limit, used)),
            _ => None,
        }
    }

    use opl_types::quota::QuotaDimension;

    #[test]
    fn vm_count_quota_refuses_the_extra_vm() {
        let mut sm = Sm::new();
        sm.create(Kind::Folder, id(1), quota_folder(None, json!({"vms": 2})))
            .unwrap();
        for n in 10..12 {
            assert_eq!(sm.create(Kind::Vm, id(n), vm(Some(id(1)))), Ok(()));
        }
        assert_eq!(
            exceeded(sm.create(Kind::Vm, id(12), vm(Some(id(1))))),
            Some((id(1), QuotaDimension::Vms, 2, 3))
        );
        // 템플릿은 디스크만 센다. 루트·다른 폴더는 묶이지 않는다.
        let mut template = vm(Some(id(1)));
        template["template"] = json!(true);
        assert_eq!(sm.create(Kind::Vm, id(13), template), Ok(()));
        assert_eq!(sm.create(Kind::Vm, id(14), vm(None)), Ok(()));
        // 루트의 VM을 넣는 옮기기도 늘리기다.
        assert!(exceeded(sm.update(Kind::Vm, id(14), vm(Some(id(1))))).is_some());
        // 지우면 다시 자리가 난다.
        sm.delete(Kind::Vm, id(10)).unwrap();
        assert_eq!(sm.update(Kind::Vm, id(14), vm(Some(id(1)))), Ok(()));
    }

    /// 넘은 폴더에서도 늘리지 않는 쓰기(노드·전원 바꾸기, 줄이기)는 받는다.
    #[test]
    fn only_growing_writes_are_refused() {
        let mut sm = Sm::new();
        sm.create(
            Kind::Folder,
            id(1),
            quota_folder(None, json!({"memory_mib": 4096})),
        )
        .unwrap();
        sm.create(Kind::Vm, id(10), sized_vm(Some(id(1)), 1, 2048, &[]))
            .unwrap();
        sm.create(Kind::Vm, id(11), sized_vm(Some(id(1)), 1, 2048, &[]))
            .unwrap();
        // 상한을 사용량 아래로 줄일 수 있다(초과로 보인다).
        assert_eq!(
            sm.update(
                Kind::Folder,
                id(1),
                quota_folder(None, json!({"memory_mib": 1024}))
            ),
            Ok(())
        );
        let mut moved = sized_vm(Some(id(1)), 1, 2048, &[]);
        moved["node"] = json!(id(77));
        moved["power"] = json!("stopped");
        assert_eq!(sm.update(Kind::Vm, id(10), moved), Ok(()));
        assert_eq!(
            sm.update(Kind::Vm, id(10), sized_vm(Some(id(1)), 4, 1024, &[])),
            Ok(())
        );
        assert_eq!(
            exceeded(sm.update(Kind::Vm, id(10), sized_vm(Some(id(1)), 4, 1536, &[]))),
            Some((id(1), QuotaDimension::MemoryMib, 1024, 3584))
        );
    }

    /// 사용량은 하위 트리 합이고 할당량 있는 조상마다 지킨다.
    #[test]
    fn every_ancestor_quota_holds() {
        let mut sm = Sm::new();
        sm.create(Kind::Folder, id(1), quota_folder(None, json!({"vcpus": 4})))
            .unwrap();
        sm.folder(id(2), "plain", Some(id(1))).unwrap();
        sm.create(
            Kind::Folder,
            id(3),
            quota_folder(Some(id(2)), json!({"vcpus": 100})),
        )
        .unwrap();
        sm.create(Kind::Vm, id(10), sized_vm(Some(id(2)), 2, 1, &[]))
            .unwrap();
        assert_eq!(
            exceeded(sm.create(Kind::Vm, id(11), sized_vm(Some(id(3)), 3, 1, &[]))),
            Some((id(1), QuotaDimension::Vcpus, 4, 5))
        );
        assert_eq!(
            sm.create(Kind::Vm, id(11), sized_vm(Some(id(3)), 2, 1, &[])),
            Ok(())
        );
    }

    #[test]
    fn disk_quota_counts_attached_volumes_and_their_growth() {
        let mut sm = Sm::new();
        sm.create(
            Kind::Folder,
            id(1),
            quota_folder(None, json!({"disk_bytes": 100})),
        )
        .unwrap();
        sm.create(Kind::Volume, id(20), volume(60)).unwrap();
        sm.create(Kind::Volume, id(21), volume(60)).unwrap();
        sm.create(Kind::Vm, id(10), sized_vm(Some(id(1)), 1, 1, &[id(20)]))
            .unwrap();
        assert_eq!(
            exceeded(sm.create(Kind::Vm, id(11), sized_vm(Some(id(1)), 1, 1, &[id(21)]))),
            Some((id(1), QuotaDimension::DiskBytes, 100, 120))
        );
        // 붙인 볼륨 늘리기도 막고, 붙지 않은 볼륨은 세지 않는다.
        assert_eq!(
            exceeded(sm.update(Kind::Volume, id(20), volume(101))),
            Some((id(1), QuotaDimension::DiskBytes, 100, 101))
        );
        assert_eq!(sm.update(Kind::Volume, id(20), volume(100)), Ok(()));
        assert_eq!(sm.update(Kind::Volume, id(21), volume(1000)), Ok(()));
        // 디스크 붙이기(핫플러그)도 늘리기다.
        assert!(
            exceeded(sm.update(
                Kind::Vm,
                id(10),
                sized_vm(Some(id(1)), 1, 1, &[id(20), id(21)])
            ))
            .is_some()
        );
    }

    #[test]
    fn moving_a_folder_brings_its_subtree_usage() {
        let mut sm = Sm::new();
        sm.create(Kind::Folder, id(1), quota_folder(None, json!({"vms": 1})))
            .unwrap();
        sm.folder(id(2), "a", None).unwrap();
        sm.folder(id(3), "b", Some(id(2))).unwrap();
        sm.create(Kind::Vm, id(10), vm(Some(id(3)))).unwrap();
        sm.create(Kind::Vm, id(11), vm(Some(id(1)))).unwrap();
        assert_eq!(
            exceeded(sm.update(Kind::Folder, id(2), folder("a", "vm", Some(id(1))))),
            Some((id(1), QuotaDimension::Vms, 1, 2))
        );
        // 빈 폴더 옮기기, 이름 바꾸기는 된다.
        sm.folder(id(4), "c", None).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(4), folder("c", "vm", Some(id(1)))),
            Ok(())
        );
        assert_eq!(
            sm.update(Kind::Folder, id(4), folder("d", "vm", Some(id(1)))),
            Ok(())
        );
        sm.delete(Kind::Vm, id(11)).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(2), folder("a", "vm", Some(id(1)))),
            Ok(())
        );
    }

    /// 상태를 apply 없이 바로 쓴다(규모 시험의 준비).
    fn put_all(sm: &Sm, items: Vec<(Kind, Uuid, String, Value)>) {
        let txn = sm.db.begin_write().unwrap();
        {
            let mut objects = txn.open_table(OBJECTS).unwrap();
            let mut names = txn.open_table(NAMES).unwrap();
            for (kind, id, name, spec) in items {
                let mut r: RawResource = Resource::new(name.clone(), spec);
                r.id = id;
                objects
                    .insert(
                        (kind.as_str(), id.as_u128()),
                        serde_json::to_vec(&r).unwrap().as_slice(),
                    )
                    .unwrap();
                names
                    .insert((kind.as_str(), name.as_str()), id.as_u128())
                    .unwrap();
            }
        }
        txn.commit().unwrap();
    }

    /// 설계 §9·§14의 apply 비용 기준: VM 1만 개(디스크·NIC 하나씩)가 폴더 20개(할당량 10개)에 흩어진 상태에서 VM
    /// 쓰기 하나의 apply 시간. 할당량 폴더에 만들기(겹침 훑기 + 사용량, 볼륨 크기 읽기)를 할당량 없는 폴더에 만들기
    /// (겹침 훑기만)와 견주고, 늘리지 않는 쓰기(노드 바꾸기)는 훑지 않음을 본다.
    /// `cargo test --release -p opl-cluster --features scale-it --lib quota_apply_cost -- --nocapture`
    #[test]
    #[cfg_attr(not(feature = "scale-it"), ignore = "규모 시험(--features scale-it)")]
    fn quota_apply_cost_with_10k_vms() {
        use std::time::{Duration, Instant};
        const VMS: u128 = 10_000;
        const RUNS: u128 = 20;
        let mut sm = Sm::new();
        let mut items = Vec::new();
        for f in 0..20u128 {
            let mut spec = folder(&format!("f{f}"), "vm", None);
            if f % 2 == 0 {
                spec["quota"] = json!({"vcpus": 1_000_000, "memory_mib": 1u64 << 40,
                                        "disk_bytes": 1u64 << 60, "vms": 1_000_000});
            }
            items.push((Kind::Folder, id(1_000 + f), id(1_000 + f).to_string(), spec));
        }
        let mac = |n: u128| {
            format!(
                "52:54:00:{:02x}:{:02x}:{:02x}",
                (n >> 16) & 0xff,
                (n >> 8) & 0xff,
                n & 0xff
            )
        };
        let vm_with = |n: u128, f: u128| {
            let mut spec = sized_vm(Some(id(1_000 + f)), 2, 2048, &[id(100_000 + n)]);
            spec["nics"] = json!([{"network": "user", "model": "virtio", "mac": mac(n)}]);
            spec
        };
        for n in 0..VMS {
            let vol = id(100_000 + n);
            items.push((Kind::Volume, vol, vol.to_string(), volume(10 << 30)));
            items.push((
                Kind::Vm,
                id(200_000 + n),
                format!("vm-{n}"),
                vm_with(n, n % 20),
            ));
        }
        put_all(&sm, items);
        let time = |sm: &mut Sm, f: &dyn Fn(&mut Sm, u128) -> Result<(), Rejection>| {
            let start = Instant::now();
            for i in 0..RUNS {
                f(sm, i).unwrap();
            }
            start.elapsed() / RUNS as u32
        };
        let add_volume = |sm: &mut Sm, n: u128| {
            put_all(
                sm,
                vec![(
                    Kind::Volume,
                    id(100_000 + n),
                    id(100_000 + n).to_string(),
                    volume(1 << 30),
                )],
            )
        };
        for i in 0..2 * RUNS {
            add_volume(&mut sm, VMS + i);
        }
        let plain = time(&mut sm, &|sm, i| {
            sm.create(Kind::Vm, id(300_000 + i), vm_with(VMS + i, 1))
        });
        let quota = time(&mut sm, &|sm, i| {
            sm.create(Kind::Vm, id(400_000 + i), vm_with(VMS + RUNS + i, 0))
        });
        let moved = time(&mut sm, &|sm, i| {
            let mut spec = vm_with(VMS + RUNS + i, 0);
            spec["node"] = json!(id(9));
            sm.update(Kind::Vm, id(400_000 + i), spec)
        });
        eprintln!(
            "apply with {VMS} VMs: create without quota {plain:?}, create under quota {quota:?}, node change under quota {moved:?}"
        );
        // 할당량 검사는 같은 훑기에 붙으므로 겹침 훑기만 하는 쓰기의 몇 배를 넘지 않는다. 늘리지 않는 쓰기는 사용량을
        // 세지 않아 겹침 훑기만큼이다.
        assert!(
            quota < plain * 4 + Duration::from_millis(5),
            "{quota:?} vs {plain:?}"
        );
        assert!(
            moved < plain * 2 + Duration::from_millis(5),
            "{moved:?} vs {plain:?}"
        );
    }
}
