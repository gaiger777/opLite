import { cn } from "cn";
import { ChevronDown, ChevronRight, Folder as FolderIcon, FolderOpen } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { type DragPayload, DRAG_TYPE, type FolderTree as Tree, ROOT_ID } from "@/folders/folders";

/** 지금 끄는 객체. `dragover`에서는 `dataTransfer`를 읽을 수 없으므로 여기에 둔다. */
let dragging: DragPayload | null = null;

/** 행을 끌기 시작할 때 부른다(`draggable` 행의 `onDragStart`). */
export function startDrag(e: React.DragEvent, payload: DragPayload) {
  dragging = payload;
  e.dataTransfer.effectAllowed = "move";
  e.dataTransfer.setData(DRAG_TYPE, JSON.stringify(payload));
  e.dataTransfer.setData("text/plain", String(payload.ids.length));
}

export function endDrag() {
  dragging = null;
}

/** `open`에 `id`와 그 조상을 더한 것 */
function withAncestors(open: ReadonlySet<string>, tree: Tree, id: string): ReadonlySet<string> {
  const out = new Set(open).add(id);
  let at = tree.byId.get(id);
  while (at?.spec.parent && tree.byId.has(at.spec.parent)) {
    out.add(at.spec.parent);
    at = tree.byId.get(at.spec.parent);
  }
  return out;
}

interface Row {
  id: string;
  name: string;
  level: number;
  parent: string | null;
  hasChildren: boolean;
}

/**
 * 폴더 트리(설계 §13 웹): `role="tree"`, 펼친 노드만 그린다. 화살표로 옮기고(→ 펼치기·첫 자식, ← 접기·부모),
 * `Home`·`End`, `Enter`·`Space`로 고른다. 행을 끌어다 놓으면 옮긴다(`canDrop`이 이유를 주면 놓을 수 없고, 놓기 전에
 * 이유를 보인다).
 */
export function FolderTree({
  tree,
  rootLabel,
  selected,
  label,
  onSelect,
  canDrop,
  onDrop,
}: {
  tree: Tree;
  rootLabel: string;
  /** 고른 노드(`ROOT_ID` 포함) */
  selected: string;
  /** 트리의 이름 */
  label: string;
  onSelect(id: string): void;
  /** 이 노드에 놓을 수 없는 이유(번역 키), 놓을 수 있으면 null */
  canDrop?(id: string, payload: DragPayload): string | null;
  onDrop?(id: string, payload: DragPayload): void;
}) {
  const { t } = useTranslation();
  // 고른 폴더와 그 조상은 펼친다.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() =>
    withAncestors(new Set([ROOT_ID]), tree, selected),
  );
  const [focused, setFocused] = useState(selected);
  // 처음 뒤에 고른 폴더(새로 만든 하위 폴더, 앞·뒤로 가기)도 조상을 펼쳐 보이게 한다. 새 폴더가 아직 트리에 없으면
  // 들어온 뒤에 한 번 한다(그 뒤에 사용자가 접은 것은 그대로 둔다).
  const [revealed, setRevealed] = useState(selected);
  if (revealed !== selected && (selected === ROOT_ID || tree.byId.has(selected))) {
    setRevealed(selected);
    setExpanded((open) => withAncestors(open, tree, selected));
    setFocused(selected);
  }
  const [over, setOver] = useState<{ id: string; reason: string | null } | null>(null);
  const items = useRef(new Map<string, HTMLElement>());

  const rows = useMemo(() => {
    const out: Row[] = [
      {
        id: ROOT_ID,
        name: rootLabel,
        level: 1,
        parent: null,
        hasChildren: (tree.children.get(ROOT_ID) ?? []).length > 0,
      },
    ];
    const walk = (parent: string, level: number) => {
      if (!expanded.has(parent)) {
        return;
      }
      for (const f of tree.children.get(parent) ?? []) {
        out.push({
          id: f.id,
          name: f.spec.name,
          level,
          parent,
          hasChildren: (tree.children.get(f.id) ?? []).length > 0,
        });
        walk(f.id, level + 1);
      }
    };
    walk(ROOT_ID, 2);
    return out;
  }, [tree, expanded, rootLabel]);

  // 고른 노드가 지워졌거나 접혀 보이지 않으면 포커스 자리를 고른 노드(없으면 루트)로 둔다.
  const focusId = rows.some((r) => r.id === focused)
    ? focused
    : rows.some((r) => r.id === selected)
      ? selected
      : ROOT_ID;

  const toggle = (id: string, open: boolean) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (open) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });
  const move = (id: string) => {
    setFocused(id);
    items.current.get(id)?.focus();
  };

  function onKeyDown(e: React.KeyboardEvent, row: Row, index: number) {
    const open = expanded.has(row.id);
    switch (e.key) {
      case "ArrowDown":
        if (index + 1 < rows.length) {
          move(rows[index + 1]!.id);
        }
        break;
      case "ArrowUp":
        if (index > 0) {
          move(rows[index - 1]!.id);
        }
        break;
      case "ArrowRight":
        if (row.hasChildren && !open) {
          toggle(row.id, true);
        } else if (row.hasChildren && rows[index + 1]) {
          move(rows[index + 1]!.id);
        }
        break;
      case "ArrowLeft":
        if (row.hasChildren && open) {
          toggle(row.id, false);
        } else if (row.parent !== null) {
          move(row.parent);
        }
        break;
      case "Home":
        move(rows[0]!.id);
        break;
      case "End":
        move(rows[rows.length - 1]!.id);
        break;
      case "Enter":
      case " ":
        onSelect(row.id);
        break;
      default:
        return;
    }
    e.preventDefault();
  }

  const accepts = (e: React.DragEvent) =>
    dragging !== null && e.dataTransfer.types.includes(DRAG_TYPE);

  return (
    <ul role="tree" aria-label={label} className="flex flex-col gap-0.5 text-sm">
      {rows.map((row, index) => {
        const open = expanded.has(row.id);
        const isOver = over?.id === row.id;
        const Icon = open && row.hasChildren ? FolderOpen : FolderIcon;
        return (
          <li
            key={row.id}
            role="treeitem"
            aria-level={row.level}
            aria-expanded={row.hasChildren ? open : undefined}
            aria-selected={row.id === selected}
            tabIndex={row.id === focusId ? 0 : -1}
            ref={(el) => {
              if (el) {
                items.current.set(row.id, el);
              } else {
                items.current.delete(row.id);
              }
            }}
            data-drop={isOver ? (over.reason ? "no" : "ok") : undefined}
            onKeyDown={(e) => {
              // 안의 펼치기 단추에서 온 키도 받는다(같은 행).
              onKeyDown(e, row, index);
            }}
            onFocus={(e) => {
              if (e.target === e.currentTarget) {
                setFocused(row.id);
              }
            }}
            onClick={() => onSelect(row.id)}
            onDragOver={(e) => {
              if (!accepts(e) || !canDrop) {
                return;
              }
              const reason = canDrop(row.id, dragging!);
              if (!isOver || over.reason !== reason) {
                setOver({ id: row.id, reason });
              }
              if (reason === null) {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
              } else {
                e.dataTransfer.dropEffect = "none";
              }
            }}
            onDragLeave={() => isOver && setOver(null)}
            onDrop={(e) => {
              e.preventDefault();
              setOver(null);
              const payload = dragging;
              dragging = null;
              if (payload && canDrop?.(row.id, payload) === null) {
                onDrop?.(row.id, payload);
              }
            }}
            className={cn(
              "flex min-h-9 cursor-pointer items-center gap-1 rounded-md pr-2 outline-none select-none",
              "hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
              "aria-selected:bg-muted aria-selected:font-medium",
              "data-[drop=ok]:bg-brand/15 data-[drop=ok]:ring-2 data-[drop=ok]:ring-brand",
              "data-[drop=no]:bg-destructive/10",
            )}
            style={{ paddingLeft: `${(row.level - 1) * 1}rem` }}
          >
            {row.hasChildren ? (
              <button
                type="button"
                tabIndex={-1}
                aria-hidden
                className="flex size-6 shrink-0 items-center justify-center text-muted-foreground"
                onClick={(e) => {
                  e.stopPropagation();
                  toggle(row.id, !open);
                }}
              >
                {open ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
              </button>
            ) : (
              <span className="size-6 shrink-0" aria-hidden />
            )}
            <Icon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
            <span className="min-w-0 truncate">{row.name}</span>
            {isOver && over.reason && (
              <span role="status" className="ml-auto shrink-0 text-xs text-destructive">
                {t(over.reason)}
              </span>
            )}
          </li>
        );
      })}
    </ul>
  );
}
