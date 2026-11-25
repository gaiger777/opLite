import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { localized, type MigrationItem, refreshVmware } from "@/vmware/api";

/**
 * 켠 뒤 되돌리기 확인(계획 72 §10.3): 대상 VM을 끄고 원본을 다시 켠다. 대상에서 쓴 데이터는 원본으로 가지 않으므로 대상 VM
 * 이름을 그대로 적어야 진행한다(서버도 같은 이름을 `confirm`으로 다시 본다 — 전환 뒤 VM 이름을 바꿨으면 지금 이름, 아니면
 * 400 `confirm_mismatch`).
 */
export function RevertDialog({ item, onClose }: { item: MigrationItem; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const vms = useList("vm");
  const name = vms.data?.find((v) => v.id === item.status.target_vm)?.name ?? item.spec.target_name;
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={t("vmware.revert.title", { name })}
      description={t("vmware.revert.description", {
        name,
        source: item.status.source?.vm.name ?? name,
      })}
      confirmLabel={t("vmware.revert.confirm")}
      typeToConfirm={name}
      impact={[t("vmware.revert.dataLost"), t("vmware.revert.keeps")]}
      onConfirm={async () => {
        try {
          await call(
            api.POST("/vmware-vms/{key}/revert", {
              params: { path: { key: item.id } },
              body: { confirm: name },
            }),
          );
        } catch (err) {
          throw localized(t, err);
        }
        await refreshVmware(queryClient);
        toast.success(t("vmware.revert.started", { name }));
      }}
    />
  );
}
