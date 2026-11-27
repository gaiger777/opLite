// 지도에서 고른 것(계획 75 §4): 홈의 자세히 판과 지도가 주고받는 모양, `?focus=` 글, 장면 열쇠 사이의 바꿈(순수).
// 홈(`dashboard/selection.ts`)도 이 모양을 다시 내보내 쓴다.
import type { Focus } from "@/clusterMap/keys";
import type { MapScene } from "@/clusterMap/layout";
import type { ClusterModel } from "@/clusterMap/model";

/**
 * 고른 것: 노드(id), VM(id), 포트(노드 id·포트 이름), 레일(`carrier` — 브리지·physnet 이름, VXLAN·오버레이는 `underlay`;
 * 그 레일 위 네트워크는 `PhysicalTopology.networks`에서 `carrier`가 같은 것).
 */
export type MapSelection =
  | { kind: "node"; id: string }
  | { kind: "vm"; id: string }
  | { kind: "port"; node: string; port: string }
  | { kind: "rail"; id: string };

/** `?focus=`(또는 홈의 `focus`)의 뜻 — 노드·VM은 이름 또는 id */
export type FocusTarget =
  | { kind: "node"; node: string }
  | { kind: "vm"; vm: string }
  | { kind: "port"; node: string; port: string }
  | { kind: "rail"; rail: string };

/** `node:<이름|id>`·`vm:<이름|id>`·`port:<노드>/<포트>`·`rail:<carrier>` → 뜻(모르면 `null`) */
export function parseFocus(text: string | null | undefined): FocusTarget | null {
  if (!text) {
    return null;
  }
  const i = text.indexOf(":");
  if (i <= 0) {
    return null;
  }
  const kind = text.slice(0, i);
  const rest = text.slice(i + 1);
  if (!rest) {
    return null;
  }
  switch (kind) {
    case "node":
      return { kind: "node", node: rest };
    case "vm":
      return { kind: "vm", vm: rest };
    case "port": {
      // 인터페이스 이름에는 `/`가 없다 — 마지막 `/` 앞이 노드
      const j = rest.lastIndexOf("/");
      return j > 0 && j < rest.length - 1
        ? { kind: "port", node: rest.slice(0, j), port: rest.slice(j + 1) }
        : null;
    }
    case "rail":
      return { kind: "rail", rail: rest };
    default:
      return null;
  }
}

/** 고른 것 → `focus` 글(노드·VM·포트는 id로) */
export function focusText(sel: MapSelection): string {
  switch (sel.kind) {
    case "node":
      return `node:${sel.id}`;
    case "vm":
      return `vm:${sel.id}`;
    case "port":
      return `port:${sel.node}/${sel.port}`;
    case "rail":
      return `rail:${sel.id}`;
  }
}

/** 장면 열쇠 → 고른 것("+N"은 그 노드, 유령·화살표는 그 VM, 배치 안 됨 판은 없음) */
export function selectionOf(key: string, scene: MapScene): MapSelection | null {
  const [kind, ...parts] = key.split(":");
  const rest = parts.join(":");
  switch (kind) {
    case "node":
      return { kind: "node", id: rest };
    case "vm":
    case "ghost":
      return { kind: "vm", id: rest };
    case "more":
    case "hidden":
    case "fold":
      return rest.startsWith("node:") ? { kind: "node", id: rest.slice(5) } : null;
    case "move":
    case "rec": {
      const link = scene.links.find((l) => l.key === key);
      return link ? { kind: "vm", id: link.vm } : null;
    }
    case "port": {
      const port = scene.ports.get(key);
      const node = port ? scene.byKey.get(port.node) : undefined;
      return port && node ? { kind: "port", node: node.id, port: port.name } : null;
    }
    case "rail": {
      const rail = scene.rails.get(key);
      return rail ? { kind: "rail", id: rail.carrier } : null;
    }
    default:
      return null;
  }
}

/** 고른 것 → 장면 열쇠(그림에 없으면 `null`; 레일은 첫 판의 것) */
export function keyOfSelection(sel: MapSelection, scene: MapScene): string | null {
  switch (sel.kind) {
    case "node":
      return scene.byKey.has(`node:${sel.id}`) ? `node:${sel.id}` : null;
    case "vm":
      return scene.tiles.has(`vm:${sel.id}`) ? `vm:${sel.id}` : null;
    case "port": {
      const key = `port:${sel.node}/${sel.port}`;
      return scene.ports.has(key)
        ? key
        : scene.byKey.has(`node:${sel.node}`)
          ? `node:${sel.node}`
          : null;
    }
    case "rail":
      return [...scene.rails.values()].find((r) => r.carrier === sel.id)?.key ?? null;
  }
}

/**
 * `focus`의 뜻 → 그림의 초점. 노드·VM은 이름이 먼저, 그다음 id. 포트가 줄에 없으면(8개 넘음·물리 관계 없음) 그 노드,
 * 레일은 그 레일에 선이 있는 첫 노드에서 들어간 것으로.
 */
export function focusOf(target: FocusTarget, model: ClusterModel, scene: MapScene): Focus | null {
  const nodeOf = (text: string) =>
    model.nodes.find((n) => n.name === text) ?? model.nodes.find((n) => n.id === text);
  switch (target.kind) {
    case "node": {
      const n = nodeOf(target.node);
      return n && scene.byKey.has(`node:${n.id}`) ? { kind: "node", key: `node:${n.id}` } : null;
    }
    case "vm": {
      const vm =
        model.vms.find((x) => x.name === target.vm) ?? model.vms.find((x) => x.id === target.vm);
      const tile = vm ? scene.tiles.get(`vm:${vm.id}`) : undefined;
      return tile ? { kind: "tile", node: tile.node.key, key: tile.cell.key } : null;
    }
    case "port": {
      const n = nodeOf(target.node);
      if (!n || !scene.byKey.has(`node:${n.id}`)) {
        return null;
      }
      // 본딩 이름(본딩 경보의 지도 링크)이면 그 묶음의 끊긴(없으면 첫) 구성 포트로(리뷰 2)
      const group = n.net?.ports.filter((p) => p.bond === target.port) ?? [];
      const name = (group.find((p) => p.health === "down") ?? group[0])?.name ?? target.port;
      const key = `port:${n.id}/${name}`;
      return scene.ports.has(key)
        ? { kind: "port", node: `node:${n.id}`, key }
        : { kind: "node", key: `node:${n.id}` };
    }
    case "rail": {
      const rail = [...scene.rails.values()].find((r) => r.carrier === target.rail);
      const wire = rail ? scene.wires.find((w) => w.rail === rail.key) : undefined;
      return rail && wire ? { kind: "rail", node: wire.node, key: rail.key } : null;
    }
  }
}
