// 키보드 이웃(계획 70 §10.3): 초점은 두 단계다 — 노드 단계(화살표가 판 안 격자의 이웃, 판 끝에서는 옆 판의 가장 가까운
// 노드)와 타일 단계(한 노드 위 타일 사이). `]`가 노드의 첫 타일로 들어가고 `[`·`Esc`가 노드로 나온다.
import { nodeTop, type MapScene, type NodeItem, type TileCell } from "@/clusterMap/layout";

export type Focus = { kind: "node"; key: string } | { kind: "tile"; node: string; key: string };

export type NavKey =
  "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" | "Home" | "End" | "[" | "]" | "Escape";

const NAV_KEYS: readonly string[] = [
  "ArrowLeft",
  "ArrowRight",
  "ArrowUp",
  "ArrowDown",
  "Home",
  "End",
  "[",
  "]",
  "Escape",
];

export function isNavKey(key: string): key is NavKey {
  return NAV_KEYS.includes(key);
}

/** 판 차례(장면의 판 목록 차례) */
function slabIndex(scene: MapScene): Map<string, number> {
  return new Map(scene.slabs.map((s, i) => [s.key, i]));
}

/** 읽는 차례: 판 → 줄(증인 −1 먼저) → 열. Home·End와 처음 초점이 쓴다. */
export function readingOrder(scene: MapScene): NodeItem[] {
  const slab = slabIndex(scene);
  return [...scene.nodes].sort(
    (a, b) => slab.get(a.slab)! - slab.get(b.slab)! || a.row - b.row || a.col - b.col,
  );
}

/** 초점을 받을 수 있는 칸(유령 타일 — 옮겨 오는 VM의 미리 보기 — 은 아님) */
export function focusableCells(node: NodeItem): TileCell[] {
  return (node.grid?.cells ?? []).filter((c) => c.kind !== "ghost");
}

const centerX = (n: NodeItem) => n.box.x + n.box.w / 2;

function nearest(from: NodeItem, candidates: readonly NodeItem[], world = false): NodeItem | null {
  const a = nodeTop(from);
  let best: NodeItem | null = null;
  let bestD = Infinity;
  for (const c of candidates) {
    let d: number;
    if (world) {
      d = Math.abs(centerX(c) - centerX(from));
    } else {
      const b = nodeTop(c);
      d = Math.hypot(b.x - a.x, b.y - a.y);
    }
    if (d < bestD || (d === bestD && best !== null && c.col < best.col)) {
      best = c;
      bestD = d;
    }
  }
  return best;
}

function nodeNeighbor(scene: MapScene, at: NodeItem, key: NavKey): NodeItem | null {
  const slabs = scene.slabs;
  const si = slabs.findIndex((s) => s.key === at.slab);
  const inSlab = (key: string) => scene.nodes.filter((n) => n.slab === key);
  const here = inSlab(at.slab);
  switch (key) {
    case "ArrowLeft":
    case "ArrowRight": {
      const step = key === "ArrowRight" ? 1 : -1;
      const same = here.find((n) => n.row === at.row && n.col === at.col + step);
      if (same) {
        return same;
      }
      // 판 끝: 같은 판 줄(같은 y)의 옆 판, 마주 보는 가장자리 노드 중 가장 가까운 것
      const next = slabs[si + step];
      if (!next || next.box.y !== slabs[si]!.box.y) {
        return null;
      }
      const there = inSlab(next.key);
      const edge = there.filter((n) =>
        step > 0 ? n.col === 0 : !there.some((m) => m.row === n.row && m.col > n.col),
      );
      return nearest(at, edge);
    }
    case "ArrowUp":
    case "ArrowDown": {
      const down = key === "ArrowDown";
      const rows = [...new Set(here.map((n) => n.row))].sort((a, b) => a - b);
      const target = rows[rows.indexOf(at.row) + (down ? 1 : -1)];
      if (target !== undefined) {
        return nearest(
          at,
          here.filter((n) => n.row === target),
          true,
        );
      }
      // 판 끝: 앞(뒤) 판 줄의 판들, 마주 보는 줄의 가장 가까운 노드
      const y = slabs[si]!.box.y;
      const ys = [...new Set(slabs.map((s) => s.box.y))].sort((a, b) => a - b);
      const ny = ys[ys.indexOf(y) + (down ? 1 : -1)];
      if (ny === undefined) {
        return null;
      }
      const edge = slabs
        .filter((s) => s.box.y === ny)
        .flatMap((s) => {
          const there = inSlab(s.key);
          if (there.length === 0) {
            return [];
          }
          const r = down
            ? Math.min(...there.map((n) => n.row))
            : Math.max(...there.map((n) => n.row));
          return there.filter((n) => n.row === r);
        });
      return nearest(at, edge);
    }
    default:
      return null;
  }
}

function tileNeighbor(
  node: NodeItem,
  cells: readonly TileCell[],
  at: TileCell,
  key: NavKey,
): TileCell | null {
  const i = cells.indexOf(at);
  const per = node.grid?.perLayer ?? 1;
  switch (key) {
    case "ArrowLeft":
      return cells[i - 1] ?? null;
    case "ArrowRight":
      return cells[i + 1] ?? null;
    case "ArrowUp": {
      // 위 층 같은 자리, 위 층이 덜 찼으면 그 층의 끝 칸, 맨 위 층이면 멈춤
      const upper = cells.filter((c) => c.layer === at.layer + 1);
      return upper.find((c) => c.index === at.index + per) ?? upper[upper.length - 1] ?? null;
    }
    case "ArrowDown":
      return cells.find((c) => c.layer === at.layer - 1 && c.index === at.index - per) ?? null;
    case "Home":
      return cells[0] ?? null;
    case "End":
      return cells[cells.length - 1] ?? null;
    default:
      return null;
  }
}

/**
 * 키 하나 → 다음 초점. 움직일 곳이 없으면 `null`(초점 그대로). 초점이 없거나 장면에서 사라졌으면 첫 노드.
 * 노드 단계: ←·→ 같은 줄의 열, ↑·↓ 줄(판 끝에서는 옆·앞뒤 판), Home·End 첫·끝 노드, `]` 첫 타일.
 * 타일 단계: ←·→ 이름 순 앞뒤 칸, ↑·↓ 위·아래 층의 같은 자리, Home·End 첫·끝 칸, `[`·Esc 노드로.
 */
export function neighbor(scene: MapScene, focus: Focus | null, key: NavKey): Focus | null {
  const order = readingOrder(scene);
  const nodeAt = focus
    ? scene.byKey.get(focus.kind === "node" ? focus.key : focus.node)
    : undefined;
  if (!nodeAt) {
    const first = order[0];
    return first ? { kind: "node", key: first.key } : null;
  }
  if (focus!.kind === "node") {
    switch (key) {
      case "Home":
      case "End": {
        const n = key === "Home" ? order[0]! : order[order.length - 1]!;
        return n === nodeAt ? null : { kind: "node", key: n.key };
      }
      case "]": {
        const first = focusableCells(nodeAt)[0];
        return first ? { kind: "tile", node: nodeAt.key, key: first.key } : null;
      }
      case "[":
      case "Escape":
        return null;
      default: {
        const n = nodeNeighbor(scene, nodeAt, key);
        return n ? { kind: "node", key: n.key } : null;
      }
    }
  }
  if (key === "[" || key === "Escape") {
    return { kind: "node", key: nodeAt.key };
  }
  const cells = focusableCells(nodeAt);
  const at = cells.find((c) => c.key === focus!.key);
  if (!at) {
    return { kind: "node", key: nodeAt.key };
  }
  const next = tileNeighbor(nodeAt, cells, at, key);
  return next && next !== at ? { kind: "tile", node: nodeAt.key, key: next.key } : null;
}
