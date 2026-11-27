// VM 그림(계획 77-5 — 진단 §4.3 2단계, 계획 71 틀을 그림으로): 가운데 VM 본체(머리 줄·디스크 칸·CD 칸·부팅 순서), 왼쪽
// 포트에 CPU·메모리(눌러 고르는 칩), 오른쪽 포트에 NIC, 아래에 첫 부팅·보호·배치·보안 모듈. 칸은 놓을 자리
// (`useDropSlot`)이고 문제는 그 칸에(오류 빨강·경고 노랑 — 아이콘과 글도). 칸마다 "+" 단추가 기본 부품을 붙인다(끌지 않는 길).
// 블록(디스크·CD·NIC 한 줄)은 잡는 손잡이·누르면 그림 위 카드(`Inspector`)·`⋮` 메뉴(위로·아래로·빼기)다.
import {
  CircleX,
  Cpu,
  Disc,
  EllipsisVertical,
  GripVertical,
  HardDrive,
  MemoryStick,
  Network,
  Pencil,
  Plus,
  ShieldCheck,
  TriangleAlert,
} from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { useDnd } from "@/dnd/DndProvider";
import { useDraggable } from "@/dnd/useDraggable";
import { useDropSlot } from "@/dnd/useDropSlot";
import { formatMib } from "@/lib/format";
import { cn } from "@/lib/utils";
import { bootList } from "@/vms/boot";
import { useBuilder } from "@/vmBuilder/context";
import { Inspector } from "@/vmBuilder/Inspector";
import { blockLabel, problemSlot, problemText, slotLabel, type Thing } from "@/vmBuilder/labels";
import { accepts } from "@/vmBuilder/model/accepts";
import { memoryFit } from "@/vmBuilder/model/parts";
import type { Problem } from "@/vmBuilder/model/problems";
import { defaultNetwork, defaultPool } from "@/vmBuilder/model/purposes";
import type { Block, ListKind } from "@/vmBuilder/model/slots";
import { formOnly } from "@/vmBuilder/model/state";
import { MAX_CDROMS } from "@/vms/form";

/** 칸 하나: 놓을 자리, 제목(아이콘), 비었을 때 안내, 그 칸의 문제 */
function SlotBox({
  id,
  icon,
  empty,
  add,
  children,
  inspect = true,
  plain = false,
  className,
}: {
  id: string;
  icon?: ReactNode;
  /** 비었을 때 안내(없으면 안내 없음) */
  empty?: boolean;
  /** 제목 줄의 "+" 단추(기본 부품 붙이기) */
  add?: ReactNode;
  children?: ReactNode;
  inspect?: boolean;
  /** 비었을 때 안내를 점선 상자 없이(작은 모듈) */
  plain?: boolean;
  className?: string;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const label = slotLabel(t, id, b.s);
  // VM 본체는 제목 줄 대신 그림의 머리 줄이 이름을 보인다(제목은 화면 읽기·초점용으로 남김).
  const quietHeading = id === "vm" && !inspect && !add;
  const { ref, props, verdict } = useDropSlot<HTMLDivElement>(id, label);
  const own = b.list.filter((p) => problemSlot(p.at) === id);
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
      className={cn("flex min-w-0 flex-col gap-1.5 rounded-lg border bg-card p-2", className)}
    >
      <div
        className={cn("flex min-h-7 items-center justify-between gap-1", quietHeading && "sr-only")}
      >
        <h3
          id={`vb-${id}`}
          tabIndex={-1}
          className="flex min-w-0 items-center gap-1.5 text-xs font-medium text-muted-foreground"
        >
          {icon}
          <span className="truncate">{label}</span>
        </h3>
        <span className="flex shrink-0 items-center">
          {add}
          {inspect && (
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("vmBuilder.page.inspectSlot", { slot: label })}
              title={t("vmBuilder.page.inspectSlot", { slot: label })}
              onClick={() => b.select(id)}
            >
              <Pencil />
            </Button>
          )}
        </span>
      </div>
      {children}
      {empty && (
        <p
          className={cn(
            "text-xs text-muted-foreground",
            !plain && "rounded-md border border-dashed px-2 py-1.5",
          )}
        >
          {t(`vmBuilder.empty.${id}`)}
        </p>
      )}
      {dnd.state.phase === "picked" && verdict?.ok && (
        <span className="flex min-h-9 items-center justify-center rounded-md border border-dashed text-sm font-medium">
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

/** "+" 단추: 칸 이름을 읽는 작은 단추 */
function AddButton({
  label,
  disabled,
  onClick,
}: {
  label: string;
  disabled?: boolean;
  onClick(): void;
}) {
  const b = useBuilder();
  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-sm"
      aria-label={label}
      title={label}
      disabled={disabled || b.busy}
      onClick={onClick}
    >
      <Plus />
    </Button>
  );
}

const CORES = [1, 2, 4, 8, 16];
const MEMORY_GIB = [1, 2, 4, 8, 16, 32];

/** 눌러 고르는 값 칩 줄(지금 값은 눌린 칩) */
function Chips({
  label,
  values,
  current,
  unit,
  onPick,
}: {
  label: string;
  values: number[];
  current: number;
  unit: (n: number) => string;
  onPick(n: number): void;
}) {
  const b = useBuilder();
  return (
    <div role="group" aria-label={label} className="flex flex-wrap gap-1">
      {values.map((n) => (
        <button
          key={n}
          type="button"
          aria-pressed={n === current}
          aria-label={`${label} ${unit(n)}`}
          disabled={b.busy}
          onClick={() => onPick(n)}
          className="min-w-7 rounded-md border px-1 py-0.5 text-xs aria-pressed:border-brand aria-pressed:bg-brand aria-pressed:text-brand-foreground"
        >
          {n}
        </button>
      ))}
    </div>
  );
}

/** 왼쪽 포트: CPU와 메모리(칩으로 고름 — 다른 값·CPU 모델은 "고치기") */
function Compute() {
  const { t } = useTranslation();
  const b = useBuilder();
  const v = b.s.values;
  const vcpus = v.sockets * (Number(v.cores) || 0);
  const mib = Number(v.memory) || 0;
  const fit = memoryFit(b.ctx, v.node, mib);
  const set = (patch: Partial<typeof v>) => b.record({ ...b.s, values: { ...v, ...patch } });
  return (
    <SlotBox
      id="compute"
      icon={<Cpu aria-hidden className="size-3.5" />}
      className="vb-cmp vb-port vb-port-t"
    >
      <p className="sr-only">
        {t("vmBuilder.page.compute", { vcpus, memory: formatMib(mib), model: v.model })}
      </p>
      <div className="grid gap-2 sm:grid-cols-2">
        <div className="flex flex-col gap-1">
          <span className="flex items-baseline gap-1.5 text-xs text-muted-foreground">
            {t("vmBuilder.pic.cores")}
            <span className="text-base font-semibold text-foreground">{vcpus}</span>
          </span>
          <Chips
            label={t("vmBuilder.pic.cores")}
            values={CORES}
            current={v.sockets === 1 ? Number(v.cores) : 0}
            unit={(n) => t("vmBuilder.pic.vcpu", { count: n })}
            onPick={(n) => set({ sockets: 1, cores: String(n) })}
          />
          <span className="truncate text-xs text-muted-foreground">
            {t("vmBuilder.check.cpu", { model: v.model })}
          </span>
        </div>
        <div className="flex flex-col gap-1">
          <span className="flex items-baseline gap-1.5 text-xs text-muted-foreground">
            <MemoryStick aria-hidden className="size-3.5 self-center" />
            {t("vmBuilder.pic.memory")}
            <span className="text-base font-semibold text-foreground">{formatMib(mib)}</span>
          </span>
          <Chips
            label={t("vmBuilder.pic.memory")}
            values={MEMORY_GIB}
            current={mib % 1024 === 0 ? mib / 1024 : 0}
            unit={(n) => `${n} GiB`}
            onPick={(n) => set({ memory: String(n * 1024) })}
          />
          {fit && (
            <span
              className="text-xs text-muted-foreground"
              data-level={fit === "over" ? "warn" : undefined}
            >
              {t(`vmBuilder.pic.fit.${fit}`)}
            </span>
          )}
        </div>
      </div>
    </SlotBox>
  );
}

export function Chassis({ narrow }: { narrow: boolean }) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const v = b.s.values;
  const holdingBlock =
    (dnd.state.phase === "dragging" || dnd.state.phase === "picked") &&
    (dnd.state.item as Thing).kind === "block";
  const node = b.ctx.nodes?.find((n) => n.id === v.node);
  const template = b.s.base.kind === "template";
  // 그림 위 칩(설계 §8.6): 템플릿 바탕이면 놓을 때 비운 칸, 아니면 고급에서 고친 칸
  const carried = template ? (b.s.cleared ?? []) : formOnly(v);
  const boot = bootList(
    v.disks.map((d) => (d.bootOrder ? Number(d.bootOrder) : null)),
    v.cdroms.map((c) => (c.bootOrder ? Number(c.bootOrder) : null)),
  );
  const bootText = boot.booting
    .map((d) => t(`vmBuilder.pic.boot.${d.kind}`, { n: d.index + 1 }))
    .join(" → ");
  // "+" 단추: 기본 부품을 그 목록 끝에 붙인다(놓기와 같은 규칙·알림).
  const addPart = (list: ListKind) => {
    const part =
      list === "disks"
        ? defaultPool(b.s, b.ctx)
        : list === "nics"
          ? defaultNetwork(b.s, b.ctx)
          : undefined;
    if (part) {
      const said = b.put(part as Thing, list);
      dnd.announce(said ?? t("dnd.dropped", { item: part.name, slot: slotLabel(t, list, b.s) }));
    } else if (list === "cdroms") {
      b.record({ ...b.s, values: { ...v, cdroms: [...v.cdroms, { iso: "", bootOrder: "" }] } });
      dnd.announce(t("vmBuilder.page.count.cdroms", { count: v.cdroms.length + 1 }));
    }
  };
  const security = [
    v.firmware === "uefi" ? "UEFI" : "BIOS",
    v.secureBoot && t("vmBuilder.pic.secureBoot"),
    v.tpm && "TPM",
    v.guestAgent && t("vmBuilder.pic.agent"),
  ].filter(Boolean);

  return (
    <section
      aria-label={t("vmBuilder.pic.title")}
      className="vb-col relative flex min-w-0 flex-col gap-2"
    >
      <h2 className="sr-only">{t("vmBuilder.pic.title")}</h2>
      {carried.length > 0 && (
        <p className="self-start rounded-full border bg-muted px-3 py-1 text-xs">
          {t(template ? "vmBuilder.page.formCleared" : "vmBuilder.page.formOnly", {
            count: carried.length,
            names: carried.map((k) => t(`vmBuilder.carried.${k}`)).join(", "),
          })}
        </p>
      )}
      <div className="vb-pic">
        <Compute />
        <SlotBox
          id="vm"
          inspect={template}
          className="vb-chassis vb-box gap-2 border-foreground/25 p-3 shadow-sm"
        >
          <div className="flex items-center gap-2 rounded-md bg-muted px-2 py-1.5">
            <span aria-hidden className="flex gap-1">
              <span className="vb-led" style={{ background: "var(--success)" }} />
              <span className="vb-led" />
              <span className="vb-led" />
            </span>
            <span className="min-w-0 truncate text-sm font-medium">
              {v.name || t("vmBuilder.pic.unnamed")}
            </span>
            <span className="ml-auto truncate text-xs text-muted-foreground">
              {template
                ? t("vmBuilder.page.baseTemplate", {
                    name:
                      b.ctx.vms.find((x) => x.id === (b.s.base as { id: string }).id)?.name ?? "",
                  })
                : t(`vmBuilder.pic.preset.${b.s.preset}`)}
            </span>
          </div>
          <ListSlot
            list="disks"
            icon={<HardDrive aria-hidden className="size-3.5" />}
            add={
              !template && (
                <AddButton label={t("vmBuilder.pic.addDisk")} onClick={() => addPart("disks")} />
              )
            }
          />
          <ListSlot
            list="cdroms"
            icon={<Disc aria-hidden className="size-3.5" />}
            add={
              !template && (
                <AddButton
                  label={t("vmBuilder.pic.addCdrom")}
                  disabled={v.cdroms.length >= MAX_CDROMS}
                  onClick={() => addPart("cdroms")}
                />
              )
            }
          />
          {bootText && (
            <p className="text-xs text-muted-foreground">
              {t("vmBuilder.pic.bootOrder", { order: bootText })}
            </p>
          )}
        </SlotBox>
        <div className="vb-nic flex min-w-0 flex-col gap-2">
          <ListSlot
            list="nics"
            icon={<Network aria-hidden className="size-3.5" />}
            className="vb-port vb-port-r"
            add={<AddButton label={t("vmBuilder.pic.addNic")} onClick={() => addPart("nics")} />}
          />
          <SlotBox id="cloudInit" plain empty={!v.cloudInit.enabled}>
            {v.cloudInit.enabled && (
              <p className="text-sm">
                {v.cloudInit.user || t("vmBuilder.page.ciOn")}
                {v.cloudInit.sshKeys.trim() !== "" && ` · ${t("vmBuilder.page.keys")}`}
              </p>
            )}
          </SlotBox>
          <SlotBox id="protection" plain empty={!v.ha}>
            {v.ha && (
              <p className="text-sm">
                {v.haGroup ? `${t("vmBuilder.part.ha")} · ${v.haGroup}` : t("vmBuilder.part.ha")}
              </p>
            )}
          </SlotBox>
        </div>
      </div>
      <div className="grid grid-cols-2 gap-2">
        <SlotBox id="placement">
          <p className="truncate text-sm">
            {b.s.zone
              ? t("vmBuilder.part.zone", { name: b.s.zone.value })
              : node
                ? node.name
                : t("vmBuilder.part.auto")}
          </p>
        </SlotBox>
        <div className="flex min-w-0 flex-col gap-1.5 rounded-lg border bg-card p-2">
          <h3 className="flex min-h-7 items-center gap-1.5 text-xs font-medium text-muted-foreground">
            <ShieldCheck aria-hidden className="size-3.5" />
            {t("vmBuilder.pic.security")}
          </h3>
          <p className="truncate text-sm">{security.join(" · ")}</p>
          <Button
            type="button"
            variant="link"
            size="sm"
            className="h-auto self-start p-0 text-xs"
            onClick={b.openAdvanced}
          >
            {t("vmBuilder.pic.inAdvanced")}
          </Button>
        </div>
      </div>
      {holdingBlock && <SlotBox id="trash" inspect={false} empty />}
      {b.selected && (
        <div
          className={cn(
            !narrow && "absolute top-0 right-0 z-10 max-h-full w-80 overflow-y-auto shadow-lg",
          )}
        >
          <Inspector />
        </div>
      )}
    </section>
  );
}

function ListSlot({
  list,
  icon,
  add,
  className,
}: {
  list: ListKind;
  icon: ReactNode;
  add?: ReactNode;
  className?: string;
}) {
  const b = useBuilder();
  const rows = b.s.values[list];
  return (
    <SlotBox
      id={list}
      icon={icon}
      add={add}
      empty={rows.length === 0}
      inspect={false}
      className={className}
    >
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
      className="flex flex-col gap-1 rounded-md border bg-background p-0.5"
    >
      <div className="flex items-center gap-0.5">
        <button
          type="button"
          {...drag}
          disabled={b.busy}
          aria-label={t("vmBuilder.page.grip", { name: label })}
          className="vb-part flex size-7 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-muted"
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
          className="min-w-0 flex-1 truncate rounded-md px-1 py-1 text-left text-xs hover:bg-muted"
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
