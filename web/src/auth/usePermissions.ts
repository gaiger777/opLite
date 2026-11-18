import { useQuery } from "@tanstack/react-query";
import { useCallback, useMemo } from "react";

import { api, call } from "@/api/client";
import { useOptionalPrincipal, usePrincipal } from "@/auth/AuthProvider";
import type { Vm } from "@/api/types";
import { objectTarget, Permissions, type Privilege } from "@/auth/permissions";

/** 내 권한의 캐시 키. 실시간 연결이 열 때마다 다시 읽는다(`LiveProvider`). */
export const PERMISSIONS_KEY = ["permissions", "mine"] as const;

export function fetchPermissions() {
  return call(api.GET("/permissions/mine"));
}

/**
 * 내 권한(`GET /permissions/mine`, 설계 §13 웹 "숨기기"). 받기 전이나 받지 못하면 전역 역할의 권한이다. 권한이
 * 바뀌면 서버가 watch를 `permissions`로 닫고, 다시 연결하며 새로 읽는다. 로그인 밖(시험의 단독 화면)에서는
 * admin처럼 모두 보인다.
 */
export function usePermissions(): Permissions {
  const me = useOptionalPrincipal();
  const mine = useQuery({
    queryKey: PERMISSIONS_KEY,
    queryFn: fetchPermissions,
    staleTime: Infinity,
    retry: false,
    enabled: me !== null,
  });
  const role = me?.role ?? "admin";
  return useMemo(
    () => (mine.data ? new Permissions(mine.data) : Permissions.fromRole(role)),
    [mine.data, role],
  );
}

/** 탭 보이기·마지막 탭 기억에 쓰는 사용자와 권한(`nav.ts`의 `NavUser`) */
export function useNavUser(): { user: string; gate: Permissions } {
  const me = usePrincipal();
  const gate = usePermissions();
  return useMemo(() => ({ user: me.user, gate }), [me.user, gate]);
}

/** VM 하나의 권한 묻기(그 VM의 폴더·객체 grant까지). VM이 아직 없으면 거짓이다. */
export function useVmCan(vm: Vm | null | undefined): (privilege: Privilege) => boolean {
  const perms = usePermissions();
  return useCallback(
    (privilege: Privilege) => !!vm && perms.has(privilege, objectTarget("vm", vm)),
    [perms, vm],
  );
}
