import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import { usePrincipal } from "@/auth/AuthProvider";
import { StatusChip } from "@/components/StatusChip";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { alive, useRefs, type ResourceRef } from "@/lib/recent";
import { VmPhaseBadge } from "@/vms/VmPhaseBadge";
import { VmPowerButton } from "@/vms/VmPowerButton";

/** 즐겨찾기의 상세 주소. 풀은 상세 화면이 없어 목록 검색이다. */
function hrefOf(ref: ResourceRef): string {
  const name = encodeURIComponent(ref.name);
  switch (ref.kind) {
    case "vm":
      return `/vms/${name}`;
    case "node":
      return `/nodes/${name}`;
    default:
      return `/storage-pools?q=${name}`;
  }
}

/**
 * 즐겨찾기(★, 설계 §5.7): 이름, 상태 칩, VM이면 전원 단추. 휴대폰 당직 보기에서는 경보·상태 요약 다음에 온다(§5.9).
 * 지운 리소스는 뺀다.
 */
export function FavoritesWidget() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { pins } = useRefs(me.user);
  const vms = useList("vm");
  const nodes = useList("node");
  const pools = useList("storage-pool");
  const shown = alive(pins, { vm: vms.data, node: nodes.data, "storage-pool": pools.data });
  return (
    <WidgetCard title={t("favorites.title")}>
      {shown.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("favorites.empty")}</p>
      ) : (
        <ul aria-label={t("favorites.title")} className="flex flex-col divide-y">
          {shown.map((ref) => {
            const vm = ref.kind === "vm" ? vms.data?.find((v) => v.id === ref.id) : undefined;
            const node = ref.kind === "node" ? nodes.data?.find((n) => n.id === ref.id) : undefined;
            return (
              <li key={`${ref.kind}:${ref.id}`} className="flex min-h-12 items-center gap-3 py-1">
                <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-3 gap-y-1">
                  <Link
                    to={hrefOf(ref)}
                    className="truncate font-medium underline-offset-4 hover:underline"
                  >
                    {ref.name}
                  </Link>
                  <span className="text-xs text-muted-foreground">
                    {t(`favorites.kind.${ref.kind}`)}
                  </span>
                  {vm && <VmPhaseBadge vm={vm} />}
                  {node && (
                    <StatusChip tone={node.status.online ? "success" : "danger"}>
                      {t(node.status.online ? "dashboard.nodeOnline" : "dashboard.nodeOffline")}
                    </StatusChip>
                  )}
                </div>
                {vm && <VmPowerButton vm={vm} />}
              </li>
            );
          })}
        </ul>
      )}
    </WidgetCard>
  );
}
