// 노드별 할당·사용 표(계획 75 §4.1): 행 = 노드, 열 = CPU·RAM·스토리지(같은 막대), 오버커밋 ×n, 정렬, "문제만" 거르기. 행을
// 누르면 그 노드를 고른다(지도·자세히 판이 따라간다). 좁은 화면은 카드 목록이다. 공유 풀은 표 아래 한 줄.
import { ArrowDown, ArrowUp } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { StatusChip } from "@/components/StatusChip";
import type { MapSelection } from "@/dashboard/selection";
import { cpuText, memoryText, storageText } from "@/dashboard/texts";
import { UsageBar } from "@/dashboard/UsageBar";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { formatBytes } from "@/lib/format";
import type { Capacity, NodePhysical } from "@/physical/api";
import { type CapacityRow, capacityRows, type SortKey } from "@/physical/capacity";

function Bars({ row, compact }: { row: CapacityRow; compact: boolean }) {
  const { t } = useTranslation();
  return (
    <>
      <UsageBar
        compact={compact}
        label={t("physical.table.cpuOf", { node: row.node })}
        m={row.cpu}
        text={cpuText(t, row.cpu, row.cpuOvercommit, row.cpuUsePct, row.cpuDedicated)}
      />
      <UsageBar
        compact={compact}
        label={t("physical.table.memoryOf", { node: row.node })}
        m={row.memory}
        text={memoryText(t, row.memory, row.memoryOvercommit)}
      />
      <UsageBar
        compact={compact}
        label={t("physical.table.storageOf", { node: row.node })}
        m={row.storage}
        text={storageText(t, row.storage)}
      />
    </>
  );
}

const COUNT: Partial<Record<CapacityRow["problems"][number], keyof CapacityRow>> = {
  link: "faults",
  pool: "poolErrors",
  unreachable: "unreachable",
};

function Problems({ row }: { row: CapacityRow }) {
  const { t } = useTranslation();
  // 물리 관계를 받지 못했으면 "장애 없음"이 아니라 "모름"이다(리뷰 1).
  const unknown = row.linkUnknown && (
    <StatusChip tone="neutral">? {t("physical.problem.linkUnknown")}</StatusChip>
  );
  if (row.problems.length === 0) {
    return (
      unknown || <span className="text-xs text-muted-foreground">{t("physical.table.ok")}</span>
    );
  }
  return (
    <span className="flex flex-wrap gap-1">
      {row.problems.map((p) => {
        const key = COUNT[p];
        return (
          <StatusChip
            key={p}
            tone={p === "link" || p === "offline" || p === "unreachable" ? "danger" : "warning"}
          >
            {t(`physical.problem.${p}`, { count: key ? (row[key] as number) : 0 })}
          </StatusChip>
        );
      })}
      {unknown}
    </span>
  );
}

export function CapacityTable({
  capacity,
  physical,
  selected,
  onSelect,
  phone,
}: {
  capacity: Capacity;
  physical?: readonly NodePhysical[];
  selected: MapSelection | null;
  onSelect(s: MapSelection): void;
  phone: boolean;
}) {
  const { t } = useTranslation();
  const titleId = useId();
  const [sort, setSort] = useState<SortKey>("node");
  const [desc, setDesc] = useState(false);
  const [problemsOnly, setProblemsOnly] = useState(false);
  const rows = capacityRows(capacity.nodes, physical, { sort, desc, problemsOnly });
  const current = selected?.kind === "node" ? selected.id : undefined;
  const pick = (row: CapacityRow) => onSelect({ kind: "node", id: row.id });

  const header = (key: SortKey, text: string) => {
    const on = sort === key;
    const Icon = desc ? ArrowDown : ArrowUp;
    return (
      <TableHead aria-sort={on ? (desc ? "descending" : "ascending") : "none"}>
        <button
          type="button"
          className="inline-flex items-center gap-1 font-medium underline-offset-4 hover:underline"
          onClick={() => {
            if (on) {
              setDesc(!desc);
            } else {
              setSort(key);
              // 수치 열은 큰 것부터 보는 일이 많다.
              setDesc(key !== "node");
            }
          }}
        >
          {text}
          {on && <Icon aria-hidden className="size-3" />}
        </button>
      </TableHead>
    );
  };

  const filter = (
    <div className="flex items-center gap-2">
      <Checkbox
        id={`${titleId}-problems`}
        checked={problemsOnly}
        onCheckedChange={(on) => setProblemsOnly(on === true)}
      />
      <Label htmlFor={`${titleId}-problems`} className="text-sm font-normal">
        {t("physical.table.problemsOnly")}
      </Label>
    </div>
  );

  const empty = rows.length === 0 && (
    <p className="text-sm text-muted-foreground">
      {problemsOnly ? t("physical.table.noProblems") : t("physical.table.noNodes")}
    </p>
  );

  return (
    <WidgetCard title={t("physical.table.title")} titleId={titleId} action={filter}>
      {phone ? (
        <ul aria-labelledby={titleId} className="flex flex-col gap-3">
          {rows.map((row) => (
            <li
              key={row.id}
              aria-current={row.id === current ? "true" : undefined}
              className="flex flex-col gap-2 rounded-lg border p-3 aria-[current]:border-brand"
            >
              <div className="flex items-center justify-between gap-2">
                <button
                  type="button"
                  className="font-medium underline-offset-4 hover:underline"
                  onClick={() => pick(row)}
                >
                  {row.node}
                </button>
                <Problems row={row} />
              </div>
              <Bars row={row} compact={false} />
            </li>
          ))}
        </ul>
      ) : (
        <Table aria-labelledby={titleId}>
          <TableHeader>
            <TableRow>
              {header("node", t("common.node"))}
              {header("cpu", t("physical.table.cpu"))}
              {header("memory", t("physical.table.memory"))}
              {header("storage", t("physical.table.storage"))}
              <TableHead>{t("physical.table.problems")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row) => (
              <TableRow
                key={row.id}
                aria-current={row.id === current ? "true" : undefined}
                className="cursor-pointer aria-[current]:bg-muted"
                onClick={() => pick(row)}
              >
                <TableCell className="font-medium">
                  <button
                    type="button"
                    className="underline-offset-4 hover:underline"
                    onClick={(e) => {
                      e.stopPropagation();
                      pick(row);
                    }}
                  >
                    {row.node}
                  </button>
                  {!row.online && (
                    <span className="ml-1 text-xs text-muted-foreground">
                      {t("physical.table.offline")}
                    </span>
                  )}
                </TableCell>
                <TableCell className="min-w-40">
                  <UsageBar
                    compact
                    label={t("physical.table.cpuOf", { node: row.node })}
                    m={row.cpu}
                    text={cpuText(t, row.cpu, row.cpuOvercommit, row.cpuUsePct, row.cpuDedicated)}
                  />
                </TableCell>
                <TableCell className="min-w-40">
                  <UsageBar
                    compact
                    label={t("physical.table.memoryOf", { node: row.node })}
                    m={row.memory}
                    text={memoryText(t, row.memory, row.memoryOvercommit)}
                  />
                </TableCell>
                <TableCell className="min-w-40">
                  <UsageBar
                    compact
                    label={t("physical.table.storageOf", { node: row.node })}
                    m={row.storage}
                    text={storageText(t, row.storage)}
                  />
                </TableCell>
                <TableCell>
                  <Problems row={row} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {empty}
      {capacity.shared_pools.length > 0 && (
        <p className="text-xs text-muted-foreground">
          {t("physical.table.shared")}{" "}
          {capacity.shared_pools
            .map((p) =>
              t("physical.table.sharedPool", {
                pool: p.pool,
                used: formatBytes(p.used_bytes),
                total: formatBytes(p.total_bytes),
                provisioned: formatBytes(p.provisioned_bytes),
                ready: p.nodes_ready,
                nodes: p.nodes,
              }),
            )
            .join(" · ")}
        </p>
      )}
    </WidgetCard>
  );
}
