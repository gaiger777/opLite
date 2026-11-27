// 감사 항목을 사람 말로(계획 77-7, 진단 §2.11): 항목의 `action`은 "<METHOD> <라우트 템플릿>"이라 판이 섞여도(옛 노드의
// 항목도) 같은 열쇠다 — 서버에 새 칸 없이 라우트 → 문구 표(`auditOps.route`)로 "VM web 켜기"를 만든다. 템플릿의 `{…}`
// 자리는 실제 경로(`path`)의 같은 조각이다. 노드가 스스로 남긴 항목(`placement.move` …)은 `auditOps.system`.
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

/** 동작의 사람 말. 표에 없으면 null(부르는 쪽이 원문을 보인다). */
export function describeAudit(entry: AuditEntry, t: TFunction): string | null {
  const space = entry.action.indexOf(" ");
  if (space < 0) {
    const key = `auditOps.system.${entry.action}`;
    return t(key, { defaultValue: "" }) || null;
  }
  const template = entry.action.slice(space + 1);
  const params = routeParams(template, entry.path);
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

/** 결과: 성공·실패(+ 흔한 까닭). 노드가 스스로 남긴 항목(상태 0)은 없음. */
export function auditOutcome(
  status: number,
  t: TFunction,
): { text: string; failed: boolean } | null {
  if (status === 0) {
    return null;
  }
  if (status < 400) {
    return { text: t("auditOps.ui.ok"), failed: false };
  }
  const why = t(`auditOps.ui.why.${status}`, { defaultValue: "" });
  return {
    text: why ? t("auditOps.ui.failedWhy", { status, why }) : t("auditOps.ui.failed", { status }),
    failed: true,
  };
}
