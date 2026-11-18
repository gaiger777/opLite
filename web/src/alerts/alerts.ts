import { useQuery } from "@tanstack/react-query";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";

type Schemas = components["schemas"];
export type AlertView = Schemas["AlertView"];
export type AlertsResponse = Schemas["AlertsResponse"];
export type AlertSubject = Schemas["AlertSubject"];
export type AlertRule = Schemas["Resource_AlertRuleSpec_AlertRuleStatus"];
export type AlertRuleSpec = Schemas["AlertRuleSpec"];
export type AlertCondition = Schemas["AlertCondition"];
export type NotificationRecord = Schemas["NotificationRecord"];
export type NotificationTargetView = Schemas["NotificationTargetView"];
export type NotificationTargetRequest = Schemas["NotificationTargetRequest"];

/** 활성 경보의 캐시 키. 경보 화면·홈 위젯(나중에 종 서랍도)이 이 하나만 읽는다. */
export const ALERTS_KEY = ["alerts"] as const;
/** 활성 경보를 다시 읽는 주기 */
export const ALERTS_REFRESH_MS = 30_000;

/** 활성 경보(`GET /alerts`). 다른 곳에서 따로 폴링하지 않는다. */
export function useAlerts() {
  return useQuery({
    queryKey: ALERTS_KEY,
    queryFn: () => call(api.GET("/alerts")),
    refetchInterval: ALERTS_REFRESH_MS,
  });
}

/** 경보 규칙(이름 순). 누구나 읽는다. */
export function useAlertRules() {
  return useQuery({
    queryKey: ["alert-rules"],
    queryFn: () => call(api.GET("/alert-rules")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
  });
}

/** 알림 대상(이름 순). admin만 읽는다. */
export function useNotificationTargets(enabled = true) {
  return useQuery({
    queryKey: ["notification-targets"],
    queryFn: () => call(api.GET("/notification-targets")),
    select: (list) => [...list.items].sort((a, b) => a.name.localeCompare(b.name)),
    enabled,
  });
}

/** 보낸 알림 기록에서 볼 기간 */
export const HISTORY_PERIODS = ["day", "week"] as const;
export type HistoryPeriod = (typeof HISTORY_PERIODS)[number];
const PERIOD_MS: Record<HistoryPeriod, number> = { day: 86_400_000, week: 7 * 86_400_000 };

/** 보낸 알림 기록(새 것부터). 기간 시작은 읽을 때의 지금에서 센다. */
export function useAlertHistory(period: HistoryPeriod) {
  return useQuery({
    queryKey: ["alert-history", period],
    queryFn: () =>
      call(
        api.GET("/alerts/history", {
          params: { query: { since: String(Date.now() - PERIOD_MS[period]), limit: 500 } },
        }),
      ),
    refetchInterval: 60_000,
  });
}

/** 경보 대상 표시: 노드 경보는 노드 이름, 나머지는 `kind/name@node`(CLI와 같음) */
export function subjectText(subject: AlertSubject): string {
  if (subject.kind === "node") {
    return subject.name;
  }
  return `${subject.kind}/${subject.name}${subject.node ? `@${subject.node}` : ""}`;
}

/** 화면에 보이는 단계: 해제를 기다리는 `firing`은 `clearing` */
export function alertPhase(alert: AlertView): "firing" | "pending" | "clearing" {
  return alert.clear_since !== undefined ? "clearing" : alert.state;
}
