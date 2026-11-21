import { Network as NetworkIcon } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";

import { useList } from "@/api/live";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { folderPath, folderTree } from "@/folders/folders";
import { readPref, writePref } from "@/lib/prefs";
import { isProvider } from "@/routing/routing";
import { NetworkSheet } from "@/sdn/NetworkSheet";
import { layoutTopology, type Scene, type SceneItem } from "@/topology/layout";
import { gatewayText, type OpenTarget, TopologyList } from "@/topology/TopologyList";
import { TONE_COLOR, TopologyScene } from "@/topology/TopologyScene";
import { type Topology, type TopoVm, useTopology } from "@/topology/topology";

/** 이만큼 넘으면 폴더로 거르기를 권한다(§12.1 크기) */
const BIG_NETWORKS = 200;
const BIG_VMS = 2000;
/** VM 이름을 그림에 적는 VM 수 상한 */
const NAMED_VMS = 40;

type Mode = "picture" | "list";

/**
 * 인프라 › 네트워크 › 토폴로지(계획 67 §12): 네트워크·라우터·게이트웨이·유동 IP·VM·부하 분산기를 2.5D 아이소메트릭 그림
 * 하나로(3D처럼 보이는 2D SVG — WebGL 없음) 또는 트리 목록으로. 늦게 읽는 조각이라 첫 화면 묶음에 그림 코드가 없다.
 */
export function TopologyPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const perms = usePermissions();
  const folders = useList("folder");
  const networks = useList("network");
  const [folder, setFolder] = useState<string>("");
  const [mode, setMode] = useState<Mode>(() =>
    readPref("topology.mode") === "list" ? "list" : "picture",
  );
  const [hideSimple, setHideSimple] = useState(false);
  const [hideVms, setHideVms] = useState(false);
  const topology = useTopology(folder || null);
  const [sheet, setSheet] = useState<
    { kind: "network"; id: string } | { kind: "more"; network: string; vms: TopoVm[] } | null
  >(null);
  const graph = topology.data;
  const scene: Scene | null = useMemo(
    () => (graph ? layoutTopology(graph, { hideSimple, hideVms }) : null),
    [graph, hideSimple, hideVms],
  );
  const labels = useMemo(
    () => (graph && scene ? itemLabels(t, graph, scene) : new Map<string, string>()),
    [t, graph, scene],
  );
  const tree = folderTree(folders.data, "network");
  const big = graph && (graph.networks.length > BIG_NETWORKS || graph.vms.length > BIG_VMS);
  const hidden = graph ? Object.values(graph.hidden).reduce((sum, n) => sum + n, 0) : 0;

  const open = (target: OpenTarget) => {
    switch (target.kind) {
      case "network":
        setSheet({ kind: "network", id: target.id });
        return;
      case "simple":
      case "provider": {
        // 공급자 네트워크도 서랍이 있다(서브넷·유동 IP 범위). 단순 네트워크는 목록에서.
        const n = networks.data?.find((x) => x.id === target.id);
        if (target.kind === "provider" && n) {
          setSheet({ kind: "network", id: target.id });
        } else {
          void navigate("/networks");
        }
        return;
      }
      case "router":
        void navigate(`/routers?open=${encodeURIComponent(target.id)}`);
        return;
      case "lb":
        void navigate(`/load-balancers?open=${encodeURIComponent(target.id)}`);
        return;
      case "fip":
        void navigate("/floating-ips");
        return;
      case "vm":
        void navigate(`/vms/${encodeURIComponent(target.name)}`);
        return;
    }
  };

  const openItem = (item: SceneItem) => {
    if (!graph) {
      return;
    }
    switch (item.kind) {
      case "more":
        setSheet({ kind: "more", network: item.id, vms: scene?.folded[`net:${item.id}`] ?? [] });
        return;
      case "vm": {
        const vm = graph.vms.find((v) => v.id === item.id);
        if (vm) {
          open({ kind: "vm", name: vm.name });
        }
        return;
      }
      case "vip":
        open({ kind: "lb", id: item.id });
        return;
      default:
        open({ kind: item.kind, id: item.id });
    }
  };

  const opened =
    sheet?.kind === "network" ? networks.data?.find((n) => n.id === sheet.id) : undefined;

  return (
    <div className="flex flex-col gap-4">
      <PageHeader id="topology-title" title={t("nav.topology")} description={t("topology.hint")} />
      <div className="flex flex-wrap items-end gap-x-6 gap-y-3">
        <ToggleGroup
          type="single"
          variant="outline"
          value={mode}
          aria-label={t("topology.mode")}
          onValueChange={(value) => {
            if (value === "picture" || value === "list") {
              setMode(value);
              writePref("topology.mode", value);
            }
          }}
        >
          <ToggleGroupItem value="picture">{t("topology.picture")}</ToggleGroupItem>
          <ToggleGroupItem value="list">{t("topology.list")}</ToggleGroupItem>
        </ToggleGroup>
        {tree.folders.length > 0 && (
          <Field id="topology-folder" label={t("topology.folder")}>
            <NativeSelect
              id="topology-folder"
              value={folder}
              onChange={(e) => setFolder(e.target.value)}
            >
              <NativeSelectOption value="">{t("topology.allFolders")}</NativeSelectOption>
              {tree.folders
                .map((f) => ({ id: f.id, path: folderPath(tree, f.id) }))
                .sort((a, b) => a.path.localeCompare(b.path))
                .map((f) => (
                  <NativeSelectOption key={f.id} value={f.id}>
                    {f.path}
                  </NativeSelectOption>
                ))}
            </NativeSelect>
          </Field>
        )}
        <div className="flex items-center gap-2">
          <Checkbox
            id="topology-hide-simple"
            checked={hideSimple}
            onCheckedChange={(on) => setHideSimple(on === true)}
          />
          <Label htmlFor="topology-hide-simple">{t("topology.hideSimple")}</Label>
        </div>
        <div className="flex items-center gap-2">
          <Checkbox
            id="topology-hide-vms"
            checked={hideVms}
            onCheckedChange={(on) => setHideVms(on === true)}
          />
          <Label htmlFor="topology-hide-vms">{t("topology.hideVms")}</Label>
        </div>
      </div>
      {big && (
        <p role="status" className="rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg">
          {t("topology.big")}
        </p>
      )}
      {!graph || !scene ? (
        <ListPending error={topology.error} />
      ) : graph.networks.length === 0 ? (
        <EmptyState
          icon={NetworkIcon}
          title={t("topology.empty.title")}
          description={t("topology.empty.description")}
          action={
            perms.anywhere("network.config") && (
              <Link to="/networks?create=1" className="text-sm underline">
                {t("topology.empty.create")}
              </Link>
            )
          }
        />
      ) : mode === "picture" ? (
        <>
          <TopologyScene
            scene={scene}
            labels={labels}
            onOpen={openItem}
            showVmNames={graph.vms.length <= NAMED_VMS}
          />
          <Legend />
          <p className="text-xs text-muted-foreground">{t("topology.keys")}</p>
        </>
      ) : (
        <TopologyList graph={graph} hideSimple={hideSimple} hideVms={hideVms} onOpen={open} />
      )}
      {hidden > 0 && (
        <p className="text-sm text-muted-foreground">{t("topology.hidden", { count: hidden })}</p>
      )}
      {topology.nicless > 0 && (
        <p className="text-sm text-muted-foreground">
          {t("topology.nicless", { count: topology.nicless })}
        </p>
      )}
      {opened && (
        <NetworkSheet
          network={opened}
          writes={
            perms.has("network.config", objectTarget("network", opened)) &&
            (!isProvider(opened) || perms.has("cluster.admin"))
          }
          onClose={() => setSheet(null)}
        />
      )}
      {sheet?.kind === "more" && (
        <Sheet open onOpenChange={(on) => !on && setSheet(null)}>
          <SheetContent className="w-full overflow-y-auto sm:max-w-md" aria-describedby={undefined}>
            <SheetTitle className="px-4 pt-4 text-lg">
              {t("topology.moreTitle", {
                count: sheet.vms.length,
                network: graph?.networks.find((n) => n.id === sheet.network)?.name ?? "",
              })}
            </SheetTitle>
            <ul className="flex flex-col gap-1 px-4 pb-4 text-sm">
              {sheet.vms.map((vm) => (
                <li key={vm.id} className="flex justify-between gap-2">
                  <Link to={`/vms/${encodeURIComponent(vm.name)}`} className="underline">
                    {vm.name}
                  </Link>
                  <span className="text-muted-foreground">{t(`vms.phase.${vm.power}`)}</span>
                </li>
              ))}
            </ul>
          </SheetContent>
        </Sheet>
      )}
    </div>
  );
}

/** 색 설명 */
function Legend() {
  const { t } = useTranslation();
  const rows: [string, string][] = [
    [TONE_COLOR.overlay, t("topology.legend.overlay")],
    [TONE_COLOR.provider, t("topology.legend.provider")],
    [TONE_COLOR.simple, t("topology.legend.simple")],
    [TONE_COLOR.router, t("topology.legend.router")],
    [TONE_COLOR.running, t("topology.legend.running")],
    [TONE_COLOR.stopped, t("topology.legend.stopped")],
    [TONE_COLOR.failed, t("topology.legend.failed")],
    [TONE_COLOR.lb, t("topology.legend.lb")],
    [TONE_COLOR.fip, t("topology.legend.fip")],
    [TONE_COLOR.warning, t("topology.legend.warning")],
  ];
  return (
    <ul aria-label={t("topology.legendLabel")} className="flex flex-wrap gap-x-4 gap-y-1 text-xs">
      {rows.map(([color, label]) => (
        <li key={label} className="flex items-center gap-1.5">
          <span aria-hidden className="size-3 rounded-sm border" style={{ background: color }} />
          {label}
        </li>
      ))}
    </ul>
  );
}

type Translate = (key: string, values?: Record<string, unknown>) => string;

/** 원소 열쇠 → 화면 읽기 이름(§12.2 "라우터 r1, 인터페이스 2개, 활성 게이트웨이 n2"). 장면마다 한 번 센다 — 이름마다 VM을
 * 훑지 않게 id → 원소·네트워크마다 VM 수를 먼저 만든다(리뷰 1 I5). */
export function itemLabels(t: Translate, graph: Topology, scene: Scene): Map<string, string> {
  const vms = new Map(graph.vms.map((v) => [v.id, v]));
  const nets = new Map(graph.networks.map((n) => [n.id, n]));
  const routers = new Map(graph.routers.map((r) => [r.id, r]));
  const lbs = new Map(graph.load_balancers.map((l) => [l.id, l]));
  const fips = new Map(graph.floating_ips.map((f) => [f.id, f]));
  const perNet = new Map<string, number>();
  for (const vm of graph.vms) {
    for (const id of new Set(vm.nics.map((nic) => nic.network))) {
      perNet.set(id, (perNet.get(id) ?? 0) + 1);
    }
  }
  const label = (item: SceneItem): string => {
    switch (item.kind) {
      case "network":
      case "simple":
      case "provider":
        return t(`topology.aria.${item.kind}`, {
          name: item.label,
          detail: item.detail ?? t(`topology.kind.${nets.get(item.id)?.kind ?? "sdn"}`),
          count: perNet.get(item.id) ?? 0,
        });
      case "router": {
        const r = routers.get(item.id);
        return t("topology.aria.router", {
          name: item.label,
          count: r?.interfaces.length ?? 0,
          gateway: r ? gatewayText(t, r.gateway) : "",
        });
      }
      case "vm": {
        const vm = vms.get(item.id);
        return t("topology.aria.vm", {
          name: item.label,
          power: vm ? t(`vms.phase.${vm.power}`) : "",
          ips: item.detail ?? "—",
        });
      }
      case "more":
        return t("topology.aria.more", {
          count: item.count ?? 0,
          network: nets.get(item.id)?.name ?? "",
        });
      case "lb": {
        const lb = lbs.get(item.id);
        return t("topology.aria.lb", {
          name: item.label,
          vip: lb?.vip ?? "—",
          online: lb?.members.filter((m) => m.health === "online").length ?? 0,
          total: lb?.members.length ?? 0,
        });
      }
      case "fip": {
        const f = fips.get(item.id);
        return t("topology.aria.fip", {
          address: item.label,
          state: f ? t(`fip.state.${f.state}`, { defaultValue: f.state }) : "",
          vm: (f?.vm && vms.get(f.vm)?.name) || "—",
        });
      }
      case "vip":
        return t("topology.aria.vip", { address: item.label, name: lbs.get(item.id)?.name ?? "" });
    }
  };
  return new Map(scene.items.map((it) => [it.key, label(it)]));
}
