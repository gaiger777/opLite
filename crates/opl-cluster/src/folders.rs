//! 폴더 트리 검사(계획 62 설계 §5). 상태 머신이 커밋 순서대로 보므로, 여러 노드가 동시에 폴더를 옮기거나 지우고
//! 리소스를 넣어도 하나만 이긴다. 폴더(`Kind::Folder`)가 하나도 없으면 리소스 쓰기에는 아무 일도 하지 않아,
//! 업그레이드 중 같은 로그를 적용한 옛 노드와 결과가 같다. 형제 이름 중복은 `exclusive.rs`가 막는다.

use std::collections::{HashMap, HashSet};

use opl_types::folder::{FolderKind, FolderSpec, MAX_FOLDER_DEPTH};
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
}
