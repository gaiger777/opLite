// "목록으로 보기"(계획 70 §10.3): 같은 모델의 나무 목록 — 판 → 노드(상태·부하 글) → VM(상태). `role="tree"`가 아닌 평범한
// 중첩 목록과 머리글이라 화면 읽기 프로그램이 이미 잘 읽고 인쇄에도 맞다. 노드 단위 정렬·숫자 비교는 노드 표가 맡는다.
// 그림의 화살표(진행 중 마이그레이션·켠 추천)는 "마이그레이션과 추천" 절의 단추 — 누르면 같은 화살표 서랍(Task 줄·"적용").
// 적용·끝남으로 그 단추가 사라지면 초점은 같은 VM의 새 화살표 → 이 절의 머리글 → 출발 노드로 돌아온다.
import { useTranslation } from "react-i18next";

import type { MapLink, MapScene, NodeItem, SlabItem } from "@/clusterMap/layout";
import type { ClusterModel, MapNode, MapVm } from "@/clusterMap/model";
import { readingOrder } from "@/clusterMap/keys";
import { Button } from "@/components/ui/button";

const L = "clusterMap.label";

function nodeState(t: (k: string, v?: Record<string, unknown>) => string, n: MapNode): string {
  const parts = [t(n.online ? `${L}.online` : `${L}.offline`)];
  if (n.maintenance) {
    parts.push(t(`${L}.maintenance`));
  }
  if (n.frozen) {
    parts.push(t(`${L}.frozen`));
  }
  if (n.leader) {
    parts.push(t(`${L}.leader`));
  }
  if (!n.witness) {
    parts.push(
      n.load
        ? t(`${L}.load`, { cpu: Math.round(n.load.cpu), mem: Math.round(n.load.mem) })
        : t(`${L}.loadUnknown`),
    );
  }
  if (n.alerts > 0) {
    parts.push(t(`${L}.alerts`, { count: n.alerts }));
  }
  return parts.join(", ");
}

export function MapList({
  model,
  scene,
  links,
  labels,
  slabName,
  onOpen,
}: {
  model: ClusterModel;
  scene: MapScene;
  /** 그림에 그린 화살표(추천은 레이어를 켰을 때만) */
  links: readonly MapLink[];
  /** 열쇠 → 화면 읽기 이름(화살표 글) */
  labels: ReadonlyMap<string, string>;
  slabName(slab: SlabItem): string;
  onOpen(key: string, owner: Element | null): void;
}) {
  const { t } = useTranslation();
  const nodes = new Map(model.nodes.map((n) => [n.id, n]));
  const byNode = new Map<string | null, MapVm[]>();
  for (const vm of model.vms) {
    byNode.set(vm.node, [...(byNode.get(vm.node) ?? []), vm]);
  }
  const order = readingOrder(scene);

  const vmItems = (vms: readonly MapVm[]) =>
    vms.length === 0 ? null : (
      <ul className="ml-5 flex flex-col gap-0.5">
        {vms.map((vm) => (
          <li key={vm.id} className="flex flex-wrap items-baseline gap-x-2">
            <Button
              variant="link"
              size="sm"
              className="h-auto p-0"
              data-key={`vm:${vm.id}`}
              onClick={(e) => onOpen(`vm:${vm.id}`, e.currentTarget)}
            >
              {vm.name}
            </Button>
            <span className="text-xs text-muted-foreground">
              {t(`vms.phase.${vm.phase}`)}
              {vm.migration &&
                ` · ${t(`${L}.moving`, { node: nodes.get(vm.migration.to)?.name ?? "" })}`}
            </span>
          </li>
        ))}
      </ul>
    );

  const nodeItem = (item: NodeItem) => {
    const n = nodes.get(item.id);
    if (!n) {
      return null;
    }
    return (
      <li key={item.key} className="flex flex-col gap-0.5">
        <div className="flex flex-wrap items-baseline gap-x-2">
          <Button
            variant="link"
            size="sm"
            className="h-auto p-0 font-medium"
            data-key={item.key}
            onClick={(e) => onOpen(item.key, e.currentTarget)}
          >
            {n.witness ? t(`${L}.witness`, { name: n.name }) : n.name}
          </Button>
          <span className="text-xs text-muted-foreground">{nodeState(t, n)}</span>
        </div>
        {vmItems(byNode.get(n.id) ?? [])}
      </li>
    );
  };

  return (
    <section
      aria-label={t("clusterMap.listLabel")}
      className="flex flex-col gap-4"
      data-map-list=""
    >
      {scene.slabs
        .filter((s) => s.kind !== "unplaced")
        .map((slab) => (
          <div key={slab.key} className="flex flex-col gap-1">
            <h2 className="text-sm font-semibold">{slabName(slab)}</h2>
            <ul className="flex flex-col gap-2">
              {order.filter((n) => n.slab === slab.key).map(nodeItem)}
            </ul>
          </div>
        ))}
      {links.length > 0 && (
        <div className="flex flex-col gap-1">
          {/* 목록에서 연 화살표가 사라지면 초점이 여기로 온다(ClusterMapPage `resolveReturn`) */}
          <h2 className="text-sm font-semibold" tabIndex={-1} data-moves="">
            {t("clusterMap.moves")}
          </h2>
          <ul className="flex flex-col gap-0.5">
            {links.map((link) => (
              <li key={link.key}>
                <Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0 text-left whitespace-normal"
                  data-key={link.key}
                  onClick={(e) => onOpen(link.key, e.currentTarget)}
                >
                  {labels.get(link.key) ?? link.vmName}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {(byNode.get(null) ?? []).length > 0 && (
        <div className="flex flex-col gap-1">
          <h2 className="text-sm font-semibold">{t("clusterMap.unplaced")}</h2>
          {vmItems(byNode.get(null) ?? [])}
        </div>
      )}
      {model.nodes.length > 0 && model.vms.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("clusterMap.label.noVms")}</p>
      )}
    </section>
  );
}
