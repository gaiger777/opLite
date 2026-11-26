// 운영자 대시보드(계획 75 §4.1) — 홈이 늦게 읽는 칸: "물리 장애 n" 배지·링크 경보 카드, [지도 | 자세히 판], 노드별 할당·사용 표.
// 넓은 화면은 판이 지도·표 오른쪽 열(따라 내려옴), 좁은 화면은 지도 → 표(카드)이고 판은 고르면 서랍으로 열린다. 고른 것은
// 지도·표·판이 함께 쓴다(`MapSelection`). 섞인 판(409)이면 홈이 이 칸을 읽지 않는다(`DashboardPage`).
import { TriangleAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { CapacityTable } from "@/dashboard/CapacityTable";
import { DetailPanel } from "@/dashboard/DetailPanel";
import { HomeMap } from "@/dashboard/HomeMap";
import { LinkAlertsCard } from "@/dashboard/LinkAlertsCard";
import { focusOf, type MapSelection } from "@/dashboard/selection";
import { WidgetCard } from "@/dashboard/WidgetCard";
import { errorMessage } from "@/i18n/errors";
import { useCapacity, usePhysicalTopology } from "@/physical/api";
import { faultList } from "@/physical/chain";

export default function OperatorBoard({ phone }: { phone: boolean }) {
  const { t } = useTranslation();
  const capacity = useCapacity();
  const physical = usePhysicalTopology();
  const [selection, setSelection] = useState<MapSelection | null>(null);
  // 배지는 지금 값만 센다(오프라인 노드의 마지막으로 본 장애는 판의 목록에만).
  const faults = faultList(physical.data?.nodes ?? []).filter((f) => !f.stale);
  // 물리 관계를 한 번도 받지 못하고 실패했으면 "장애 없음"이 아니라 "모름"이다(리뷰 1). 다시 읽다 실패하면 마지막 값을 쓴다.
  const physicalUnknown = !physical.data && physical.isError;
  const select = (s: MapSelection | null) => setSelection(s);
  const first = faults[0];

  const badge = physicalUnknown ? (
    <Button
      variant="outline"
      size="sm"
      className="text-muted-foreground"
      onClick={() => void physical.refetch()}
    >
      <TriangleAlert aria-hidden />? {t("physical.badgeUnknown")}
    </Button>
  ) : (
    first && (
      <Button
        variant="outline"
        size="sm"
        className="border-danger text-danger-fg"
        onClick={() =>
          select(
            first.port !== undefined
              ? { kind: "port", node: first.id, port: first.port }
              : { kind: "node", id: first.id },
          )
        }
      >
        <TriangleAlert aria-hidden />
        {t("physical.badge", { count: faults.length })}
      </Button>
    )
  );

  const map = (
    <WidgetCard
      title={t("physical.map.title")}
      action={
        <Link to="/cluster/map" className="text-sm underline-offset-4 hover:underline">
          {t("physical.map.full")}
        </Link>
      }
    >
      <HomeMap selected={selection ?? undefined} onSelect={select} focus={focusOf(selection)} />
    </WidgetCard>
  );

  const waiting = (
    <p className="text-sm text-muted-foreground">
      {capacity.isError ? errorMessage(t, capacity.error) : t("common.loading")}
    </p>
  );

  const table = capacity.data ? (
    <CapacityTable
      capacity={capacity.data}
      physical={physicalUnknown ? undefined : (physical.data?.nodes ?? [])}
      selected={selection}
      onSelect={select}
      phone={phone}
    />
  ) : (
    <WidgetCard title={t("physical.table.title")}>{waiting}</WidgetCard>
  );

  const panel = capacity.data ? (
    <DetailPanel
      selection={selection}
      capacity={capacity.data}
      physical={physical.data}
      physicalError={physicalUnknown ? physical.error : undefined}
      onRetry={() => void physical.refetch()}
      onSelect={select}
    />
  ) : (
    waiting
  );

  // 배지도 카드도 없으면 빈 줄을 남기지 않는다.
  const top = (
    <div className="flex flex-wrap items-center gap-(--gap) empty:hidden">
      {badge}
      <LinkAlertsCard />
    </div>
  );

  if (phone) {
    return (
      <>
        {top}
        {map}
        {table}
        <Sheet open={selection !== null} onOpenChange={(open) => !open && select(null)}>
          <SheetContent className="overflow-y-auto p-4">
            <SheetTitle className="sr-only">{t("physical.panel.title")}</SheetTitle>
            {panel}
          </SheetContent>
        </Sheet>
      </>
    );
  }
  return (
    <>
      {top}
      <div className="grid grid-cols-1 gap-(--gap) lg:grid-cols-[minmax(0,1fr)_24rem]">
        <div className="flex min-w-0 flex-col gap-(--gap)">
          {map}
          {table}
        </div>
        <aside
          aria-label={t("physical.panel.title")}
          className="flex flex-col gap-3 self-start rounded-lg border bg-card p-4 lg:sticky lg:top-4 lg:max-h-[calc(100vh-2rem)] lg:overflow-y-auto"
        >
          {panel}
        </aside>
      </div>
    </>
  );
}
