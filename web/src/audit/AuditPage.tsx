import { useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import type { AuditEntry } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { focusBack } from "@/components/focusBack";
import { TableMessage } from "@/components/TableMessage";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatTime } from "@/lib/format";

/** 고를 수 있는 시간 범위(지금부터 거꾸로, 밀리초) */
const RANGES = { "1h": 3_600_000, "24h": 86_400_000, "7d": 604_800_000 } as const;
type Range = keyof typeof RANGES;

/**
 * 모든 노드의 감사 로그. admin만 읽는다. 자동으로 다시 읽지 않고, 범위·거르기를 바꾸거나 새로고침할 때
 * 읽는다(그때의 지금부터 범위를 잰다).
 */
export function AuditPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const admin = me.role === "admin";
  const [range, setRange] = useState<Range>("24h");
  // 입력한 값과 적용한 값. 적용 버튼을 눌러야 쿼리에 들어간다.
  const [user, setUser] = useState("");
  const [action, setAction] = useState("");
  const [filter, setFilter] = useState({ user: "", action: "" });
  const [opened, setOpened] = useState<AuditEntry | null>(null);
  const returnFocus = useRef<HTMLElement | null>(null);
  const audit = useQuery({
    queryKey: ["audit", range, filter],
    queryFn: () =>
      call(
        api.GET("/audit", {
          params: {
            query: {
              since: String(Date.now() - RANGES[range]),
              ...(filter.user && { user: filter.user }),
              ...(filter.action && { action: filter.action }),
            },
          },
        }),
      ),
    enabled: admin,
  });

  // 메뉴에 없지만 주소로 직접 들어올 수 있다. admin이 아니면 읽지 않고 안내한다.
  if (!admin) {
    return (
      <div className="flex flex-col gap-4">
        <h1 className="text-2xl font-semibold">{t("nav.audit")}</h1>
        <p className="text-sm text-muted-foreground">{t("audit.adminOnly")}</p>
      </div>
    );
  }

  const entries = audit.data?.entries ?? [];
  const unreachable = audit.data?.unreachable ?? [];
  const disabled = audit.data?.disabled ?? [];
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="audit-title" className="text-2xl font-semibold">
          {t("nav.audit")}
        </h1>
        <Button variant="outline" disabled={audit.isFetching} onClick={() => void audit.refetch()}>
          <RefreshCw />
          {t("audit.refresh")}
        </Button>
      </div>
      <div className="flex flex-wrap items-end gap-4">
        <div className="flex flex-col gap-2">
          <Label htmlFor="audit-range">{t("audit.range")}</Label>
          <NativeSelect
            id="audit-range"
            value={range}
            onChange={(e) => setRange(e.target.value as Range)}
          >
            {(Object.keys(RANGES) as Range[]).map((r) => (
              <NativeSelectOption key={r} value={r}>
                {t(`audit.ranges.${r}`)}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </div>
        <form
          noValidate
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            setFilter({ user: user.trim(), action: action.trim() });
          }}
        >
          <div className="flex flex-col gap-2">
            <Label htmlFor="audit-user">{t("audit.user")}</Label>
            <Input id="audit-user" value={user} onChange={(e) => setUser(e.target.value)} />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="audit-action">{t("audit.action")}</Label>
            <Input
              id="audit-action"
              placeholder="POST /vms"
              value={action}
              onChange={(e) => setAction(e.target.value)}
            />
          </div>
          <Button type="submit" variant="outline">
            {t("audit.apply")}
          </Button>
        </form>
      </div>
      {/* 다시 읽지 못하면 마지막으로 읽은 내용을 그대로 보이고 오류를 알린다. */}
      {audit.isError && entries.length > 0 && (
        <p role="alert" className="text-sm text-destructive">
          {t("audit.refreshFailed", { error: errorMessage(t, audit.error) })}
        </p>
      )}
      {unreachable.length > 0 && (
        <p
          role="alert"
          className="rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm"
        >
          {t("audit.unreachable", { nodes: unreachable.join(", ") })}
        </p>
      )}
      {disabled.length > 0 && (
        <p role="status" className="rounded-md border border-border bg-muted/50 px-3 py-2 text-sm">
          {t("audit.disabled", { nodes: disabled.join(", ") })}
        </p>
      )}
      {audit.data?.truncated && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("audit.truncated", { count: entries.length })}
        </p>
      )}
      <Table aria-labelledby="audit-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("audit.time")}</TableHead>
            <TableHead>{t("common.node")}</TableHead>
            <TableHead>{t("audit.user")}</TableHead>
            <TableHead>{t("audit.ip")}</TableHead>
            <TableHead>{t("audit.action")}</TableHead>
            <TableHead>{t("audit.target")}</TableHead>
            <TableHead>{t("audit.status")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.length === 0 && (
            <TableMessage colSpan={7} query={audit} empty={t("audit.empty")} />
          )}
          {entries.map((entry) => {
            const time = new Date(entry.time);
            // 노드가 스스로 남긴 항목(예: 균형 조정기의 `placement.move`)은 번역한 이름과 detail을 함께 보인다.
            const named = `audit.actions.${entry.action}`;
            const known = i18n.exists(named);
            return (
              <TableRow
                key={entry.id}
                className="cursor-pointer"
                onClick={(e) => {
                  returnFocus.current = e.currentTarget.querySelector("button");
                  setOpened(entry);
                }}
              >
                <TableCell className="whitespace-nowrap">
                  <time dateTime={time.toISOString()}>{formatTime(time, i18n.language)}</time>
                </TableCell>
                <TableCell>{entry.node}</TableCell>
                <TableCell>{entry.user ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{entry.ip}</TableCell>
                <TableCell>
                  {/* 키보드로도 열 수 있게 단추로 둔다. 누르면 행의 onClick이 연다. */}
                  <button
                    type="button"
                    className="text-left font-mono text-xs underline-offset-4 hover:underline"
                  >
                    {entry.action}
                  </button>
                  {known && <span className="block text-xs text-muted-foreground">{t(named)}</span>}
                </TableCell>
                <TableCell>
                  {/* 대상이 없으면 detail(예: audit.dropped의 수, 로그인의 username=…) */}
                  {entry.target
                    ? `${entry.target.kind}/${entry.target.key}`
                    : (entry.detail ?? "—")}
                  {known && entry.target && entry.detail && (
                    <span className="block text-xs text-muted-foreground">{entry.detail}</span>
                  )}
                </TableCell>
                <TableCell
                  className={entry.status >= 400 ? "font-medium text-destructive" : undefined}
                >
                  {entry.status}
                </TableCell>
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {opened && (
        <Dialog open onOpenChange={(open) => !open && setOpened(null)}>
          <DialogContent
            aria-describedby={undefined}
            className="sm:max-w-2xl"
            onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
          >
            <DialogHeader>
              <DialogTitle>{t("audit.entryTitle")}</DialogTitle>
            </DialogHeader>
            <pre className="max-h-96 overflow-auto rounded-md bg-muted p-2 font-mono text-xs">
              {JSON.stringify(opened, null, 2)}
            </pre>
            <DialogFooter>
              <Button variant="outline" onClick={() => setOpened(null)}>
                {t("common.done")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
    </div>
  );
}
