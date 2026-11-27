// VM 틀(계획 71 설계 §8.2): 칸이 정해진 틀 — 이름·폴더, 계산·배치·보호·cloud-init, 디스크(위가 먼저 부팅)·CD·NIC 블록,
// 블록을 들면 나타나는 빼기 칸. 칸은 놓을 자리(`useDropSlot`)이고 문제는 그 칸에(오류 빨강·경고 노랑 — 아이콘과 글도).
// 블록은 잡는 손잡이(끌기·들기)와 살피기 단추와 `⋮` 메뉴(위로·아래로·빼기 — 끌지 않는 길)다.
import { CircleX, EllipsisVertical, GripVertical, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { useDnd } from "@/dnd/DndProvider";
import { useDraggable } from "@/dnd/useDraggable";
import { useDropSlot } from "@/dnd/useDropSlot";
import { folderPath, folderTree } from "@/folders/folders";
import { formatMib } from "@/lib/format";
import { useBuilder } from "@/vmBuilder/context";
import { Inspector } from "@/vmBuilder/Inspector";
import { blockLabel, problemSlot, problemText, slotLabel, type Thing } from "@/vmBuilder/labels";
import { accepts } from "@/vmBuilder/model/accepts";
import { drop } from "@/vmBuilder/model/drop";
import { parts } from "@/vmBuilder/model/parts";
import type { Problem } from "@/vmBuilder/model/problems";
import type { Block, ListKind } from "@/vmBuilder/model/slots";
import { formOnly, setName } from "@/vmBuilder/model/state";
import { isShared } from "@/storage/pools";

/** 칸 하나: 놓을 자리, 제목, 비었을 때 안내, 그 칸의 문제 */
function SlotBox({
  id,
  empty,
  children,
  inspect = true,
}: {
  id: string;
  /** 비었을 때 안내(없으면 안내 없음) */
  empty?: boolean;
  children?: ReactNode;
  inspect?: boolean;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const label = slotLabel(t, id, b.s);
  const { ref, props, verdict } = useDropSlot<HTMLDivElement>(id, label);
  const own = b.list.filter((p) => problemSlot(p.at) === id);
  // 틀 전체가 h2, 그 안의 칸이 h3(휴대폰에는 부품 상자 제목이 없다)
  const Heading = id === "vm" ? "h2" : "h3";
  return (
    <div
      ref={ref}
      {...props}
      role="group"
      aria-label={
        own.length ? t("vmBuilder.slot.problems", { name: label, count: own.length }) : label
      }
      data-slot={id}
      data-level={level(own)}
      className="flex min-w-0 flex-col gap-2 rounded-lg border bg-card p-3"
    >
      <div className="flex items-center justify-between gap-2">
        <Heading id={`vb-${id}`} tabIndex={-1} className="text-sm font-medium">
          {label}
        </Heading>
        {inspect && (
          <Button type="button" variant="ghost" size="sm" onClick={() => b.select(id)}>
            {t("vmBuilder.page.inspect")}
          </Button>
        )}
      </div>
      {children}
      {empty && (
        <p className="rounded-md border border-dashed p-2 text-sm text-muted-foreground">
          {t(`vmBuilder.empty.${id}`)}
        </p>
      )}
      {dnd.state.phase === "picked" && verdict?.ok && (
        <span className="flex min-h-11 items-center justify-center rounded-md border border-dashed text-sm font-medium">
          {t("vmBuilder.page.dropHere")}
        </span>
      )}
      <Problems list={own} />
    </div>
  );
}

const level = (list: readonly Problem[]) =>
  list.some((p) => p.level === "error") ? "error" : list.length > 0 ? "warn" : undefined;

function Problems({ list }: { list: readonly Problem[] }) {
  const { t } = useTranslation();
  return list.map((p, i) => (
    <p key={i} className="flex items-start gap-1 text-xs" data-level={p.level}>
      {p.level === "error" ? (
        <CircleX aria-hidden className="size-3.5 shrink-0" />
      ) : (
        <TriangleAlert aria-hidden className="size-3.5 shrink-0" />
      )}
      <span>
        <span className="sr-only">{t(`vmBuilder.check.${p.level}`)}: </span>
        {problemText(t, p)}
      </span>
    </p>
  ));
}

export function Chassis() {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const v = b.s.values;
  const nameProblem = b.list.find((p) => p.at === "name");
  const holdingBlock =
    (dnd.state.phase === "dragging" || dnd.state.phase === "picked") &&
    (dnd.state.item as Thing).kind === "block";
  const empty = v.disks.length === 0 && v.nics.length === 0 && b.s.base.kind === "blank";
  const tree = folderTree(b.ctx.folders, "vm");
  const node = b.ctx.nodes?.find((n) => n.id === v.node);

  // "보통 크기 Linux 서버로 시작": Linux + 크기 보통 + 첫 공유 풀 새 디스크 + 첫 네트워크 NIC(설계 §8.3). 프리셋이
  // 메모리를 정하므로 크기를 뒤에 얹는다.
  function quickStart() {
    let s = b.s;
    const firstPool = b.ctx.pools.find(isShared) ?? b.ctx.pools[0];
    const steps: [string, string][] = [
      ["os:linux", "vm"],
      ["size:m", "compute"],
      ...(firstPool ? ([[`pool:${firstPool.name}`, "disks"]] as [string, string][]) : []),
      [`network:${b.ctx.networks[0]?.name ?? "user"}`, "nics"],
    ];
    for (const [id, at] of steps) {
      const part = parts(s, b.ctx).find((p) => p.id === id);
      if (part) {
        s = drop(s, part, at, b.ctx);
      }
    }
    b.record(s);
  }

  // 틀 위 칩(설계 §8.6): 빈 VM이면 폼에서 고친 들고 다니는 칸(그대로 보냄), 템플릿 바탕이면 놓을 때 비운 칸
  const template = b.s.base.kind === "template";
  const carried = template ? (b.s.cleared ?? []) : formOnly(v);

  return (
    <div className="flex min-w-0 flex-col gap-4">
      {carried.length > 0 && (
        <p className="self-start rounded-full border bg-muted px-3 py-1 text-xs">
          {t(template ? "vmBuilder.page.formCleared" : "vmBuilder.page.formOnly", {
            count: carried.length,
            names: carried.map((k) => t(`vmBuilder.carried.${k}`)).join(", "),
          })}
        </p>
      )}
      <SlotBox id="vm" inspect={template}>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field
            id="vb-name"
            label={t("common.name")}
            // 빈 이름의 오류는 칸 밑에 바로 보이지 않는다(만들기 단추 옆 까닭이 알림).
            error={v.name === "" ? undefined : (nameProblem?.message ?? nameProblem?.key)}
            errorValues={nameProblem?.values as Record<string, string> | undefined}
            help={t("vmBuilder.help.name")}
          >
            {(a11y) => (
              <Input
                id="vb-name"
                value={v.name}
                disabled={b.busy}
                autoComplete="off"
                onChange={(e) => b.set(setName(b.s, e.target.value))}
                {...a11y}
              />
            )}
          </Field>
          <SlotBox id="folder" inspect={false}>
            <p className="text-sm">{v.folder ? folderPath(tree, v.folder) : "/"}</p>
          </SlotBox>
        </div>
        <p className="text-sm text-muted-foreground">
          {b.s.base.kind === "template"
            ? t("vmBuilder.page.baseTemplate", {
                name: b.ctx.vms.find((x) => x.id === (b.s.base as { id: string }).id)?.name ?? "",
              })
            : t("vmBuilder.page.baseBlank")}
        </p>
        {empty && (
          <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed p-3">
            <p className="text-sm text-muted-foreground">{t("vmBuilder.page.start")}</p>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={b.busy}
              onClick={quickStart}
            >
              {t("vmBuilder.page.quickStart")}
            </Button>
          </div>
        )}
        <div className="grid gap-3 sm:grid-cols-2">
          <SlotBox id="compute">
            <p className="text-sm">
              {t("vmBuilder.page.compute", {
                vcpus: v.sockets * (Number(v.cores) || 0),
                memory: formatMib(Number(v.memory) || 0),
                model: v.model,
              })}
            </p>
          </SlotBox>
          <SlotBox id="placement">
            <p className="text-sm">
              {b.s.zone
                ? t("vmBuilder.part.zone", { name: b.s.zone.value })
                : node
                  ? node.name
                  : t("vmBuilder.part.auto")}
            </p>
          </SlotBox>
          <SlotBox id="protection" empty={!v.ha}>
            {v.ha && (
              <p className="text-sm">
                {v.haGroup ? `${t("vmBuilder.part.ha")} · ${v.haGroup}` : t("vmBuilder.part.ha")}
              </p>
            )}
          </SlotBox>
          <SlotBox id="cloudInit" empty={!v.cloudInit.enabled}>
            {v.cloudInit.enabled && (
              <p className="text-sm">
                {v.cloudInit.user || t("vmBuilder.page.ciOn")}
                {v.cloudInit.sshKeys.trim() !== "" && ` · ${t("vmBuilder.page.keys")}`}
              </p>
            )}
          </SlotBox>
        </div>
        <ListSlot list="disks" />
        <ListSlot list="cdroms" />
        <ListSlot list="nics" />
        {holdingBlock && <SlotBox id="trash" inspect={false} />}
      </SlotBox>
      {b.selected && <Inspector />}
    </div>
  );
}

function ListSlot({ list }: { list: ListKind }) {
  const b = useBuilder();
  const rows = b.s.values[list];
  return (
    <SlotBox id={list} empty={rows.length === 0} inspect={false}>
      {rows.length > 0 && (
        <div className="flex flex-col gap-1">
          {rows.map((_, index) => (
            <BlockRow key={index} list={list} index={index} />
          ))}
        </div>
      )}
    </SlotBox>
  );
}

function BlockRow({ list, index }: { list: ListKind; index: number }) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const block: Block = { kind: "block", id: `block:${list}.${index}`, list, index };
  const label = blockLabel(t, block, b.s);
  const thing: Thing = { ...block, label };
  const id = `${list}.${index}`;
  const drag = useDraggable(thing, t("vmBuilder.part.blockRole"));
  const { ref, props } = useDropSlot<HTMLDivElement>(id, label);
  const own = b.list.filter((p) => problemSlot(p.at) === id);
  const v = b.s.values;
  const summary = (() => {
    if (list === "disks") {
      const d = v.disks[index]!;
      return [
        d.volume,
        d.kind === "new" ? `${d.sizeGib} GiB · ${d.pool}` : d.sizeGib && `→ ${d.sizeGib} GiB`,
        d.bus,
      ]
        .filter(Boolean)
        .join(" · ");
    }
    if (list === "cdroms") {
      const c = v.cdroms[index]!;
      return [
        c.iso || t("vmBuilder.page.emptyDrive"),
        c.bootOrder === "1" && t("vmBuilder.page.bootFirst"),
      ]
        .filter(Boolean)
        .join(" · ");
    }
    const n = v.nics[index]!;
    const net = v.cloudInit.enabled ? v.cloudInit.networks[index] : undefined;
    return [
      n.network,
      n.model,
      n.ips,
      net &&
        (net.ipv4 === "static"
          ? net.ipv4Address || t("vmBuilder.part.addr.static")
          : t("vmBuilder.part.addr.dhcp")),
    ]
      .filter(Boolean)
      .join(" · ");
  })();
  const move = (to: string) => {
    const said = b.put(thing, to);
    dnd.announce(said ?? t("dnd.dropped", { item: label, slot: slotLabel(t, to, b.s) }));
  };
  const can = (to: string) => accepts(thing, to, b.s, b.ctx).ok;
  const up = `${list}.${index - 1}`;
  const down = `${list}.${index + 1}`;
  return (
    <div
      ref={ref}
      {...props}
      role="group"
      aria-label={label}
      data-slot={id}
      data-level={level(own)}
      className="flex flex-col gap-1 rounded-md border bg-background p-1"
    >
      <div className="flex items-center gap-1">
        <button
          type="button"
          {...drag}
          disabled={b.busy}
          aria-label={t("vmBuilder.page.grip", { name: label })}
          className="vb-part flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted"
        >
          <GripVertical aria-hidden className="size-4" />
        </button>
        <button
          type="button"
          data-block={id}
          onClick={() => {
            if (dnd.state.phase !== "picked") {
              b.select(id);
            }
          }}
          className="min-w-0 flex-1 truncate rounded-md px-1 py-1 text-left text-sm hover:bg-muted"
        >
          <span className="font-medium">{label}</span>{" "}
          <span className="text-muted-foreground">{summary}</span>
        </button>
        {list === "nics" &&
          v.nics[index]!.securityGroups.map((g) => (
            <span key={g} className="rounded-full border px-2 text-xs">
              {g}
            </span>
          ))}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              disabled={b.busy}
              aria-label={t("vmBuilder.page.actions", { name: label })}
            >
              <EllipsisVertical />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem disabled={!can(up)} onSelect={() => move(up)}>
              {t("vmBuilder.page.moveUp")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={!can(down)} onSelect={() => move(down)}>
              {t("vmBuilder.page.moveDown")}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={!can("trash")} onSelect={() => move("trash")}>
              {t("vmBuilder.page.remove")}
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => b.select(id)}>
              {t("vmBuilder.page.inspect")}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <Problems list={own} />
    </div>
  );
}
