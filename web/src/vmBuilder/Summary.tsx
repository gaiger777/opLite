// 오른쪽 고정 요약·검사(계획 77-5 — 진단 §4.3, EC2 요약 판·vSphere 호환성 판): 이름·폴더·놓일 노드(자동이면 서버 미리
// 확인이 고른 노드)와 그 노드의 여유 막대(이미 쓴 것 + 이 VM, 실제보다 많이 나눠 주면 경고), 서버 확인 상태와 문제(누르면 그
// 칸), 만든 뒤 켜기, 고급 설정·요청 보기(JSON·CLI) 서랍, 만들기 단추(늘 보임 — 막는 문제가 있으면 누를 때 보이고 그 칸으로).
// 1280px 밑에서는 그림 아래에 오고 만들기 줄은 아래 고정 띠다.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { CopyButton } from "@/components/CopyButton";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetDescription, SheetTitle } from "@/components/ui/sheet";
import { useDnd } from "@/dnd/DndProvider";
import { serverErrorMessage } from "@/i18n/errors";
import { formatBytes, formatMib } from "@/lib/format";
import { readUserPref, writeUserPref } from "@/lib/prefs";
import { cn } from "@/lib/utils";
import { useBuilder } from "@/vmBuilder/context";
import { partLabel, problemSlot, problemText, type Thing } from "@/vmBuilder/labels";
import { cliText } from "@/vmBuilder/model/cli";
import { parts } from "@/vmBuilder/model/parts";
import type { Problem } from "@/vmBuilder/model/problems";
import { jsonText, requests } from "@/vmBuilder/model/requests";
import { type Bar, nodeRoom, percent, targetNode } from "@/vmBuilder/model/room";
import { setName } from "@/vmBuilder/model/state";
import type { ServerCheck } from "@/vmBuilder/useServerCheck";

/** 문제의 칸으로 초점(이름 칸이면 입력) */
export function focusProblem(p: Problem) {
  const at = problemSlot(p.at);
  const el = document.getElementById(at === "name" ? "vb-name" : `vb-${at}`);
  el?.scrollIntoView?.({ block: "nearest" });
  el?.focus();
}

/** 요약에 바로 보이는 문제 수(나머지는 "n개 더" — 요약이 한 화면을 넘지 않게) */
const SHOWN = 3;

export function Summary({
  server,
  failure,
  blocked,
  onCreate,
}: {
  server: ServerCheck;
  failure: string | null;
  /** 만들기를 막은 첫 문제(만들기를 누른 뒤에만) */
  blocked: Problem | null;
  onCreate(): void;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const [all, setAll] = useState(false);
  const [code, setCode] = useState(false);
  const check = server.check;
  const count = check?.problems.length ?? 0;
  // 서버 확인 결과가 바뀌면 알림 줄에 읽는다(초점은 옮기지 않음 — 계획 71 설계 §11).
  const { announce } = dnd;
  useEffect(() => {
    if (server.at > 0) {
      announce(count ? t("vmBuilder.check.problems", { count }) : t("vmBuilder.check.ok"));
    }
  }, [server.at, count, announce, t]);
  const v = b.s.values;
  const status =
    b.s.base.kind === "template"
      ? t("vmBuilder.check.template")
      : server.status === "done"
        ? count
          ? t("vmBuilder.check.problems", { count })
          : t("vmBuilder.check.ok")
        : server.status === "off" && v.name === ""
          ? t("vmBuilder.check.needName")
          : t(`vmBuilder.check.${server.status}`);
  const nameProblem = b.showName ? b.list.find((p) => p.at === "name") : undefined;
  const shown = all ? b.list : b.list.slice(0, SHOWN);

  return (
    <aside
      aria-label={t("vmBuilder.page.side")}
      className="vb-col flex min-w-0 flex-col gap-3 rounded-lg border bg-card p-3"
    >
      <Field
        id="vb-name"
        label={t("common.name")}
        error={nameProblem && (nameProblem.message ?? nameProblem.key)}
        errorValues={nameProblem?.values as Record<string, string> | undefined}
        help={t("vmBuilder.help.name")}
      >
        {(a11y) => (
          <Input
            id="vb-name"
            value={v.name}
            disabled={b.busy}
            autoComplete="off"
            placeholder="web-01"
            onChange={(e) => b.set(setName(b.s, e.target.value))}
            onBlur={() => v.name !== "" && b.touch()}
            {...a11y}
          />
        )}
      </Field>
      <div className="grid grid-cols-2 gap-2">
        <FolderSelect />
        <NodeSelect />
      </div>
      <Room />
      <section aria-labelledby="vb-checks" className="flex flex-col gap-1">
        <h2 id="vb-checks" className="text-xs font-medium text-muted-foreground">
          {t("vmBuilder.check.title")}
        </h2>
        <p className="text-sm" data-stale={server.stale ? "" : undefined}>
          {status}
          {server.stale && server.status !== "off" && (
            <span className="block text-xs text-muted-foreground">
              {t("vmBuilder.check.stale")}
            </span>
          )}
        </p>
        {server.status === "failed" && (
          <p className="text-xs text-muted-foreground">
            {serverErrorMessage(t, server.error, ["not_found"])}{" "}
            <Button
              type="button"
              variant="link"
              size="sm"
              className="h-auto p-0 text-xs"
              onClick={server.retry}
            >
              {t("vmBuilder.check.retry")}
            </Button>
          </p>
        )}
        {check && !server.stale && check.cpu_model && (
          <p className="text-xs text-muted-foreground">
            {[
              t("vmBuilder.check.cpu", { model: check.cpu_model }),
              check.machine && t("vmBuilder.check.machine", { machine: check.machine }),
              ...(check.ips ?? []).flatMap((ips, i) =>
                ips.length ? [t("vmBuilder.check.ips", { n: i + 1, ips: ips.join(", ") })] : [],
              ),
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
        )}
        {b.list.length > 0 && (
          <ul className="flex flex-col" aria-label={t("vmBuilder.check.list")}>
            {shown.map((p, i) => (
              <li key={i}>
                <button
                  type="button"
                  data-level={p.level}
                  className="w-full truncate rounded-md px-1 py-0.5 text-left text-xs hover:bg-muted"
                  title={problemText(t, p)}
                  onClick={() => focusProblem(p)}
                >
                  <span className="font-medium">{t(`vmBuilder.check.${p.level}`)}</span>{" "}
                  {problemText(t, p)}
                </button>
              </li>
            ))}
          </ul>
        )}
        {b.list.length > SHOWN && (
          <Button
            type="button"
            variant="link"
            size="sm"
            className="h-auto self-start p-0 text-xs"
            onClick={() => setAll(!all)}
          >
            {all
              ? t("vmBuilder.check.fewer")
              : t("vmBuilder.check.more", { count: b.list.length - SHOWN })}
          </Button>
        )}
      </section>
      <div className="flex flex-wrap gap-2">
        <Button type="button" variant="outline" size="sm" onClick={b.openAdvanced}>
          {t("vmBuilder.page.advanced")}
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={() => setCode(true)}>
          {t("vmBuilder.page.code")}
        </Button>
      </div>
      <div className="vb-bar mt-auto flex flex-col gap-2 border-t bg-card pt-3">
        {failure && (
          <p role="alert" className="text-sm text-destructive">
            {failure}
          </p>
        )}
        <div className="flex items-center gap-2">
          <Checkbox
            id="vb-start"
            checked={v.start}
            disabled={b.busy}
            onCheckedChange={(c) => b.edit({ start: c === true })}
          />
          <Label htmlFor="vb-start">{t("vmForm.startAfter")}</Label>
        </div>
        <Button type="button" disabled={b.busy} onClick={onCreate}>
          {b.busy ? t("vmBuilder.page.creating") : t("common.create")}
        </Button>
        {blocked && (
          <button
            type="button"
            className="text-left text-xs text-destructive underline"
            onClick={() => focusProblem(blocked)}
          >
            {t("vmBuilder.page.blocked", { reason: problemText(t, blocked) })}
          </button>
        )}
      </div>
      {code && <CodeSheet onClose={() => setCode(false)} />}
    </aside>
  );
}

/** 폴더 고르기(만들 수 있는 폴더 — 놓기 규칙과 같음) */
function FolderSelect() {
  const { t } = useTranslation();
  const b = useBuilder();
  const list = useMemo(() => parts(b.s, b.ctx).filter((p) => p.kind === "folder"), [b.s, b.ctx]);
  const v = b.s.values;
  const current = list.some((p) => p.kind === "folder" && p.folder === v.folder);
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <Label htmlFor="vb-folder">{t("folders.folder")}</Label>
      <NativeSelect
        id="vb-folder"
        className="w-full"
        disabled={b.busy}
        value={v.folder}
        onChange={(e) => {
          const part = list.find((p) => p.kind === "folder" && p.folder === e.target.value);
          if (part) {
            b.put({ ...part, label: partLabel(t, part) } as Thing, "folder");
          }
        }}
      >
        {!current && <NativeSelectOption value={v.folder}>{v.folder || "/"}</NativeSelectOption>}
        {list.map((p) => (
          <NativeSelectOption key={p.id} value={p.kind === "folder" ? p.folder : ""}>
            {p.name}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </div>
  );
}

/** 놓일 노드 고르기: 자동·노드(못 쓰면 흐림)·장애 영역. 노드를 읽지 못하면 "서버가 고름" */
function NodeSelect() {
  const { t } = useTranslation();
  const b = useBuilder();
  const list = useMemo(
    () => parts(b.s, b.ctx).filter((p) => p.group === "placement"),
    [b.s, b.ctx],
  );
  if (b.ctx.nodes === null) {
    return (
      <p className="self-end text-xs text-muted-foreground">
        {t("vmBuilder.page.placementServer")}
      </p>
    );
  }
  const value = b.s.zone
    ? `zone:${b.s.zone.value}`
    : b.s.values.node
      ? `node:${b.s.values.node}`
      : "auto";
  return (
    <div className="flex min-w-0 flex-col gap-1.5">
      <Label htmlFor="vb-node">{t("common.node")}</Label>
      <NativeSelect
        id="vb-node"
        className="w-full"
        disabled={b.busy}
        value={value}
        onChange={(e) => {
          const part = list.find((p) => p.id === e.target.value);
          if (part) {
            b.put({ ...part, label: partLabel(t, part) } as Thing, "placement");
          }
        }}
      >
        {list.map((p) => (
          <NativeSelectOption key={p.id} value={p.id} disabled={!!p.blocked}>
            {partLabel(t, p)}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </div>
  );
}

/** 놓일 노드의 여유 막대 셋(vCPU·메모리·디스크) */
function Room() {
  const { t } = useTranslation();
  const b = useBuilder();
  // 고른 노드, 아니면 서버 미리 확인이 고른 노드(지금 값에 대한 답일 때), 아니면 어림
  const at = targetNode(b.s, b.ctx, b.check);
  if (!at) {
    return null;
  }
  const room = nodeRoom(b.s, b.ctx, at.node);
  const over = room.cpu.used + room.cpu.add > room.cpu.total;
  return (
    <section aria-labelledby="vb-room" className="flex flex-col gap-1.5">
      <h2 id="vb-room" className="text-xs font-medium text-muted-foreground">
        {t(`vmBuilder.room.${at.how}`, { node: at.node.name })}
      </h2>
      <Meter
        label={t("vmBuilder.room.cpu")}
        bar={room.cpu}
        text={t("vmBuilder.room.cpuText", {
          used: room.cpu.used + room.cpu.add,
          total: room.cpu.total,
          add: room.cpu.add,
        })}
      />
      {room.memory && (
        <Meter
          label={t("vmBuilder.room.memory")}
          bar={room.memory}
          text={t("vmBuilder.room.text", {
            used: formatMib(room.memory.used + room.memory.add),
            total: formatMib(room.memory.total),
            add: formatMib(room.memory.add),
          })}
        />
      )}
      {room.disk && (
        <Meter
          label={t("vmBuilder.room.disk", { pool: room.disk.pool })}
          bar={room.disk}
          text={t("vmBuilder.room.text", {
            used: formatBytes(room.disk.used + room.disk.add),
            total: formatBytes(room.disk.total),
            add: formatBytes(room.disk.add),
          })}
        />
      )}
      {over && (
        <p className="text-xs" data-level="warn">
          {t("vmBuilder.room.overcommit", { pct: percent(room.cpu) })}
        </p>
      )}
    </section>
  );
}

/** 막대 하나: 이미 쓴 것(진함) + 이 VM(밝은 색), 넘치면 경고색 */
function Meter({ label, bar, text }: { label: string; bar: Bar; text: string }) {
  const pct = percent(bar);
  const used = bar.total > 0 ? Math.min(100, (bar.used / bar.total) * 100) : 0;
  const add = bar.total > 0 ? Math.min(100 - used, (bar.add / bar.total) * 100) : 0;
  const full = pct > 100;
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-baseline justify-between gap-2 text-xs">
        <span>{label}</span>
        <span className={cn("text-muted-foreground", full && "font-medium")}>{text}</span>
      </div>
      <div
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.min(pct, 100)}
        aria-valuetext={text}
        className="vb-bars"
      >
        <span style={{ width: `${used}%`, background: "var(--muted-foreground)" }} />
        <span style={{ width: `${add}%`, background: full ? "var(--warning)" : "var(--brand)" }} />
      </div>
    </div>
  );
}

const TABS = ["json", "cli"] as const;

/** 보낼 요청 그대로(JSON·CLI, 복사) — 오른쪽 서랍 */
function CodeSheet({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const b = useBuilder();
  const { user } = usePrincipal();
  const [tab, setTab] = useState(() => {
    const saved = readUserPref(user, "vmBuilder.panel");
    return TABS.find((x) => x === saved) ?? "json";
  });
  const choose = (x: (typeof TABS)[number]) => {
    setTab(x);
    writeUserPref(user, "vmBuilder.panel", x);
  };
  const reqs = useMemo(() => requests(b.s, b.ctx), [b.s, b.ctx]);
  const text = tab === "json" ? jsonText(reqs, t("vmBuilder.json.masked")) : cliText(b.s, b.ctx, t);
  return (
    <Sheet open onOpenChange={(o) => !o && onClose()}>
      <SheetContent side="right" className="flex w-full flex-col gap-3 p-4 sm:max-w-xl">
        <SheetTitle>{t("vmBuilder.page.code")}</SheetTitle>
        <SheetDescription>{t("vmBuilder.page.codeText")}</SheetDescription>
        {/* 탭 둘(화살표 ←·→로 옮김) */}
        <div
          role="tablist"
          aria-label={t("vmBuilder.page.code")}
          className="inline-flex self-start rounded-lg border p-0.5 text-sm"
          onKeyDown={(e) => {
            const dir = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
            if (dir) {
              const next = TABS[(TABS.indexOf(tab) + dir + TABS.length) % TABS.length]!;
              choose(next);
              document.getElementById(`vb-tab-${next}`)?.focus();
            }
          }}
        >
          {TABS.map((x) => (
            <button
              key={x}
              type="button"
              role="tab"
              id={`vb-tab-${x}`}
              aria-selected={x === tab}
              aria-controls="vb-panel"
              tabIndex={x === tab ? 0 : -1}
              onClick={() => choose(x)}
              className={cn(
                "rounded-md px-3 py-1",
                x === tab ? "bg-muted font-medium" : "text-muted-foreground",
              )}
            >
              {t(`vmBuilder.check.tab.${x}`)}
            </button>
          ))}
        </div>
        <div
          role="tabpanel"
          id="vb-panel"
          aria-labelledby={`vb-tab-${tab}`}
          className="flex min-h-0 flex-1 flex-col gap-2"
        >
          <CopyButton
            text={text}
            label={t(tab === "json" ? "vmBuilder.check.copyJson" : "vmBuilder.check.copyCli")}
          />
          <pre className="min-h-0 flex-1 overflow-auto rounded-md border bg-muted p-2 text-xs">
            {text}
          </pre>
        </div>
      </SheetContent>
    </Sheet>
  );
}
