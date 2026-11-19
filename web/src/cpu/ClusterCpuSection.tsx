import { useState } from "react";
import { useTranslation } from "react-i18next";

import type { components } from "@/api/schema";
import { usePermissions } from "@/auth/usePermissions";
import { ListPending } from "@/components/ListPending";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { describeBaseline, describeResolved, useCpuBaseline, useCpuModels } from "@/cpu/cpu";
import { formatTime } from "@/lib/format";

type BaselineInfo = components["schemas"]["CpuBaselineInfo"];

/**
 * 클러스터 보기의 "CPU 기준"(설계 §13): 모드·숨김 목록, 아키텍처마다 기준 모델·플래그·제한한 노드·경고·`cluster` VM 수,
 * 그리고 모델 × 노드 표. 루트 `cluster.read`가 있어야 보인다. 바꾸기는 설정 › 일반이다.
 */
export function ClusterCpuSection() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const readable = perms.has("cluster.read");
  const baseline = useCpuBaseline(readable);
  if (!readable) {
    return null;
  }
  const data = baseline.data;
  return (
    <section aria-labelledby="cluster-cpu" className="flex flex-col gap-3">
      <h2 id="cluster-cpu" className="font-medium">
        {t("cpu.cluster.title")}
      </h2>
      {!data ? (
        <ListPending error={baseline.error} />
      ) : (
        <>
          <p className="text-sm">
            {t("cpu.cluster.mode", { mode: t(`cpu.mode.${data.mode}`) })}
            {(data.mask ?? []).length > 0 && (
              <span className="block text-muted-foreground">
                {t("cpu.cluster.mask", { flags: (data.mask ?? []).join(", ") })}
              </span>
            )}
          </p>
          {data.arches.map((a) => (
            <ArchBaseline key={a.arch} info={a} />
          ))}
        </>
      )}
    </section>
  );
}

function ArchBaseline({ info }: { info: BaselineInfo }) {
  const { t, i18n } = useTranslation();
  const b = info.baseline;
  return (
    <div className="flex flex-col gap-2 rounded-md border p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{info.arch}</span>
        {b && <Badge variant="outline">{t(`cpu.source.${b.mode}`)}</Badge>}
      </div>
      {b ? (
        <>
          <p className="font-mono text-xs">{describeBaseline(b)}</p>
          {b.since ? (
            <p className="text-xs text-muted-foreground">
              {t("cpu.cluster.since", {
                time: formatTime(new Date(b.since * 1000), i18n.language),
              })}
              {b.previous &&
                ` · ${t("cpu.cluster.previous", { cpu: describeResolved(b.previous) })}`}
            </p>
          ) : null}
        </>
      ) : (
        <p className="text-muted-foreground">{t("cpu.cluster.noBaseline")}</p>
      )}
      <p className="text-xs text-muted-foreground">
        {t("cpu.cluster.counts", {
          nodes: (info.nodes ?? []).join(", ") || "—",
          vms: info.cluster_vms,
        })}
      </p>
      {(info.limited_by ?? []).length > 0 && (
        <p className="text-warning-fg">
          {t("cpu.cluster.limitedBy", { nodes: (info.limited_by ?? []).join(", ") })}
        </p>
      )}
      {(b?.warnings ?? []).length > 0 && (
        <ul className="list-disc pl-5 text-warning-fg">
          {(b?.warnings ?? []).map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}
      <ModelMatrix arch={info.arch} />
    </div>
  );
}

/** 모델 × 노드 표: 노드마다 켤 수 있는지(✓)와 빠진 플래그. 찾은 모델만 보인다. */
function ModelMatrix({ arch }: { arch: components["schemas"]["Arch"] }) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const models = useCpuModels(arch);
  const all = (models.data?.models ?? []).filter((m) => !m.alias_of && m.kind !== "basic");
  const nodes = [...new Set(all.flatMap((m) => (m.nodes ?? []).map((n) => n.node)))].sort();
  if (all.length === 0 || nodes.length === 0) {
    return null;
  }
  const q = query.trim().toLowerCase();
  const shown = all.filter((m) => q === "" || m.name.toLowerCase().includes(q));
  const id = `cpu-matrix-${arch}`;
  return (
    <details className="flex flex-col gap-2">
      <summary id={id} className="cursor-pointer text-xs font-medium">
        {t("cpu.cluster.matrix", { count: all.length })}
      </summary>
      <Input
        type="search"
        className="my-2 max-w-xs"
        aria-label={t("cpu.model.search")}
        placeholder={t("cpu.model.search")}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
      <div className="max-h-96 overflow-auto">
        <Table aria-labelledby={id}>
          <TableHeader>
            <TableRow>
              <TableHead>{t("cpu.node.model")}</TableHead>
              {nodes.map((n) => (
                <TableHead key={n}>{n}</TableHead>
              ))}
            </TableRow>
          </TableHeader>
          <TableBody>
            {shown.map((m) => (
              <TableRow key={m.name}>
                <TableCell className="font-mono text-xs">{m.name}</TableCell>
                {nodes.map((n) => {
                  const fit = m.nodes?.find((f) => f.node === n);
                  return (
                    <TableCell key={n} className="font-mono text-xs whitespace-normal">
                      {!fit
                        ? "—"
                        : fit.can_run
                          ? "✓"
                          : (fit.missing ?? []).join(", ") || t("cpu.node.unusable")}
                    </TableCell>
                  );
                })}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
    </details>
  );
}
