import { useTranslation } from "react-i18next";

import type { Vm } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { describeNuma, describeRunning, numaInUse, pinningRows } from "@/numa/numa";

/**
 * VM 상세 › 요약의 "CPU 배치"(설계 §13): 스펙의 배치, 켤 때 기록한 게스트 모양과 호스트 NUMA 노드, 펼치는 vCPU → pCPU
 * 표(자리마다, 에뮬레이터·비워 둔 형제). 계획 64 칸을 쓰지 않고 기록도 없으면 보이지 않는다. 보류 `numa`는 "보류 중인
 * 변경"의 전원 껐다 켜기 띠가 맡는다. 호스트 NUMA 노드·에뮬레이터·형제·pCPU 표는 루트 `cluster.read`가 있을 때만
 * 보인다(`/vms/{key}/numa`처럼 폴더 사용자에게 호스트 CPU 번호를 숨긴다, 리뷰 2 웹).
 */
export function VmPlacement({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const hw = vm.status.hardware;
  if (!numaInUse(vm.spec) && !hw?.numa) {
    return null;
  }
  const running = hw ? describeRunning(t, hw) : null;
  const placement = perms.has("cluster.read") ? hw?.host_placement : undefined;
  const rows = hw && placement ? pinningRows(hw) : [];
  const pinned = rows.some((r) => r.pcpu !== null);
  return (
    <section aria-labelledby="vm-placement" className="mt-6 flex flex-col gap-2">
      <h2 id="vm-placement" className="font-medium">
        {t("numa.vm.title")}
      </h2>
      <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
        <div>
          <dt className="text-xs text-muted-foreground">{t("numa.vm.wanted")}</dt>
          <dd className="text-sm">{describeNuma(t, vm.spec)}</dd>
        </div>
        {running && (
          <div>
            <dt className="text-xs text-muted-foreground">{t("numa.vm.running")}</dt>
            <dd className="text-sm">{running}</dd>
          </div>
        )}
        {(placement?.host_nodes ?? []).length > 0 && (
          <div>
            <dt className="text-xs text-muted-foreground">{t("numa.vm.hostNodes")}</dt>
            <dd className="text-sm">{placement!.host_nodes!.join(", ")}</dd>
          </div>
        )}
        {placement && (
          <div>
            <dt className="text-xs text-muted-foreground">{t("numa.vm.emulator")}</dt>
            <dd className="font-mono text-sm">{placement.emulator || t("numa.vm.sharedCpus")}</dd>
          </div>
        )}
        {placement?.siblings && (
          <div>
            <dt className="text-xs text-muted-foreground">{t("numa.vm.siblings")}</dt>
            <dd className="font-mono text-sm">{placement.siblings}</dd>
          </div>
        )}
      </dl>
      {pinned && (
        <details className="rounded-md border px-3 py-2">
          <summary className="cursor-pointer text-sm font-medium">{t("numa.vm.pinning")}</summary>
          <Table aria-label={t("numa.vm.pinning")} className="mt-2">
            <TableHeader>
              <TableRow>
                <TableHead>{t("numa.vm.slot")}</TableHead>
                <TableHead>{t("numa.vm.guestNode")}</TableHead>
                <TableHead>{t("numa.vm.hostNode")}</TableHead>
                <TableHead>{t("numa.vm.pcpu")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((r) => (
                <TableRow key={r.slot}>
                  <TableCell>{r.slot}</TableCell>
                  <TableCell>{r.guestNode ?? "—"}</TableCell>
                  <TableCell>{r.hostNode ?? "—"}</TableCell>
                  <TableCell className="font-mono">{r.pcpu ?? "—"}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          <p className="mt-2 text-xs text-muted-foreground">{t("numa.vm.pinningNote")}</p>
        </details>
      )}
    </section>
  );
}
