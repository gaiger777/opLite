// 권한(grant) 화면의 공통: 대상 이름, 역할 이름, 주체 이름, 목록 읽기(계획 62 설계 §6·§13).
import { useQuery } from "@tanstack/react-query";
import type { TFunction } from "i18next";

import { api, ApiError, call } from "@/api/client";
import type { AccessRole, Grant, GrantTarget, PrincipalRef, RoleRef } from "@/api/types";
import { type Permissions, type Privilege, type Target } from "@/auth/permissions";

/** API 쿼리의 대상 글: `root`, `folder/<id>`, `vm/<id>` … */
export function objectParam(target: Target): string {
  switch (target.type) {
    case "root":
      return "root";
    case "folder":
      return `folder/${target.id}`;
    case "object":
      return `${target.kind}/${target.id ?? ""}`;
  }
}

/** 화면의 대상 → grant의 대상 */
export function grantTarget(target: Target): GrantTarget {
  switch (target.type) {
    case "root":
      return { type: "root" };
    case "folder":
      return { type: "folder", id: target.id };
    case "object":
      return { type: "object", kind: target.kind, id: target.id ?? "" };
  }
}

/** 역할 목록(내장 셋 + 사용자 정의). 누구나 읽는다. */
export function useAccessRoles() {
  return useQuery({
    queryKey: ["access-roles"],
    queryFn: () => call(api.GET("/access-roles")),
    select: (list) => list.items,
    staleTime: 60_000,
  });
}

/** 그룹 목록(`cluster.admin`만 읽는다) */
export function useGroups(enabled = true) {
  return useQuery({
    queryKey: ["groups"],
    queryFn: () => call(api.GET("/groups")),
    select: (list) => list.items,
    enabled,
  });
}

/** 역할 참조의 이름 */
export function roleName(
  role: RoleRef,
  roles: readonly AccessRole[] | undefined,
  t: (key: string) => string,
): string {
  if (role.type === "builtin") {
    return t(`roles.${role.role}`);
  }
  return roles?.find((r) => r.id === role.id)?.name ?? role.id.slice(-8);
}

/** 역할 참조의 권한. 역할 목록을 아직 모르거나 없는 역할이면 빈 목록이다(그때는 `permission.modify`만 본다). */
export function rolePrivileges(role: RoleRef, roles: readonly AccessRole[] | undefined): string[] {
  const found =
    role.type === "builtin"
      ? roles?.find((r) => r.builtin && r.name === role.role)
      : roles?.find((r) => r.id === role.id);
  return found?.privileges ?? [];
}

/** 주체 이름: 아는 사용자·그룹이면 이름, 모르면 id 끝(위임자는 사용자 목록을 읽지 못한다) */
export function principalName(principal: PrincipalRef, names: ReadonlyMap<string, string>): string {
  return names.get(principal.id) ?? principal.id.slice(-8);
}

/** 대상 글(`/teamA` 경로는 부르는 쪽이 준다) */
export function targetLabel(
  target: GrantTarget,
  folderPath: (id: string) => string,
  objectName: (kind: string, id: string) => string | undefined,
  t: (key: string) => string,
): string {
  switch (target.type) {
    case "root":
      return t("permissions.rootTarget");
    case "folder":
      return folderPath(target.id);
    case "object":
      return `${target.kind}/${objectName(target.kind, target.id) ?? target.id.slice(-8)}`;
  }
}

/**
 * 대상 이름: 폴더는 경로의 마지막 이름, 객체는 그 이름(모르면 id 끝), 루트는 "클러스터 전체". 내 접근을 빼는
 * 확인에 넣게 한다(위임자는 주체 이름을 몰라 id 끝을 넣어야 했다, 리뷰 2).
 */
export function targetName(
  target: GrantTarget,
  folderPath: (id: string) => string,
  objectName: (kind: string, id: string) => string | undefined,
  t: (key: string) => string,
): string {
  switch (target.type) {
    case "root":
      return t("permissions.rootTarget");
    case "folder":
      return folderPath(target.id).split("/").pop() || target.id.slice(-8);
    case "object":
      return objectName(target.kind, target.id) ?? target.id.slice(-8);
  }
}

/**
 * 이 역할을 이 대상에 줄 수 있는지(서버의 위임 상승 막기와 같은 셈, 설계 §18 리뷰 1): 대상에서 역할의 권한과
 * `permission.modify`(루트면 `cluster.admin`)를 가져야 하고, propagate면 그 아래(폴더·루트 안 객체 층)에서도 가져야
 * 한다. 화면은 줄 수 없는 역할을 고르지 못하게만 하고, 막는 것은 서버다.
 */
export function grantable(
  perms: Pick<Permissions, "has">,
  privileges: readonly string[],
  target: Target,
  propagate: boolean,
): boolean {
  const needed = privileges as readonly Privilege[];
  const holds = (at: Target, extra: Privilege) => [...needed, extra].every((p) => perms.has(p, at));
  switch (target.type) {
    case "root":
      return (
        holds(target, "cluster.admin") &&
        (!propagate || needed.every((p) => perms.has(p, { type: "object", kind: "vm" })))
      );
    case "folder":
      return (
        holds(target, "permission.modify") &&
        (!propagate ||
          holds({ type: "object", kind: "vm", folder: target.id }, "permission.modify"))
      );
    case "object":
      return holds(target, "permission.modify");
  }
}

/**
 * grant 주기·빼기의 오류. 위임 상승(403 `forbidden`)은 서버가 무엇이 모자란지 말하므로 그 설명을 보인다(일반 "권한
 * 없음" 문구 대신).
 */
export function grantError(t: TFunction, error: unknown): unknown {
  if (error instanceof ApiError && error.status === 403 && error.code === "forbidden") {
    return new Error(t("permissions.escalation", { message: error.message }));
  }
  return error;
}

/**
 * 이 grant를 빼면 내 접근이 줄어드는지: 나에게 건 것이거나, 내가 든 그룹에 건 것이거나, 이 대상에서 내 권한의
 * 근거(`/permissions/effective`의 `grants`)인 것.
 */
export function revokesSelf(
  grant: Grant,
  me: string,
  {
    mine,
    groups,
  }: {
    mine?: readonly { id: string }[];
    groups?: readonly { id: string; spec: { members?: string[] } }[];
  },
): boolean {
  const p = grant.spec.principal;
  if (p.type === "user") {
    return p.id === me;
  }
  return (
    (mine ?? []).some((g) => g.id === grant.id) ||
    (groups ?? []).some((g) => g.id === p.id && (g.spec.members ?? []).includes(me))
  );
}

export type { Grant };
