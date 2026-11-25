// VM 내보내기(계획 72 §12)의 질의와 도움 함수. `vm-export`는 watch 종류가 아니라서 내보내기·지우기 Task 사건이 오면 목록을
// 다시 읽는다(`LiveProvider`의 `REFRESH_ON.task`).
import { type QueryClient, useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";
import type { StoragePool, Vm } from "@/api/types";
import { startDownload } from "@/fileRestore/download";

export type VmExport = components["schemas"]["Resource_VmExportSpec_VmExportStatus"];
export type ExportVmRequest = components["schemas"]["ExportVmRequest"];

export const EXPORTS_KEY = ["vm-exports"] as const;

/** 내보내기 목록(내보낸 VM을 읽을 수 있는 것만 — 서버가 거른다) */
export function useExports() {
  return useQuery({
    queryKey: EXPORTS_KEY,
    queryFn: () => call(api.GET("/vm-exports")),
    select: (list) => list.items,
  });
}

export async function refreshExports(queryClient: QueryClient) {
  await queryClient.invalidateQueries({ queryKey: EXPORTS_KEY });
}

/** 파일을 둘 수 있는 풀(dir·NFS — 아니면 서버가 400 `pool_not_file`) */
export function filePools(pools: readonly StoragePool[]): StoragePool[] {
  return pools
    .filter((p) => p.spec.backend.kind === "dir" || p.spec.backend.kind === "nfs")
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** 내보낼 수 없는 까닭(번역 키): 켜져 있거나 바뀌는 중이면 막는다(서버 409 `vm_running`·`vm_busy`). 템플릿은 된다. */
export function exportReason(vm: Vm): string | null {
  return vm.status.phase === "stopped" && vm.spec.power !== "running" ? null : "vms.reason.running";
}

/** 요청 본문. 비운 칸은 서버 기본값(이름 `{VM}-{시각}`, 하드웨어 버전 14, 가져온 VM의 원본 guestId)이다. */
export function exportBody(form: {
  pool: string;
  name: string;
  guestOs: string;
  hwVersion: string;
  keepMacs: boolean;
}): ExportVmRequest {
  return {
    pool: form.pool,
    ...(form.name.trim() ? { name: form.name.trim() } : {}),
    ...(form.guestOs.trim() ? { guest_os: form.guestOs.trim() } : {}),
    ...(form.hwVersion.trim() && form.hwVersion.trim() !== "14"
      ? { hw_version: Number(form.hwVersion) }
      : {}),
    ...(form.keepMacs ? { keep_macs: true } : {}),
  };
}

/** 하드웨어 버전 칸이 맞는지(7~21, 비면 기본) */
export function validHwVersion(text: string): boolean {
  const t = text.trim();
  return t === "" || (/^\d+$/.test(t) && Number(t) >= 7 && Number(t) <= 21);
}

/** 내려받기: 10분 사는 서명 링크를 받아 브라우저가 스스로 받게 한다(OVA를 JS 메모리에 담지 않는다). */
export async function downloadExport(exp: VmExport) {
  const link = await call(
    api.POST("/vm-exports/{key}/download-link", { params: { path: { key: exp.id } } }),
  );
  startDownload(`/api/v1${link.url}`);
}
