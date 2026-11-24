import { Server, SlidersHorizontal } from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useSearchParams } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import { COALESCE_MS, type ClusterMapData, useClusterMapData } from "@/clusterMap/data";
import type { Focus } from "@/clusterMap/keys";
import { mapLabels } from "@/clusterMap/labels";
import {
  type GroupBy,
  layoutCluster,
  type MapLink,
  type MapScene as Scene,
  type SlabItem,
} from "@/clusterMap/layout";
import { Legend } from "@/clusterMap/Legend";
import type { Lod } from "@/clusterMap/lod";
import { NodeCard, TextCard, VmCard } from "@/clusterMap/MapCard";
import { MapList } from "@/clusterMap/MapList";
import { MapScene, type MapOpen } from "@/clusterMap/MapScene";
import type { DragLogic, SheetTarget } from "@/clusterMap/MapSheet";
import { type ClusterModel, labelKeys, toModel } from "@/clusterMap/model";
import { MAP_LAYERS, type MapLayer } from "@/clusterMap/paths";
import { useMapDrag } from "@/clusterMap/useMapDrag";
import { useIsPhone } from "@/components/data-table/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { DndProvider } from "@/dnd/DndProvider";
import { readUserPref, writeUserPref } from "@/lib/prefs";
import { useNodeActions } from "@/nodes/NodeActions";

/** 서랍(미리 보기와 그 자리의 동작)은 처음 누를 때 받는다 — 그림을 그리는 데 필요 없다(지도 조각을 가볍게). */
const MapSheet = lazy(() => import("@/clusterMap/MapSheet").then((m) => ({ default: m.MapSheet })));

/** 이만큼 넘으면 판 하나만 보기를 권한다(§9.2) */
const BIG_NODES = 200;

type Mode = "picture" | "list";

interface Filter {
  /** 판 하나만(판 열쇠), 빈 글이면 모두 */
  slab: string;
  hideStopped: boolean;
  problems: boolean;
}

const NO_FILTER: Filter = { slab: "", hideStopped: false, problems: false };

/** 묶기 열쇠 ↔ 기억 글(`zone`·`rack`·`none`·`label:<키>`) */
export function groupText(by: GroupBy): string {
  return by.kind === "label" ? `label:${by.key}` : by.kind;
}

export function parseGroup(text: string | null): GroupBy {
  if (text === "rack" || text === "none" || text === "zone") {
    return { kind: text };
  }
  if (text?.startsWith("label:") && text.length > 6) {
    return { kind: "label", key: text.slice(6) };
  }
  return { kind: "zone" };
}

export function parseLayers(text: string | null): MapLayer[] {
  if (text === null) {
    return ["load"];
  }
  return MAP_LAYERS.filter((l) => text.split(",").includes(l));
}

function parseFilter(text: string | null): Filter {
  try {
    const v = JSON.parse(text ?? "null") as Partial<Filter> | null;
    return {
      slab: typeof v?.slab === "string" ? v.slab : "",
      hideStopped: v?.hideStopped === true,
      problems: v?.problems === true,
    };
  } catch {
    return NO_FILTER;
  }
}

/** 거르기: 꺼진 VM 감추기, 판 하나만(그 판의 노드와 그 위 VM만) */
function filterModel(model: ClusterModel, scene: Scene, filter: Filter): ClusterModel {
  let { nodes, vms } = model;
  if (filter.slab && scene.slabs.some((s) => s.key === filter.slab && s.kind !== "unplaced")) {
    const keep = new Set(scene.nodes.filter((n) => n.slab === filter.slab).map((n) => n.id));
    nodes = nodes.filter((n) => keep.has(n.id));
    vms = vms.filter((vm) => vm.node !== null && keep.has(vm.node));
  }
  if (filter.hideStopped) {
    vms = vms.filter((vm) => vm.phase !== "stopped");
  }
  if (nodes === model.nodes && vms === model.vms) {
    return model;
  }
  const ids = new Set(nodes.map((n) => n.id));
  return {
    ...model,
    nodes,
    vms,
    moves: model.moves.filter((m) => ids.has(m.from) && ids.has(m.to)),
  };
}

/** 주소의 `?focus=node:<이름>`·`?focus=vm:<이름>` → 그림의 초점 */
function focusFromUrl(value: string | null, model: ClusterModel, scene: Scene): Focus | null {
  if (!value) {
    return null;
  }
  const [kind, ...rest] = value.split(":");
  const name = rest.join(":");
  if (kind === "node") {
    const n = model.nodes.find((x) => x.name === name);
    return n && scene.byKey.has(`node:${n.id}`) ? { kind: "node", key: `node:${n.id}` } : null;
  }
  if (kind === "vm") {
    const vm = model.vms.find((x) => x.name === name);
    const tile = vm ? scene.tiles.get(`vm:${vm.id}`) : undefined;
    return tile ? { kind: "tile", node: tile.node.key, key: tile.cell.key } : null;
  }
  return null;
}

/**
 * 인프라 › 노드의 보기 "지도"(계획 70): 노드를 장애 영역 판 위의 2.5D 블록으로, VM을 그 위 타일로 그린다(3D처럼 보이는
 * 2D SVG — WebGL 없음). 가리키면 카드, 누르면 지금 있는 미리 보기 서랍과 그 자리의 동작. 레이어(부하·스토리지·네트워크·
 * HA·추천)·묶기·거르기·그림 | 목록은 사용자별로 기억한다. 늦게 읽는 조각이다.
 */
export function ClusterMapPage() {
  const { t } = useTranslation();
  const { user } = usePrincipal();
  const navigate = useNavigate();
  const [search] = useSearchParams();
  const phone = useIsPhone();
  const data = useClusterMapData();
  const actions = useNodeActions();
  const keysId = useId();
  // 켜진 VM 타일을 노드로 끌거나 `m`으로 옮기기 대화상자를 연다(계획 71 §10). 판정 함수는 서랍 조각과 함께 그림을
  // 그린 뒤 미리 받는다(첫 누르기의 서랍도 빨라진다).
  const [logic, setLogic] = useState<DragLogic | null>(null);
  useEffect(() => {
    let live = true;
    void import("@/clusterMap/MapSheet").then((m) => live && setLogic(m.dragLogic));
    return () => {
      live = false;
    };
  }, []);
  const drag = useMapDrag(data.nodes.data, data.vms.data, data.placement.data, logic);

  const pref = (key: string) => readUserPref(user, `clusterMap.${key}`);
  const [mode, setModeState] = useState<Mode>(() => (pref("mode") === "list" ? "list" : "picture"));
  const [group, setGroupState] = useState<GroupBy>(() => parseGroup(pref("group")));
  const [layers, setLayersState] = useState<MapLayer[]>(() => parseLayers(pref("layers")));
  const [filter, setFilterState] = useState<Filter>(() => parseFilter(pref("filter")));
  const save = (key: string, value: string) => writeUserPref(user, `clusterMap.${key}`, value);
  const setMode = (m: Mode) => {
    setModeState(m);
    save("mode", m);
  };
  const setGroup = (g: GroupBy) => {
    setGroupState(g);
    save("group", groupText(g));
    // 판 열쇠가 묶기에 따라 바뀐다
    setFilter({ ...filter, slab: "" });
  };
  const setLayers = (next: MapLayer[]) => {
    setLayersState(next);
    save("layers", next.join(","));
  };
  const setFilter = (f: Filter) => {
    setFilterState(f);
    save("filter", JSON.stringify(f));
  };

  const full = useMemo(() => (data.input ? toModel(data.input) : null), [data.input]);
  // 기억한 묶기 레이블·판이 이제 없으면(레이블을 지움) 기본으로 되돌리고 기억도 고친다 — 고르기 상자가 없는 값을 갖지 않게
  if (full && group.kind === "label" && !labelKeys(full.nodes).includes(group.key)) {
    setGroupState({ kind: "zone" });
    save("group", "zone");
  }
  const fullScene = useMemo(() => (full ? layoutCluster(full, { group }) : null), [full, group]);
  if (
    fullScene &&
    filter.slab &&
    !fullScene.slabs.some((s) => s.key === filter.slab && s.kind !== "unplaced")
  ) {
    setFilter({ ...filter, slab: "" });
  }
  const model = useMemo(
    () => (full && fullScene ? filterModel(full, fullScene, filter) : null),
    [full, fullScene, filter],
  );
  const scene = useMemo(
    () => (model ? (model === full ? fullScene : layoutCluster(model, { group })) : null),
    [model, full, fullScene, group],
  );
  const labels = useMemo(
    () => (model && scene ? mapLabels(t, model, scene) : new Map<string, string>()),
    [t, model, scene],
  );
  const nodesByKey = useMemo(
    () => new Map((model?.nodes ?? []).map((n) => [`node:${n.id}`, n])),
    [model],
  );
  const vmNames = useMemo(() => new Map((model?.vms ?? []).map((v) => [v.id, v.name])), [model]);

  // 추천 레이어: `/placement`를 못 읽거나 균형 조정기가 꺼졌으면 막는다
  const recsReason = !full
    ? null
    : !full.placement
      ? "clusterMap.recsUnavailable"
      : full.balancer === "off"
        ? "clusterMap.recsOff"
        : null;
  const shown = layers.filter((l) => l !== "recommendations" || recsReason === null);
  // 그림에 그리는 화살표 — 목록 보기와 노드 서랍의 "마이그레이션과 추천"도 같은 것(키보드·화면 읽기로 화살표 서랍에 닿는 길)
  const showRecs = shown.includes("recommendations");
  const links = (scene?.links ?? []).filter((l) => l.kind === "migration" || showRecs);

  const [sheet, setSheet] = useState<SheetTarget | null>(null);
  // 한 번 연 서랍은 닫아도 두어 닫힘·초점 돌려주기가 그대로 돈다
  const [opened, setOpened] = useState(false);
  if (sheet && !opened) {
    setOpened(true);
  }
  const returnFocus = useRef<HTMLElement | null>(null);
  // 목록에서 연 화살표는 단추가 아니라 화살표를 기억한다 — 적용하거나 마이그레이션이 끝나면 그 단추가 사라진다
  const returnLink = useRef<MapLink | null>(null);
  const [lod, setLod] = useState<Lod | null>(null);
  const [settings, setSettings] = useState(false);
  const focus = useRef<Focus | null>(null);
  const [focusRequest, setFocusRequest] = useState<{
    focus: Focus;
    seq: number;
    dom: boolean;
  } | null>(null);

  // 주소로 가리키기: 처음 그릴 때 그 원소로 맞추고 초점을 준다(서랍은 열지 않음)
  const focusParam = search.get("focus");
  const [applied, setApplied] = useState<string | null>(null);
  if (model && scene && focusParam && applied !== focusParam) {
    setApplied(focusParam);
    const f = focusFromUrl(focusParam, model, scene);
    if (f) {
      setFocusRequest((r) => ({ focus: f, seq: (r?.seq ?? 0) + 1, dom: true }));
    }
  }

  const slabName = useCallback(
    (slab: SlabItem) => {
      switch (slab.kind) {
        case "group":
          return t("clusterMap.slabName", { name: slab.name, count: slab.count });
        case "nogroup":
          return t("clusterMap.slabName", { name: t("clusterMap.noGroup"), count: slab.count });
        case "all":
          return t("clusterMap.slabName", {
            name: slab.name ?? t("clusterMap.cluster"),
            count: slab.count,
          });
        case "unplaced":
          return t("clusterMap.unplacedName", { count: slab.count });
      }
    },
    [t],
  );

  const targetOf = (key: string): SheetTarget | null => {
    const [kind, ...rest] = key.split(":");
    const id = rest.join(":");
    switch (kind) {
      case "node":
        return { kind: "node", id };
      case "unplaced":
        return { kind: "vms", node: null };
      case "vm":
      case "ghost":
        return { kind: "vm", id };
      case "more":
      case "hidden":
        return { kind: "vms", node: id === "unplaced" ? null : id.replace(/^node:/, "") };
      case "move":
      case "rec":
        return { kind: "move", key };
      default:
        return null;
    }
  };

  const onOpen = (open: MapOpen) => {
    returnFocus.current = open.owner as HTMLElement | null;
    returnLink.current = null;
    setSheet(targetOf(open.key));
  };
  /**
   * 서랍이 닫힐 때: 목록에서 연 화살표의 단추가 사라졌으면 같은 VM의 새 화살표, 없으면 "마이그레이션과 추천" 머리글,
   * 그것도 없으면 출발 노드로 초점을 돌린다(`<body>`로 떨어뜨리지 않게).
   */
  const resolveReturn = () => {
    const link = returnLink.current;
    const list = document.querySelector<HTMLElement>("[data-map-list]");
    if (!link || !list || returnFocus.current?.isConnected) {
      return;
    }
    const byKey = (key: string) =>
      [...list.querySelectorAll<HTMLElement>("[data-key]")].find((el) => el.dataset.key === key);
    returnFocus.current =
      byKey(link.key) ??
      byKey(`move:${link.vm}`) ??
      list.querySelector<HTMLElement>("[data-moves]") ??
      byKey(link.ends[0]) ??
      null;
  };
  // 닫은 바로 뒤 모은 갱신(`COALESCE_MS`)이 늦게 와서 돌려준 단추를 없애도 초점을 `<body>`에 두지 않는다 —
  // 진행 중 마이그레이션이 있으면 갱신이 늘 모여 오므로 "적용"의 추천 단추는 닫은 뒤에 사라지기 쉽다.
  const returnUntil = useRef(0);
  const beforeReturn = () => {
    returnUntil.current = returnLink.current ? Date.now() + 2 * COALESCE_MS : 0;
    resolveReturn();
  };
  useEffect(() => {
    const lost = returnFocus.current && !returnFocus.current.isConnected;
    if (lost && Date.now() < returnUntil.current && document.activeElement === document.body) {
      resolveReturn();
      returnFocus.current?.focus();
    }
  });

  const onDetail = (open: MapOpen) => {
    const target = targetOf(open.key);
    if (target?.kind === "node") {
      const n = full?.nodes.find((x) => x.id === target.id);
      if (n) {
        void navigate(`/nodes/${encodeURIComponent(n.name)}`);
      }
    } else if (target?.kind === "vm") {
      const vm = full?.vms.find((x) => x.id === target.id);
      if (vm) {
        void navigate(`/vms/${encodeURIComponent(vm.name)}`);
      }
    } else if (target) {
      // 상세 화면이 없는 것("+N"·화살표)은 서랍 — 닫으면 이 원소로 초점이 돌아오게
      returnFocus.current = open.owner as HTMLElement | null;
      setSheet(target);
    }
  };

  const renderCard = (key: string) => {
    const node = nodesByKey.get(key);
    const item = scene?.byKey.get(key);
    if (node && item) {
      return <NodeCard node={node} item={item} />;
    }
    const tile = scene?.tiles.get(key);
    if (tile?.cell.vm && (tile.cell.kind === "vm" || tile.cell.kind === "ghost")) {
      const vm = data.vms.data?.find((v) => v.id === tile.cell.vm);
      if (vm) {
        return (
          <VmCard
            vm={vm}
            nodeName={(id) => full?.nodes.find((n) => n.id === id)?.name ?? "—"}
            progress={
              full?.moves.find((m) => m.vm === vm.id && m.kind === "migration")?.progress ?? null
            }
          />
        );
      }
    }
    const text = labels.get(key);
    return text ? <TextCard text={text} /> : null;
  };

  const switchMode = (m: Mode) => {
    if (m === "picture" && focus.current) {
      // 목록에서 돌아오면 그림의 초점(탭 차례 원소)을 지킨다 — 화면 초점은 전환 단추에 둔다
      setFocusRequest((r) => ({ focus: focus.current!, seq: (r?.seq ?? 0) + 1, dom: false }));
    }
    setMode(m);
  };

  const groupKeys = full ? labelKeys(full.nodes).filter((k) => k !== "zone" && k !== "rack") : [];
  const controls = (
    <>
      <Field id="cluster-map-group" label={t("clusterMap.group")}>
        <NativeSelect
          id="cluster-map-group"
          value={groupText(group)}
          onChange={(e) => setGroup(parseGroup(e.target.value))}
        >
          <NativeSelectOption value="zone">{t("clusterMap.groupZone")}</NativeSelectOption>
          <NativeSelectOption value="rack">{t("clusterMap.groupRack")}</NativeSelectOption>
          {groupKeys.map((k) => (
            <NativeSelectOption key={k} value={`label:${k}`}>
              {t("clusterMap.groupLabel", { key: k })}
            </NativeSelectOption>
          ))}
          <NativeSelectOption value="none">{t("clusterMap.groupNone")}</NativeSelectOption>
        </NativeSelect>
      </Field>
      {fullScene && fullScene.slabs.filter((s) => s.kind !== "unplaced").length > 1 && (
        <Field id="cluster-map-slab" label={t("clusterMap.slab")}>
          <NativeSelect
            id="cluster-map-slab"
            value={filter.slab}
            onChange={(e) => setFilter({ ...filter, slab: e.target.value })}
          >
            <NativeSelectOption value="">{t("clusterMap.allSlabs")}</NativeSelectOption>
            {fullScene.slabs
              .filter((s) => s.kind !== "unplaced")
              .map((s) => (
                <NativeSelectOption key={s.key} value={s.key}>
                  {slabName(s)}
                </NativeSelectOption>
              ))}
          </NativeSelect>
        </Field>
      )}
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("clusterMap.layersLabel")}</legend>
        <div className="flex flex-wrap gap-x-4 gap-y-2">
          {MAP_LAYERS.map((layer) => {
            const id = `cluster-map-layer-${layer}`;
            const blocked = layer === "recommendations" ? recsReason : null;
            return (
              <div key={layer} className="flex items-center gap-2">
                <Checkbox
                  id={id}
                  checked={shown.includes(layer)}
                  disabled={blocked !== null}
                  aria-describedby={blocked ? `${id}-why` : undefined}
                  onCheckedChange={(on) =>
                    setLayers(
                      on === true
                        ? MAP_LAYERS.filter((l) => l === layer || layers.includes(l))
                        : layers.filter((l) => l !== layer),
                    )
                  }
                />
                <Label htmlFor={id}>{t(`clusterMap.layer.${layer}`)}</Label>
                {blocked && (
                  <span id={`${id}-why`} className="text-xs text-muted-foreground">
                    {t(blocked)}
                  </span>
                )}
              </div>
            );
          })}
        </div>
      </fieldset>
      <div className="flex flex-wrap gap-x-4 gap-y-2">
        <div className="flex items-center gap-2">
          <Checkbox
            id="cluster-map-hide-stopped"
            checked={filter.hideStopped}
            onCheckedChange={(on) => setFilter({ ...filter, hideStopped: on === true })}
          />
          <Label htmlFor="cluster-map-hide-stopped">{t("clusterMap.hideStopped")}</Label>
        </div>
        <div className="flex items-center gap-2">
          <Checkbox
            id="cluster-map-problems"
            checked={filter.problems}
            onCheckedChange={(on) => setFilter({ ...filter, problems: on === true })}
          />
          <Label htmlFor="cluster-map-problems">{t("clusterMap.problems")}</Label>
        </div>
      </div>
    </>
  );

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="cluster-map-title"
        title={t("nav.clusterMap")}
        description={t("clusterMap.hint")}
      />
      <div className="flex flex-wrap items-end gap-x-6 gap-y-3">
        <ToggleGroup
          type="single"
          variant="outline"
          value={mode}
          aria-label={t("topology.mode")}
          onValueChange={(value) => {
            if (value === "picture" || value === "list") {
              switchMode(value);
            }
          }}
        >
          <ToggleGroupItem value="picture">{t("topology.picture")}</ToggleGroupItem>
          <ToggleGroupItem value="list">{t("topology.list")}</ToggleGroupItem>
        </ToggleGroup>
        {phone ? (
          <Button variant="outline" onClick={() => setSettings(true)}>
            <SlidersHorizontal />
            {t("clusterMap.settings")}
          </Button>
        ) : (
          controls
        )}
      </div>
      {mode === "picture" && lod && (
        <p className="text-xs text-muted-foreground" role="status">
          {t(`clusterMap.lod.${lod}`)}
        </p>
      )}
      {full && full.nodes.length > BIG_NODES && !filter.slab && (
        <p role="status" className="rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg">
          {t("clusterMap.big")}
        </p>
      )}
      {data.placement.isError && (
        <p role="status" className="text-sm text-muted-foreground">
          {t("clusterMap.loadUnknownNote")}
        </p>
      )}
      <Body
        data={data}
        model={model}
        scene={scene}
        mode={mode}
        render={(m, s) =>
          mode === "picture" ? (
            <DndProvider accepts={drag.accepts} onDrop={drag.onDrop}>
              <MapScene
                scene={s}
                nodes={nodesByKey}
                labels={labels}
                vmNames={vmNames}
                layers={shown}
                slabName={slabName}
                problemsOnly={filter.problems}
                describedBy={keysId}
                focusRequest={focusRequest}
                onOpen={onOpen}
                onDetail={onDetail}
                onSummaryEnter={(nodeKey, owner) =>
                  onOpen({ key: nodeKey === "unplaced" ? "unplaced" : `more:${nodeKey}`, owner })
                }
                onFocusChange={(f) => {
                  focus.current = f;
                }}
                onLod={setLod}
                renderCard={renderCard}
                drag={drag.drag ?? undefined}
              />
              <Legend layers={shown} />
              <p id={keysId} className="text-xs text-muted-foreground">
                {t("clusterMap.keys")}
              </p>
            </DndProvider>
          ) : (
            <MapList
              model={m}
              scene={s}
              links={links}
              labels={labels}
              slabName={slabName}
              onOpen={(key, owner) => {
                onOpen({ key, owner });
                returnLink.current = links.find((l) => l.key === key) ?? null;
              }}
            />
          )
        }
      />
      {opened && (
        <Suspense fallback={null}>
          <MapSheet
            target={sheet}
            phone={phone}
            nodes={data.nodes.data ?? []}
            vms={data.vms.data ?? []}
            links={links}
            moves={data.placement.data?.recommendations ?? []}
            actions={actions}
            returnFocus={returnFocus}
            beforeReturn={beforeReturn}
            onTarget={setSheet}
            onClose={() => setSheet(null)}
          />
        </Suspense>
      )}
      {phone && (
        <Sheet open={settings} onOpenChange={setSettings}>
          <SheetContent
            side="bottom"
            className="max-h-[85vh] overflow-y-auto"
            aria-describedby={undefined}
          >
            <SheetHeader>
              <SheetTitle>{t("clusterMap.settings")}</SheetTitle>
            </SheetHeader>
            <div className="flex flex-col gap-4 px-4 pb-6">{controls}</div>
          </SheetContent>
        </Sheet>
      )}
      {actions.dialogs}
    </div>
  );
}

/** 기다림·오류·빈 클러스터, 아니면 그림이나 목록 */
function Body({
  data,
  model,
  scene,
  render,
}: {
  data: ClusterMapData;
  model: ClusterModel | null;
  scene: Scene | null;
  mode: Mode;
  render(model: ClusterModel, scene: Scene): React.ReactNode;
}) {
  const { t } = useTranslation();
  if (!model || !scene) {
    return <ListPending error={data.error} />;
  }
  if (model.nodes.length === 0) {
    return <EmptyState icon={Server} title={t("dashboard.noNodes")} />;
  }
  return <>{render(model, scene)}</>;
}
