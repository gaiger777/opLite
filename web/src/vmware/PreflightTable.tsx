import { useTranslation } from "react-i18next";

import { StatusChip } from "@/components/StatusChip";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type { PreflightVm } from "@/vmware/api";
import { levelTone } from "@/vmware/inventory";

/** 미리 확인 판정 칩 */
export function LevelChip({ level }: { level: PreflightVm["level"] }) {
  const { t } = useTranslation();
  return <StatusChip tone={levelTone(level)}>{t(`vmware.level.${level}`)}</StatusChip>;
}

/** 판정별 수 */
export function levelCounts(vms: readonly PreflightVm[]) {
  return {
    ok: vms.filter((v) => v.level === "ok").length,
    warn: vms.filter((v) => v.level === "warn").length,
    blocked: vms.filter((v) => v.level === "blocked").length,
  };
}

/**
 * 미리 확인 표(계획 72 §8.5·§13 ⑤): VM마다 판정 칩, 고른 노드, 문제(자리·까닭)를 펼쳐 본다. 막힌 VM은 시작에서 빠진다.
 */
export function PreflightTable({ vms }: { vms: readonly PreflightVm[] }) {
  const { t } = useTranslation();
  const counts = levelCounts(vms);
  return (
    <div className="flex flex-col gap-2">
      <p role="status" className="text-sm">
        {t("vmware.preflight.summary", counts)}
      </p>
      <div className="overflow-x-auto">
        <Table aria-label={t("vmware.preflight.table")}>
          <TableHeader>
            <TableRow>
              <TableHead>{t("vmware.preflight.vm")}</TableHead>
              <TableHead>{t("vmware.preflight.item")}</TableHead>
              <TableHead>{t("vmware.preflight.level")}</TableHead>
              <TableHead>{t("common.node")}</TableHead>
              <TableHead>{t("vmware.preflight.problems")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {vms.map((vm) => (
              <TableRow key={vm.item}>
                <TableCell className="font-medium">{vm.name}</TableCell>
                <TableCell>{vm.item}</TableCell>
                <TableCell>
                  <LevelChip level={vm.level} />
                </TableCell>
                <TableCell>{vm.node ?? "—"}</TableCell>
                <TableCell className="whitespace-normal">
                  {vm.problems.length === 0 ? (
                    "—"
                  ) : (
                    <details>
                      <summary className="cursor-pointer text-sm">
                        {t("vmware.preflight.problemCount", { count: vm.problems.length })}
                      </summary>
                      <ul className="mt-1 flex flex-col gap-1">
                        {vm.problems.map((p, i) => (
                          <li key={i} className="text-xs break-words">
                            <span
                              className={
                                p.level === "error" ? "text-destructive" : "text-muted-foreground"
                              }
                            >
                              {t(`vmware.problemLevel.${p.level}`)}
                            </span>{" "}
                            <span className="font-mono">{p.at}</span>: {p.message}
                          </li>
                        ))}
                      </ul>
                    </details>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </div>
  );
}
