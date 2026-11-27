import { ArrowLeft, History } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";

import { usePermissions } from "@/auth/usePermissions";
import { CopyButton } from "@/components/CopyButton";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import {
  actorName,
  canRestore,
  type HistoryEntry,
  useHistoryDetail,
  useResourceHistory,
} from "@/history/api";
import { ChangeTable } from "@/history/ChangeTable";
import { kindName } from "@/history/describe";
import { hasSecret } from "@/history/diff";
import { RestoreDialog } from "@/history/RestoreDialog";
import { EntryLine, SourceChip, useEntryName } from "@/history/EntryLine";
import { formatTime } from "@/lib/format";

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[max-content_1fr] gap-x-4 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/**
 * 이력 줄 하나(계획 73 §13 웹 `ChangeDrawer`의 몸): 누가·언제·어디서·왜, 비교(앞 줄·지금)의 바뀐 칸 표, 펼치면 그때의 전체
 * spec(비밀은 표시만), 감사 줄 id, "이 revision으로 되돌리기…"(선언 종류·쓰기 권한이 있을 때).
 */
export function ChangeDetail({ entry }: { entry: HistoryEntry }) {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const [compare, setCompare] = useState<"prev" | "live">("prev");
  const [restoring, setRestoring] = useState(false);
  const detail = useHistoryDetail(entry, compare);
  const record = detail.data?.record;
  const folder = (record?.spec as { folder?: unknown } | undefined)?.folder;
  const restore = record !== undefined && canRestore(perms, entry.kind, entry.id, folder);
  const diff = detail.data?.diff ?? [];
  return (
    <div className="flex flex-col gap-4">
      <dl className="flex flex-col gap-1">
        <Row label={t("history.when")}>{formatTime(new Date(entry.at_ms), i18n.language)}</Row>
        <Row label={t("history.who")}>{actorName(entry.actor)}</Row>
        <Row label={t("history.source")}>
          <span className="inline-flex flex-wrap items-center gap-2">
            <SourceChip source={entry.source} />
            <span className="text-xs text-muted-foreground">
              {t(`history.sourceHints.${entry.source}`)}
            </span>
          </span>
        </Row>
        <Row label={t("history.revision")}>{entry.revision}</Row>
        {entry.reason && <Row label={t("history.reason")}>{entry.reason}</Row>}
        {entry.restored_from !== undefined && (
          <Row label={t("history.restoredFrom")}>{entry.restored_from}</Row>
        )}
        {entry.request && (
          <Row label={t("history.auditEntry")}>
            <span className="inline-flex items-center gap-1">
              <code className="text-xs break-all">{entry.request}</code>
              <CopyButton text={entry.request} label={t("history.copyAuditId")} />
            </span>
          </Row>
        )}
      </dl>
      {restore && (
        <div className="flex flex-col items-start gap-1">
          <Button size="sm" onClick={() => setRestoring(true)}>
            {t(entry.op === "delete" ? "history.restore.recreateOpen" : "history.restore.open")}
          </Button>
          <p className="text-xs text-muted-foreground">
            {t(entry.op === "delete" ? "history.recreateHint" : "history.restoreHint")}
          </p>
        </div>
      )}
      <div className="flex flex-wrap items-end gap-2">
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={`compare-${entry.revision}`}>{t("history.compare")}</Label>
          <NativeSelect
            id={`compare-${entry.revision}`}
            value={compare}
            onChange={(e) => setCompare(e.target.value as "prev" | "live")}
          >
            <NativeSelectOption value="prev">{t("history.comparePrev")}</NativeSelectOption>
            <NativeSelectOption value="live">{t("history.compareLive")}</NativeSelectOption>
          </NativeSelect>
        </div>
      </div>
      {detail.data === undefined ? (
        <ListPending error={detail.error} />
      ) : diff.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("history.noDiff")}</p>
      ) : (
        <ChangeTable changes={diff} label={t("history.changesOf", { name: entry.name })} />
      )}
      {record && (
        <details>
          <summary className="cursor-pointer text-sm">{t("history.fullSpec")}</summary>
          {hasSecret(record.spec) && (
            <p className="mt-2 text-xs text-muted-foreground">{t("history.secretHint")}</p>
          )}
          <pre className="mt-2 max-h-80 overflow-auto rounded-md bg-muted p-2 font-mono text-xs">
            {JSON.stringify(record.spec, null, 2)}
          </pre>
        </details>
      )}
      {restoring && <RestoreDialog entry={entry} onClose={() => setRestoring(false)} />}
    </div>
  );
}

/** 설정 기록 화면에서 줄을 누르면 여는 오른쪽 서랍 */
export function ChangeDrawer({ entry, onClose }: { entry: HistoryEntry; onClose(): void }) {
  const { t } = useTranslation();
  const name = useEntryName(entry);
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-2xl">
        <SheetTitle className="px-4 pt-4 text-lg break-all">
          {t("history.drawerTitle", { kind: kindName(entry.kind, t), name })}
        </SheetTitle>
        <SheetDescription className="px-4">
          {t(`history.ops.${entry.op}`)} · {t("history.revisionN", { revision: entry.revision })}
        </SheetDescription>
        <div className="px-4 pb-4">
          <ChangeDetail entry={entry} />
        </div>
      </SheetContent>
    </Sheet>
  );
}

/**
 * 리소스 ⋯ 메뉴의 "설정 기록…" 서랍(같은 부품): 그 리소스의 줄 목록, 줄을 누르면 같은 서랍 안에서 그 줄을 보인다.
 */
export function ResourceHistoryDrawer({
  kind,
  id,
  name,
  onClose,
}: {
  kind: string;
  id: string;
  name: string;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const rows = useResourceHistory(kind, id);
  const [opened, setOpened] = useState<HistoryEntry | null>(null);
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-2xl">
        <SheetTitle className="px-4 pt-4 text-lg break-all">
          {t("history.resourceTitle", { name })}
        </SheetTitle>
        <SheetDescription className="px-4">{t("history.resourceHint")}</SheetDescription>
        <div className="flex flex-col gap-4 px-4 pb-4">
          {opened ? (
            <>
              <div>
                <Button variant="outline" size="sm" onClick={() => setOpened(null)}>
                  <ArrowLeft />
                  {t("history.backToList")}
                </Button>
              </div>
              <p className="text-sm font-medium">
                {t(`history.ops.${opened.op}`)} ·{" "}
                {t("history.revisionN", { revision: opened.revision })}
              </p>
              <ChangeDetail entry={opened} />
            </>
          ) : rows.data === undefined ? (
            <ListPending error={rows.error} />
          ) : rows.data.length === 0 ? (
            <EmptyState icon={History} title={t("history.emptyResource")} />
          ) : (
            <ul aria-label={t("history.resourceTitle", { name })} className="flex flex-col gap-1">
              {rows.data.map((entry) => (
                <li key={entry.revision}>
                  <EntryLine entry={entry} onOpen={() => setOpened(entry)} showName={false} />
                </li>
              ))}
            </ul>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
