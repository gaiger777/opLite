//! 유효 권한의 참조 구현(계획 62 설계 §6.1, IO 없음). 대상에서 조상 폴더를 따라 루트까지 걸어 올라가며 grant를
//! 합친다. 빠르지 않다: API 노드의 평가 색인(opl-api `authz/`)이 같은 답을 내는지 견주는 시험 기준이다.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::auth::Role;
use crate::authz::{
    GrantSpec, GrantTarget, GroupSpec, PrincipalRef, PrivSet, RoleRef, TokenScope,
    builtin_privileges,
};
use crate::folder::{FolderKind, MAX_FOLDER_DEPTH};
use crate::realm::same_group;
use crate::resource::Kind;

/// 평가에 드는 상태만 모은 것
#[derive(Debug, Clone, Default)]
pub struct AuthzSnapshot {
    pub folders: HashMap<Uuid, FolderNode>,
    pub users: HashMap<Uuid, UserEntry>,
    pub groups: HashMap<Uuid, GroupSpec>,
    /// 사용자 정의 역할의 권한(모르는 이름은 이미 버린 것)
    pub roles: HashMap<Uuid, PrivSet>,
    pub grants: Vec<GrantSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FolderNode {
    pub parent: Option<Uuid>,
    pub kind: FolderKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEntry {
    /// 전역 역할 = 루트에 건 propagate grant
    pub role: Role,
    /// 외부 사용자의 영역. 로컬 사용자는 None
    pub realm: Option<String>,
    pub external_groups: Vec<String>,
}

/// 권한을 묻는 대상
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthzTarget {
    /// 클러스터 객체(노드·설정·영역 등)와 루트 자체
    Root,
    /// 폴더 자체(이름 바꾸기·지우기·그 폴더의 권한)
    Folder(Uuid),
    /// 폴더에 드는 객체. `folder`가 없거나 없는 폴더를 가리키면 그 종류의 루트에 있다.
    Object {
        kind: Kind,
        id: Uuid,
        folder: Option<Uuid>,
    },
}

/// `folder`부터 루트 쪽으로 조상 폴더(자기 포함). 없는 폴더에서 멈추고, 순환과 너무 깊은 트리는 [`MAX_FOLDER_DEPTH`]
/// 넘게 걷지 않는다(상태 머신이 막지만 참조 구현도 끝나야 한다).
pub fn ancestors(snap: &AuthzSnapshot, folder: Option<Uuid>) -> Vec<Uuid> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = folder;
    while let Some(id) = cur {
        let Some(node) = snap.folders.get(&id) else {
            break;
        };
        if !seen.insert(id) || out.len() >= MAX_FOLDER_DEPTH {
            break;
        }
        out.push(id);
        cur = node.parent;
    }
    out
}

/// 사용자가 든 로컬 그룹: `members`에 있거나, 외부 사용자의 `external_groups`에 그 영역의 연결 그룹이 있다.
pub fn groups_of(snap: &AuthzSnapshot, user: Uuid) -> HashSet<Uuid> {
    let entry = snap.users.get(&user);
    snap.groups
        .iter()
        .filter(|(_, g)| {
            g.members.contains(&user)
                || entry.is_some_and(|u| {
                    u.realm.as_deref().is_some_and(|realm| {
                        g.external.iter().any(|e| {
                            e.realm == realm
                                && u.external_groups.iter().any(|x| same_group(x, &e.group))
                        })
                    })
                })
        })
        .map(|(id, _)| *id)
        .collect()
}

fn role_privileges(snap: &AuthzSnapshot, role: &RoleRef) -> PrivSet {
    match role {
        RoleRef::Builtin { role } => builtin_privileges(*role),
        RoleRef::Custom { id } => snap.roles.get(id).copied().unwrap_or_default(),
    }
}

/// grant 대상 `on`(propagate `propagate`)이 `target`에 닿는가. `chain`은 대상 쪽 조상 폴더(자기 폴더 포함)다.
pub fn reaches(on: &GrantTarget, propagate: bool, target: &AuthzTarget, chain: &[Uuid]) -> bool {
    match (on, target) {
        (GrantTarget::Root, AuthzTarget::Root) => true,
        (GrantTarget::Root, _) => propagate,
        // 폴더 자체에는 propagate와 관계없이, 조상 폴더의 grant는 propagate일 때만
        (GrantTarget::Folder { id }, AuthzTarget::Folder(f)) => {
            id == f || (propagate && chain.contains(id))
        }
        (GrantTarget::Folder { id }, AuthzTarget::Object { .. }) => propagate && chain.contains(id),
        (GrantTarget::Object { kind, id }, AuthzTarget::Object { kind: k, id: i, .. }) => {
            kind == k && id == i
        }
        _ => false,
    }
}

pub fn chain_of(snap: &AuthzSnapshot, target: &AuthzTarget) -> Vec<Uuid> {
    match target {
        AuthzTarget::Root => Vec::new(),
        AuthzTarget::Folder(f) => ancestors(snap, Some(*f)),
        AuthzTarget::Object { folder, .. } => ancestors(snap, *folder),
    }
}

/// 사용자 `user`의 `target`에서의 유효 권한(설계 §6.1): 전역 역할(루트 암묵 grant) ∪ 사용자와 그 그룹의 grant
/// 가운데 대상에 직접 건 것과 조상에 propagate로 건 것. 하나라도 있으면 `system.read`를 품는다. 없는 사용자는
/// 빈 집합이다.
pub fn effective(snap: &AuthzSnapshot, user: Uuid, target: &AuthzTarget) -> PrivSet {
    let Some(entry) = snap.users.get(&user) else {
        return PrivSet::EMPTY;
    };
    let chain = chain_of(snap, target);
    let groups = groups_of(snap, user);
    let mut set = builtin_privileges(entry.role);
    for g in &snap.grants {
        let mine = match g.principal {
            PrincipalRef::User { id } => id == user,
            PrincipalRef::Group { id } => groups.contains(&id),
        };
        if mine && reaches(&g.target, g.propagate, target, &chain) {
            set |= role_privileges(snap, &g.role);
        }
    }
    set.with_implied()
}

/// 범위 있는 API 토큰의 유효 권한(설계 §6.6): 주인 권한 ∩ 범위 천장. 천장은 `targets`마다 `privileges`를 건
/// propagate grant로 읽는다. 범위가 없으면 주인과 같다.
pub fn effective_scoped(
    snap: &AuthzSnapshot,
    user: Uuid,
    scope: Option<&TokenScope>,
    target: &AuthzTarget,
) -> PrivSet {
    let owner = effective(snap, user, target);
    let Some(scope) = scope else {
        return owner;
    };
    let chain = chain_of(snap, target);
    let ceiling = if scope
        .targets
        .iter()
        .any(|t| reaches(t, true, target, &chain))
    {
        scope.privset().with_implied()
    } else {
        PrivSet::EMPTY
    };
    owner & ceiling
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{ExternalGroup, Privilege};

    const ALICE: Uuid = Uuid::from_u128(0xa1);
    const BOB: Uuid = Uuid::from_u128(0xb0);
    const TEAM_A: Uuid = Uuid::from_u128(0x10);
    const PROD: Uuid = Uuid::from_u128(0x11);
    const TEAM_B: Uuid = Uuid::from_u128(0x20);
    const NET_A: Uuid = Uuid::from_u128(0x30);
    const GROUP: Uuid = Uuid::from_u128(0x40);
    const ROLE: Uuid = Uuid::from_u128(0x50);

    fn user(role: Role) -> UserEntry {
        UserEntry {
            role,
            realm: None,
            external_groups: vec![],
        }
    }

    /// /teamA/prod, /teamB(VM 트리), netA(네트워크 트리)
    fn snap() -> AuthzSnapshot {
        let mut s = AuthzSnapshot::default();
        for (id, parent, kind) in [
            (TEAM_A, None, FolderKind::Vm),
            (PROD, Some(TEAM_A), FolderKind::Vm),
            (TEAM_B, None, FolderKind::Vm),
            (NET_A, None, FolderKind::Network),
        ] {
            s.folders.insert(id, FolderNode { parent, kind });
        }
        s.users.insert(ALICE, user(Role::None));
        s.users.insert(BOB, user(Role::Viewer));
        s
    }

    fn vm(id: u128, folder: Option<Uuid>) -> AuthzTarget {
        AuthzTarget::Object {
            kind: Kind::Vm,
            id: Uuid::from_u128(id),
            folder,
        }
    }

    fn grant(
        principal: PrincipalRef,
        role: RoleRef,
        target: GrantTarget,
        propagate: bool,
    ) -> GrantSpec {
        GrantSpec {
            principal,
            role,
            target,
            propagate,
        }
    }

    const OPERATOR: RoleRef = RoleRef::Builtin {
        role: Role::Operator,
    };

    #[test]
    fn global_role_is_a_root_grant() {
        let s = snap();
        for t in [
            AuthzTarget::Root,
            AuthzTarget::Folder(PROD),
            vm(1, Some(PROD)),
            vm(2, None),
        ] {
            assert_eq!(effective(&s, BOB, &t), builtin_privileges(Role::Viewer));
            assert_eq!(effective(&s, ALICE, &t), PrivSet::EMPTY);
        }
        assert_eq!(
            effective(&s, Uuid::from_u128(999), &AuthzTarget::Root),
            PrivSet::EMPTY
        );
    }

    #[test]
    fn folder_grants_propagate_down_only() {
        let mut s = snap();
        s.grants.push(grant(
            PrincipalRef::User { id: ALICE },
            OPERATOR,
            GrantTarget::Folder { id: TEAM_A },
            true,
        ));
        let op = builtin_privileges(Role::Operator);
        assert_eq!(effective(&s, ALICE, &vm(1, Some(TEAM_A))), op);
        assert_eq!(effective(&s, ALICE, &vm(1, Some(PROD))), op);
        assert_eq!(effective(&s, ALICE, &AuthzTarget::Folder(PROD)), op);
        assert_eq!(effective(&s, ALICE, &AuthzTarget::Folder(TEAM_A)), op);
        assert_eq!(effective(&s, ALICE, &vm(1, Some(TEAM_B))), PrivSet::EMPTY);
        assert_eq!(effective(&s, ALICE, &vm(1, None)), PrivSet::EMPTY);
        assert_eq!(effective(&s, ALICE, &AuthzTarget::Root), PrivSet::EMPTY);
        // 없는 폴더를 가리키면 루트에 있는 것으로 본다.
        assert_eq!(
            effective(&s, ALICE, &vm(1, Some(Uuid::from_u128(77)))),
            PrivSet::EMPTY
        );
    }

    #[test]
    fn non_propagating_grants_touch_only_their_target() {
        let mut s = snap();
        s.grants.push(grant(
            PrincipalRef::User { id: ALICE },
            OPERATOR,
            GrantTarget::Folder { id: TEAM_A },
            false,
        ));
        s.grants.push(grant(
            PrincipalRef::User { id: ALICE },
            RoleRef::Builtin { role: Role::Viewer },
            GrantTarget::Root,
            false,
        ));
        let op = builtin_privileges(Role::Operator);
        assert_eq!(effective(&s, ALICE, &AuthzTarget::Folder(TEAM_A)), op);
        assert_eq!(
            effective(&s, ALICE, &AuthzTarget::Folder(PROD)),
            PrivSet::EMPTY
        );
        assert_eq!(effective(&s, ALICE, &vm(1, Some(TEAM_A))), PrivSet::EMPTY);
        assert_eq!(
            effective(&s, ALICE, &AuthzTarget::Root),
            builtin_privileges(Role::Viewer)
        );
    }

    #[test]
    fn object_grants_and_custom_roles() {
        let mut s = snap();
        let power: PrivSet = Privilege::VmPower.into();
        s.roles.insert(ROLE, power);
        s.grants.push(grant(
            PrincipalRef::User { id: ALICE },
            RoleRef::Custom { id: ROLE },
            GrantTarget::Object {
                kind: Kind::Vm,
                id: Uuid::from_u128(1),
            },
            false,
        ));
        // 객체 grant는 propagate와 관계없이 그 객체에 닿고, 모든 권한은 보기를 품는다.
        let want = power | Privilege::SystemRead.into();
        assert_eq!(effective(&s, ALICE, &vm(1, Some(TEAM_B))), want);
        assert_eq!(effective(&s, ALICE, &vm(1, None)), want);
        assert_eq!(effective(&s, ALICE, &vm(2, Some(TEAM_B))), PrivSet::EMPTY);
        // 같은 id라도 종류가 다르면 아니다.
        let net = AuthzTarget::Object {
            kind: Kind::Network,
            id: Uuid::from_u128(1),
            folder: None,
        };
        assert_eq!(effective(&s, ALICE, &net), PrivSet::EMPTY);
        // 지운 역할은 아무것도 주지 않는다.
        s.roles.clear();
        assert_eq!(effective(&s, ALICE, &vm(1, None)), PrivSet::EMPTY);
    }

    #[test]
    fn group_grants_reach_members_and_external_users() {
        let mut s = snap();
        let carol = Uuid::from_u128(0xc0);
        s.users.insert(
            carol,
            UserEntry {
                role: Role::None,
                realm: Some("corp".into()),
                external_groups: vec!["  CN=Team-A,DC=corp ".into()],
            },
        );
        let dave = Uuid::from_u128(0xd0);
        s.users.insert(
            dave,
            UserEntry {
                role: Role::None,
                realm: Some("other".into()),
                external_groups: vec!["cn=team-a,dc=corp".into()],
            },
        );
        s.groups.insert(
            GROUP,
            GroupSpec {
                members: vec![ALICE],
                external: vec![ExternalGroup {
                    realm: "corp".into(),
                    group: "cn=team-a,dc=corp".into(),
                }],
                description: String::new(),
            },
        );
        s.grants.push(grant(
            PrincipalRef::Group { id: GROUP },
            OPERATOR,
            GrantTarget::Folder { id: TEAM_A },
            true,
        ));
        let op = builtin_privileges(Role::Operator);
        let t = vm(1, Some(PROD));
        assert_eq!(effective(&s, ALICE, &t), op);
        assert_eq!(effective(&s, carol, &t), op);
        assert_eq!(
            effective(&s, dave, &t),
            PrivSet::EMPTY,
            "다른 영역의 같은 그룹 이름"
        );
        assert_eq!(effective(&s, BOB, &t), builtin_privileges(Role::Viewer));
        assert_eq!(groups_of(&s, carol), HashSet::from([GROUP]));
    }

    #[test]
    fn token_scope_is_a_ceiling() {
        let mut s = snap();
        s.users.insert(ALICE, user(Role::Operator));
        let scope = TokenScope {
            privileges: vec!["vm.power".into(), "vm.delete".into(), "vm.teleport".into()],
            targets: vec![GrantTarget::Folder { id: TEAM_A }],
        };
        let inside = vm(1, Some(PROD));
        let want: PrivSet = [
            Privilege::SystemRead,
            Privilege::VmPower,
            Privilege::VmDelete,
        ]
        .into_iter()
        .collect();
        assert_eq!(effective_scoped(&s, ALICE, Some(&scope), &inside), want);
        assert_eq!(
            effective_scoped(&s, ALICE, Some(&scope), &vm(1, Some(TEAM_B))),
            PrivSet::EMPTY
        );
        assert_eq!(
            effective_scoped(&s, ALICE, Some(&scope), &AuthzTarget::Root),
            PrivSet::EMPTY
        );
        assert_eq!(
            effective_scoped(&s, ALICE, None, &inside),
            builtin_privileges(Role::Operator)
        );
        // 범위는 주인이 갖지 않은 권한을 주지 않는다.
        s.users.insert(ALICE, user(Role::Viewer));
        assert_eq!(
            effective_scoped(&s, ALICE, Some(&scope), &inside),
            Privilege::SystemRead.into()
        );
    }

    #[test]
    fn ancestors_stop_on_cycles_and_missing_folders() {
        let mut s = snap();
        assert_eq!(ancestors(&s, Some(PROD)), vec![PROD, TEAM_A]);
        assert_eq!(ancestors(&s, None), Vec::<Uuid>::new());
        assert_eq!(ancestors(&s, Some(Uuid::from_u128(77))), Vec::<Uuid>::new());
        // 순환(상태 머신이 막지만 참조 구현도 끝나야 한다)
        s.folders.get_mut(&TEAM_A).unwrap().parent = Some(PROD);
        assert_eq!(ancestors(&s, Some(PROD)), vec![PROD, TEAM_A]);
        // 깊이 상한
        let mut deep = AuthzSnapshot::default();
        let mut parent = None;
        for i in 0..40u128 {
            let id = Uuid::from_u128(1000 + i);
            deep.folders.insert(
                id,
                FolderNode {
                    parent,
                    kind: FolderKind::Vm,
                },
            );
            parent = Some(id);
        }
        assert_eq!(ancestors(&deep, parent).len(), MAX_FOLDER_DEPTH);
    }
}
