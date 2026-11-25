import { useTranslation } from "react-i18next";

import { StatusChip, type StatusTone } from "@/components/StatusChip";
import { actorName, type ChangeSource, type HistoryEntry } from "@/history/api";

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

/** 바뀐 칸을 몇 개만(나머지는 수) */
const SHOWN = 3;

/** 시간 줄 한 줄: 누르면 그 줄의 서랍(키보드로도 — 단추) */
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
  const changed = entry.changed ?? [];
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
          <span className="font-medium break-all">
            {entry.kind} {entry.name}
          </span>
        )}
        <span className="text-muted-foreground">{actorName(entry.actor)}</span>
        <StatusChip tone={SOURCE_TONE[entry.source]}>
          {t(`history.sources.${entry.source}`)}
        </StatusChip>
        {entry.restored_from !== undefined && (
          <span className="text-xs text-muted-foreground">
            {t("history.restoredFromN", { revision: entry.restored_from })}
          </span>
        )}
      </span>
      {changed.length > 0 && (
        <span className="font-mono text-xs break-all text-muted-foreground">
          {changed.slice(0, SHOWN).join(", ")}
          {changed.length > SHOWN && ` ${t("history.more", { count: changed.length - SHOWN })}`}
        </span>
      )}
      {entry.reason && <span className="text-xs break-words">“{entry.reason}”</span>}
    </button>
  );
}
