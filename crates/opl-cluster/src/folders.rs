//! 폴더 트리 검사(계획 62 설계 §5). 상태 머신이 커밋 순서대로 보므로, 여러 노드가 동시에 폴더를 옮기거나 지우고
//! 리소스를 넣어도 하나만 이긴다. 폴더(`Kind::Folder`)가 하나도 없으면 리소스 쓰기에는 아무 일도 하지 않아,
//! 업그레이드 중 같은 로그를 적용한 옛 노드와 결과가 같다. 형제 이름 중복은 `exclusive.rs`가 막는다.

use std::collections::{HashMap, HashSet};

use opl_types::backup::BackupStatus;
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
        Some(k) => k.member_kinds().to_vec(),
        None => FolderKind::ALL
            .iter()
            .flat_map(|k| k.member_kinds())
            .copied()
            .collect(),
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
    // 폴더가 하나도 없어도 거절한다(리뷰 1): 마지막 폴더를 지운 뒤 커밋된 넣기가 없는 폴더를 가리키면 그 VM은 루트로
    // 평가되고 할당량 밖에 놓인다. `folder` 칸은 기능 게이트 뒤라 옛 노드는 쓰지 않는다.
    if found {
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

/// 할당량 검사(설계 §9) 하나. [`quota_check`]가 만들고, 폴더에 드는 리소스(VM, 계획 66의 라우터·유동 IP)를 한 번
/// 훑으며 [`QuotaCheck::add`]로 사용량을 모은 뒤 [`QuotaCheck::finish`]가 판정한다. VM 쓰기는 `check_exclusive`의 VM
/// 훑기에 붙여 한 번만 푼다.
pub(crate) struct QuotaCheck {
    before: QuotaTree,
    after: QuotaTree,
    /// 할당량 있는 조상이 있는 폴더(쓰기 전이나 뒤). 그 밖의 리소스는 세지 않는다.
    relevant: HashSet<Uuid>,
    /// 셀 종류: VM 쓰기·볼륨 늘리기는 VM, 네트워크 폴더 리소스(네트워크·라우터·유동 IP·부하 분산기) 쓰기는 그 넷, 폴더
    /// 옮기기는 모두
    members: &'static [Kind],
    /// 쓰는 리소스(훑기에서 빼고 전·후 몫을 따로 더한다)
    this: Option<Uuid>,
    old_this: Option<(Option<Uuid>, FolderUsage)>,
    new_this: Option<(Option<Uuid>, FolderUsage)>,
    /// 늘리는 볼륨의 이름과 새 크기
    resized: Option<(String, u64)>,
    before_direct: DirectUsage,
    after_direct: DirectUsage,
    /// VM을 이미 다 훑었다.
    pub(crate) scanned: bool,
    /// 할당량 `backup_bytes`(계획 68)가 걸린 폴더가 있을 때 VM 쓰기·폴더 옮기기가 세는 백업
    backups: Option<Backups>,
}

/// 할당량 `backup_bytes`(계획 68 §13)에 드는 백업. 있는 VM의 백업은 그 VM의 몫이라 VM을 옮기면 따라가고, 지운 VM의
/// 백업은 백업한 때의 VM 폴더(`vm_spec.folder`)에 든다. 백업 바이트는 상태(Task가 씀)에만 있어 백업 쓰기는 막지 않는다
/// — 넘었을 때 새 백업을 막는 것은 API다.
#[derive(Default)]
struct Backups {
    /// (VM id, 백업한 때의 VM 폴더, 바이트)
    list: Vec<(Uuid, Option<Uuid>, u64)>,
    /// VM id → 그 VM 백업의 바이트 합
    by_vm: HashMap<Uuid, u64>,
    /// 훑기에서 만난 VM(그 VM의 백업은 VM 몫으로 셌다)
    seen: HashSet<Uuid>,
}

/// 바이트가 있는 백업 전부(풀 수 없는 스펙은 뺀다)
fn load_backups(objects: &Objects) -> Result<Backups, StoreError> {
    let mut out = Backups::default();
    let kind = Kind::Backup.as_str();
    for item in objects.range((kind, 0u128)..=(kind, u128::MAX))? {
        let (_, v) = item?;
        let raw: RawResource = decode(v.value())?;
        let bytes = BackupStatus::backup_bytes_value(&raw.status);
        let Some(vm) = raw.spec["vm"].as_str().and_then(|s| s.parse().ok()) else {
            continue;
        };
        if bytes == 0 {
            continue;
        }
        out.list.push((vm, folder_of(&raw.spec["vm_spec"]), bytes));
        let sum = out.by_vm.entry(vm).or_default();
        *sum = sum.saturating_add(bytes);
    }
    Ok(out)
}

/// 네트워크 폴더에 드는 할당량 대상(계획 66 라우터·유동 IP, 계획 67 네트워크·부하 분산기)
const NETWORK_MEMBERS: &[Kind] = &[
    Kind::Network,
    Kind::Router,
    Kind::FloatingIp,
    Kind::LoadBalancer,
];

/// 폴더 옮기기가 세는 모든 대상
const ALL_MEMBERS: &[Kind] = &[
    Kind::Vm,
    Kind::Network,
    Kind::Router,
    Kind::FloatingIp,
    Kind::LoadBalancer,
];

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
    if !matches!(kind, Kind::Vm | Kind::Volume | Kind::Folder) && !NETWORK_MEMBERS.contains(&kind) {
        return Ok(None);
    }
    // 볼륨은 늘릴 때만 본다(붙지 않은 볼륨은 세지 않는다). 늘리기 Task가 이미 늘린 디스크의 실제 크기
    // (`status.disk_bytes`, Task만 쓴다)까지 올리는 기록은 막지 않는다: 디스크는 이미 컸고, 막으면 사용량이 실제보다
    // 적게 남는다(리뷰 1). 할당량은 Task가 저장소를 늘리기 전에 요청한 크기로 미리 잡는다.
    if kind == Kind::Volume {
        let size = |v: &Value| v["size_bytes"].as_u64().unwrap_or(0);
        if old.is_none_or(|o| size(spec) <= size(o)) {
            return Ok(None);
        }
        if let Some(v) = objects.get((kind.as_str(), id.as_u128()))? {
            let raw: RawResource = decode(v.value())?;
            if raw.status["disk_bytes"]
                .as_u64()
                .is_some_and(|disk| size(spec) <= disk)
            {
                return Ok(None);
            }
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
        members: &[Kind::Vm],
        this: None,
        old_this: None,
        new_this: None,
        resized: None,
        before_direct: DirectUsage::new(),
        after_direct: DirectUsage::new(),
        scanned: false,
        backups: None,
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
                check.old_this = Some((from, was));
            }
            check.this = Some(id);
            check.new_this = Some((to, new));
        }
        Kind::Network | Kind::Router | Kind::FloatingIp | Kind::LoadBalancer => {
            // 몫은 거의 늘 같아(부하 분산기는 바깥 VIP를 붙일 때만 유동 IP가 는다 — 계획 67) 만들기, 몫이 느는 바꾸기와
            // 할당량 있는 조상이 늘어나는 옮기기만 본다(계획 66).
            let (to, from) = (folder_of(spec), old.and_then(folder_of));
            let bound = quota_ancestors(&check.before, to);
            if bound.is_empty() {
                return Ok(None);
            }
            let usage = FolderUsage::of_network_member_value(kind, spec);
            if let Some(old) = old {
                let was = FolderUsage::of_network_member_value(kind, old);
                if usage.within(&was) && subset(&bound, &quota_ancestors(&check.before, from)) {
                    return Ok(None);
                }
                check.old_this = Some((from, was));
            }
            check.members = NETWORK_MEMBERS;
            check.this = Some(id);
            check.new_this = Some((to, usage));
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
            check.members = ALL_MEMBERS;
        }
        _ => unreachable!(),
    }
    if matches!(kind, Kind::Vm | Kind::Folder)
        && check
            .before
            .values()
            .chain(after.values())
            .any(|n| n.quota.is_some_and(|q| q.backup_bytes.is_some()))
    {
        let backups = load_backups(objects)?;
        let bytes = backups.by_vm.get(&id).copied().unwrap_or(0);
        for this in [&mut check.old_this, &mut check.new_this]
            .into_iter()
            .flatten()
        {
            this.1.backup_bytes = this.1.backup_bytes.saturating_add(bytes);
        }
        check.backups = Some(backups);
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
    /// 훑기에서 만난 리소스 하나(종류 `kind`, `id`, 저장된 스펙)를 센다.
    pub(crate) fn add(
        &mut self,
        objects: &Objects,
        names: &Names,
        kind: Kind,
        id: Uuid,
        spec: &Value,
    ) -> Result<(), StoreError> {
        if kind == Kind::Vm
            && let Some(b) = self.backups.as_mut()
        {
            b.seen.insert(id);
        }
        if self.this == Some(id) {
            return Ok(());
        }
        let Some(folder) = folder_of(spec).filter(|f| self.relevant.contains(f)) else {
            return Ok(());
        };
        if kind != Kind::Vm {
            let usage = FolderUsage::of_network_member_value(kind, spec);
            self.before_direct.entry(folder).or_default().add(&usage);
            self.after_direct.entry(folder).or_default().add(&usage);
            return Ok(());
        }
        let mut before = vm_usage(objects, names, spec)?;
        if let Some(b) = &self.backups {
            before.backup_bytes = b.by_vm.get(&id).copied().unwrap_or(0);
        }
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

    /// 아직 훑지 않은 대상을 훑고, 할당량을 넘으면 `QuotaExceeded`.
    pub(crate) fn finish(
        mut self,
        objects: &Objects,
        names: &Names,
    ) -> Result<Result<(), Rejection>, StoreError> {
        for kind in self.members {
            if *kind == Kind::Vm && self.scanned {
                continue;
            }
            let name = kind.as_str();
            for item in objects.range((name, 0u128)..=(name, u128::MAX))? {
                let (key, v) = item?;
                let raw: RawResource = decode(v.value())?;
                self.add(
                    objects,
                    names,
                    *kind,
                    Uuid::from_u128(key.value().1),
                    &raw.spec,
                )?;
            }
        }
        // 지운 VM의 백업은 백업한 때의 VM 폴더에 든다(쓰기 앞뒤 같다).
        if let Some(b) = &self.backups {
            for (vm, folder, bytes) in &b.list {
                let Some(f) = folder.filter(|f| self.relevant.contains(f)) else {
                    continue;
                };
                if b.seen.contains(vm) || self.this == Some(*vm) {
                    continue;
                }
                let usage = FolderUsage {
                    backup_bytes: *bytes,
                    ..FolderUsage::default()
                };
                self.before_direct.entry(f).or_default().add(&usage);
                self.after_direct.entry(f).or_default().add(&usage);
            }
        }
        if let Some((Some(f), usage)) = self.old_this {
            self.before_direct.entry(f).or_default().add(&usage);
        }
        if let Some((Some(f), usage)) = self.new_this {
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

        /// 검사 없이 저장소에 바로 쓴다(검사가 생기기 전의 데이터 흉내).
        fn raw_put(&mut self, kind: Kind, id: Uuid, spec: Value) {
            let mut resource: RawResource = Resource::new(id.to_string(), spec);
            resource.id = id;
            let txn = self.db.begin_write().unwrap();
            {
                let mut objects = txn.open_table(OBJECTS).unwrap();
                let mut names = txn.open_table(NAMES).unwrap();
                objects
                    .insert(
                        (kind.as_str(), id.as_u128()),
                        serde_json::to_vec(&resource).unwrap().as_slice(),
                    )
                    .unwrap();
                names
                    .insert((kind.as_str(), resource.name.as_str()), id.as_u128())
                    .unwrap();
            }
            txn.commit().unwrap();
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

    /// 리뷰 1: 폴더가 하나도 없어도(마지막 폴더를 지운 뒤) 없는 폴더를 가리키는 쓰기는 거절한다. 폴더 없는 쓰기와
    /// 폴더를 바꾸지 않는 쓰기는 그대로 받는다.
    #[test]
    fn a_missing_folder_is_refused_even_when_no_folder_is_left() {
        let mut sm = Sm::new();
        sm.folder(id(10), "a", None).unwrap();
        assert_eq!(sm.create(Kind::Vm, id(1), vm(Some(id(10)))), Ok(()));
        assert_eq!(sm.update(Kind::Vm, id(1), vm(None)), Ok(()));
        assert_eq!(sm.delete(Kind::Folder, id(10)), Ok(()));
        assert_eq!(
            sm.create(Kind::Vm, id(2), vm(Some(id(10)))),
            not_found(id(10))
        );
        assert_eq!(
            sm.update(Kind::Vm, id(1), vm(Some(id(10)))),
            not_found(id(10))
        );
        assert_eq!(
            sm.create(Kind::Network, id(3), json!({"folder": id(97)})),
            not_found(id(97))
        );
        assert_eq!(sm.create(Kind::Vm, id(4), vm(None)), Ok(()));
        assert_eq!(sm.update(Kind::Vm, id(4), vm(None)), Ok(()));
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

    /// 없는 폴더를 가리키는 `folder` 값(이 검사가 생기기 전에 들어간 것)은 그대로 두면 다른 칸을 바꿀 수 있다.
    #[test]
    fn an_unchanged_folder_reference_is_not_checked() {
        let mut sm = Sm::new();
        sm.raw_put(Kind::Vm, id(1), vm(Some(id(99))));
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

    /// 리뷰 1 I6: 늘리기 Task가 상태 `disk_bytes`에 적은 실제 크기까지는 할당량을 넘어도 기록한다(디스크는 이미
    /// 컸다). 그보다 큰 크기는 여전히 막는다.
    #[test]
    fn a_grown_disk_is_recorded_even_past_the_quota() {
        let mut sm = Sm::new();
        sm.create(
            Kind::Folder,
            id(1),
            quota_folder(None, json!({"disk_bytes": 100})),
        )
        .unwrap();
        sm.create(Kind::Volume, id(20), volume(90)).unwrap();
        sm.create(Kind::Vm, id(10), sized_vm(Some(id(1)), 1, 1, &[id(20)]))
            .unwrap();
        assert!(exceeded(sm.update(Kind::Volume, id(20), volume(104))).is_some());
        sm.apply(Command::UpdateStatus {
            kind: Kind::Volume,
            id: id(20),
            status: json!({"phase": "ready", "disk_bytes": 104}),
        })
        .unwrap();
        assert_eq!(sm.update(Kind::Volume, id(20), volume(104)), Ok(()));
        assert!(exceeded(sm.update(Kind::Volume, id(20), volume(105))).is_some());
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

    /// 백업 `bid`(VM `vm`, 백업한 때의 VM 폴더 `folder`)를 만들고 상태에 바이트를 적는다(계획 68).
    fn backup(sm: &mut Sm, bid: Uuid, vm: Uuid, folder: Option<Uuid>, status: Value) {
        sm.create(
            Kind::Backup,
            bid,
            json!({"vm": vm, "vm_name": "x", "pool": "bk", "vm_spec": {"folder": folder}}),
        )
        .unwrap();
        sm.apply(Command::UpdateStatus {
            kind: Kind::Backup,
            id: bid,
            status,
        })
        .unwrap();
    }

    fn pool_bytes(n: u64) -> Value {
        json!({"phase": "ready", "disks": [{"name": "disk0", "file": "disk0.qcow2", "virtual_bytes": 1000, "stored_bytes": n}]})
    }

    /// 계획 68: 할당량 `backup_bytes`는 VM 폴더를 따라 백업을 센다 — VM을 옮기면 그 백업 몫도 옮겨지고, 지운 VM의 백업은
    /// 백업한 때의 VM 폴더에 든다. 저장소 백업은 처음 쓴 바이트(`store_stats.new_bytes`)다.
    #[test]
    fn backup_bytes_follow_the_vm_folder() {
        let mut sm = Sm::new();
        sm.create(
            Kind::Folder,
            id(1),
            quota_folder(None, json!({"backup_bytes": 100})),
        )
        .unwrap();
        sm.folder(id(2), "plain", None).unwrap();
        sm.create(Kind::Vm, id(10), vm(Some(id(1)))).unwrap();
        sm.create(Kind::Vm, id(11), vm(Some(id(2)))).unwrap();
        sm.create(Kind::Vm, id(12), vm(Some(id(2)))).unwrap();
        sm.create(Kind::Vm, id(13), vm(Some(id(2)))).unwrap();
        backup(&mut sm, id(30), id(10), Some(id(1)), pool_bytes(30));
        // 지운 VM(99)의 백업은 백업한 때의 폴더(1)에 든다.
        backup(&mut sm, id(31), id(99), Some(id(1)), pool_bytes(50));
        // VM 11의 저장소 백업 — 백업한 때는 폴더 1이었어도 VM이 있으면 VM 폴더(2)를 따른다.
        backup(
            &mut sm,
            id(32),
            id(11),
            Some(id(1)),
            json!({"phase": "ready", "disks": [{"name": "disk0", "file": "disk0.fidx", "virtual_bytes": 1000, "stored_bytes": 999}], "store_stats": {"new_bytes": 30}}),
        );
        backup(&mut sm, id(33), id(12), None, pool_bytes(10));
        // 30 + 50 + 30 = 110 > 100
        assert_eq!(
            exceeded(sm.update(Kind::Vm, id(11), vm(Some(id(1))))),
            Some((id(1), QuotaDimension::BackupBytes, 100, 110))
        );
        // 30 + 50 + 10 = 90
        assert_eq!(sm.update(Kind::Vm, id(12), vm(Some(id(1)))), Ok(()));
        // 백업 없는 VM은 늘리지 않는다.
        assert_eq!(sm.update(Kind::Vm, id(13), vm(Some(id(1)))), Ok(()));
        // 폴더 옮기기도 하위 트리 VM의 백업을 가져온다.
        sm.folder(id(3), "sub", None).unwrap();
        assert_eq!(sm.update(Kind::Vm, id(11), vm(Some(id(3)))), Ok(()));
        assert_eq!(
            exceeded(sm.update(Kind::Folder, id(3), folder("sub", "vm", Some(id(1))))),
            Some((id(1), QuotaDimension::BackupBytes, 100, 120))
        );
        // 백업을 지우면 자리가 난다.
        sm.delete(Kind::Backup, id(31)).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(3), folder("sub", "vm", Some(id(1)))),
            Ok(())
        );
        // 넘었어도(백업 상태는 막지 않는다) 늘리지 않는 쓰기는 받는다.
        sm.apply(Command::UpdateStatus {
            kind: Kind::Backup,
            id: id(30),
            status: pool_bytes(500),
        })
        .unwrap();
        assert_eq!(sm.create(Kind::Vm, id(14), vm(Some(id(1)))), Ok(()));
    }

    /// 계획 68: 백업 저장소 하나는 한 곳(풀이나 S3 버킷·접두사)에만 둔다.
    #[test]
    fn a_store_location_holds_one_store() {
        let mut sm = Sm::new();
        sm.create(Kind::BackupStore, id(1), json!({"backend": {"pool": "bk"}}))
            .unwrap();
        assert_eq!(
            in_use(sm.create(Kind::BackupStore, id(2), json!({"backend": {"pool": "bk"}}))),
            Some((
                Kind::BackupStore,
                "backup-store-target".into(),
                "pool:bk".into(),
                id(1)
            ))
        );
        let s3 = |prefix: &str| {
            json!({"backend": {"s3": {"endpoint": "https://s3.example.com", "bucket": "orbi-bk",
                "prefix": prefix, "access_key_id": "k", "secret_access_key": "s"}}})
        };
        sm.create(Kind::BackupStore, id(2), s3("a")).unwrap();
        assert!(in_use(sm.create(Kind::BackupStore, id(3), s3("a/"))).is_some());
        assert_eq!(sm.create(Kind::BackupStore, id(3), s3("b")), Ok(()));
    }

    /// 계획 72: 한 원본 VM(`instance_uuid`)은 끝나지 않은 VMware 이전 항목 하나에만 든다. 취소한 항목은 놓아 준다.
    #[test]
    fn a_source_vm_is_in_one_unfinished_migration_item() {
        let mut sm = Sm::new();
        let item = |m: u128| json!({"migration": id(m), "source_ref": "vm-12", "instance_uuid": "5003-aa", "target_name": "web"});
        sm.create(Kind::VmwareVm, id(1), item(10)).unwrap();
        assert_eq!(
            in_use(sm.create(Kind::VmwareVm, id(2), item(11))),
            Some((Kind::VmwareVm, "vmware-vm".into(), "5003-aa".into(), id(1)))
        );
        sm.apply(Command::UpdateStatus {
            kind: Kind::VmwareVm,
            id: id(1),
            status: json!({"state": "cancelled"}),
        })
        .unwrap();
        assert_eq!(sm.create(Kind::VmwareVm, id(2), item(11)), Ok(()));
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

    fn net_quota_folder(parent: Option<Uuid>, quota: Value) -> Value {
        let mut v = folder("nq", "network", parent);
        v["quota"] = quota;
        v
    }

    fn router(folder: Option<Uuid>) -> Value {
        let mut v = json!({});
        if let Some(f) = folder {
            v["folder"] = json!(f);
        }
        v
    }

    fn fip(folder: Option<Uuid>, address: &str) -> Value {
        let mut v = json!({"network": "ext", "address": address});
        if let Some(f) = folder {
            v["folder"] = json!(f);
        }
        v
    }

    /// 계획 66: 네트워크 폴더의 라우터·유동 IP 수 할당량. 늘리는 쓰기(만들기·옮겨 넣기)만 막는다.
    #[test]
    fn router_and_floating_ip_quotas_count_network_folder_members() {
        let mut sm = Sm::new();
        sm.create(
            Kind::Folder,
            id(1),
            net_quota_folder(None, json!({"routers": 1, "floating_ips": 2})),
        )
        .unwrap();
        sm.create(Kind::Folder, id(2), folder("sub", "network", Some(id(1))))
            .unwrap();
        assert_eq!(sm.create(Kind::Router, id(10), router(Some(id(2)))), Ok(()));
        assert_eq!(
            exceeded(sm.create(Kind::Router, id(11), router(Some(id(1))))),
            Some((id(1), QuotaDimension::Routers, 1, 2))
        );
        // 루트의 라우터는 세지 않고, 루트에서 할당량 폴더로 옮기면 넘는다.
        assert_eq!(sm.create(Kind::Router, id(11), router(None)), Ok(()));
        assert!(exceeded(sm.update(Kind::Router, id(11), router(Some(id(2))))).is_some());
        // 같은 폴더 안의 바꾸기는 늘리지 않는다.
        let mut described = router(Some(id(2)));
        described["description"] = json!("edge");
        assert_eq!(sm.update(Kind::Router, id(10), described), Ok(()));
        // 라우터 수와 유동 IP 수는 따로 센다. 네트워크는 세지 않는다.
        assert_eq!(
            sm.create(Kind::Network, id(20), json!({"sdn": {}, "folder": id(2)})),
            Ok(())
        );
        assert_eq!(
            sm.create(Kind::FloatingIp, id(30), fip(Some(id(2)), "203.0.113.10")),
            Ok(())
        );
        assert_eq!(
            sm.create(Kind::FloatingIp, id(31), fip(Some(id(1)), "203.0.113.11")),
            Ok(())
        );
        assert_eq!(
            exceeded(sm.create(Kind::FloatingIp, id(32), fip(Some(id(2)), "203.0.113.12"))),
            Some((id(1), QuotaDimension::FloatingIps, 2, 3))
        );
        // 하위 트리를 할당량 폴더로 옮기면 그 라우터·유동 IP가 더해진다.
        sm.create(Kind::Folder, id(3), folder("other", "network", None))
            .unwrap();
        sm.create(Kind::Router, id(12), router(Some(id(3))))
            .unwrap();
        assert!(
            exceeded(sm.update(Kind::Folder, id(3), folder("other", "network", Some(id(1)))))
                .is_some()
        );
        sm.delete(Kind::Router, id(10)).unwrap();
        assert_eq!(
            sm.update(Kind::Folder, id(3), folder("other", "network", Some(id(1)))),
            Ok(())
        );
        // 라우터·유동 IP가 든 폴더는 지우지 않는다.
        assert_eq!(
            sm.delete(Kind::Folder, id(3)),
            Err(Rejection::FolderNotEmpty { id: id(3) })
        );
        // 라우터는 네트워크 폴더에만 든다.
        sm.create(Kind::Folder, id(4), folder("vms", "vm", None))
            .unwrap();
        assert_eq!(
            sm.create(Kind::Router, id(13), router(Some(id(4)))),
            not_found(id(4))
        );
    }

    fn in_use(r: Result<(), Rejection>) -> Option<(Kind, String, String, Uuid)> {
        match r {
            Err(Rejection::InUse {
                kind,
                field,
                value,
                holder,
            }) => Some((kind, field, value, holder)),
            _ => None,
        }
    }

    /// 계획 66: 주소 `ip`는 VM NIC·라우터 외부 주소·유동 IP 사이에서도 겹치지 않는다. 라우터 인터페이스 네트워크, 유동
    /// IP의 NIC, 공급자 네트워크의 physnet·VLAN도 하나뿐이다.
    #[test]
    fn routing_values_are_exclusive_across_kinds() {
        let mut sm = Sm::new();
        let nic_vm = |ip: &str| {
            let mut v = vm(None);
            v["nics"] = json!([{"network": "ext", "mac": "52:54:00:00:00:01", "ips": [ip]}]);
            v
        };
        sm.create(Kind::Vm, id(1), nic_vm("203.0.113.5")).unwrap();
        let ext = |ip: &str| json!({"external": {"network": "ext", "ips": [ip]}});
        assert_eq!(
            in_use(sm.create(Kind::Router, id(10), ext("203.0.113.5"))),
            Some((Kind::Router, "ip".into(), "ext/203.0.113.5".into(), id(1)))
        );
        sm.create(Kind::Router, id(10), ext("203.0.113.6")).unwrap();
        assert_eq!(
            in_use(sm.create(Kind::FloatingIp, id(20), fip(None, "203.0.113.6"))),
            Some((
                Kind::FloatingIp,
                "ip".into(),
                "ext/203.0.113.6".into(),
                id(10)
            ))
        );
        sm.create(Kind::FloatingIp, id(20), fip(None, "203.0.113.7"))
            .unwrap();
        // VM 쪽에서 본 겹침도 같다.
        assert_eq!(
            in_use(sm.create(Kind::Vm, id(2), {
                let mut v = nic_vm("203.0.113.7");
                v["nics"][0]["mac"] = json!("52:54:00:00:00:02");
                v
            })),
            Some((Kind::Vm, "ip".into(), "ext/203.0.113.7".into(), id(20)))
        );
        // 다른 네트워크의 같은 주소는 된다.
        assert_eq!(
            sm.create(
                Kind::FloatingIp,
                id(21),
                json!({"network": "ext2", "address": "203.0.113.5"})
            ),
            Ok(())
        );
        // 한 네트워크는 라우터 하나의 인터페이스
        sm.create(
            Kind::Router,
            id(11),
            json!({"interfaces": [{"network": "t1"}]}),
        )
        .unwrap();
        assert_eq!(
            in_use(sm.create(
                Kind::Router,
                id(12),
                json!({"interfaces": [{"network": "t2"}, {"network": "t1"}]})
            )),
            Some((Kind::Router, "router-net".into(), "t1".into(), id(11)))
        );
        // NIC 하나에 유동 IP 하나
        let target = json!({"vm": id(1), "mac": "52:54:00:00:00:01"});
        let mut a = fip(None, "203.0.113.8");
        a["target"] = target.clone();
        sm.create(Kind::FloatingIp, id(22), a).unwrap();
        let mut b = fip(None, "203.0.113.9");
        b["target"] = target;
        assert_eq!(
            in_use(sm.create(Kind::FloatingIp, id(23), b)).map(|r| r.1),
            Some("fip-nic".into())
        );
        // 같은 physnet·VLAN의 공급자 네트워크는 하나뿐이다(flat과 VLAN은 다르다).
        let provider = |vlan: Option<u16>| {
            let mut p = json!({"physnet": "ext"});
            if let Some(v) = vlan {
                p["vlan"] = json!(v);
            }
            json!({"sdn": {"provider": p}})
        };
        sm.create(Kind::Network, id(30), provider(None)).unwrap();
        sm.create(Kind::Network, id(31), provider(Some(100)))
            .unwrap();
        assert_eq!(
            in_use(sm.create(Kind::Network, id(32), provider(Some(100)))),
            Some((Kind::Network, "physnet".into(), "ext/100".into(), id(31)))
        );
        // 오버레이 네트워크는 지금처럼 겹침 값이 없다.
        sm.create(Kind::Network, id(33), json!({"sdn": {}}))
            .unwrap();
        sm.create(Kind::Network, id(34), json!({"sdn": {}}))
            .unwrap();
    }

    /// 계획 67: 네트워크 폴더의 네트워크·부하 분산기 수, 바깥 VIP는 유동 IP로도 센다. 바깥 VIP를 붙이는 바꾸기도 막는다.
    #[test]
    fn network_and_load_balancer_quotas_count_network_folder_members() {
        let mut sm = Sm::new();
        sm.create(
            Kind::Folder,
            id(1),
            net_quota_folder(
                None,
                json!({"networks": 1, "load_balancers": 2, "floating_ips": 1}),
            ),
        )
        .unwrap();
        let net = |folder: Option<Uuid>| {
            let mut v = json!({"sdn": {}});
            if let Some(f) = folder {
                v["folder"] = json!(f);
            }
            v
        };
        assert_eq!(sm.create(Kind::Network, id(10), net(Some(id(1)))), Ok(()));
        assert_eq!(
            exceeded(sm.create(Kind::Network, id(11), net(Some(id(1))))),
            Some((id(1), QuotaDimension::Networks, 1, 2))
        );
        // 루트에서 옮겨 넣으면 넘는다. 같은 폴더 안 바꾸기는 된다.
        assert_eq!(sm.create(Kind::Network, id(11), net(None)), Ok(()));
        assert!(exceeded(sm.update(Kind::Network, id(11), net(Some(id(1))))).is_some());
        let mut mtu = net(Some(id(1)));
        mtu["mtu"] = json!(1400);
        assert_eq!(sm.update(Kind::Network, id(10), mtu), Ok(()));
        let lb = |n: u8, external: bool| {
            let mut v = json!({
                "folder": id(1),
                "network": "t1",
                "vip": format!("10.10.0.{n}"),
                "listeners": [{"protocol": "tcp", "port": 80}]
            });
            if external {
                v["external"] = json!({"network": "ext", "ip": format!("203.0.113.{n}")});
            }
            v
        };
        assert_eq!(sm.create(Kind::LoadBalancer, id(20), lb(20, true)), Ok(()));
        // 바깥 VIP가 유동 IP 하나를 썼으므로 유동 IP는 넘는다.
        assert_eq!(
            exceeded(sm.create(Kind::FloatingIp, id(30), fip(Some(id(1)), "203.0.113.30"))),
            Some((id(1), QuotaDimension::FloatingIps, 1, 2))
        );
        assert_eq!(sm.create(Kind::LoadBalancer, id(21), lb(21, false)), Ok(()));
        // 바깥 VIP를 붙이는 바꾸기는 유동 IP를 늘린다.
        assert_eq!(
            exceeded(sm.update(Kind::LoadBalancer, id(21), lb(21, true))),
            Some((id(1), QuotaDimension::FloatingIps, 1, 2))
        );
        assert_eq!(
            exceeded(sm.create(Kind::LoadBalancer, id(22), lb(22, false))),
            Some((id(1), QuotaDimension::LoadBalancers, 2, 3))
        );
        // 떼는 바꾸기는 줄이므로 된다.
        assert_eq!(sm.update(Kind::LoadBalancer, id(20), lb(20, false)), Ok(()));
        assert_eq!(sm.update(Kind::LoadBalancer, id(21), lb(21, true)), Ok(()));
        // 부하 분산기·옮기기가 든 폴더는 지우지 않는다.
        assert_eq!(
            sm.delete(Kind::Folder, id(1)),
            Err(Rejection::FolderNotEmpty { id: id(1) })
        );
        // 옮기기는 할당량이 없고 네트워크 폴더에만 든다.
        sm.create(Kind::Folder, id(4), folder("vms", "vm", None))
            .unwrap();
        assert_eq!(
            sm.create(
                Kind::NetworkMigration,
                id(40),
                json!({"folder": id(4), "source": "lan", "target": "t-lan"})
            ),
            not_found(id(4))
        );
        assert_eq!(
            sm.create(
                Kind::NetworkMigration,
                id(40),
                json!({"folder": id(1), "source": "lan", "target": "t-lan"})
            ),
            Ok(())
        );
    }

    /// 계획 67: 부하 분산기 주소는 VM·라우터·유동 IP와 한 `ip` 공간이고, 끝나지 않은 옮기기는 원본·VM을 하나만 잡는다.
    #[test]
    fn sdn_service_values_are_exclusive() {
        let mut sm = Sm::new();
        let mut v = vm(None);
        v["nics"] = json!([{"network": "t1", "mac": "52:54:00:00:00:01", "ips": ["10.10.0.5"]}]);
        sm.create(Kind::Vm, id(1), v).unwrap();
        let lb = |vip: &str, monitor: &str| {
            json!({
                "network": "t1",
                "vip": vip,
                "listeners": [{"protocol": "tcp", "port": 80}],
                "monitors": [{"network": "t1", "ip": monitor}]
            })
        };
        assert_eq!(
            in_use(sm.create(Kind::LoadBalancer, id(10), lb("10.10.0.5", "10.10.0.60"))),
            Some((
                Kind::LoadBalancer,
                "ip".into(),
                "t1/10.10.0.5".into(),
                id(1)
            ))
        );
        assert_eq!(
            in_use(sm.create(Kind::LoadBalancer, id(10), lb("10.10.0.50", "10.10.0.5"))),
            Some((
                Kind::LoadBalancer,
                "ip".into(),
                "t1/10.10.0.5".into(),
                id(1)
            ))
        );
        sm.create(Kind::LoadBalancer, id(10), lb("10.10.0.50", "10.10.0.60"))
            .unwrap();
        // 라우터·유동 IP·VM 쪽에서 본 겹침도 같다.
        assert_eq!(
            in_use(sm.create(
                Kind::FloatingIp,
                id(20),
                json!({"network": "t1", "address": "10.10.0.60"})
            )),
            Some((
                Kind::FloatingIp,
                "ip".into(),
                "t1/10.10.0.60".into(),
                id(10)
            ))
        );
        let mut v2 = vm(None);
        v2["nics"] = json!([{"network": "t1", "mac": "52:54:00:00:00:02", "ips": ["10.10.0.50"]}]);
        assert_eq!(
            in_use(sm.create(Kind::Vm, id(2), v2)),
            Some((Kind::Vm, "ip".into(), "t1/10.10.0.50".into(), id(10)))
        );
        // 한 네트워크에 끝나지 않은 옮기기 하나, 한 VM은 한 옮기기에만
        let vm1 = id(1).to_string();
        let mig =
            |source: &str, vms: &[&str]| json!({"source": source, "target": "t-lan", "vms": vms});
        sm.create(Kind::NetworkMigration, id(30), mig("lan", &[&vm1]))
            .unwrap();
        assert_eq!(
            in_use(sm.create(Kind::NetworkMigration, id(31), mig("lan", &[]))),
            Some((
                Kind::NetworkMigration,
                "migration-source".into(),
                "lan".into(),
                id(30)
            ))
        );
        assert_eq!(
            in_use(sm.create(Kind::NetworkMigration, id(31), mig("lan2", &[&vm1]))),
            Some((
                Kind::NetworkMigration,
                "migration-vm".into(),
                vm1.clone(),
                id(30)
            ))
        );
        // 상태에만 있는 VM(비운 `vms` — 만들 때 계획을 적음)도 잡는다.
        let mut resource: RawResource = Resource::new(id(32).to_string(), mig("lan3", &[]));
        resource.id = id(32);
        resource.status = json!({"phase": "planned", "vms": [{"vm": vm1, "nic": 0}]});
        assert_eq!(
            in_use(sm.apply(Command::Create {
                kind: Kind::NetworkMigration,
                resource
            }))
            .map(|r| r.1),
            Some("migration-vm".into())
        );
        // 끝난 옮기기는 더 잡지 않는다.
        sm.apply(Command::UpdateStatus {
            kind: Kind::NetworkMigration,
            id: id(30),
            status: json!({"phase": "finished"}),
        })
        .unwrap();
        assert_eq!(
            sm.create(Kind::NetworkMigration, id(31), mig("lan", &[&vm1])),
            Ok(())
        );
    }
}
