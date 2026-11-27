import { useTranslation } from "react-i18next";

import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { actorName, type ChangeSource, type HistoryEntry } from "@/history/api";
import { changeSummary, kindName } from "@/history/describe";

const OP_TONE: Record<HistoryEntry["op"], StatusTone> = {
  create: "success",
  update: "info",
  delete: "danger",
};

/** 출처 칩: 사람이 바꾼 것(웹·CLI·API)은 회색, 선언(적용·Terraform)은 파랑, 자동(Task·system)은 노랑 */
const SOURCE_TONE: Record<ChangeSource, StatusTone> = {
  web: "neutral",
  cli: "neutral",
  api: "neutral",
  apply: "info",
  terraform: "info",
  task: "warning",
  system: "warning",
};

/** 출처 칩: 사람 말 이름 + 풀어 쓴 설명(마우스를 올리면) */
export function SourceChip({ source }: { source: ChangeSource }) {
  const { t } = useTranslation();
  return (
    <span title={t(`history.sourceHints.${source}`)}>
      <StatusChip tone={SOURCE_TONE[source]}>{t(`history.sources.${source}`)}</StatusChip>
    </span>
  );
}

/**
 * 시간 줄 한 줄(계획 77-7): "바꿈 · 가상 머신 web-01 · admin · 웹 화면" 아래 "메모리 4 GiB → 8 GiB · CPU 코어 1 → 2"처럼
 * 바뀐 칸을 사람 말로. 누르면 그 줄의 서랍(키보드로도 — 단추).
 */
export function EntryLine({
  entry,
  onOpen,
  showName = true,
}: {
  entry: HistoryEntry;
  onOpen(): void;
  showName?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const summary = changeSummary(entry, t);
  const time = new Date(entry.at_ms);
  return (
    <button
      type="button"
      onClick={onOpen}
      className="flex w-full flex-col gap-1 rounded-md border px-3 py-2 text-left text-sm hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none"
    >
      <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <time dateTime={time.toISOString()} className="text-muted-foreground tabular-nums">
          {time.toLocaleTimeString(i18n.language, { hour: "2-digit", minute: "2-digit" })}
        </time>
        <StatusChip tone={OP_TONE[entry.op]}>{t(`history.ops.${entry.op}`)}</StatusChip>
        {showName && (
          <span className="break-all">
            <span className="text-muted-foreground">{kindName(entry.kind, t)}</span>{" "}
            <span className="font-medium">{entry.name}</span>
          </span>
        )}
        <span className="text-muted-foreground">{actorName(entry.actor)}</span>
        <SourceChip source={entry.source} />
        {entry.restored_from !== undefined && (
          <span className="text-xs text-muted-foreground">
            {t("history.restoredFromN", { revision: entry.restored_from })}
          </span>
        )}
      </span>
      {summary.length > 0 && (
        <span className="text-xs break-words text-muted-foreground">{summary.join(" · ")}</span>
      )}
      {entry.reason && <span className="text-xs break-words">“{entry.reason}”</span>}
    </button>
  );
}
