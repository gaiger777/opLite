import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import { newestFirst } from "@/backups/backups";
import { BackupTable } from "@/backups/BackupTable";
import { EmptyState } from "@/components/EmptyState";
import { PageHeader } from "@/components/PageHeader";

/** 모든 백업. VM을 지운 백업도 보인다. 백업은 VM 화면의 백업 탭에서 만든다. */
export function BackupsPage() {
  const { t } = useTranslation();
  const backups = useList("backup");
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="backups-title"
        title={t("nav.backups")}
        help="backups"
        intro={t("help.backups.intro")}
      />
      <BackupTable
        backups={newestFirst(backups.data ?? [])}
        query={backups}
        labelledBy="backups-title"
        showVm
        url
        emptyState={
          <EmptyState
            title={t("backups.empty")}
            description={t("help.backups.empty")}
            diagram="backupChain"
            help="backups.page"
          />
        }
      />
    </div>
  );
}
