import { useTranslation } from "react-i18next";

import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import type { Change } from "@/history/api";
import { secretMark, shortValue } from "@/history/diff";

/** 칸 값 하나: 비밀 표시는 번역한 글, 없음은 "—" */
function Value({ value, tone }: { value: unknown; tone: "old" | "new" }) {
  const { t } = useTranslation();
  const mark = secretMark(value);
  if (mark !== null) {
    return <span className="text-muted-foreground italic">{t(`history.secret.${mark}`)}</span>;
  }
  const text = shortValue(value);
  if (text === null) {
    return <span className="text-muted-foreground">—</span>;
  }
  return (
    <code
      className={
        tone === "old"
          ? "rounded-sm bg-danger-soft px-1 break-all text-danger-fg"
          : "rounded-sm bg-success-soft px-1 break-all text-success-fg"
      }
    >
      {text}
    </code>
  );
}

/**
 * 칸 바뀜 표(전·후 — 바뀐 경로만, 설계 §13 웹). 설정 기록 서랍과 계획 표가 같이 쓴다. 글은 상태 토큰(`-soft` 위 `-fg`)이라
 * 두 테마에서 대비가 맞는다(`tokens.test.ts`).
 */
export function ChangeTable({ changes, label }: { changes: readonly Change[]; label: string }) {
  const { t } = useTranslation();
  return (
    <div className="overflow-x-auto">
      <Table aria-label={label} className="text-xs">
        <TableHeader>
          <TableRow>
            <TableHead>{t("history.path")}</TableHead>
            <TableHead>{t("history.before")}</TableHead>
            <TableHead>{t("history.after")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {changes.map((c) => (
            <TableRow key={c.path}>
              <TableCell className="align-top font-mono break-all whitespace-normal">
                {c.path || t("history.wholeSpec")}
              </TableCell>
              <TableCell className="max-w-56 align-top whitespace-normal">
                <Value value={c.op === "add" ? undefined : c.old} tone="old" />
              </TableCell>
              <TableCell className="max-w-56 align-top whitespace-normal">
                <Value value={c.op === "remove" ? undefined : c.new} tone="new" />
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
