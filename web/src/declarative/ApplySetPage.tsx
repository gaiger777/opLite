import { useQueryClient } from "@tanstack/react-query";
import { FileDown, History } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ListPending } from "@/components/ListPending";
import { ResourceHeader } from "@/components/ResourceHeader";
import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  type AppliedObject,
  type DriftItem,
  fetchSetManifests,
  refreshApplySets,
  useApplySet,
} from "@/declarative/api";
import { DriftChip } from "@/declarative/ApplySetsPage";
import { ExportYamlDialog } from "@/declarative/ExportYamlDialog";
import { actorName, canRestoreAnything, refreshHistory } from "@/history/api";
import { formatTime } from "@/lib/format";

type RowState = DriftItem["state"] | "same";

const TONE: Record<RowState, StatusTone> = {
  same: "success",
  modified: "warning",
  missing: "danger",
  unmanaged: "neutral",
};

/** 받아들일 수 있는 드리프트 줄(관리 밖은 묶음에 없어 받아들일 것이 없다) */
function isAdoptable(item: DriftItem): boolean {
  return item.state !== "unmanaged";
}

/** 다른 칸과 "누가 바꿨나"(이력 줄 — 사용자·시각·출처·사유) */
function DriftDetail({ item }: { item: DriftItem }) {
  const { t, i18n } = useTranslation();
  const paths = item.paths ?? [];
  return (
    <div className="flex flex-col gap-1 text-xs">
      {paths.length > 0 && (
        <details>
          <summary className="cursor-pointer">
            {t("declarative.set.paths", { count: paths.length })}
          </summary>
          <ul className="mt-1 list-disc pl-5 font-mono break-all whitespace-normal">
            {paths.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        </details>
      )}
      {item.by && (
        <p className="whitespace-normal text-muted-foreground">
          {t("declarative.set.by", {
            user: actorName(item.by.actor),
            time: formatTime(new Date(item.by.at_ms), i18n.language),
            source: t(`history.sources.${item.by.source}`),
            revision: item.by.revision,
          })}
          {item.by.reason && ` “${item.by.reason}”`}
        </p>
      )}
    </div>
  );
}

/**
 * 선언 묶음 하나(계획 73 §12.5·§13): 묶음이 기억하는 객체 표 — 상태 칩(같음·다름·없음·관리 밖), 다른 칸 펼침과 누가 바꿨나,
 * 고른 것 "실제를 선언으로 받아들이기"(`adopt` — 묶음의 선언만 바꾼다, 그 뒤 git 파일은 `export --set`으로), "YAML로
 * 내보내기", 이 묶음의 설정 기록 링크. 다시 선언대로 되돌리려면 `orbivisorctl apply`를 다시 한다.
 */
export function ApplySetPage() {
  const { t, i18n } = useTranslation();
  const { name = "" } = useParams();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const set = useApplySet(name);
  const [chosen, setChosen] = useState<ReadonlySet<string>>(new Set());
  const [exporting, setExporting] = useState(false);
  const [confirming, setConfirming] = useState(false);
  if (set.data === undefined) {
    return <ListPending error={set.error} />;
  }
  const s = set.data;
  const drift = new Map((s.status.drift?.items ?? []).map((d) => [d.id, d]));
  const objects: AppliedObject[] = [...s.status.objects].sort(
    (a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name),
  );
  // 묶음에 없는데 묶음 레이블을 단 것(드리프트 보고에만)
  const extra = [...drift.values()].filter((d) => !objects.some((o) => o.id === d.id));
  const rows = [
    ...objects.map((o) => ({ id: o.id, kind: o.kind, name: o.name, item: drift.get(o.id) })),
    ...extra.map((d) => ({ id: d.id, kind: d.kind, name: d.name, item: d })),
  ];
  // 받아들일 수 있는 줄: 관리 밖이 아닌 드리프트(관리 밖은 묶음에 없어 받아들일 것이 없다). 밖에서 지운 객체는 서버가
  // 마지막 폴더를 볼 수 있으면 `status.objects`에 남기고 드리프트 `missing`으로 알린다(id로 이어 붙음, 옛 서버는
  // `extra` 줄로) — 받아들이면 묶음에서 빠진다.
  const candidates = rows.filter((r) => r.item !== undefined && isAdoptable(r.item));
  const adoptable = canRestoreAnything(perms) && candidates.length > 0;
  // 고른 것은 늘 지금 후보와 겹친 것만(묶음을 다시 받을 때마다 정리 — 사라진 id가 "모두"로 넓어지지 않게)
  const selecting = chosen.size > 0;
  const picked = candidates.filter((r) => chosen.has(r.id));
  const targets = selecting ? picked : candidates;
  const pick = (id: string, on: boolean) =>
    setChosen((prev) => {
      const next = new Set(prev);
      if (on) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });

  async function adopt() {
    // "모두"는 빈 본문 — 서버가 부른 이가 보고 바꿀 수 있는 것만(밖에서 지운 것 포함) 받아들인다
    await call(
      api.POST("/apply-sets/{key}/adopt", {
        params: { path: { key: s.id } },
        body: selecting ? { objects: picked.map((r) => r.id) } : {},
      }),
    );
    toast.success(t("declarative.set.adopted", { name: s.name }));
    setChosen(new Set());
    await Promise.all([refreshApplySets(queryClient), refreshHistory(queryClient)]);
  }

  return (
    <div className="flex flex-col gap-4">
      <ResourceHeader
        name={s.name}
        status={<DriftChip set={s} />}
        meta={s.spec.description}
        actions={
          <>
            <Button variant="outline" asChild>
              <Link to={`/history?apply_set=${encodeURIComponent(s.name)}&period=all`}>
                <History />
                {t("declarative.set.history")}
              </Link>
            </Button>
            <Button variant="outline" onClick={() => setExporting(true)}>
              <FileDown />
              {t("configMenu.yaml")}
            </Button>
          </>
        }
        summary={[
          { label: t("declarative.sets.objects"), value: s.status.objects.length },
          {
            label: t("declarative.sets.lastApplied"),
            value: s.status.applied_at
              ? formatTime(new Date(s.status.applied_at), i18n.language)
              : "—",
          },
          { label: t("declarative.set.appliedBy"), value: s.status.applied_by ?? "—" },
          {
            label: t("declarative.set.checked"),
            value: s.status.drift
              ? formatTime(new Date(s.status.drift.checked_at), i18n.language)
              : "—",
          },
        ]}
      />
      {s.status.last_error && (
        <p role="alert" className="text-sm break-words text-destructive">
          {t("declarative.set.lastError", { error: s.status.last_error })}
        </p>
      )}
      {drift.size > 0 && (
        <div className="flex flex-col gap-2 rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm text-warning-fg">
          <p>{t("declarative.set.driftHint", { count: drift.size })}</p>
          {adoptable && (
            <div>
              <Button
                size="sm"
                variant="outline"
                disabled={selecting && picked.length === 0}
                onClick={() => setConfirming(true)}
              >
                {selecting
                  ? t("declarative.set.adoptChosen", { count: picked.length })
                  : t("declarative.set.adoptAll")}
              </Button>
            </div>
          )}
        </div>
      )}
      <div className="overflow-x-auto">
        <Table aria-label={t("declarative.set.objectsOf", { name: s.name })}>
          <TableHeader>
            <TableRow>
              {adoptable && (
                <TableHead>
                  <span className="sr-only">{t("declarative.set.choose")}</span>
                </TableHead>
              )}
              <TableHead>{t("history.kind")}</TableHead>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("common.state")}</TableHead>
              <TableHead>{t("declarative.set.difference")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((row) => {
              const state: RowState = row.item?.state ?? "same";
              return (
                <TableRow key={row.id}>
                  {adoptable && (
                    <TableCell>
                      {candidates.includes(row) && (
                        <Checkbox
                          aria-label={t("declarative.set.chooseNamed", { name: row.name })}
                          checked={chosen.has(row.id)}
                          onCheckedChange={(v) => pick(row.id, v === true)}
                        />
                      )}
                    </TableCell>
                  )}
                  <TableCell className="font-mono text-xs">{row.kind}</TableCell>
                  <TableCell className="font-medium break-all">{row.name}</TableCell>
                  <TableCell>
                    <StatusChip tone={TONE[state]}>{t(`declarative.state.${state}`)}</StatusChip>
                  </TableCell>
                  <TableCell className="max-w-md align-top whitespace-normal">
                    {row.item ? <DriftDetail item={row.item} /> : "—"}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </div>
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={t("declarative.set.adoptTitle", { name: s.name })}
        description={t("declarative.set.adoptHint")}
        confirmLabel={t("declarative.set.adopt")}
        // 고른 것이 그새 모두 사라졌으면 보내지 않는다(빈 목록은 서버가 거절한다)
        confirmDisabled={targets.length === 0}
        impact={targets.map((r) =>
          t(
            r.item?.state === "missing"
              ? "declarative.set.adoptMissing"
              : "declarative.set.adoptItem",
            {
              kind: r.kind,
              name: r.name,
            },
          ),
        )}
        onConfirm={adopt}
      />
      {exporting && (
        <ExportYamlDialog
          title={t("declarative.export.title", { name: s.name })}
          fileName={`${s.name}.yaml`}
          queryKey={["set", s.name]}
          load={() => fetchSetManifests(s.name)}
          onClose={() => setExporting(false)}
        />
      )}
    </div>
  );
}
