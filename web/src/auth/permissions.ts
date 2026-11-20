import type { components } from "@/api/schema";
import type { Role, WatchKind } from "@/api/types";

/** 읽기, 쓰기(만들기·고치기·지우기), 동작(시작·정지와 콘솔) */
export type Action = "read" | "write" | "operate";

/** 권한을 따지는 리소스 종류. 이름은 서버의 `Kind`와 같다. */
export type Kind =
  | WatchKind
  | "settings"
  | "user"
  | "api-token"
  | "join-token"
  | "realm"
  | "remote"
  | "ha-check"
  | "ceph"
  | "sdn"
  | "alert-rule"
  | "notification-target"
  | "grant"
  | "group"
  | "access-role"
  // 라우터·유동 IP(계획 66): 네트워크 폴더에 들고 watch로 나가지 않는다.
  | "router"
  | "floating-ip";

/** 권한(계획 62 설계 §6.2). 이름은 서버의 `Privilege`와 같다. */
export const PRIVILEGES = [
  "system.read",
  "vm.create",
  "vm.delete",
  "vm.config",
  "vm.power",
  "vm.console",
  "vm.snapshot",
  "vm.migrate",
  "vm.clone",
  // 전용 CPU·hugepages(계획 64). 드문 노드 자원이라 내장 operator에는 없다.
  "vm.dedicated",
  "backup.create",
  "backup.restore",
  "backup.delete",
  "backup.job",
  "storage.allocate",
  "storage.config",
  "network.assign",
  "network.config",
  "inventory.folder",
  "inventory.move",
  "inventory.quota",
  "permission.modify",
  "alert.read",
  "alert.manage",
  "cluster.read",
  "cluster.admin",
] as const;
export type Privilege = (typeof PRIVILEGES)[number];

/** 계정과 비밀을 담은 종류. 읽기도 `cluster.admin`이다(서버 `is_account_kind`). */
const ACCOUNT: ReadonlySet<Kind> = new Set([
  "user",
  "api-token",
  "join-token",
  "realm",
  "notification-target",
  "grant",
  "group",
]);

/** 폴더를 따르는(객체마다 거르는) 종류. 나머지는 루트(클러스터 객체)의 권한으로 본다. */
const FOLDER_KINDS: ReadonlySet<Kind> = new Set([
  "vm",
  "volume",
  "snapshot",
  "backup",
  "vm-import",
  "network",
  "router",
  "floating-ip",
  "storage-pool",
  "task",
  "folder",
]);

/** 이 종류의 목록·watch를 객체마다 거르는지(루트 권한 없이도 볼 것이 있을 수 있다) */
export function perObject(kind: Kind): boolean {
  return FOLDER_KINDS.has(kind) || kind === "access-role";
}

/** 종류 단위 확인 `(action, kind)`이 요구하는 권한(서버 `kind_privilege`와 같다) */
export function kindPrivilege(action: Action, kind: Kind): Privilege {
  if (ACCOUNT.has(kind)) {
    return "cluster.admin";
  }
  switch (action) {
    case "read":
      return kind === "alert-rule"
        ? "alert.read"
        : FOLDER_KINDS.has(kind)
          ? "system.read"
          : "cluster.read";
    case "write":
      switch (kind) {
        case "vm":
          return "vm.config";
        case "vm-import":
          return "vm.create";
        case "volume":
          return "storage.allocate";
        case "backup":
          return "backup.create";
        case "backup-job":
          return "backup.job";
        case "storage-pool":
          return "storage.config";
        case "network":
        case "router":
        case "floating-ip":
        case "security-group":
          return "network.config";
        case "alert-rule":
          return "alert.manage";
        case "folder":
          return "inventory.folder";
        default:
          return "cluster.admin";
      }
    case "operate":
      switch (kind) {
        case "vm":
          return "vm.power";
        case "vm-import":
          return "vm.create";
        case "volume":
          return "storage.allocate";
        case "backup":
          return "backup.restore";
        case "backup-job":
          return "backup.job";
        default:
          return "cluster.admin";
      }
  }
}

const VIEWER: readonly Privilege[] = ["system.read", "cluster.read", "alert.read"];
const OPERATOR: readonly Privilege[] = [
  ...VIEWER,
  "vm.create",
  "vm.delete",
  "vm.config",
  "vm.power",
  "vm.console",
  "vm.snapshot",
  "vm.migrate",
  "vm.clone",
  "backup.create",
  "backup.restore",
  "backup.delete",
  "backup.job",
  "storage.allocate",
  "network.assign",
];

/** 내장 역할의 권한(서버 `builtin_privileges`). 전역 역할은 루트에 건 grant다. */
export function builtinPrivileges(role: Role): readonly Privilege[] {
  switch (role) {
    case "admin":
      return PRIVILEGES;
    case "operator":
      return OPERATOR;
    case "viewer":
      return VIEWER;
    case "none":
      return [];
  }
}

/**
 * 역할만으로 본 종류 단위 확인(옛 `RoleAuthorizer`와 같다). 권한을 아직 받지 못했을 때의 기본값이 쓰고, 객체 권한은
 * `usePermissions()`의 `Permissions`가 본다.
 */
export function can(role: Role, action: Action, kind: Kind): boolean {
  return builtinPrivileges(role).includes(kindPrivilege(action, kind));
}

/** 폴더를 둘 수 있는 객체 종류(서버 `FolderKind::member_kinds` — 네트워크 폴더에는 라우터·유동 IP도 든다, 계획 66) */
export type MemberKind = "vm" | "network" | "router" | "floating-ip" | "storage-pool";

/** 권한을 따지는 대상: 루트(클러스터), 폴더 자체, 객체(폴더 안, `folder`가 없으면 루트) */
export type Target =
  | { type: "root" }
  | { type: "folder"; id: string }
  | { type: "object"; kind: MemberKind; id?: string; folder?: string | null };

export const ROOT: Target = { type: "root" };

/** 객체 대상. `id`가 없으면 그 폴더에 새로 둘 객체다. */
export function objectTarget(
  kind: MemberKind,
  item: { id?: string; spec: { folder?: string | null } } | null,
  folder?: string | null,
): Target {
  return { type: "object", kind, id: item?.id, folder: item ? item.spec.folder : folder };
}

type Mine = components["schemas"]["MyPermissions"];

function setOf(names: readonly string[] | undefined): ReadonlySet<Privilege> {
  return new Set((names ?? []) as Privilege[]);
}

/**
 * 내 권한(`GET /permissions/mine`). 객체의 유효 권한 = 그 객체 폴더 안의 권한(`contents`, 루트면 `root_contents`)
 * ∪ 그 객체에 직접 건 권한(`objects`). 서버가 실제로 막고, 화면은 없는 동작을 숨기는 데만 쓴다.
 */
export class Permissions {
  private readonly all: boolean;
  private readonly rootSet: ReadonlySet<Privilege>;
  private readonly rootContents: ReadonlySet<Privilege>;
  private readonly folders: Map<string, ReadonlySet<Privilege>>;
  private readonly contents: Map<string, ReadonlySet<Privilege>>;
  private readonly objects: Map<string, ReadonlySet<Privilege>>;
  /** 어디서든 가진 권한(+ 만들기·팔레트가 "만들 곳이 하나라도 있나"를 본다) */
  private readonly any: ReadonlySet<Privilege>;

  constructor(mine: Mine) {
    this.all = mine.all;
    this.rootSet = setOf(mine.root);
    this.rootContents = setOf(mine.root_contents);
    this.folders = new Map(Object.entries(mine.folders).map(([id, p]) => [id, setOf(p)]));
    this.contents = new Map(Object.entries(mine.contents).map(([id, p]) => [id, setOf(p)]));
    this.objects = new Map(mine.objects.map((o) => [`${o.kind}/${o.id}`, setOf(o.privileges)]));
    this.any = new Set([
      ...this.rootSet,
      ...this.rootContents,
      ...[...this.folders.values(), ...this.contents.values(), ...this.objects.values()].flatMap(
        (s) => [...s],
      ),
    ]);
  }

  /** 전역 역할만 있는 것처럼(권한을 받기 전·옛 서버) */
  static fromRole(role: Role): Permissions {
    const privileges = [...builtinPrivileges(role)];
    return new Permissions({
      all: role === "admin",
      root: privileges,
      root_contents: privileges,
      folders: {},
      contents: {},
      objects: [],
    });
  }

  /** 대상에서 이 권한이 있는지(기본은 루트) */
  has(privilege: Privilege, target: Target = ROOT): boolean {
    if (this.all) {
      return true;
    }
    switch (target.type) {
      case "root":
        return this.rootSet.has(privilege);
      case "folder":
        return this.folders.get(target.id)?.has(privilege) ?? false;
      case "object": {
        // 표에 없는 폴더(권한을 읽은 뒤 생긴 폴더 포함)도 루트에서 내려온 것은 받는다. 루트의 것은 모든 폴더의
        // `contents`에 들어 있으므로, 표에 있는데 비었으면 루트의 것도 비었다.
        const inside = target.folder
          ? (this.contents.get(target.folder) ?? this.rootContents)
          : this.rootContents;
        return (
          inside.has(privilege) ||
          (target.id !== undefined &&
            (this.objects.get(`${target.kind}/${target.id}`)?.has(privilege) ?? false))
        );
      }
    }
  }

  /** 어딘가(루트·폴더·객체)에 이 권한이 있다 */
  anywhere(privilege: Privilege): boolean {
    return this.all || this.any.has(privilege);
  }

  /** 종류 단위 루트 확인(옛 `can(role, action, kind)` 자리). 객체가 있는 곳은 `has(priv, target)`을 쓴다. */
  can(action: Action, kind: Kind): boolean {
    return this.has(kindPrivilege(action, kind));
  }

  /** 이 종류를 읽을 수 있는지: 객체마다 거르는 종류는 늘 참(서버가 거른다), 나머지는 루트에서 */
  reads(kind: Kind): boolean {
    return perObject(kind) || this.has(kindPrivilege("read", kind));
  }

  /** 새 객체를 둘 수 있는 VM·네트워크·스토리지 폴더 id(루트 포함은 `has(priv)`로 본다) */
  foldersWith(privilege: Privilege): string[] {
    return [...this.contents].filter(([, set]) => set.has(privilege)).map(([id]) => id);
  }

  /** 폴더 자체에 이 권한이 있는 폴더 id */
  foldersAllowing(privilege: Privilege): string[] {
    return [...this.folders].filter(([, set]) => set.has(privilege)).map(([id]) => id);
  }
}
