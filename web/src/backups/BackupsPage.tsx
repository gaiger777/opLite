import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import { newestFirst } from "@/backups/backups";
import { BackupTable } from "@/backups/BackupTable";

/** 모든 백업. VM을 지운 백업도 보인다. 백업은 VM 화면의 백업 탭에서 만든다. */
export function BackupsPage() {
  const { t } = useTranslation();
  const backups = useList("backup");
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <h1 id="backups-title" className="text-2xl font-semibold">
          {t("nav.backups")}
        </h1>
        <p className="text-sm text-muted-foreground">{t("backups.pageHint")}</p>
      </div>
      <BackupTable
        backups={newestFirst(backups.data ?? [])}
        query={backups}
        labelledBy="backups-title"
        showVm
      />
    </div>
  );
}
