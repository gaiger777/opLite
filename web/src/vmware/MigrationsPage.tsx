import { ArrowRightLeft, Plus } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";

import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { useCreateParam } from "@/lib/createParam";
import { formatTime, uuidTime } from "@/lib/format";
import { type Migration, type MigrationPhase, useMigrations, useSources } from "@/vmware/api";

const PHASE_TONE = {
  draft: "neutral",
  running: "info",
  paused: "warning",
  finished: "success",
  cancelled: "neutral",
} as const;

/** 계획 상태 칩 */
export function MigrationPhaseChip({ phase }: { phase: MigrationPhase }) {
  const { t } = useTranslation();
  return (
    <StatusChip tone={PHASE_TONE[phase]} busy={phase === "running"}>
      {t(`vmware.phase.${phase}`)}
    </StatusChip>
  );
}

/** 항목 상태별 수를 짧게(`3 syncing · 1 done`) */
export function CountsText({ migration }: { migration: Migration }) {
  const { t } = useTranslation();
  const counts = Object.entries(migration.status.counts ?? {}).filter(([, n]) => n > 0);
  if (counts.length === 0) {
    return <>—</>;
  }
  return (
    <span className="text-xs text-muted-foreground">
      {counts
        .map(([state, n]) => t("vmware.countOf", { count: n, state: t(`vmware.state.${state}`) }))
        .join(" · ")}
    </span>
  );
}

/**
 * 가상 머신 › 가져오기 › VMware 이전(계획 72 §13): 이전 계획 목록. 새 계획은 마법사(`/vmware-migrations/-new`), 계획을 열면
 * VM별 진행·전환 화면이다. 만들기는 루트 `vm.create`다. 늦게 읽는 조각이다.
 */
export function MigrationsPage() {
  const { t, i18n } = useTranslation();
  const perms = usePermissions();
  const navigate = useNavigate();
  const migrations = useMigrations(perms.reads("vmware-migration"));
  const sources = useSources(perms.reads("vmware-source"));
  const write = perms.has("vm.create");
  useCreateParam(() => void navigate("/vmware-migrations/-new"), write);
  const items = [...(migrations.data ?? [])].sort((a, b) => b.id.localeCompare(a.id));
  const sourceName = (id: string) => sources.data?.find((s) => s.id === id)?.name ?? "—";

  const newButton = write && (
    <Button asChild>
      <Link to="/vmware-migrations/-new">
        <Plus />
        {t("vmware.migrations.new")}
      </Link>
    </Button>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="vmware-migrations-title"
        title={t("nav.vmwareMigrations")}
        help="vmwareMigrations"
        intro={t("vmware.help.migrationsIntro")}
        actions={items.length > 0 && newButton}
      />
      {migrations.data === undefined ? (
        <ListPending error={migrations.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={ArrowRightLeft}
          title={t("vmware.migrations.empty")}
          description={t("vmware.help.migrationsEmpty")}
          help="vmwareMigrations.page"
          action={newButton}
        />
      ) : (
        <div className="overflow-x-auto">
          <Table aria-labelledby="vmware-migrations-title">
            <TableHeader>
              <TableRow>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("vmware.migration.source")}</TableHead>
                <TableHead>{t("vmware.migration.mode")}</TableHead>
                <TableHead>{t("vmware.migration.vms")}</TableHead>
                <TableHead>{t("common.state")}</TableHead>
                <TableHead>{t("vmware.migration.created")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((m) => {
                const total = Object.values(m.status.counts ?? {}).reduce((a, b) => a + b, 0);
                return (
                  <TableRow key={m.id}>
                    <TableCell className="font-medium">
                      <Link
                        to={`/vmware-migrations/${encodeURIComponent(m.name)}`}
                        className="underline-offset-4 hover:underline"
                      >
                        {m.name}
                      </Link>
                    </TableCell>
                    <TableCell>{sourceName(m.spec.source)}</TableCell>
                    <TableCell>{t(`vmware.mode.${m.spec.mode}`)}</TableCell>
                    <TableCell>
                      <span className="flex flex-col">
                        <span>{total}</span>
                        <CountsText migration={m} />
                      </span>
                    </TableCell>
                    <TableCell>
                      <MigrationPhaseChip phase={m.status.phase} />
                    </TableCell>
                    <TableCell>{formatTime(uuidTime(m.id), i18n.language)}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}
