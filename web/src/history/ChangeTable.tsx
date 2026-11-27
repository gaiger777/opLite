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
import { fieldName, valueText } from "@/history/describe";
import { secretMark } from "@/history/diff";

/** 칸 값 하나: 비밀 표시는 번역한 글, 크기는 단위(8 GiB), 참거짓은 켬·끔, 없음은 "—" */
function Value({ path, value, tone }: { path: string; value: unknown; tone: "old" | "new" }) {
  const { t } = useTranslation();
  const mark = secretMark(value);
  if (mark !== null) {
    return <span className="text-muted-foreground italic">{t(`history.secret.${mark}`)}</span>;
  }
  const text = valueText(path, value, t);
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

/** 칸 이름: 사람 말(사전에 있으면) 아래 원래 경로를 작게 */
export function FieldCell({ path }: { path: string }) {
  const { t } = useTranslation();
  const { label, raw } = fieldName(path, t);
  return raw ? (
    <span className="font-mono break-all">{label}</span>
  ) : (
    <>
      <span className="break-words">{label}</span>
      {path && (
        <span className="block font-mono text-[11px] break-all text-muted-foreground">{path}</span>
      )}
    </>
  );
}

/**
 * 칸 바뀜 표(전·후 — 바뀐 경로만, 설계 §13 웹). 설정 기록 서랍과 계획 표가 같이 쓴다. 글은 상태 토큰(`-soft` 위 `-fg`)이라
 * 두 테마에서 대비가 맞는다(`tokens.test.ts`).
 */
export function ChangeTable({
  changes,
  label,
  removed,
}: {
  changes: readonly Change[];
  label: string;
  /** 지운 칸의 "후" 자리 글(되돌리기 계획 — spec 전체를 바꾸므로 지운 칸은 기본값으로 돌아간다). 없으면 "—" */
  removed?: string;
}) {
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
              <TableCell className="align-top whitespace-normal">
                <FieldCell path={c.path} />
              </TableCell>
              <TableCell className="max-w-56 align-top whitespace-normal">
                <Value path={c.path} value={c.op === "add" ? undefined : c.old} tone="old" />
              </TableCell>
              <TableCell className="max-w-56 align-top whitespace-normal">
                {c.op === "remove" && removed !== undefined ? (
                  <span className="text-muted-foreground italic">{removed}</span>
                ) : (
                  <Value path={c.path} value={c.op === "remove" ? undefined : c.new} tone="new" />
                )}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
