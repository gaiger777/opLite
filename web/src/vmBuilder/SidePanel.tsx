// 오른쪽 판(계획 71 설계 §8.2·§8.4): 탭 셋 — 확인(서버 확인 상태 줄·문제 목록(누르면 그 칸)·서버가 고를 노드·CPU 모델·SDN
// 주소·폴더 할당량 막대), JSON·CLI(보낼 요청 그대로, 복사). 아래 "켠 채로 만들기"와 만들기 단추 — 브라우저 오류나 지금
// 값에 대한 서버 오류가 있으면 막히고 첫 까닭(누르면 그 칸)이 옆에 있다.
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { CopyButton } from "@/components/CopyButton";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { useDnd } from "@/dnd/DndProvider";
import { UsageBars } from "@/folders/UsageBars";
import { serverErrorMessage } from "@/i18n/errors";
import { readUserPref, writeUserPref } from "@/lib/prefs";
import { cn } from "@/lib/utils";
import { useBuilder } from "@/vmBuilder/context";
import { problemSlot, problemText } from "@/vmBuilder/labels";
import { cliText } from "@/vmBuilder/model/cli";
import { blocking, type Problem } from "@/vmBuilder/model/problems";
import { jsonText, requests } from "@/vmBuilder/model/requests";
import type { ServerCheck } from "@/vmBuilder/useServerCheck";

const TABS = ["check", "json", "cli"] as const;

/** 문제의 칸으로 초점(이름 칸이면 입력) */
export function focusProblem(p: Problem) {
  const at = problemSlot(p.at);
  const el = document.getElementById(at === "name" ? "vb-name" : `vb-${at}`);
  el?.scrollIntoView?.({ block: "nearest" });
  el?.focus();
}

export function SidePanel({
  server,
  failure,
  onCreate,
}: {
  server: ServerCheck;
  failure: string | null;
  onCreate(): void;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const { user } = usePrincipal();
  const [tab, setTab] = useState(() => {
    const saved = readUserPref(user, "vmBuilder.panel");
    return TABS.find((x) => x === saved) ?? "check";
  });
  const choose = (x: (typeof TABS)[number]) => {
    setTab(x);
    writeUserPref(user, "vmBuilder.panel", x);
  };
  const reqs = useMemo(() => requests(b.s, b.ctx), [b.s, b.ctx]);
  const json = jsonText(reqs, t("vmBuilder.json.masked"));
  const cli = cliText(b.s, b.ctx, t);
  const first = blocking(b.list);
  const check = server.check;
  const count = check?.problems.length ?? 0;
  // 서버 확인 결과가 바뀌면 알림 줄에 읽는다(초점은 옮기지 않음 — 설계 §11).
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
        : t(`vmBuilder.check.${server.status}`);

  return (
    <aside aria-label={t("vmBuilder.page.side")} className="vb-side flex min-w-0 flex-col gap-3">
      {/* 탭 셋(화살표 ←·→로 옮김) — 공용 탭 조각을 쓰면 첫 화면 묶음이 나뉘어 커진다. */}
      <div
        role="tablist"
        aria-label={t("vmBuilder.page.side")}
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
        className="flex flex-col gap-3"
      >
        {tab === "check" && (
          <>
            <p className="text-sm" data-stale={server.stale ? "" : undefined}>
              {status}
              {server.stale && server.status !== "off" && (
                <span className="block text-xs text-muted-foreground">
                  {t("vmBuilder.check.stale")}
                </span>
              )}
            </p>
            {server.status === "failed" && (
              // 404는 서버의 설명 그대로(볼 수 없는 폴더 등 — 번역 "더는 없음"보다 정확함)
              <p className="text-xs text-muted-foreground">
                {serverErrorMessage(t, server.error, ["not_found"])}
              </p>
            )}
            {server.status === "failed" && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="self-start"
                onClick={server.retry}
              >
                {t("vmBuilder.check.retry")}
              </Button>
            )}
            {check && !server.stale && (check.node || check.cpu_model) && (
              <ul className="text-sm text-muted-foreground">
                {check.node && <li>{t("vmBuilder.check.node", { node: check.node })}</li>}
                {check.cpu_model && <li>{t("vmBuilder.check.cpu", { model: check.cpu_model })}</li>}
                {check.machine && (
                  <li>{t("vmBuilder.check.machine", { machine: check.machine })}</li>
                )}
                {(check.ips ?? []).map(
                  (ips, i) =>
                    ips.length > 0 && (
                      <li key={i}>{t("vmBuilder.check.ips", { n: i + 1, ips: ips.join(", ") })}</li>
                    ),
                )}
              </ul>
            )}
            {b.list.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("vmBuilder.check.none")}</p>
            ) : (
              <ul className="flex flex-col gap-1" aria-label={t("vmBuilder.check.list")}>
                {b.list.map((p, i) => (
                  <li key={i}>
                    <button
                      type="button"
                      data-level={p.level}
                      className="w-full rounded-md px-1 py-0.5 text-left text-sm hover:bg-muted"
                      onClick={() => focusProblem(p)}
                    >
                      <span className="font-medium">{t(`vmBuilder.check.${p.level}`)}</span>{" "}
                      {problemText(t, p)}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {b.ctx.usage && <UsageBars view={b.ctx.usage} />}
          </>
        )}
        {tab !== "check" && (
          <>
            <CopyButton
              text={tab === "json" ? json : cli}
              label={t(tab === "json" ? "vmBuilder.check.copyJson" : "vmBuilder.check.copyCli")}
            />
            <pre className="vb-pre overflow-auto rounded-md border bg-muted p-2 text-xs">
              {tab === "json" ? json : cli}
            </pre>
          </>
        )}
      </div>
      <div className="vb-bar flex flex-col gap-2 border-t bg-background pt-3">
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
        <Button type="button" disabled={b.busy || first !== null} onClick={onCreate}>
          {b.busy ? t("vmBuilder.page.creating") : t("common.create")}
        </Button>
        {first && (
          <button
            type="button"
            className="text-left text-xs text-muted-foreground underline"
            onClick={() => focusProblem(first)}
          >
            {t("vmBuilder.page.blocked", { reason: problemText(t, first) })}
          </button>
        )}
      </div>
    </aside>
  );
}
