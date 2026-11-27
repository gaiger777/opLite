import { BellOff, Send } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useSearchParams } from "react-router";

import { AlertList } from "@/alerts/AlertList";
import { AlertRulesTab } from "@/alerts/AlertRulesTab";
import { HISTORY_PERIODS, useAlertHistory, useAlerts, type HistoryPeriod } from "@/alerts/alerts";
import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
import { TableMessage } from "@/components/TableMessage";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { formatTime } from "@/lib/format";
import { alertHref } from "@/tasks/target";

const TABS = ["active", "rules", "history"] as const;

/** 경보: 활성 경보, 규칙, 보낸 알림 기록(`?tab=active|rules|history`) */
export function AlertsPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const [params, setParams] = useSearchParams();
  // 규칙 목록은 루트 `alert.read`다(서버 `Root(AlertRead)`). 폴더에만 있으면 탭을 보이지 않는다.
  const tabs = TABS.filter((id) => id !== "rules" || perms.has("alert.read"));
  const tab = tabs.find((id) => id === params.get("tab")) ?? "active";
  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t("nav.alerts")} help="alerts" intro={t("help.alerts.intro")} />
      <Tabs
        value={tab}
        onValueChange={(value) =>
          setParams(
            (p) => {
              p.set("tab", value);
              return p;
            },
            { replace: true },
          )
        }
      >
        <TabsList>
          {tabs.map((id) => (
            <TabsTrigger key={id} value={id}>
              {t(`alerts.tab.${id}`)}
            </TabsTrigger>
          ))}
        </TabsList>
        <TabsContent value="active">
          <ActiveAlerts />
        </TabsContent>
        <TabsContent value="rules">{tab === "rules" && <AlertRulesTab />}</TabsContent>
        <TabsContent value="history">{tab === "history" && <NotificationHistory />}</TabsContent>
      </Tabs>
    </div>
  );
}

/** 답하지 않은 노드·옛 노드(그 노드의 경보·기록은 빠졌다) */
export function MissingNodes({
  unreachable,
  unsupported,
}: {
  unreachable: string[];
  unsupported: string[];
}) {
  const { t } = useTranslation();
  return (
    <>
      {unreachable.length > 0 && (
        <p className="text-sm text-muted-foreground">
          {t("alerts.unreachable", { nodes: unreachable.join(", ") })}
        </p>
      )}
      {unsupported.length > 0 && (
        <p className="text-sm text-muted-foreground">
          {t("alerts.unsupported", { nodes: unsupported.join(", ") })}
        </p>
      )}
    </>
  );
}

function ActiveAlerts() {
  const { t } = useTranslation();
  const alerts = useAlerts();
  if (!alerts.data) {
    return <ListPending error={alerts.error} />;
  }
  return (
    <div className="flex flex-col gap-2">
      {alerts.data.alerts.length === 0 ? (
        <EmptyState
          icon={BellOff}
          title={t("alerts.none")}
          description={t("help.alerts.empty")}
          help="alerts.page"
        />
      ) : (
        <AlertList
          alerts={alerts.data.alerts}
          label={t("alerts.tab.active")}
          subjectHref={alertHref}
        />
      )}
      <MissingNodes unreachable={alerts.data.unreachable} unsupported={alerts.data.unsupported} />
    </div>
  );
}

function NotificationHistory() {
  const { t, i18n } = useTranslation();
  const [period, setPeriod] = useState<HistoryPeriod>("day");
  const history = useAlertHistory(period);
  const records = history.data?.records ?? [];
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <label htmlFor="alert-history-period" className="text-sm">
          {t("alerts.history.period")}
        </label>
        <NativeSelect
          id="alert-history-period"
          value={period}
          onChange={(e) => setPeriod(e.target.value as HistoryPeriod)}
        >
          {HISTORY_PERIODS.map((p) => (
            <NativeSelectOption key={p} value={p}>
              {t(`alerts.history.periods.${p}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </div>
      {history.isSuccess && records.length === 0 ? (
        <EmptyState
          icon={Send}
          title={t("alerts.history.empty")}
          description={t("help.alerts.historyEmpty")}
          help="alerts.page"
        />
      ) : (
        <Table aria-label={t("alerts.tab.history")}>
          <TableHeader>
            <TableRow>
              <TableHead>{t("alerts.history.at")}</TableHead>
              <TableHead>{t("alerts.history.state")}</TableHead>
              <TableHead>{t("alerts.history.rule")}</TableHead>
              <TableHead>{t("alerts.history.subject")}</TableHead>
              <TableHead>{t("alerts.history.target")}</TableHead>
              <TableHead>{t("common.node")}</TableHead>
              <TableHead>{t("alerts.history.result")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {records.length === 0 && (
              <TableMessage colSpan={7} query={history} empty={t("alerts.history.empty")} />
            )}
            {records.map((r, i) => (
              <TableRow key={`${r.at}/${r.node}/${r.target}/${i}`}>
                <TableCell className="whitespace-nowrap">
                  {formatTime(new Date(r.at), i18n.language)}
                </TableCell>
                <TableCell>{t(`alerts.history.states.${r.state}`)}</TableCell>
                <TableCell>{r.rule ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">{r.subject ?? "—"}</TableCell>
                <TableCell>{r.target ?? "—"}</TableCell>
                <TableCell>{r.node}</TableCell>
                <TableCell>
                  {r.ok ? (
                    <StatusChip tone="success">{t("alerts.history.sent")}</StatusChip>
                  ) : (
                    <>
                      <StatusChip tone="danger">{t("alerts.history.failed")}</StatusChip>
                      {r.error && (
                        <span className="block text-xs text-muted-foreground">{r.error}</span>
                      )}
                    </>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {history.data?.truncated && (
        <p className="text-sm text-muted-foreground">{t("alerts.history.truncated")}</p>
      )}
      {history.data && (
        <MissingNodes
          unreachable={history.data.unreachable}
          unsupported={history.data.unsupported}
        />
      )}
    </div>
  );
}
