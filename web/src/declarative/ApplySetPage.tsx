import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FileDown, History } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CopyButton } from "@/components/CopyButton";
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
  fetchSetLive,
  fetchSetManifests,
  type Manifest,
  refreshApplySets,
  useApplySet,
} from "@/declarative/api";
import { DriftChip, LastApplied, useManagedSummary } from "@/declarative/ApplySetsPage";
import { compareDeclared, liveOf, sameValue } from "@/declarative/compare";
import { ExportYamlDialog } from "@/declarative/ExportYamlDialog";
import { actorName, canRestoreAnything, refreshHistory } from "@/history/api";
import { FieldCell } from "@/history/ChangeTable";
import { kindName, valueText } from "@/history/describe";
import { formatTime } from "@/lib/format";
import { kindHref } from "@/tasks/target";
import { newestFirst, TaskTable } from "@/tasks/TasksPage";

/** 줄 상태: 서버 드리프트가 알린 것, 견준 뒤 알리지 않은 것은 같음, 아직 견주지 않은 묶음은 모두 "아직 비교 전" */
type RowState = DriftItem["state"] | "same" | "unchecked";

const TONE: Record<RowState, StatusTone> = {
  same: "success",
  modified: "warning",
  missing: "danger",
  unmanaged: "neutral",
  unchecked: "neutral",
};

/** 리소스 줄의 상태(`checked` — 서버가 이 묶음을 한 번이라도 견주었나: `status.drift`가 있다) */
function rowState(row: Row, checked: boolean): RowState {
  return row.item?.state ?? (checked ? "same" : "unchecked");
}

/** 견주어 보일 리소스 수(묶음은 1000개까지 — 넘으면 CLI `drift`로) */
const COMPARED = 50;

interface Row {
  id: string;
  kind: string;
  name: string;
  item?: DriftItem;
  declared?: unknown;
}

/** 밖에서 이름을 바꿨으면 지금 이름(파일·묶음은 옛 이름 — 서버 드리프트는 이름을 견주지 않는다) */
function renamedTo(row: Row, live: Manifest | undefined): string | undefined {
  const now = live?.metadata.name ?? row.item?.name;
  return now !== undefined && now !== row.name ? now : undefined;
}

/** "이름이 바뀜(파일: a → 지금: b)" */
function Renamed({ from, to }: { from: string; to: string }) {
  const { t } = useTranslation();
  return (
    <p className="text-xs break-all text-warning-fg">
      {t("declarative.set.renamed", { from, to })}
    </p>
  );
}

/** 받아들일 수 있는 드리프트 줄(관리 밖은 묶음에 없어 받아들일 것이 없다) */
function isAdoptable(item: DriftItem): boolean {
  return item.state !== "unmanaged";
}

/** 누가 바꿨나(이력 줄 — 사용자·시각·출처·사유) */
function ChangedBy({ item }: { item: DriftItem }) {
  const { t, i18n } = useTranslation();
  if (!item.by) {
    return null;
  }
  return (
    <p className="text-xs whitespace-normal text-muted-foreground">
      {t("declarative.set.by", {
        user: actorName(item.by.actor),
        time: formatTime(new Date(item.by.at_ms), i18n.language),
        source: t(`history.sources.${item.by.source}`),
        revision: item.by.revision,
      })}
      {item.by.reason && ` “${item.by.reason}”`}
    </p>
  );
}

/** 목록 칸 하나(객체면 "칸 값" 쌍 — 빈 칸은 뺀다) */
function itemText(item: unknown): string {
  if (typeof item === "object" && item !== null && !Array.isArray(item)) {
    return Object.entries(item)
      .filter(([, v]) => v !== null && v !== undefined)
      .map(([k, v]) => `${k}: ${typeof v === "string" ? v : JSON.stringify(v)}`)
      .join(", ");
  }
  return typeof item === "string" ? item : JSON.stringify(item);
}

/** 값 칸: 비밀·없음은 회색 글, 목록은 한 줄에 하나씩, 그 밖은 사람 말 값(8 GiB, 켬) */
function CompareValue({ path, value, empty }: { path: string; value: unknown; empty: string }) {
  const { t } = useTranslation();
  if (Array.isArray(value) && value.length > 0) {
    return (
      <ul className="flex flex-col gap-0.5 font-mono text-xs break-all">
        {value.map((item, i) => (
          <li key={i}>{itemText(item)}</li>
        ))}
      </ul>
    );
  }
  const text = valueText(path, value, t);
  return text === null ? (
    <span className="text-muted-foreground italic">{empty}</span>
  ) : (
    <code className="font-mono text-xs break-all">{text}</code>
  );
}

/** 리소스 하나의 "파일 값 ↔ 실제 값" 표(다른 칸은 노랑 바탕 + "다름") */
function ObjectCompare({
  row,
  live,
  loading,
  checked,
}: {
  row: Row;
  live: Manifest | undefined;
  loading: boolean;
  checked: boolean;
}) {
  const { t } = useTranslation();
  const kind = kindName(row.kind, t);
  const state = rowState(row, checked);
  // 서버 드리프트가 기준이다(서버는 기본값·목록 차례·서버가 지키는 칸을 맞춘 뒤 견준다): 같다고 한 리소스는 강조하지 않고,
  // 다르다고 하며 칸(저장 꼴 경로 — 같은 경로이거나 그 위·아래 칸)을 알렸으면 그 칸만 강조한다. 화면 자체 비교(`covers`)는
  // 서버가 칸을 알리지 않았을 때(아직 견주지 않은 묶음, 칸 없는 "다름"·"없음"·"관리 밖")만 쓰고, 그때 "다름"은 참고다.
  const serverPaths = row.item?.paths ?? [];
  const byServer = state === "modified" && serverPaths.length > 0;
  const touched = (path: string) =>
    serverPaths.some(
      (p) =>
        p === path ||
        p.startsWith(`${path}.`) ||
        path.startsWith(`${p}.`) ||
        p === `metadata.${path}`,
    );
  const rows = (row.declared === undefined ? [] : compareDeclared(row.declared, live)).map((r) => ({
    ...r,
    differs: !r.secret && state !== "same" && (byServer ? touched(r.path) : r.differs),
  }));
  const differ = rows.filter((r) => r.differs).length;
  const gone = state === "missing";
  // 서버 판단이 아닌 강조(밖에서 지운 것은 서버가 없다고 했으니 모든 칸이 다른 것이 맞다)
  const hint = differ > 0 && !byServer && !gone;
  // 서버가 다르다고 했는데 여기서 고를 칸이 없으면(이름이 다른 저장 꼴 경로) 서버 경로를 그대로 보인다
  const serverOnly = byServer && differ === 0 ? serverPaths : [];
  const renamed = renamedTo(row, live);
  return (
    <details
      open={(state !== "same" && state !== "unchecked") || differ > 0 || renamed !== undefined}
      className="rounded-md border open:pb-2"
      aria-label={t("declarative.set.compareOf", { kind, name: row.name })}
    >
      <summary className="flex cursor-pointer flex-wrap items-center gap-x-2 gap-y-1 px-3 py-2 text-sm">
        <span className="text-muted-foreground">{kind}</span>
        <span className="font-medium break-all">{row.name}</span>
        <StatusChip tone={TONE[state]}>{t(`declarative.state.${state}`)}</StatusChip>
        <span className="text-xs text-muted-foreground">
          {differ > 0
            ? t(hint ? "declarative.set.pathsHint" : "declarative.set.paths", { count: differ })
            : state === "same" && rows.length > 0
              ? t("declarative.set.sameCount", { count: rows.length })
              : ""}
        </span>
      </summary>
      <div className="flex flex-col gap-2 px-3">
        {renamed !== undefined && <Renamed from={row.name} to={renamed} />}
        {row.item && <ChangedBy item={row.item} />}
        {state === "unchecked" && (
          <p className="text-xs text-muted-foreground">{t("declarative.set.uncheckedHint")}</p>
        )}
        {hint && <p className="text-xs text-muted-foreground">{t("declarative.set.webHint")}</p>}
        {state === "unmanaged" && (
          <p className="text-xs text-muted-foreground">{t("declarative.set.unmanagedHint")}</p>
        )}
        {serverOnly.length > 0 && (
          <p className="font-mono text-xs break-all text-muted-foreground">
            {t("declarative.set.serverPaths", { paths: serverOnly.join(", ") })}
          </p>
        )}
        {loading && !gone ? (
          <p className="text-xs text-muted-foreground">{t("declarative.set.loadingLive")}</p>
        ) : (
          rows.length > 0 && (
            <div className="overflow-x-auto">
              <Table aria-label={t("declarative.set.compareOf", { kind, name: row.name })}>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("declarative.set.field")}</TableHead>
                    <TableHead>{t("declarative.set.fileValue")}</TableHead>
                    <TableHead>{t("declarative.set.liveValue")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((r) => (
                    <TableRow
                      key={r.path}
                      className={r.differs ? "bg-warning-soft hover:bg-warning-soft" : undefined}
                    >
                      <TableCell className="align-top text-xs whitespace-normal">
                        <FieldCell path={r.path} />
                        {r.differs && (
                          <span className="mt-0.5 inline-block rounded-sm border border-warning px-1 text-[11px] font-medium text-warning-fg">
                            {t(hint ? "declarative.set.differsHint" : "declarative.set.differs")}
                          </span>
                        )}
                        {/* 서버는 다르다는데 두 값이 같게 보이면 가린 값(알림 대상 URL·머리 — 서버가 양쪽을 같게 가린다) */}
                        {byServer && r.differs && sameValue(r.file, r.live) && (
                          <span className="mt-0.5 block text-[11px] text-muted-foreground">
                            {t("declarative.set.masked")}
                          </span>
                        )}
                      </TableCell>
                      <TableCell className="max-w-64 align-top whitespace-normal">
                        {r.secret ? (
                          <span className="text-xs text-muted-foreground italic">
                            {t("declarative.set.secret")}
                          </span>
                        ) : (
                          <CompareValue path={r.path} value={r.file} empty="—" />
                        )}
                      </TableCell>
                      <TableCell className="max-w-64 align-top whitespace-normal">
                        {r.secret ? (
                          <span className="text-xs text-muted-foreground italic">
                            {t("declarative.set.secret")}
                          </span>
                        ) : (
                          <CompareValue
                            path={r.path}
                            value={r.live}
                            empty={
                              gone
                                ? t("declarative.set.gone")
                                : live
                                  ? "—"
                                  : t("declarative.set.noLive")
                            }
                          />
                        )}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )
        )}
      </div>
    </details>
  );
}

/** 이 묶음을 적용한 Task(작업 기록에 남은 것, 새것부터) — 줄을 누르면 Task 서랍 */
function Applies({ setId }: { setId: string }) {
  const { t } = useTranslation();
  const tasks = useList("task");
  const applies = useMemo(
    () =>
      newestFirst(
        (tasks.data ?? []).filter(
          (task) =>
            task.spec.op.kind === "apply" &&
            (task.spec.op.set === setId || task.spec.target === setId),
        ),
      ).slice(0, 20),
    [tasks.data, setId],
  );
  return (
    <section aria-labelledby="set-applies" className="flex flex-col gap-2">
      <h2 id="set-applies" className="text-base font-medium">
        {t("declarative.set.applies")}
      </h2>
      <p className="text-sm text-muted-foreground">{t("declarative.set.appliesHint")}</p>
      <TaskTable tasks={applies} labelledBy="set-applies" empty={t("declarative.set.noApplies")} />
    </section>
  );
}

/**
 * 코드로 관리(YAML) 묶음 하나(계획 73 §12.5·§13, 계획 77-7 — 진단 §3.5): 무엇을 관리하나·마지막 적용(결과), 파일과 다르면
 * 두 길(파일대로 되돌리기 — 다시 적용 명령 복사 / 지금 값을 파일 값으로 삼기 — `adopt`), 관리하는 리소스(링크·상태·고르기),
 * 리소스마다 "파일 값 ↔ 실제 값" 2열 비교(다른 칸 강조, 같은 리소스는 접음), 적용 기록(Task — 서랍), YAML 내보내기·변경 기록.
 */
export function ApplySetPage() {
  const { t, i18n } = useTranslation();
  const { name = "" } = useParams();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const set = useApplySet(name);
  const managed = useManagedSummary();
  const [chosen, setChosen] = useState<ReadonlySet<string>>(new Set());
  const [exporting, setExporting] = useState(false);
  const [confirming, setConfirming] = useState(false);
  // 실제 값: 묶음이 기억하는 리소스의 지금 매니페스트(볼 수 있는 것만) — 묶음 revision이 바뀌면 다시 읽는다
  const manifests = useQuery({
    queryKey: ["set-manifests", name, set.data?.revision],
    queryFn: () => fetchSetLive(name),
    enabled: set.data !== undefined,
  });
  if (set.data === undefined) {
    return <ListPending error={set.error} />;
  }
  const s = set.data;
  // 서버가 이 묶음을 견준 적이 없으면(적용 직후 검사 전·리더가 바뀜·옛 리더) 줄은 "같음"이 아니라 "아직 비교 전"
  const checked = s.status.drift !== undefined && s.status.drift !== null;
  const drift = new Map((s.status.drift?.items ?? []).map((d) => [d.id, d]));
  const objects: AppliedObject[] = [...(s.status.objects ?? [])].sort(
    (a, b) => a.kind.localeCompare(b.kind) || a.name.localeCompare(b.name),
  );
  // 묶음에 없는데 묶음 레이블을 단 것(드리프트 보고에만)
  const extra = [...drift.values()].filter((d) => !objects.some((o) => o.id === d.id));
  const rows: Row[] = [
    ...objects.map((o) => ({
      id: o.id,
      kind: o.kind,
      name: o.name,
      item: drift.get(o.id),
      declared: o.declared,
    })),
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
  // 다른 것부터 견준다(같은 것은 접혀 있다)
  const compared = [...rows]
    .sort((a, b) => Number(b.item !== undefined) - Number(a.item !== undefined))
    .slice(0, COMPARED);
  const live = manifests.data ?? { documents: [] };
  const revert = t("declarative.set.revertCommand", { name: s.name });

  async function adopt() {
    // 확인 대화상자에 보인 것(`targets`)만 id로 보낸다 — "모두"도 빈 본문이 아니다(대화상자를 연 뒤 새로 생긴 드리프트까지
    // 받아들이지 않게). 서버는 그 가운데 부른 이가 보고 바꿀 수 있는 것만 받아들인다.
    await call(
      api.POST("/apply-sets/{key}/adopt", {
        params: { path: { key: s.id } },
        body: { objects: targets.map((r) => r.id) },
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
          {
            label: t("declarative.sets.objects"),
            value: (
              <span className="block text-sm whitespace-normal">
                {t("declarative.set.total", { count: s.status.objects?.length ?? 0 })}
                <span className="block text-xs font-normal text-muted-foreground">
                  {managed(s)}
                </span>
              </span>
            ),
          },
          {
            label: t("declarative.sets.lastApplied"),
            value: (
              <span className="block text-sm whitespace-normal">
                <LastApplied set={s} />
              </span>
            ),
          },
          {
            label: t("declarative.set.checked"),
            value: (
              <span className="block text-sm whitespace-normal">
                {s.status.drift
                  ? formatTime(new Date(s.status.drift.checked_at), i18n.language)
                  : "—"}
              </span>
            ),
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
          <p className="font-medium">{t("declarative.set.driftHint", { count: drift.size })}</p>
          <ol className="flex list-decimal flex-col gap-2 pl-5">
            <li>
              <p>{t("declarative.set.revertWay")}</p>
              <span className="mt-1 inline-flex max-w-full items-center gap-1">
                <code className="min-w-0 rounded-sm bg-background/70 px-1.5 py-0.5 font-mono text-xs break-all text-foreground">
                  {revert}
                </code>
                <CopyButton text={revert} label={t("declarative.set.copyCommand")} />
              </span>
            </li>
            <li>
              <p>{t("declarative.set.adoptWay", { name: s.name })}</p>
              {adoptable && (
                <Button
                  size="sm"
                  variant="outline"
                  className="mt-1"
                  disabled={selecting && picked.length === 0}
                  onClick={() => setConfirming(true)}
                >
                  {selecting
                    ? t("declarative.set.adoptChosen", { count: picked.length })
                    : t("declarative.set.adoptAll")}
                </Button>
              )}
            </li>
          </ol>
        </div>
      )}
      <section aria-labelledby="set-resources" className="flex flex-col gap-2">
        <h2 id="set-resources" className="text-base font-medium">
          {t("declarative.set.resources")}
        </h2>
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
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((row) => {
                const state = rowState(row, checked);
                const now = renamedTo(row, liveOf(row as AppliedObject, live, row.item?.name));
                const href =
                  state === "missing" ? undefined : kindHref(row.kind, row.id, now ?? row.name);
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
                    <TableCell className="text-muted-foreground">{kindName(row.kind, t)}</TableCell>
                    <TableCell className="font-medium break-all">
                      {href ? (
                        <Link to={href} className="underline-offset-4 hover:underline">
                          {row.name}
                        </Link>
                      ) : (
                        row.name
                      )}
                      {now !== undefined && <Renamed from={row.name} to={now} />}
                    </TableCell>
                    <TableCell>
                      <StatusChip tone={TONE[state]}>{t(`declarative.state.${state}`)}</StatusChip>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      </section>
      <section
        id="compare"
        aria-labelledby="set-compare"
        className="flex scroll-mt-4 flex-col gap-2"
      >
        <h2 id="set-compare" className="text-base font-medium">
          {t("declarative.set.compare")}
        </h2>
        <p className="text-sm text-muted-foreground">{t("declarative.set.compareHint")}</p>
        {rows.length > COMPARED && (
          <p className="text-sm text-muted-foreground">
            {t("declarative.set.tooMany", { count: COMPARED })}
          </p>
        )}
        {compared.map((row) => (
          <ObjectCompare
            key={row.id}
            row={row}
            live={liveOf(row as AppliedObject, live, row.item?.name)}
            loading={manifests.data === undefined && !manifests.isError}
            checked={checked}
          />
        ))}
      </section>
      <Applies setId={s.id} />
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
            { kind: kindName(r.kind, t), name: r.name },
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
