// 감사 항목을 사람 말로(계획 77-7, 진단 §2.11): 항목의 `action`은 "<METHOD> <라우트 템플릿>"이라 판이 섞여도(옛 노드의
// 항목도) 같은 열쇠다 — 서버에 새 칸 없이 라우트 → 문구 표(`auditOps.route`)로 "VM web 켜기"를 만든다. 템플릿의 `{…}`
// 자리는 실제 경로(`path`)의 같은 조각이다. 노드가 스스로 남긴 항목(`placement.move` …)은 `auditOps.system`(대상은
// 항목의 `target` — `{{kind}}`·`{{key}}`).
import type { TFunction } from "i18next";

import type { AuditEntry } from "@/api/types";

/** 라우트 템플릿의 `{이름}` 자리에 든 실제 경로 조각 */
export function routeParams(template: string, path: string): Record<string, string> {
  const want = template.split("/");
  const got = path.split("/");
  const out: Record<string, string> = {};
  want.forEach((seg, i) => {
    const m = /^\{(\w+)\}$/.exec(seg);
    const value = got[i];
    if (m && value !== undefined) {
      try {
        out[m[1]!] = decodeURIComponent(value);
      } catch {
        out[m[1]!] = value;
      }
    }
  });
  return out;
}

/**
 * 동작의 사람 말. 표에 없으면 null(부르는 쪽이 원문을 보인다). `names`는 id → 이름(콘솔처럼 id로 부른 VM — 이미 받은 목록).
 */
export function describeAudit(
  entry: AuditEntry,
  t: TFunction,
  names?: ReadonlyMap<string, string>,
): string | null {
  const space = entry.action.indexOf(" ");
  if (space < 0) {
    const target = entry.target;
    return (
      t(`auditOps.system.${entry.action}`, {
        defaultValue: "",
        ...(target && {
          key: names?.get(target.key) ?? target.key,
          kind: t(`tasks.drawer.kinds.${target.kind}`, { defaultValue: target.kind }),
        }),
        interpolation: { escapeValue: false },
      }) || null
    );
  }
  const template = entry.action.slice(space + 1);
  const params = routeParams(template, entry.path);
  if (params.key && names?.has(params.key)) {
    params.key = names.get(params.key)!;
  }
  const verb = params.action
    ? t(`auditOps.vmAction.${params.action}`, { defaultValue: params.action })
    : undefined;
  const phrase = t(`auditOps.route.${entry.action}`, {
    defaultValue: "",
    ...params,
    verb,
    interpolation: { escapeValue: false },
  });
  return phrase || null;
}

/**
 * 대상(`target` — 라우트의 첫 조각과 첫 자리 값, 노드 항목은 그 대상): 동작 이름에 이미 든 대상이면 null(같은 것을 두 번
 * 쓰지 않는다). id로 부른 것은 아는 이름으로.
 */
export function auditTarget(
  entry: AuditEntry,
  named: string | null,
  names?: ReadonlyMap<string, string>,
): string | null {
  const target = entry.target;
  if (!target) {
    return null;
  }
  const key = names?.get(target.key) ?? target.key;
  return named?.includes(key) ? null : `${target.kind}/${key}`;
}

/**
 * 미리 보기(`dry_run`)가 있는 라우트: 200이면 계획·확인만 한 것이다(계획 77-7 전의 노드는 결과 칸이 없어 상태로 가른다 —
 * 실제로 하면 202·201).
 */
const PREVIEW_ROUTES: ReadonlySet<string> = new Set([
  "POST /apply",
  "POST /history/restore",
  "POST /history/{kind}/{key}/restore",
  "POST /sdn/lockdown",
  "POST /network-migrations",
]);

export interface Outcome {
  text: string;
  failed: boolean;
  /** 202 — 일을 시작했다(결과는 Task에) */
  started?: boolean;
  /** 시작한 Task(서버가 적었으면) */
  task?: string;
}

/**
 * 결과: 성공·실패(+ 흔한 까닭), 미리 보기(바꾼 것 없음), 시작함(202 — 작업). 302로 끝나는 SSO 콜백은 서버가 적은 결과로
 * (옛 노드는 `detail`의 `error=`로) 실패를 가른다. 노드가 스스로 남긴 항목(상태 0)은 없음.
 */
export function auditOutcome(entry: AuditEntry, t: TFunction): Outcome | null {
  const { status } = entry;
  if (status === 0) {
    return null;
  }
  const ssoFailed =
    entry.action === "GET /auth/oidc/callback" && /(^| )error=/.test(entry.detail ?? "");
  if (entry.outcome === "failed" || ssoFailed) {
    return { text: t("auditOps.ui.failedRedirect"), failed: true };
  }
  if (status >= 400) {
    const why = t(`auditOps.ui.why.${status}`, { defaultValue: "" });
    return {
      text: why ? t("auditOps.ui.failedWhy", { status, why }) : t("auditOps.ui.failed", { status }),
      failed: true,
    };
  }
  if (entry.outcome === "dry_run" || (status === 200 && PREVIEW_ROUTES.has(entry.action))) {
    return { text: t("auditOps.ui.preview"), failed: false };
  }
  if (status === 202) {
    return {
      text: t("auditOps.ui.started"),
      failed: false,
      started: true,
      ...(entry.task && { task: entry.task }),
    };
  }
  return { text: t("auditOps.ui.ok"), failed: false };
}
