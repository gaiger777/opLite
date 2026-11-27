// 부품 상자(계획 71 설계 §8.2·§8.3·§11, 계획 77-5): 거르기 칸과 접는 짧은 묶음 여섯(디스크·설치 매체·네트워크·첫 부팅·
// 배치·보호 — 크기·OS는 용도 카드와 그림의 칩, CPU 모델은 고급, 템플릿은 용도 카드, 폴더는 요약), 묶음마다 넷까지 보이고
// "n개 더". 부품 단추는 **누르면 붙는다**(그 부품을 받는 첫 칸 — ISO는 빈 CD 드라이브 먼저), 끌어도 되고 키보드로 들어도
// 된다. "⋯"은 붙일 곳 고르기(끌지 않는 길). 묶음 안은 화살표 ↑·↓(탭 차례 하나). 좁은 화면은 아래 서랍이다.
import { Ellipsis } from "lucide-react";
import { type KeyboardEvent, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { useDnd } from "@/dnd/DndProvider";
import { useDraggable } from "@/dnd/useDraggable";
import { readUserPref, writeUserPref } from "@/lib/prefs";
import { cn } from "@/lib/utils";
import { useBuilder } from "@/vmBuilder/context";
import { partInfo, partLabel, slotLabel, type Thing } from "@/vmBuilder/labels";
import { accepts } from "@/vmBuilder/model/accepts";
import { type Part, type PartGroup, parts } from "@/vmBuilder/model/parts";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import { slots } from "@/vmBuilder/model/slots";

/** 부품 상자에 보이는 묶음(차례대로) */
export const BOX_GROUPS: readonly PartGroup[] = [
  "disk",
  "media",
  "network",
  "cloudInit",
  "placement",
  "protection",
];

/** 묶음마다 처음 보이는 부품 수(나머지는 "n개 더") */
const FIRST = 4;

/** 누르면 붙는 칸: 받는 첫 칸(빼기 칸 빼고) — ISO는 빈 CD 드라이브가 있으면 그것 */
export function defaultSlot(thing: Thing, s: BuilderState, ctx: BuilderCtx): string | null {
  const ids = slots(s)
    .map((x) => x.id)
    .filter((id) => id !== "trash" && accepts(thing, id, s, ctx).ok);
  if (thing.kind === "iso") {
    const empty = s.values.cdroms.findIndex((c) => !c.iso);
    if (empty !== -1 && ids.includes(`cdroms.${empty}`)) {
      return `cdroms.${empty}`;
    }
  }
  return ids[0] ?? null;
}

export function PartsPanel({ narrow }: { narrow: boolean }) {
  const { t } = useTranslation();
  const dnd = useDnd();
  const [open, setOpen] = useState(false);
  // 끌거나 키보드로 들면 서랍을 닫아 그림의 놓을 칸이 보이게 한다(설계 §8.7 — 그리는 중에 상태를 맞춤). 누르기는 곧
  // 붙이므로(그 뒤 닫음) 들었을 때 닫지 않는다 — 닫으면 누른 부품이 사라져 붙이는 click이 오지 않는다.
  const held = dnd.state.phase === "dragging" || (dnd.state.phase === "picked" && dnd.state.key);
  const [wasHeld, setWasHeld] = useState(held);
  if (held !== wasHeld) {
    setWasHeld(held);
    if (held) {
      setOpen(false);
    }
  }
  if (!narrow) {
    return (
      <section aria-labelledby="vb-parts" className="vb-col flex flex-col gap-2">
        <h2 id="vb-parts" className="text-sm font-medium">
          {t("vmBuilder.page.parts")}
        </h2>
        <p className="text-xs text-muted-foreground">{t("vmBuilder.page.partsHint")}</p>
        <div className="vb-scroll">
          <PartList />
        </div>
      </section>
    );
  }
  return (
    <div>
      <Button type="button" variant="outline" className="w-full" onClick={() => setOpen(true)}>
        {t("vmBuilder.page.addParts")}
      </Button>
      <Sheet open={open} onOpenChange={setOpen}>
        <SheetContent side="bottom" style={{ height: "70vh" }} className="overflow-y-auto p-4">
          <SheetTitle>{t("vmBuilder.page.parts")}</SheetTitle>
          <PartList onAttached={() => setOpen(false)} />
        </SheetContent>
      </Sheet>
    </div>
  );
}

function PartList({ onAttached }: { onAttached?(): void }) {
  const { t } = useTranslation();
  const b = useBuilder();
  const { user } = usePrincipal();
  const [filter, setFilter] = useState("");
  const [closed, setClosed] = useState<string[]>(() => {
    try {
      const v: unknown = JSON.parse(readUserPref(user, "vmBuilder.groups") ?? "[]");
      return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
    } catch {
      return [];
    }
  });
  const toggle = (g: string) => {
    const next = closed.includes(g) ? closed.filter((x) => x !== g) : [...closed, g];
    setClosed(next);
    writeUserPref(user, "vmBuilder.groups", JSON.stringify(next));
  };
  const all = useMemo(
    () => parts(b.s, b.ctx).filter((p) => BOX_GROUPS.includes(p.group)),
    [b.s, b.ctx],
  );
  const q = filter.trim().toLowerCase();
  const shown = q ? all.filter((p) => partLabel(t, p).toLowerCase().includes(q)) : all;
  const groups = new Map<PartGroup, Part[]>();
  for (const g of BOX_GROUPS) {
    // 배치는 자동·장애 영역(적음)을 노드(많을 수 있음)보다 먼저 보인다.
    const list = shown
      .filter((p) => p.group === g)
      .sort((a, b) =>
        g === "placement" ? Number(a.kind === "node") - Number(b.kind === "node") : 0,
      );
    if (list.length > 0) {
      groups.set(g, list);
    }
  }
  return (
    <div className="flex flex-col gap-3" data-dnd-scroll="">
      <Input
        type="search"
        aria-label={t("vmBuilder.page.filter")}
        placeholder={t("vmBuilder.page.filter")}
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />
      {shown.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("vmBuilder.page.noParts")}</p>
      )}
      {[...groups].map(([g, list]) => (
        <Group
          key={g}
          group={g}
          list={list}
          open={q !== "" || !closed.includes(g)}
          everything={q !== ""}
          onToggle={() => toggle(g)}
          onAttached={onAttached}
        />
      ))}
      {/* 노드를 읽지 못하면 배치 묶음이 없다(설계 §13 — 요약의 노드 칸이 "서버가 고름"을 알린다). */}
    </div>
  );
}

function Group({
  group,
  list: whole,
  open,
  everything,
  onToggle,
  onAttached,
}: {
  group: PartGroup;
  list: Part[];
  open: boolean;
  /** 거르는 중이면 모두 보인다. */
  everything: boolean;
  onToggle(): void;
  onAttached?(): void;
}) {
  const { t } = useTranslation();
  const dnd = useDnd();
  const [active, setActive] = useState<string | null>(null);
  const [more, setMore] = useState(false);
  const list = everything || more ? whole : whole.slice(0, FIRST);
  const current = list.some((p) => p.id === active) ? active : list[0]!.id;
  // 묶음 안은 화살표로 옮긴다(든 채면 끌기 틀이 화살표로 칸을 고른다).
  const arrows = (e: KeyboardEvent<HTMLUListElement>) => {
    const dir = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
    if (!dir || dnd.state.phase === "picked") {
      return;
    }
    const buttons = [...e.currentTarget.querySelectorAll<HTMLElement>("[data-part]")];
    const at = buttons.indexOf(document.activeElement as HTMLElement);
    const next = buttons[at + dir];
    if (at !== -1 && next) {
      e.preventDefault();
      next.focus();
    }
  };
  const id = `vb-group-${group}`;
  return (
    <div className="flex flex-col gap-1">
      <button
        type="button"
        aria-expanded={open}
        aria-controls={id}
        onClick={onToggle}
        className="flex items-center gap-1 text-left text-xs font-medium tracking-wide text-muted-foreground uppercase"
      >
        <span aria-hidden>{open ? "▾" : "▸"}</span>
        {t(`vmBuilder.group.${group}`)}
      </button>
      {open && (
        <ul id={id} className="flex flex-col gap-1" onKeyDown={arrows}>
          {list.map((p) => (
            <PartRow
              key={p.id}
              part={p}
              tabbable={p.id === current}
              onFocus={() => setActive(p.id)}
              onAttached={onAttached}
            />
          ))}
        </ul>
      )}
      {open && !everything && whole.length > FIRST && (
        <Button
          type="button"
          variant="link"
          size="sm"
          className="h-auto self-start p-0 text-xs"
          aria-expanded={more}
          onClick={() => setMore(!more)}
        >
          {more
            ? t("vmBuilder.page.fewer")
            : t("vmBuilder.page.morePartsN", { count: whole.length - FIRST })}
        </Button>
      )}
    </div>
  );
}

function PartRow({
  part,
  tabbable,
  onFocus,
  onAttached,
}: {
  part: Part;
  tabbable: boolean;
  onFocus(): void;
  onAttached?(): void;
}) {
  const { t } = useTranslation();
  const b = useBuilder();
  const dnd = useDnd();
  const label = partLabel(t, part);
  const info = partInfo(t, part);
  const thing: Thing = { ...part, label };
  const drag = useDraggable(thing, t("vmBuilder.part.role"));
  // "⋯ 더하기": 지금 놓을 수 있는 칸(설계 §8.3)
  const targets = () =>
    slots(b.s)
      .map((x) => x.id)
      .filter((id) => accepts(thing, id, b.s, b.ctx).ok);
  const [menu, setMenu] = useState<string[]>([]);
  // 누르기(끌지 않고 뗌)로 든 바로 뒤의 click: 받는 첫 칸에 붙인다. 받는 칸이 없으면 내려놓고 까닭을 읽는다. 키보드로 든
  // 것(Space·Enter)은 칸을 고르게 둔다.
  const attach = () => {
    const now = dnd.current();
    if (now.phase !== "picked" || now.item.id !== thing.id || now.key) {
      return;
    }
    const to = defaultSlot(thing, b.s, b.ctx);
    if (to) {
      dnd.send({ type: "drop", slot: to });
      onAttached?.();
    } else {
      dnd.send({ type: "cancel" });
      dnd.announce(part.blocked ? info : t("vmBuilder.page.noTarget"));
    }
  };
  return (
    <li className="flex items-stretch gap-1">
      <button
        type="button"
        {...drag}
        onClick={attach}
        data-part=""
        tabIndex={tabbable ? 0 : -1}
        onFocus={onFocus}
        disabled={b.busy}
        aria-label={info ? `${label}, ${info}` : label}
        className={cn(
          "vb-part flex min-h-10 min-w-0 flex-1 flex-col items-start justify-center rounded-md border bg-card px-2 py-1 text-left text-sm",
          part.blocked && "opacity-60",
        )}
      >
        <span className="truncate">{label}</span>
        {info && <span className="text-xs text-muted-foreground">{info}</span>}
      </button>
      <DropdownMenu onOpenChange={(o) => o && setMenu(targets())}>
        <DropdownMenuTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="self-center"
            tabIndex={-1}
            disabled={b.busy}
            aria-label={t("vmBuilder.page.more", { name: label })}
          >
            <Ellipsis />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          {menu.length === 0 && (
            <DropdownMenuItem disabled>{t("vmBuilder.page.noTarget")}</DropdownMenuItem>
          )}
          {menu.map((id) => (
            <DropdownMenuItem
              key={id}
              onSelect={() => {
                const slot = slotLabel(t, id, b.s);
                const extra = b.put(thing, id);
                const said = t("dnd.dropped", { item: label, slot });
                dnd.announce(extra ? `${said} ${extra}` : said);
              }}
            >
              {t("vmBuilder.page.addTo", { slot: slotLabel(t, id, b.s) })}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
  );
}
