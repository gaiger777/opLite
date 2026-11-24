// 부품 상자(계획 71 설계 §8.2·§8.3·§11): 거르기 칸과 접는 묶음, 40px 부품 단추(끌기·누르기 들기·키보드 들기)와
// "⋯ 더하기" 메뉴(지금 놓을 수 있는 칸 — 끌지 않는 길). 묶음 안은 화살표 ↑·↓(탭 차례 하나). 좁은 화면은 아래 서랍이고,
// 부품을 들면 서랍이 닫혀 틀의 놓을 칸이 보인다.
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
import { slots } from "@/vmBuilder/model/slots";

export function PartsPanel({ narrow }: { narrow: boolean }) {
  const { t } = useTranslation();
  const dnd = useDnd();
  const [open, setOpen] = useState(false);
  const held = dnd.state.phase === "picked" || dnd.state.phase === "dragging";
  // 들면 서랍을 닫아 틀의 놓을 칸이 보이게 한다(설계 §8.7 — 그리는 중에 상태를 맞춤).
  const [wasHeld, setWasHeld] = useState(held);
  if (held !== wasHeld) {
    setWasHeld(held);
    if (held) {
      setOpen(false);
    }
  }
  if (!narrow) {
    return (
      <section aria-labelledby="vb-parts" className="vb-side flex flex-col gap-2">
        <h2 id="vb-parts" className="text-sm font-medium">
          {t("vmBuilder.page.parts")}
        </h2>
        <PartList />
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
          <PartList />
        </SheetContent>
      </Sheet>
    </div>
  );
}

function PartList() {
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
  const all = useMemo(() => parts(b.s, b.ctx), [b.s, b.ctx]);
  const q = filter.trim().toLowerCase();
  const shown = q ? all.filter((p) => partLabel(t, p).toLowerCase().includes(q)) : all;
  const groups = new Map<PartGroup, Part[]>();
  for (const p of shown) {
    groups.set(p.group, [...(groups.get(p.group) ?? []), p]);
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
          onToggle={() => toggle(g)}
        />
      ))}
      {/* 노드를 읽지 못하면 배치 묶음이 없다(설계 §13). */}
      {b.ctx.nodes === null && (
        <p className="text-sm text-muted-foreground">{t("vmBuilder.page.placementServer")}</p>
      )}
    </div>
  );
}

function Group({
  group,
  list,
  open,
  onToggle,
}: {
  group: PartGroup;
  list: Part[];
  open: boolean;
  onToggle(): void;
}) {
  const { t } = useTranslation();
  const dnd = useDnd();
  const [active, setActive] = useState<string | null>(null);
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
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function PartRow({ part, tabbable, onFocus }: { part: Part; tabbable: boolean; onFocus(): void }) {
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
  return (
    <li className="flex items-stretch gap-1">
      <button
        type="button"
        {...drag}
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
