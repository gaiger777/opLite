import { cn } from "cn";
import { Folder, Server, Building2, Boxes } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { formatBytes, formatMib } from "@/lib/format";
import type { InvVm, SourceInventory } from "@/vmware/api";
import {
  buildTree,
  filterVms,
  type PowerFilter,
  snapshotCount,
  type TreeKind,
  type TreeNode,
  type TreeView,
  vmBytes,
  vmsUnder,
} from "@/vmware/inventory";

const ICON: Record<TreeKind, typeof Folder> = {
  datacenter: Building2,
  folder: Folder,
  cluster: Boxes,
  host: Server,
};

/** 나무 한 마디와 그 아래. 고른 마디의 VM만 표에 보인다. */
function TreeBranch({
  nodes,
  chosen,
  onChoose,
  depth,
}: {
  nodes: readonly TreeNode[];
  chosen: string | null;
  onChoose(id: string): void;
  depth: number;
}) {
  return (
    <ul className="flex flex-col">
      {nodes.map((node) => {
        const Icon = ICON[node.kind];
        return (
          <li key={node.id}>
            <button
              type="button"
              aria-pressed={chosen === node.id}
              onClick={() => onChoose(node.id)}
              style={{ paddingLeft: `${depth * 0.75 + 0.5}rem` }}
              className={cn(
                "flex w-full items-center gap-1.5 rounded-md py-1 pr-2 text-left text-sm hover:bg-muted",
                chosen === node.id && "bg-muted font-medium",
              )}
            >
              <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate">{node.label}</span>
              <span className="text-xs text-muted-foreground">{node.count}</span>
            </button>
            {node.children.length > 0 && (
              <TreeBranch
                nodes={node.children}
                chosen={chosen}
                onChoose={onChoose}
                depth={depth + 1}
              />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/**
 * 인벤토리 나무 + 표에서 VM 고르기(계획 72 §13 ②): 나무(VM 폴더 또는 클러스터·호스트)에서 마디를 고르면 그 아래 VM만, 찾기·
 * 전원 거르기, 이미 끝나지 않은 계획에 든 VM은 흐리게 두고 고를 수 없다.
 */
export function VmPicker({
  inventory,
  selected,
  onChange,
}: {
  inventory: SourceInventory;
  selected: readonly string[];
  onChange(selected: string[]): void;
}) {
  const { t } = useTranslation();
  const [view, setView] = useState<TreeView>("folders");
  const [node, setNode] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [power, setPower] = useState<PowerFilter>("all");
  const all = inventory.inventory.vms;
  const busy = useMemo(
    () => new Map(inventory.migrating.map((m) => [m.instance_uuid, m] as const)),
    [inventory.migrating],
  );
  const tree = useMemo(() => buildTree(all, view), [all, view]);
  const shown = useMemo(
    () => filterVms(vmsUnder(all, view, node), query, power),
    [all, view, node, query, power],
  );
  const inPlan = (vm: InvVm) => (vm.instance_uuid ? busy.get(vm.instance_uuid) : undefined);
  const pickable = shown.filter((vm) => !inPlan(vm));
  const allShown = pickable.length > 0 && pickable.every((vm) => selected.includes(vm.moref));

  function toggle(moref: string, on: boolean) {
    onChange(on ? [...selected, moref] : selected.filter((m) => m !== moref));
  }

  function toggleAll(on: boolean) {
    const ids = new Set(pickable.map((vm) => vm.moref));
    onChange(on ? [...new Set([...selected, ...ids])] : selected.filter((m) => !ids.has(m)));
  }

  return (
    <div className="flex min-w-0 flex-col gap-3 sm:flex-row">
      <nav aria-label={t("vmware.picker.tree")} className="flex flex-col gap-2 shrink-0 sm:w-56">
        {/* 공용 ToggleGroup(Radix)을 쓰면 첫 화면 묶음의 Radix 조각이 나뉜다 — 단추 둘로 같은 일을 한다. */}
        <div role="group" aria-label={t("vmware.picker.treeView")} className="flex gap-1">
          {(["folders", "hosts"] as const).map((v) => (
            <Button
              key={v}
              type="button"
              size="sm"
              variant={view === v ? "secondary" : "outline"}
              aria-pressed={view === v}
              onClick={() => {
                setView(v);
                setNode(null);
              }}
            >
              {t(`vmware.picker.${v}`)}
            </Button>
          ))}
        </div>
        <div className="max-h-80 overflow-y-auto rounded-md border p-1">
          <button
            type="button"
            aria-pressed={node === null}
            onClick={() => setNode(null)}
            className={cn(
              "flex w-full items-center gap-1.5 rounded-md px-2 py-1 text-left text-sm hover:bg-muted",
              node === null && "bg-muted font-medium",
            )}
          >
            <span className="flex-1">{t("vmware.picker.allVms")}</span>
            <span className="text-xs text-muted-foreground">{all.length}</span>
          </button>
          <TreeBranch nodes={tree} chosen={node} onChoose={setNode} depth={0} />
        </div>
      </nav>
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <div className="flex flex-wrap items-end gap-2">
          <div className="flex min-w-32 flex-1 flex-col gap-1.5">
            <Label htmlFor="vmware-picker-search">{t("vmware.picker.search")}</Label>
            <Input
              id="vmware-picker-search"
              type="search"
              value={query}
              placeholder={t("vmware.picker.searchHint")}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="vmware-picker-power">{t("vmware.picker.power")}</Label>
            <NativeSelect
              id="vmware-picker-power"
              value={power}
              onChange={(e) => setPower(e.target.value as PowerFilter)}
            >
              <NativeSelectOption value="all">{t("vmware.picker.powerAll")}</NativeSelectOption>
              <NativeSelectOption value="powered-on">
                {t("vmware.power.powered-on")}
              </NativeSelectOption>
              <NativeSelectOption value="powered-off">
                {t("vmware.power.powered-off")}
              </NativeSelectOption>
            </NativeSelect>
          </div>
        </div>
        <p role="status" className="text-sm text-muted-foreground">
          {t("vmware.picker.selected", { count: selected.length, shown: shown.length })}
        </p>
        <div className="overflow-x-auto">
          <Table aria-label={t("vmware.picker.table")}>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8">
                  <Checkbox
                    aria-label={t("vmware.picker.selectShown")}
                    checked={allShown}
                    disabled={pickable.length === 0}
                    onCheckedChange={(c) => toggleAll(c === true)}
                  />
                </TableHead>
                <TableHead>{t("common.name")}</TableHead>
                <TableHead>{t("vmware.picker.powerColumn")}</TableHead>
                <TableHead>{t("vmware.picker.os")}</TableHead>
                <TableHead>{t("vms.cpu")}</TableHead>
                <TableHead>{t("vms.memory")}</TableHead>
                <TableHead>{t("vmware.picker.size")}</TableHead>
                <TableHead>{t("vmware.picker.snapshots")}</TableHead>
                <TableHead>{t("vmware.picker.cbt")}</TableHead>
                <TableHead>{t("vmware.picker.tools")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {shown.length === 0 && (
                <TableRow>
                  <TableCell colSpan={10} className="text-center text-muted-foreground">
                    {t("vmware.picker.none")}
                  </TableCell>
                </TableRow>
              )}
              {shown.map((vm) => {
                const plan = inPlan(vm);
                const noteId = `vmware-picker-${vm.moref}-plan`;
                const snapshots = snapshotCount(vm);
                return (
                  <TableRow key={vm.moref} className={cn(plan && "opacity-60")}>
                    <TableCell>
                      <Checkbox
                        aria-label={vm.name}
                        aria-describedby={plan ? noteId : undefined}
                        checked={selected.includes(vm.moref)}
                        disabled={!!plan}
                        onCheckedChange={(c) => toggle(vm.moref, c === true)}
                      />
                    </TableCell>
                    <TableCell className="whitespace-normal">
                      <span className="font-medium">{vm.name}</span>
                      {vm.template && (
                        <span className="ml-1 text-xs text-muted-foreground">
                          {t("vmware.picker.template")}
                        </span>
                      )}
                      <div className="text-xs break-all text-muted-foreground">{vm.path}</div>
                      {plan && (
                        <div id={noteId} className="text-xs text-muted-foreground">
                          {t("vmware.picker.inPlan", { migration: plan.migration })}
                        </div>
                      )}
                    </TableCell>
                    <TableCell>
                      <StatusChip tone={vm.power === "powered-on" ? "success" : "neutral"}>
                        {t(`vmware.power.${vm.power ?? "powered-off"}`)}
                      </StatusChip>
                    </TableCell>
                    <TableCell
                      className="max-w-xs truncate"
                      title={vm.guest_full_name ?? vm.guest_id}
                    >
                      {vm.guest_full_name ?? vm.guest_id}
                    </TableCell>
                    <TableCell>{vm.cpus}</TableCell>
                    <TableCell>{formatMib(vm.memory_mib)}</TableCell>
                    <TableCell>{formatBytes(vmBytes(vm))}</TableCell>
                    <TableCell>{snapshots > 0 ? snapshots : "—"}</TableCell>
                    <TableCell>{vm.cbt ? t("vmware.yes") : t("vmware.no")}</TableCell>
                    <TableCell>
                      {vm.tools ? t(`vmware.tools.${toolsKey(vm.tools)}`) : "—"}
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </div>
      </div>
    </div>
  );
}

/** VMware Tools 상태(`guestToolsRunning` …)의 번역 키 */
function toolsKey(tools: string): "running" | "notRunning" | "other" {
  return tools === "guestToolsRunning"
    ? "running"
    : tools === "guestToolsNotRunning"
      ? "notRunning"
      : "other";
}
