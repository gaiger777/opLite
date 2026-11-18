// 권한(grant) 화면의 공통: 대상 이름, 역할 이름, 주체 이름, 목록 읽기(계획 62 설계 §6·§13).
import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { AccessRole, Grant, GrantTarget, PrincipalRef, RoleRef } from "@/api/types";
import { type Target } from "@/auth/permissions";

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

export type { Grant };
