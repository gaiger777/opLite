// 오른쪽 고정 요약·검사(계획 77-5 — 진단 §4.3, EC2 요약 판·vSphere 호환성 판): 이름·폴더, 배치 칸(놓일 노드 — 자동이면 서버
// 미리 확인이 고른 노드 — 와 그 노드의 여유 막대: 이미 쓴 것 + 이 VM, 실제보다 많이 나눠 주면 경고), 펌웨어·보안 한 줄, 서버
// 확인 상태와 문제(누르면 그 칸 — 서랍에서만 고치는 칸이면 서랍을 열고 그 칸), 만든 뒤 켜기와 만들기 단추(늘 보임 — 막는 문제가
// 있으면 누를 때 보이고 그 칸으로). 고급 설정·요청 보기(JSON·CLI) 단추는 화면 머리줄에 있다. 1280px 밑에서는 이름·폴더가 그림
// 위로 가고 요약은 그림 아래, 만들기 줄은 페이지 아래 고정 띠다.
import { MapPin, ShieldCheck } from "lucide-react";
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
import { SlotBox } from "@/vmBuilder/Chassis";
import { useBuilder } from "@/vmBuilder/context";
import { partLabel, problemSlot, problemText, type Thing } from "@/vmBuilder/labels";
import { cliText } from "@/vmBuilder/model/cli";
import { parts } from "@/vmBuilder/model/parts";
import { inDrawer, type Problem } from "@/vmBuilder/model/problems";
import { jsonText, requests } from "@/vmBuilder/model/requests";
import { type Bar, nodeRoom, percent, targetNode } from "@/vmBuilder/model/room";
import { setName } from "@/vmBuilder/model/state";
import type { ServerCheck } from "@/vmBuilder/useServerCheck";

/**
 * 문제의 칸으로 초점(이름 칸이면 입력). 고급 서랍에서만 고치는 칸의 문제(보안 부팅·예약 스냅샷·복제·NUMA·부팅 순번 등)는
 * `openAdvanced`로 서랍을 열고 그 칸으로 간다(계획 77-5 리뷰 I4).
 */
export function focusProblem(p: Problem, openAdvanced?: (focusInvalid: boolean) => void) {
  if (openAdvanced && (p.field ? inDrawer(p.field) : p.at === "replication")) {
    openAdvanced(true);
    return;
  }
  const at = problemSlot(p.at);
  const el = document.getElementById(at === "name" ? "vb-name" : `vb-${at}`);
  el?.scrollIntoView?.({ block: "nearest" });
  el?.focus();
}

/** 요약에 바로 보이는 문제 수(나머지는 "n개 더" — 요약이 한 화면을 넘지 않게) */
const SHOWN = 3;

/** 이름과 폴더(넓은 화면은 요약 맨 위, 좁은 화면은 그림 위 — 고정 띠에 가리지 않게) */
export function NameFields() {
  const { t } = useTranslation();
  const b = useBuilder();
  const v = b.s.values;
  const nameProblem = b.showName ? b.list.find((p) => p.at === "name") : undefined;
  return (
    <div className="flex min-w-0 flex-col gap-2">
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
            onChange={(e) => b.set(setName(b.s, e.target.value))}
            onBlur={() => v.name !== "" && b.touch()}
            {...a11y}
          />
        )}
      </Field>
      <FolderSelect />
    </div>
  );
}

export function Summary({
  server,
  failure,
  blocked,
  onCreate,
  narrow,
}: {
  server: ServerCheck;
  failure: string | null;
  /** 만들기를 막은 첫 문제(만들기를 누른 뒤에만) */
  blocked: Problem | null;
  onCreate(): void;
  /** 1280px 밑: 이름·폴더는 그림 위, 만들기 띠는 페이지 아래(`CreateBar`를 페이지가 그림) */
  narrow: boolean;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const [all, setAll] = useState(false);
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
  const shown = all ? b.list : b.list.slice(0, SHOWN);
  const security = [
    v.firmware === "uefi" ? "UEFI" : "BIOS",
    v.secureBoot && t("vmBuilder.pic.secureBoot"),
    v.tpm && "TPM",
    v.guestAgent && t("vmBuilder.pic.agent"),
  ].filter(Boolean);

  return (
    <aside
      aria-label={t("vmBuilder.page.side")}
      className="vb-col vb-side flex min-w-0 flex-col gap-2.5 rounded-lg border bg-card p-3"
    >
      {!narrow && <NameFields />}
      <SlotBox
        id="placement"
        icon={<MapPin aria-hidden className="size-3.5" />}
        info={{ topic: "vmForm.placement", line: t("vmBuilder.slotInfo.placement") }}
      >
        <NodeSelect />
        <Room />
      </SlotBox>
      <div className="flex min-w-0 items-center gap-1.5 text-xs">
        <ShieldCheck aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
        <span className="sr-only">{t("vmBuilder.pic.security")}: </span>
        <span className="min-w-0 truncate" title={t("vmBuilder.pic.security")}>
          {security.join(" · ")}
        </span>
        <Button
          type="button"
          variant="link"
          size="sm"
          className="ml-auto h-auto shrink-0 p-0 text-xs"
          onClick={() => b.openAdvanced()}
        >
          {t("vmBuilder.pic.inAdvanced")}
        </Button>
      </div>
      {/* 1280px 이상은 검사 결과만 요약 안에서 스크롤한다(만들기 띠와 그림은 그대로 — 리뷰 I5). */}
      <section aria-labelledby="vb-checks" className="vb-checks flex flex-col gap-1">
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
          <ul
            className={cn("flex flex-col", all && "max-h-48 overflow-y-auto")}
            aria-label={t("vmBuilder.check.list")}
          >
            {shown.map((p, i) => (
              <li key={i}>
                <button
                  type="button"
                  data-level={p.level}
                  className="w-full rounded-md px-1 py-0.5 text-left text-xs break-words hover:bg-muted"
                  onClick={() => focusProblem(p, b.openAdvanced)}
                >
                  <span className="font-medium">{t(`vmBuilder.check.${p.level}`)}</span>{" "}
                  <Short text={problemText(t, p)} />
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
      {!narrow && <CreateBar failure={failure} blocked={blocked} onCreate={onCreate} />}
    </aside>
  );
}

/** 긴 글(서버의 영어 원문 오류 등)은 두 줄까지 — 넘으면 "자세히"로 펼친다(문제 단추 안이라 펼침 단추는 따로 두지 않고 title로). */
function Short({ text }: { text: string }) {
  return (
    <span className="vb-clamp" title={text}>
      {text}
    </span>
  );
}

/** 만들기 줄: 실패 알림, 만든 뒤 켜기, 만들기 단추, 막은 까닭(누르면 그 칸). 넓은 화면은 요약 맨 아래, 좁은 화면은 페이지 아래 고정 띠 */
export function CreateBar({
  failure,
  blocked,
  onCreate,
  fixed = false,
}: {
  failure: string | null;
  blocked: Problem | null;
  onCreate(): void;
  fixed?: boolean;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const v = b.s.values;
  return (
    <div
      className={cn(
        "vb-bar flex flex-col gap-2 border-t bg-card pt-3",
        fixed ? "vb-bar-page px-3" : "mt-auto",
      )}
    >
      {failure && (
        <p role="alert" className="text-sm text-destructive">
          {failure}
        </p>
      )}
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Checkbox
            id="vb-start"
            checked={v.start}
            disabled={b.busy}
            onCheckedChange={(c) => b.edit({ start: c === true })}
          />
          <Label htmlFor="vb-start">{t("vmForm.startAfter")}</Label>
        </div>
        <Button
          type="button"
          className={cn(!fixed && "flex-1")}
          disabled={b.busy}
          onClick={onCreate}
        >
          {b.busy ? t("vmBuilder.page.creating") : t("common.create")}
        </Button>
      </div>
      {blocked && (
        <button
          type="button"
          className="text-left text-xs text-destructive underline"
          onClick={() => focusProblem(blocked, b.openAdvanced)}
        >
          {t("vmBuilder.page.blocked", { reason: problemText(t, blocked) })}
        </button>
      )}
    </div>
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
    <div className="flex min-w-0 items-center gap-2">
      <Label htmlFor="vb-folder" className="shrink-0">
        {t("folders.folder")}
      </Label>
      <NativeSelect
        id="vb-folder"
        className="w-full min-w-0"
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
    <div className="flex min-w-0 flex-col">
      <Label htmlFor="vb-node" className="sr-only">
        {t("common.node")}
      </Label>
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
  if (at.node === null) {
    // 서버가 답했는데 노드가 없다: 막대 대신 까닭(어림 노드를 보이지 않는다)
    return (
      <p className="text-xs" data-level={at.other ? undefined : "warn"}>
        {at.other
          ? t("vmBuilder.room.other")
          : b.s.values.start
            ? t("vmBuilder.room.none")
            : t("vmBuilder.room.noneOff")}
      </p>
    );
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
          label={
            room.disk.others > 0
              ? t("vmBuilder.room.diskMore", { pool: room.disk.pool, count: room.disk.others })
              : t("vmBuilder.room.disk", { pool: room.disk.pool })
          }
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
export function CodeSheet({ onClose }: { onClose(): void }) {
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
