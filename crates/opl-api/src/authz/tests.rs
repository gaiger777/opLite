//! 권한 색인 시험(계획 62 설계 §14): 옛 `RoleAuthorizer`와의 동치, 권한 행렬, 참조 구현과의 성질 시험, 세대
//! 무효화, 규모 기준.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use opl_types::auth::Role;
use opl_types::authz::{
    Action, ExternalGroup, GrantSpec, GrantTarget, GroupSpec, PrincipalRef, PrivSet, Privilege,
    RoleRef, TokenScope, kind_privilege,
};
use opl_types::authz_eval::{
    AuthzSnapshot, AuthzTarget, FolderNode, UserEntry, effective, effective_scoped,
};
use opl_types::folder::FolderKind;
use opl_types::resource::Kind;
use proptest::prelude::*;
use uuid::Uuid;

use super::index::{EffectiveIndex, Lru};
use super::*;
use crate::auth::{Authorizer, RoleAuthorizer};

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

fn local(role: Role) -> UserEntry {
    UserEntry {
        role,
        realm: None,
        external_groups: vec![],
    }
}

fn principal(user: Uuid, role: Role) -> Principal {
    Principal {
        user,
        name: "u".into(),
        role,
        token: None,
    }
}

fn obj(kind: Kind, n: u128, folder: Option<Uuid>) -> AuthzTarget {
    AuthzTarget::Object {
        kind,
        id: id(n),
        folder,
    }
}

/// grant가 없으면 모든 (역할, 동작, 종류)가 옛 `RoleAuthorizer`와 같다. 루트 대상과, 폴더를 따르는 종류는 루트·폴더
/// 안의 객체에서도 본다(전역 역할은 루트에 건 propagate grant다).
#[test]
fn without_grants_the_index_matches_role_authorizer() {
    let folder = id(0xf0);
    for role in Role::ALL {
        let user = id(1);
        let mut snap = AuthzSnapshot::default();
        snap.users.insert(user, local(role));
        snap.folders.insert(
            folder,
            FolderNode {
                parent: None,
                kind: FolderKind::Vm,
            },
        );
        let index = EffectiveIndex::build(&snap, user, None, 1);
        let p = principal(user, role);
        for action in Action::ALL {
            for kind in Kind::ALL {
                let want = RoleAuthorizer.check(&p, action, kind);
                let privilege = kind_privilege(action, kind);
                let mut targets = vec![AuthzTarget::Root];
                if FolderKind::of(kind).is_some() {
                    targets.push(obj(kind, 7, None));
                    targets.push(obj(kind, 7, Some(folder)));
                }
                for t in targets {
                    assert_eq!(
                        index.get(&t).contains(privilege),
                        want,
                        "{role:?} {action:?} {kind:?} {t:?}"
                    );
                }
            }
        }
    }
}

// ---- 권한 행렬 ----

const ADMIN: Uuid = Uuid::from_u128(0xa0);
const OPERATOR: Uuid = Uuid::from_u128(0xa1);
const VIEWER: Uuid = Uuid::from_u128(0xa2);
/// `none` + LDAP 외부 그룹으로 든 그룹의 /A operator grant
const CAROL: Uuid = Uuid::from_u128(0xa3);
/// `none` + /A operator
const DAVE: Uuid = Uuid::from_u128(0xa4);
/// `none` + VM a1 하나에 사용자 정의 역할(켜기·콘솔)
const ERIN: Uuid = Uuid::from_u128(0xa5);
/// `none` + /A operator, propagate 거짓
const FRANK: Uuid = Uuid::from_u128(0xa6);

const A: Uuid = Uuid::from_u128(0x10);
const AB: Uuid = Uuid::from_u128(0x11);
const C: Uuid = Uuid::from_u128(0x12);
const NET_A: Uuid = Uuid::from_u128(0x13);
const TEAM_A: Uuid = Uuid::from_u128(0x20);
const POWER_USER: Uuid = Uuid::from_u128(0x30);

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

fn operator() -> RoleRef {
    RoleRef::Builtin {
        role: Role::Operator,
    }
}

/// /A, /A/B, /C(VM 트리), /netA(네트워크 트리)
fn matrix_snapshot() -> AuthzSnapshot {
    let mut s = AuthzSnapshot::default();
    for (f, parent, kind) in [
        (A, None, FolderKind::Vm),
        (AB, Some(A), FolderKind::Vm),
        (C, None, FolderKind::Vm),
        (NET_A, None, FolderKind::Network),
    ] {
        s.folders.insert(f, FolderNode { parent, kind });
    }
    for (u, role) in [
        (ADMIN, Role::Admin),
        (OPERATOR, Role::Operator),
        (VIEWER, Role::Viewer),
        (DAVE, Role::None),
        (ERIN, Role::None),
        (FRANK, Role::None),
    ] {
        s.users.insert(u, local(role));
    }
    s.users.insert(
        CAROL,
        UserEntry {
            role: Role::None,
            realm: Some("corp".into()),
            external_groups: vec![" cn=Team-A,dc=corp ".into()],
        },
    );
    s.groups.insert(
        TEAM_A,
        GroupSpec {
            members: vec![],
            external: vec![ExternalGroup {
                realm: "corp".into(),
                group: "CN=team-a,DC=corp".into(),
            }],
            description: String::new(),
        },
    );
    s.roles.insert(
        POWER_USER,
        [Privilege::VmPower, Privilege::VmConsole]
            .into_iter()
            .collect(),
    );
    s.grants = vec![
        grant(
            PrincipalRef::Group { id: TEAM_A },
            operator(),
            GrantTarget::Folder { id: A },
            true,
        ),
        grant(
            PrincipalRef::User { id: DAVE },
            operator(),
            GrantTarget::Folder { id: A },
            true,
        ),
        grant(
            PrincipalRef::User { id: ERIN },
            RoleRef::Custom { id: POWER_USER },
            GrantTarget::Object {
                kind: Kind::Vm,
                id: id(1),
            },
            true,
        ),
        grant(
            PrincipalRef::User { id: FRANK },
            operator(),
            GrantTarget::Folder { id: A },
            false,
        ),
    ];
    s
}

#[test]
fn permission_matrix() {
    let snap = matrix_snapshot();
    let scope = TokenScope {
        privileges: vec!["vm.power".into()],
        targets: vec![GrantTarget::Folder { id: AB }],
    };
    let subjects: [(&str, Uuid, Option<&TokenScope>); 8] = [
        ("admin", ADMIN, None),
        ("operator", OPERATOR, None),
        ("viewer", VIEWER, None),
        ("carol(ldap group→/A op)", CAROL, None),
        ("dave(/A op)", DAVE, None),
        ("dave-token(vm.power@/A/B)", DAVE, Some(&scope)),
        ("erin(vm a1 power-user)", ERIN, None),
        ("frank(/A op no-propagate)", FRANK, None),
    ];
    let objects = [
        ("vm a1 in /A", obj(Kind::Vm, 1, Some(A))),
        ("vm ab1 in /A/B", obj(Kind::Vm, 2, Some(AB))),
        ("template t1 in /A", obj(Kind::Vm, 3, Some(A))),
        ("vm c1 in /C", obj(Kind::Vm, 4, Some(C))),
        ("vm r1 at root", obj(Kind::Vm, 5, None)),
        ("pool local at root", obj(Kind::StoragePool, 6, None)),
        ("network n1 in /netA", obj(Kind::Network, 7, Some(NET_A))),
        ("folder /A", AuthzTarget::Folder(A)),
        ("folder /A/B", AuthzTarget::Folder(AB)),
        ("cluster (root)", AuthzTarget::Root),
    ];
    let privileges = [
        Privilege::SystemRead,
        Privilege::VmCreate,
        Privilege::VmConfig,
        Privilege::VmPower,
        Privilege::VmConsole,
        Privilege::BackupCreate,
        Privilege::StorageAllocate,
        Privilege::NetworkAssign,
        Privilege::InventoryFolder,
        Privilege::ClusterRead,
        Privilege::ClusterAdmin,
    ];
    let mut out = String::new();
    writeln!(out, "columns: {}", privileges.map(|p| p.as_str()).join(" ")).unwrap();
    for (name, user, scope) in subjects {
        let index = EffectiveIndex::build(&snap, user, scope, 1);
        writeln!(out, "\n{name}").unwrap();
        for (oname, target) in &objects {
            let got = index.get(target);
            assert_eq!(
                got,
                effective_scoped(&snap, user, scope, target),
                "{name} {oname}"
            );
            let row: String = privileges
                .iter()
                .map(|p| if got.contains(*p) { 'x' } else { '.' })
                .collect();
            writeln!(out, "  {row}  {oname}").unwrap();
        }
    }
    insta::assert_snapshot!(out);
}

// ---- 참조 구현과의 성질 시험 ----

const USERS: u128 = 4;
const GROUPS: u128 = 3;
const OBJECT_IDS: u128 = 4;
/// 트리에 없는 폴더·역할
const MISSING: Uuid = Uuid::from_u128(0xdead);

fn folder_id(i: usize) -> Uuid {
    id(0x1000 + i as u128)
}

#[derive(Debug, Clone)]
struct World {
    snap: AuthzSnapshot,
    targets: Vec<AuthzTarget>,
}

fn arb_privset() -> impl Strategy<Value = PrivSet> {
    any::<u64>().prop_map(|bits| {
        Privilege::ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| bits & (1 << i) != 0)
            .map(|(_, p)| *p)
            .collect()
    })
}

fn arb_role() -> impl Strategy<Value = Role> {
    prop::sample::select(Role::ALL.to_vec())
}

/// 폴더 `n`개 가운데 하나, 또는 트리에 없는 폴더
fn arb_folder(n: usize) -> BoxedStrategy<Uuid> {
    if n == 0 {
        Just(MISSING).boxed()
    } else {
        prop_oneof![9 => (0..n).prop_map(folder_id), 1 => Just(MISSING)].boxed()
    }
}

fn arb_target(n: usize) -> impl Strategy<Value = GrantTarget> {
    let kind = prop::sample::select(vec![Kind::Vm, Kind::Network, Kind::StoragePool]);
    prop_oneof![
        1 => Just(GrantTarget::Root),
        4 => arb_folder(n).prop_map(|id| GrantTarget::Folder { id }),
        2 => (kind, 0..OBJECT_IDS).prop_map(|(kind, i)| GrantTarget::Object { kind, id: id(i) }),
    ]
}

fn arb_grant(n: usize) -> impl Strategy<Value = GrantSpec> {
    let principal = prop_oneof![
        (0..USERS).prop_map(|i| PrincipalRef::User { id: id(0x100 + i) }),
        (0..GROUPS).prop_map(|i| PrincipalRef::Group { id: id(0x200 + i) }),
    ];
    let role = prop_oneof![
        prop::sample::select(vec![Role::Admin, Role::Operator, Role::Viewer])
            .prop_map(|role| RoleRef::Builtin { role }),
        (0..3u128).prop_map(|i| RoleRef::Custom { id: id(0x300 + i) }),
    ];
    (principal, role, arb_target(n), any::<bool>()).prop_map(
        |(principal, role, target, propagate)| GrantSpec {
            principal,
            role,
            target,
            propagate,
        },
    )
}

fn arb_scope(n: usize) -> impl Strategy<Value = TokenScope> {
    (arb_privset(), prop::collection::vec(arb_target(n), 1..3)).prop_map(|(set, targets)| {
        TokenScope {
            privileges: set.names().into_iter().map(String::from).collect(),
            targets,
        }
    })
}

/// 무작위 트리(폴더 ≤ 40, 부모는 앞 폴더나 루트나 없는 폴더)·사용자·그룹(외부 그룹 포함)·역할·grant
fn arb_world() -> impl Strategy<Value = World> {
    (0usize..=40).prop_flat_map(|n| {
        let folders = prop::collection::vec(
            (
                any::<prop::sample::Index>(),
                0u8..10,
                prop::sample::select(FolderKind::ALL.to_vec()),
            ),
            n,
        );
        let users = prop::collection::vec(
            (
                arb_role(),
                any::<bool>(),
                prop::sample::subsequence(vec!["g1", " G2 "], 0..=2),
            ),
            USERS as usize,
        );
        let groups = prop::collection::vec(
            (
                prop::sample::subsequence((0..USERS).collect::<Vec<_>>(), 0..=2),
                prop::sample::subsequence(vec!["G1", "g2"], 0..=1),
            ),
            GROUPS as usize,
        );
        let roles = prop::collection::vec(arb_privset(), 2);
        let grants = prop::collection::vec(arb_grant(n), 0..30);
        (folders, users, groups, roles, grants).prop_map(
            move |(folders, users, groups, roles, grants)| {
                let mut snap = AuthzSnapshot::default();
                for (i, (parent, how, kind)) in folders.into_iter().enumerate() {
                    let parent = match how {
                        0..=2 => None,
                        3 => Some(MISSING),
                        _ if i == 0 => None,
                        _ => Some(folder_id(parent.index(i))),
                    };
                    snap.folders
                        .insert(folder_id(i), FolderNode { parent, kind });
                }
                for (i, (role, external, ext_groups)) in users.into_iter().enumerate() {
                    snap.users.insert(
                        id(0x100 + i as u128),
                        UserEntry {
                            role,
                            realm: external.then(|| "corp".to_string()),
                            external_groups: ext_groups.into_iter().map(String::from).collect(),
                        },
                    );
                }
                for (i, (members, ext)) in groups.into_iter().enumerate() {
                    snap.groups.insert(
                        id(0x200 + i as u128),
                        GroupSpec {
                            members: members.into_iter().map(|m| id(0x100 + m)).collect(),
                            external: ext
                                .into_iter()
                                .map(|g| ExternalGroup {
                                    realm: "corp".into(),
                                    group: g.into(),
                                })
                                .collect(),
                            description: String::new(),
                        },
                    );
                }
                // 역할 0x302는 없는 역할이다.
                for (i, set) in roles.into_iter().enumerate() {
                    snap.roles.insert(id(0x300 + i as u128), set);
                }
                snap.grants = grants;
                let mut targets = vec![AuthzTarget::Root, AuthzTarget::Folder(MISSING)];
                let mut folders: Vec<Option<Uuid>> = vec![None, Some(MISSING)];
                folders.extend((0..n).map(|i| Some(folder_id(i))));
                for f in &folders {
                    if let Some(f) = f {
                        targets.push(AuthzTarget::Folder(*f));
                    }
                    // 같은 객체를 모든 폴더에 두어 본다(옮기기: 색인은 객체의 폴더를 확인 때 읽는다).
                    for kind in [Kind::Vm, Kind::Network, Kind::StoragePool] {
                        for i in 0..OBJECT_IDS {
                            targets.push(AuthzTarget::Object {
                                kind,
                                id: id(i),
                                folder: *f,
                            });
                        }
                    }
                }
                World { snap, targets }
            },
        )
    })
}

fn users() -> impl Iterator<Item = Uuid> {
    // 마지막 하나는 없는 사용자다.
    (0..=USERS).map(|i| id(0x100 + i))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 색인 = 참조 `effective`(모든 사용자 × 루트·폴더·모든 폴더에 둔 객체)
    #[test]
    fn index_matches_reference(world in arb_world()) {
        for user in users() {
            let index = EffectiveIndex::build(&world.snap, user, None, 1);
            for t in &world.targets {
                prop_assert_eq!(index.get(t), effective(&world.snap, user, t), "{:?} {:?}", user, t);
            }
        }
    }

    /// 범위 토큰의 색인 = 참조 `effective_scoped`
    #[test]
    fn scoped_index_matches_reference(
        (world, scope) in arb_world().prop_flat_map(|w| {
            let n = w.snap.folders.len();
            (Just(w), arb_scope(n))
        })
    ) {
        for user in users() {
            let index = EffectiveIndex::build(&world.snap, user, Some(&scope), 1);
            for t in &world.targets {
                prop_assert_eq!(
                    index.get(t),
                    effective_scoped(&world.snap, user, Some(&scope), t),
                    "{:?} {:?}", user, t
                );
            }
        }
    }

    /// 단조: grant를 더하면 어느 대상에서도 권한이 줄지 않는다.
    #[test]
    fn adding_a_grant_never_removes_privileges(
        (world, extra) in arb_world().prop_flat_map(|w| {
            let n = w.snap.folders.len();
            (Just(w), arb_grant(n))
        })
    ) {
        let mut more = world.snap.clone();
        more.grants.push(extra);
        for user in users() {
            let before = EffectiveIndex::build(&world.snap, user, None, 1);
            let after = EffectiveIndex::build(&more, user, None, 2);
            for t in &world.targets {
                prop_assert!(before.get(t).is_subset(after.get(t)), "{:?} {:?}", user, t);
            }
        }
    }

    /// propagate 거짓인 폴더 grant는 그 폴더 자체에만 닿고 자손 폴더·객체에는 닿지 않는다.
    #[test]
    fn a_non_propagating_grant_stays_on_its_folder(
        (world, extra) in arb_world().prop_flat_map(|w| {
            let n = w.snap.folders.len();
            (Just(w), arb_grant(n))
        })
    ) {
        let GrantTarget::Folder { id: f } = extra.target else {
            return Ok(());
        };
        let mut more = world.snap.clone();
        more.grants.push(GrantSpec { propagate: false, ..extra });
        for user in users() {
            let before = EffectiveIndex::build(&world.snap, user, None, 1);
            let after = EffectiveIndex::build(&more, user, None, 2);
            for t in &world.targets {
                if *t != AuthzTarget::Folder(f) {
                    prop_assert_eq!(before.get(t), after.get(t), "{:?} {:?}", user, t);
                }
            }
        }
    }
}

#[test]
fn lru_keeps_recently_used_entries() {
    let mut lru = Lru::new(2);
    lru.insert(1, "a");
    lru.insert(2, "b");
    // 꽉 차면 바늘이 돌며 쓰인 표시를 지우고, 표시 없는 첫 칸을 바꾼다.
    lru.insert(3, "c");
    assert_eq!(lru.len(), 2);
    assert_eq!(lru.get(&1), None);
    assert_eq!(lru.get(&3), Some("c"));
    // 2는 그 사이 쓰이지 않았고 3은 방금 쓰였다.
    lru.insert(4, "d");
    assert_eq!(lru.get(&2), None);
    assert_eq!(lru.get(&3), Some("c"));
    assert_eq!(lru.get(&4), Some("d"));
    lru.insert(4, "e");
    assert_eq!(lru.get(&4), Some("e"));
    assert_eq!(lru.len(), 2);
}

// ---- 클러스터 위의 평가기 ----

async fn one_node_cluster(dir: &std::path::Path) -> Arc<Cluster> {
    use opl_cluster::ClusterConfig;
    use opl_cluster::memnet::MemRouter;
    use opl_cluster::types::{NodeId, OrbNode};

    let router = MemRouter::default();
    let node = NodeId(Uuid::now_v7());
    let cluster = Cluster::open(ClusterConfig::new(node, dir), router.network(node))
        .await
        .unwrap();
    router.register(node, &cluster);
    cluster
        .initialize(std::collections::BTreeMap::from([(
            node,
            OrbNode::single(node.to_string()),
        )]))
        .await
        .unwrap();
    cluster
        .raft()
        .unwrap()
        .wait(Some(Duration::from_secs(10)))
        .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
        .await
        .unwrap();
    Arc::new(cluster)
}

fn named<S: ResourceSpec>(spec: S) -> opl_types::resource::Resource<S, S::Status>
where
    S::Status: Default,
{
    let mut r = opl_types::resource::Resource::new("", spec);
    r.name = r.id.to_string();
    r
}

fn user_spec(role: Role) -> opl_types::auth::UserSpec {
    serde_json::from_value(serde_json::json!({"password_hash": "", "role": role})).unwrap()
}

fn vm_in(name: &str, folder: Option<Uuid>) -> Vm {
    let mut spec: opl_types::vm::VmSpec = serde_json::from_value(serde_json::json!({
        "cpu": {"sockets": 1, "cores": 1}, "memory_mib": 256, "machine": "virt",
    }))
    .unwrap();
    spec.folder = folder;
    opl_types::resource::Resource::new(name, spec)
}

/// 커밋되면 다음 확인부터 반영된다(세대). 못 읽으면 404, 읽으면 403. 목록은 거른다. VM 이동은 세대를 올리지 않는다.
#[tokio::test]
async fn the_evaluator_follows_commits() {
    use opl_types::auth::{ApiTokenSpec, UserSpec};
    use opl_types::authz::GrantSpec;

    let dir = tempfile::TempDir::new().unwrap();
    let cluster = one_node_cluster(dir.path()).await;
    let authz = Authz::new(cluster.clone());

    let mut alice = opl_types::resource::Resource::new("alice", user_spec(Role::None));
    alice.id = id(0xa11ce);
    cluster.create(&alice).await.unwrap();
    let p = principal(alice.id, Role::None);
    let a = named(FolderSpec {
        name: "a".into(),
        kind: FolderKind::Vm,
        parent: None,
        quota: None,
    });
    let b = named(FolderSpec {
        name: "b".into(),
        ..a.spec.clone()
    });
    cluster.create(&a).await.unwrap();
    cluster.create(&b).await.unwrap();
    let mut web = vm_in("web", Some(a.id));
    let db = vm_in("db", Some(b.id));

    // 아무 권한도 없다: 404(없는 것과 같은 문구)
    let e = authz.require(&p, Privilege::VmPower, &web).unwrap_err();
    assert_eq!(e.code, "not_found");
    assert_eq!(e.message, "vm \"web\" not found");
    let g0 = authz.generation();

    let viewer_on_a = named(GrantSpec {
        principal: PrincipalRef::User { id: alice.id },
        role: RoleRef::Builtin { role: Role::Viewer },
        target: GrantTarget::Folder { id: a.id },
        propagate: true,
    });
    cluster.create(&viewer_on_a).await.unwrap();
    assert!(authz.generation() > g0);
    // 읽을 수 있지만 켤 수는 없다: 403
    let e = authz.require(&p, Privilege::VmPower, &web).unwrap_err();
    assert_eq!(e.code, "forbidden");
    assert!(authz.require(&p, Privilege::SystemRead, &web).is_ok());
    assert!(authz.require(&p, Privilege::SystemRead, &a).is_ok());
    assert_eq!(
        authz
            .require(&p, Privilege::SystemRead, &b)
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(
        authz
            .require_root(&p, Privilege::ClusterRead)
            .unwrap_err()
            .code,
        "forbidden"
    );
    let listed = authz.filter(&p, vec![web.clone(), db.clone()]).unwrap();
    assert_eq!(
        listed.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(),
        ["web"]
    );

    // 같은 세대에서는 캐시에서 꺼낸다.
    let i1 = authz.index(Subject::from(&p)).unwrap();
    let i2 = authz.index(Subject::from(&p)).unwrap();
    assert!(Arc::ptr_eq(&i1, &i2));

    // VM 이동은 세대를 올리지 않는다. 객체의 폴더는 확인할 때 읽는다.
    let g1 = authz.generation();
    cluster.create(&web).await.unwrap();
    web.spec.folder = Some(b.id);
    cluster
        .update_spec(web.id, None, Default::default(), &web.spec)
        .await
        .unwrap();
    assert_eq!(authz.generation(), g1);
    assert_eq!(
        authz
            .require(&p, Privilege::SystemRead, &web)
            .unwrap_err()
            .code,
        "not_found"
    );

    // grant를 지우면 곧바로 빠진다. 다시 만든 색인은 처음부터 만든 것과 같다.
    cluster
        .delete::<GrantSpec>(viewer_on_a.id, None)
        .await
        .unwrap();
    assert!(authz.generation() > g1);
    assert_eq!(
        authz
            .require(&p, Privilege::SystemRead, &a)
            .unwrap_err()
            .code,
        "not_found"
    );
    let fresh = Authz::new(cluster.clone());
    for t in [
        AuthzTarget::Root,
        AuthzTarget::Folder(a.id),
        web.authz_target(),
        db.authz_target(),
    ] {
        assert_eq!(
            authz.index(Subject::from(&p)).unwrap().get(&t),
            fresh.index(Subject::from(&p)).unwrap().get(&t)
        );
    }

    // 전역 역할을 바꾸면(사용자 커밋) 곧바로 반영된다.
    let mut spec: UserSpec = alice.spec.clone();
    spec.role = Role::Viewer;
    cluster
        .update_spec(alice.id, None, Default::default(), &spec)
        .await
        .unwrap();
    assert!(authz.require(&p, Privilege::SystemRead, &db).is_ok());
    assert!(authz.require_root(&p, Privilege::ClusterRead).is_ok());

    // 범위 토큰은 주인 권한 ∩ 범위다. 남의 토큰·없는 토큰은 아무 권한도 없다.
    let token = named(ApiTokenSpec {
        user: alice.id,
        secret_hash: String::new(),
        scope: Some(TokenScope {
            privileges: vec!["system.read".into()],
            targets: vec![GrantTarget::Folder { id: a.id }],
        }),
    });
    cluster.create(&token).await.unwrap();
    let scoped = authz
        .index(Subject {
            user: alice.id,
            token: Some(token.id),
        })
        .unwrap();
    assert!(
        scoped
            .get(&AuthzTarget::Folder(a.id))
            .contains(Privilege::SystemRead)
    );
    assert!(scoped.get(&db.authz_target()).is_empty());
    assert!(scoped.get(&AuthzTarget::Root).is_empty());
    for subject in [
        Subject {
            user: id(1),
            token: Some(token.id),
        },
        Subject {
            user: alice.id,
            token: Some(id(2)),
        },
    ] {
        assert!(
            authz
                .index(subject)
                .unwrap()
                .get(&AuthzTarget::Folder(a.id))
                .is_empty()
        );
    }
}

// ---- 규모 기준 ----

/// VM 1만 개·grant 500개·폴더 200개에서 상태 뜨기를 뺀 주체 색인 만들기와 목록 거르기(확인 1만 번). 설계 §7의 기준
/// (release에서 거르기 1 ms 안)을 디버그 빌드에서도 넉넉히 넘지 않는지 본다.
#[test]
fn index_scales_to_ten_thousand_vms() {
    let mut snap = AuthzSnapshot::default();
    let folders: Vec<Uuid> = (0..200).map(|i| id(0x10_000 + i)).collect();
    for (i, f) in folders.iter().enumerate() {
        // 열 개씩 묶은 두 단계 트리
        let parent = (i % 10 != 0).then(|| folders[i - i % 10]);
        snap.folders.insert(
            *f,
            FolderNode {
                parent,
                kind: FolderKind::Vm,
            },
        );
    }
    let user = id(1);
    snap.users.insert(user, local(Role::None));
    for g in 0..50u128 {
        snap.groups.insert(
            id(0x20_000 + g),
            GroupSpec {
                members: if g % 5 == 0 { vec![user] } else { vec![] },
                external: vec![],
                description: String::new(),
            },
        );
    }
    for i in 0..500usize {
        let principal = if i % 2 == 0 {
            PrincipalRef::Group {
                id: id(0x20_000 + (i % 50) as u128),
            }
        } else {
            PrincipalRef::User {
                id: id(0x30_000 + i as u128),
            }
        };
        let target = if i % 3 == 0 {
            GrantTarget::Object {
                kind: Kind::Vm,
                id: id(i as u128),
            }
        } else {
            GrantTarget::Folder {
                id: folders[i % folders.len()],
            }
        };
        snap.grants
            .push(grant(principal, operator(), target, i % 7 != 0));
    }
    let vms: Vec<AuthzTarget> = (0..10_000u128)
        .map(|i| obj(Kind::Vm, i, Some(folders[(i % 200) as usize])))
        .collect();

    let start = Instant::now();
    let index = EffectiveIndex::build(&snap, user, None, 1);
    let built = start.elapsed();
    let start = Instant::now();
    let visible = vms
        .iter()
        .filter(|t| index.get(t).contains(Privilege::SystemRead))
        .count();
    let filtered = start.elapsed();
    eprintln!("index build {built:?}, filter 10k VMs {filtered:?} ({visible} visible)");
    assert!(visible > 0 && visible < vms.len());
    // 참조 구현과 몇 개 견준다.
    for t in vms.iter().step_by(997) {
        assert_eq!(index.get(t), effective(&snap, user, t));
    }
    assert!(
        built < Duration::from_millis(100),
        "index build took {built:?}"
    );
    assert!(
        filtered < Duration::from_millis(100),
        "filtering took {filtered:?}"
    );
}

#[test]
fn admin_needs_no_index() {
    let mut snap = AuthzSnapshot::default();
    snap.users.insert(ADMIN, local(Role::Admin));
    let index = EffectiveIndex::build(&snap, ADMIN, None, 1);
    assert!(index.is_all());
    assert_eq!(index.get(&obj(Kind::Vm, 1, Some(MISSING))), PrivSet::all());
    // 범위가 있으면 빠른 길을 쓰지 않는다.
    let scope = TokenScope {
        privileges: vec!["vm.power".into()],
        targets: vec![GrantTarget::Root],
    };
    let index = EffectiveIndex::build(&snap, ADMIN, Some(&scope), 1);
    assert!(!index.is_all());
    assert_eq!(
        index.get(&AuthzTarget::Root),
        PrivSet::from(Privilege::VmPower).with_implied()
    );
}
