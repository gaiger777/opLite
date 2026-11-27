import { History, RefreshCw, RotateCcw } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSearchParams } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  canRestoreAnything,
  groupByDay,
  HISTORY_KINDS,
  type HistoryEntry,
  type HistoryFilter,
  PERIODS,
  type Period,
  SOURCES,
  useTimeline,
} from "@/history/api";
import { ChangeDrawer } from "@/history/ChangeDrawer";
import { kindName } from "@/history/describe";
import { EntryLine } from "@/history/EntryLine";
import { PointInTimeDialog } from "@/history/PointInTimeDialog";
import { errorMessage } from "@/i18n/errors";

const PERIOD_KEYS = [...(Object.keys(PERIODS) as Period[]), "all"] as const;

/** 주소의 거르기(`?kind=&key=&folder=&apply_set=&user=&source=&period=` — 선언 묶음 화면·경보가 링크로 연다) */
function filterOf(params: URLSearchParams): HistoryFilter {
  const period = params.get("period") as Period | null;
  return {
    kind: params.get("kind") ?? "",
    key: params.get("key") ?? "",
    user: params.get("user") ?? "",
    source: params.get("source") ?? "",
    folder: params.get("folder") ?? "",
    applySet: params.get("apply_set") ?? "",
    period: period && (PERIOD_KEYS as readonly string[]).includes(period) ? period : "7d",
  };
}

/**
 * 기록 › 변경 기록(계획 73 §12·§13): 설정 리소스 spec 바뀜의 시간 줄(새것부터, 날짜마다), 거르기(종류·사용자·출처·
 * 기간·폴더), 출처 칩·사유, 줄을 누르면 서랍(전·후 표·되돌리기). 누구나 읽고 서버가 읽을 수 있는 리소스의 줄만 준다.
 * "한 시점으로 되돌리기…"는 선언 종류의 쓰기 권한이 어딘가에 있는 사용자에게만 보인다. 늦게 읽는 조각이다.
 */
export function HistoryPage() {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const [params, setParams] = useSearchParams();
  const filter = filterOf(params);
  // 입력 중인 글(적용 단추로 주소에 넣는다)
  const [user, setUser] = useState(filter.user ?? "");
  const [folder, setFolder] = useState(filter.folder ?? "");
  const [opened, setOpened] = useState<HistoryEntry | null>(null);
  const [point, setPoint] = useState(false);
  const timeline = useTimeline(filter);
  const rows = timeline.data?.pages.flatMap((p) => p.items) ?? [];
  const set = (key: string, value: string) =>
    setParams(
      (p) => {
        if (value) {
          p.set(key, value);
        } else {
          p.delete(key);
        }
        if (key === "kind") {
          p.delete("key");
        }
        return p;
      },
      { replace: true },
    );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="history-title"
        title={t("nav.history")}
        help="history"
        intro={t("history.intro")}
        actions={
          <>
            <Button
              variant="outline"
              disabled={timeline.isFetching}
              onClick={() => void timeline.refetch()}
            >
              <RefreshCw />
              {t("history.refresh")}
            </Button>
            {canRestoreAnything(perms) && (
              <Button variant="outline" onClick={() => setPoint(true)}>
                <RotateCcw />
                {t("history.point.open")}
              </Button>
            )}
          </>
        }
      />
      <div className="flex flex-wrap items-end gap-3">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="history-kind">{t("history.kind")}</Label>
          <NativeSelect
            id="history-kind"
            value={filter.kind}
            onChange={(e) => set("kind", e.target.value)}
          >
            <NativeSelectOption value="">{t("history.any")}</NativeSelectOption>
            {HISTORY_KINDS.map((k) => (
              <NativeSelectOption key={k} value={k}>
                {kindName(k, t)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="history-source">{t("history.source")}</Label>
          <NativeSelect
            id="history-source"
            value={filter.source}
            onChange={(e) => set("source", e.target.value)}
          >
            <NativeSelectOption value="">{t("history.any")}</NativeSelectOption>
            {SOURCES.map((s) => (
              <NativeSelectOption key={s} value={s}>
                {t(`history.sources.${s}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="history-period">{t("history.period")}</Label>
          <NativeSelect
            id="history-period"
            value={filter.period}
            onChange={(e) => set("period", e.target.value)}
          >
            {PERIOD_KEYS.map((p) => (
              <NativeSelectOption key={p} value={p}>
                {t(`history.periods.${p}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <form
          noValidate
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setParams(
              (p) => {
                for (const [key, value] of [
                  ["user", user.trim()],
                  ["folder", folder.trim()],
                ] as const) {
                  if (value) {
                    p.set(key, value);
                  } else {
                    p.delete(key);
                  }
                }
                return p;
              },
              { replace: true },
            );
          }}
        >
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="history-user">{t("history.user")}</Label>
            <Input
              id="history-user"
              className="w-36"
              value={user}
              onChange={(e) => setUser(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="history-folder">{t("history.folder")}</Label>
            <Input
              id="history-folder"
              className="w-40"
              placeholder="/prod"
              value={folder}
              onChange={(e) => setFolder(e.target.value)}
            />
          </div>
          <Button type="submit" variant="outline">
            {t("history.apply")}
          </Button>
        </form>
      </div>
      {(filter.key || filter.applySet) && (
        <p role="status" className="text-sm text-muted-foreground">
          {filter.applySet
            ? t("history.onlySet", { name: filter.applySet })
            : t("history.onlyKey", { kind: kindName(filter.kind ?? "", t), key: filter.key })}{" "}
          <Button
            variant="link"
            className="h-auto p-0"
            onClick={() =>
              setParams(
                (p) => {
                  p.delete("key");
                  p.delete("apply_set");
                  return p;
                },
                { replace: true },
              )
            }
          >
            {t("history.showAll")}
          </Button>
        </p>
      )}
      {timeline.data === undefined ? (
        <ListPending error={timeline.error} />
      ) : rows.length === 0 ? (
        <EmptyState
          icon={History}
          title={t("history.empty")}
          description={t("history.emptyHint")}
          help="history.page"
        />
      ) : (
        <div className="flex flex-col gap-4" aria-labelledby="history-title" role="region">
          {timeline.isError && (
            <p role="alert" className="text-sm text-destructive">
              {errorMessage(t, timeline.error)}
            </p>
          )}
          {groupByDay(rows, i18n.language).map((group, i) => (
            <section key={group.day} aria-labelledby={`history-day-${i}`}>
              <h2
                id={`history-day-${i}`}
                className="mb-2 text-sm font-medium text-muted-foreground"
              >
                {group.day}
              </h2>
              <ul className="flex flex-col gap-1">
                {group.rows.map((entry) => (
                  <li key={entry.revision}>
                    <EntryLine entry={entry} onOpen={() => setOpened(entry)} />
                  </li>
                ))}
              </ul>
            </section>
          ))}
          {timeline.hasNextPage && (
            <div>
              <Button
                variant="outline"
                disabled={timeline.isFetchingNextPage}
                onClick={() => void timeline.fetchNextPage()}
              >
                {t("history.loadMore")}
              </Button>
            </div>
          )}
        </div>
      )}
      {opened && <ChangeDrawer entry={opened} onClose={() => setOpened(null)} />}
      {point && (
        <PointInTimeDialog
          folder={filter.folder}
          applySet={filter.applySet}
          onClose={() => setPoint(false)}
        />
      )}
    </div>
  );
}
