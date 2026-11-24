// 누르기 서랍(계획 70 §10.2): 지금 있는 미리 보기(`NodePeek`·`VmPeek`)와 그 자리의 동작 — 노드 동작 메뉴(`useNodeActions`),
// VM 전원·"옮기기…"(`VmMigrateDialog`)·VM 메뉴, 마이그레이션 Task 줄, 최적 배치 추천의 "적용"(최적 배치 화면과 같은 요청).
// 닫으면 그림의 그 원소로 초점이 돌아온다(`focusBack`).
import { useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal } from "lucide-react";
import { type RefObject, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { api, call } from "@/api/client";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, PlacementMove, Vm } from "@/api/types";
import { useVmCan } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { focusBack } from "@/components/focusBack";
import { PeekFacts } from "@/components/data-table/PeekFacts";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { linkText } from "@/clusterMap/labels";
import type { MapLink } from "@/clusterMap/layout";
import { byName, placeOf } from "@/clusterMap/model";
import { formatMib } from "@/lib/format";
import { NodePeek } from "@/nodes/NodePeek";
import type { useNodeActions } from "@/nodes/NodeActions";
import { moveReasonText, PLACEMENT_KEY } from "@/placement/balance";
import { migrationStageText, trackTask } from "@/tasks/tasks";
import { availability } from "@/vms/actions";
import { useVmActionHost } from "@/vms/actionHost";
import { VmMenu } from "@/vms/VmMenu";
import { VmPeek } from "@/vms/VmPeek";

/** 서랍에 연 것 */
export type SheetTarget =
  | { kind: "node"; id: string }
  | { kind: "vm"; id: string }
  /** 노드의 VM 목록("+N"·요약 단계의 `]`). `node`가 `null`이면 배치 안 됨 */
  | { kind: "vms"; node: string | null }
  | { kind: "move"; key: string };

const PHASE_TONE = {
  running: "success",
  failed: "danger",
  stopped: "neutral",
} as const;

function phaseTone(vm: Vm) {
  return PHASE_TONE[vm.status.phase as keyof typeof PHASE_TONE] ?? "info";
}

/** VM 목록(실패 먼저, 그다음 이름 순). 누르면 그 VM의 서랍. */
function VmList({ vms, onPick }: { vms: readonly Vm[]; onPick(vm: Vm): void }) {
  const { t } = useTranslation();
  const sorted = [...vms].sort(
    (a, b) =>
      Number(b.status.phase === "failed") - Number(a.status.phase === "failed") || byName(a, b),
  );
  if (sorted.length === 0) {
    return <p className="px-4 text-sm text-muted-foreground">{t("clusterMap.label.noVms")}</p>;
  }
  return (
    <ul className="flex flex-col gap-1 px-4 pb-4 text-sm">
      {sorted.map((vm) => (
        <li key={vm.id} className="flex items-center justify-between gap-2">
          <Button variant="link" size="sm" className="h-auto p-0" onClick={() => onPick(vm)}>
            {vm.name}
          </Button>
          <StatusChip tone={phaseTone(vm)}>{t(`vms.phase.${vm.status.phase}`)}</StatusChip>
        </li>
      ))}
    </ul>
  );
}

function VmSheet({ vm, nodeName }: { vm: Vm; nodeName(id?: string | null): string }) {
  const { t } = useTranslation();
  const can = useVmCan(vm);
  const host = useVmActionHost();
  const tasks = useTaskIndex().get(vm.id) ?? [];
  const reasons = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const able = availability(vm, "migrate", tasks);
  const reason = able.ok ? null : able.reason;
  return (
    <>
      <VmPeek vm={vm} nodeName={nodeName} />
      <div className="flex flex-wrap items-center gap-2 px-4 pb-4">
        {can("vm.migrate") && (
          <ReasonButton
            ref={trigger}
            variant="outline"
            size="sm"
            reason={reason}
            reasons={reasons}
            onClick={() => host.open({ vm, which: "migrate", returnFocus: trigger })}
          >
            {t("clusterMap.sheet.migrate")}
          </ReasonButton>
        )}
        <VmMenu vm={vm} />
        <ReasonList base={reasons} reasons={[reason]} className="w-full" />
      </div>
    </>
  );
}

function NodeSheet({
  node,
  vms,
  moves,
  nodeName,
  actions,
  onPick,
  onMove,
}: {
  node: Node;
  vms: readonly Vm[];
  /** 이 노드에서 나가거나 들어오는 화살표(그림에 그린 것) — 키보드·화면 읽기로 화살표 서랍에 닿는 길 */
  moves: readonly MapLink[];
  nodeName(key: string): string;
  actions: ReturnType<typeof useNodeActions>;
  onPick(vm: Vm): void;
  onMove(link: MapLink): void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <NodePeek node={node} vms={vms.length} />
      <div className="flex flex-wrap gap-2 px-4">
        {actions.write && (
          <DropdownMenu modal={false}>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="sm"
                onPointerDown={actions.rememberTrigger}
                onKeyDown={actions.rememberTrigger}
              >
                <MoreHorizontal />
                {t("clusterMap.sheet.nodeActions")}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start">{actions.items(node)}</DropdownMenuContent>
          </DropdownMenu>
        )}
        <Button variant="outline" size="sm" asChild>
          <Link to="/networks/topology">{t("clusterMap.sheet.topology")}</Link>
        </Button>
      </div>
      <h3 className="px-4 pt-2 text-sm font-medium">
        {t("clusterMap.sheet.nodeVms", { name: node.name })}
      </h3>
      <VmList vms={vms} onPick={onPick} />
      {moves.length > 0 && (
        <>
          <h3 className="px-4 pt-2 text-sm font-medium">{t("clusterMap.moves")}</h3>
          <ul className="flex flex-col gap-1 px-4 pb-4 text-sm">
            {moves.map((link) => (
              <li key={link.key}>
                <Button
                  variant="link"
                  size="sm"
                  className="h-auto p-0 text-left whitespace-normal"
                  onClick={() => onMove(link)}
                >
                  {linkText(t, link, nodeName)}
                </Button>
              </li>
            ))}
          </ul>
        </>
      )}
    </>
  );
}

function VmsSheet({
  title,
  vms,
  onPick,
}: {
  title: string;
  vms: readonly Vm[];
  onPick(vm: Vm): void;
}) {
  const { t } = useTranslation();
  const [filter, setFilter] = useState("");
  const id = useId();
  const shown = vms.filter((vm) => vm.name.toLowerCase().includes(filter.trim().toLowerCase()));
  return (
    <>
      <SheetHeader>
        <SheetTitle className="pr-8">{title}</SheetTitle>
      </SheetHeader>
      <div className="px-4">
        <label htmlFor={id} className="sr-only">
          {t("clusterMap.sheet.filter")}
        </label>
        <Input
          id={id}
          value={filter}
          placeholder={t("clusterMap.sheet.filter")}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>
      <VmList vms={shown} onPick={onPick} />
    </>
  );
}

function MoveSheet({
  link,
  move,
  vm,
  nodeName,
  onApplied,
}: {
  link: MapLink;
  /** 추천이면 `/placement`의 그 추천 */
  move: PlacementMove | undefined;
  /** 화살표의 VM(읽을 수 있으면) — "적용"은 이 VM을 옮길 수 있을 때만 */
  vm: Vm | undefined;
  nodeName(key: string): string;
  onApplied(): void;
}) {
  const { t } = useTranslation();
  const can = useVmCan(vm);
  const queryClient = useQueryClient();
  const [confirm, setConfirm] = useState(false);
  const from = nodeName(link.ends[0]);
  const to = nodeName(link.ends[1]);
  if (link.kind === "migration") {
    return (
      <>
        <SheetHeader>
          <SheetTitle className="pr-8">
            {t("clusterMap.sheet.migrationTitle", { vm: link.vmName })}
          </SheetTitle>
        </SheetHeader>
        <PeekFacts
          facts={[
            [t("clusterMap.sheet.from"), from],
            [t("clusterMap.sheet.to"), to],
            [
              t("clusterMap.sheet.progress"),
              link.progress !== null
                ? `${link.progress}%`
                : ((link.stage ? migrationStageText(link.stage, t) : null) ?? "—"),
            ],
          ]}
        />
        <div className="p-4">
          <Button size="sm" variant="outline" asChild>
            <Link to="/tasks">{t("clusterMap.sheet.tasks")}</Link>
          </Button>
        </div>
      </>
    );
  }
  // 서버도 VM마다 본다(폴더 운영자가 "적용"을 눌러 403을 보지 않게)
  const operate = can("vm.migrate");
  return (
    <>
      <SheetHeader>
        <SheetTitle className="pr-8">
          {t("clusterMap.sheet.recTitle", { vm: link.vmName })}
        </SheetTitle>
      </SheetHeader>
      <PeekFacts
        facts={[
          [t("clusterMap.sheet.from"), from],
          [t("clusterMap.sheet.to"), to],
          ...(move
            ? ([
                [t("clusterMap.sheet.memory"), formatMib(move.cost_mib)],
                [t("clusterMap.sheet.reason"), moveReasonText(t, move)],
              ] as [string, string][])
            : []),
        ]}
      />
      {operate && move && (
        <div className="p-4">
          <Button size="sm" onClick={() => setConfirm(true)}>
            {t("clusterMap.sheet.apply")}
          </Button>
        </div>
      )}
      {move && (
        <ConfirmDialog
          open={confirm}
          onOpenChange={setConfirm}
          title={t("clusterMap.sheet.applyTitle", { vm: link.vmName })}
          description={t("clusterMap.sheet.applyDescription", { vm: link.vmName, from, to })}
          confirmLabel={t("clusterMap.sheet.apply")}
          onConfirm={async () => {
            const task = await call(
              api.POST("/placement/recommendations/{id}/apply", {
                params: { path: { id: move.id } },
              }),
            );
            trackTask(task, t("placement.applyOp", { vm: move.vm_name, node: move.to_name }));
            await queryClient.invalidateQueries({ queryKey: PLACEMENT_KEY });
            onApplied();
          }}
        />
      )}
    </>
  );
}

/** 지도의 서랍. 넓은 화면은 오른쪽, 휴대폰은 아래. */
export function MapSheet({
  target,
  phone,
  nodes,
  vms,
  links,
  moves,
  actions,
  returnFocus,
  onTarget,
  onClose,
}: {
  target: SheetTarget | null;
  phone: boolean;
  nodes: readonly Node[];
  vms: readonly Vm[];
  links: readonly MapLink[];
  moves: readonly PlacementMove[];
  actions: ReturnType<typeof useNodeActions>;
  returnFocus: RefObject<HTMLElement | null>;
  onTarget(target: SheetTarget): void;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const nodeName = (id?: string | null) => nodes.find((n) => n.id === id)?.name ?? "—";
  const pick = (vm: Vm) => onTarget({ kind: "vm", id: vm.id });
  const known = new Set(nodes.map((n) => n.id));
  // 그림과 같은 자리 규칙(`placeOf` — 모르는 노드의 `status.node`는 `spec.node`로)
  const onNode = (id: string | null) =>
    vms.filter((vm) => !vm.spec.template && placeOf(vm, known) === id);
  const linkName = (key: string) => nodeName(key.replace(/^node:/, ""));
  let body: React.ReactNode = null;
  if (target?.kind === "node") {
    const node = nodes.find((n) => n.id === target.id);
    body = node && (
      <NodeSheet
        node={node}
        vms={onNode(node.id)}
        moves={links.filter((l) => l.ends.includes(`node:${node.id}`))}
        nodeName={linkName}
        actions={actions}
        onPick={pick}
        onMove={(link) => onTarget({ kind: "move", key: link.key })}
      />
    );
  } else if (target?.kind === "vm") {
    const vm = vms.find((v) => v.id === target.id);
    body = vm && <VmSheet vm={vm} nodeName={nodeName} />;
  } else if (target?.kind === "vms") {
    const list = onNode(target.node);
    body = (
      <VmsSheet
        title={
          target.node === null
            ? t("clusterMap.sheet.unplacedTitle", { count: list.length })
            : t("clusterMap.sheet.nodeVms", { name: nodeName(target.node) })
        }
        vms={list}
        onPick={pick}
      />
    );
  } else if (target?.kind === "move") {
    const link = links.find((l) => l.key === target.key);
    body = link && (
      <MoveSheet
        link={link}
        move={moves.find((m) => m.id === link.recommendation)}
        vm={vms.find((v) => v.id === link.vm)}
        nodeName={linkName}
        onApplied={onClose}
      />
    );
  }
  return (
    <Sheet open={target !== null && !!body} onOpenChange={(open) => !open && onClose()}>
      <SheetContent
        side={phone ? "bottom" : "right"}
        className={phone ? "max-h-[85vh] overflow-y-auto" : "overflow-y-auto"}
        aria-describedby={undefined}
        onCloseAutoFocus={(e) => focusBack(e, returnFocus)}
      >
        {body}
      </SheetContent>
    </Sheet>
  );
}
