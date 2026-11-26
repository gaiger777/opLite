// 네트워크 층의 합친 경로(계획 75 §4.2): 레일 띠, 포트 → 레일 선, 포트 줄(본딩 테두리·면·빗금·×·장애 표시)을 색·면마다
// `<path>` 하나로. 그림 쪽(`NetLayer.tsx`)과 원소 셈(`paths.ts`의 `elementCount`)이
// 함께 쓴다. 네트워크 레이어를 켰을 때만 부른다.
import { boxesPath, boxFace, crossPath, facesPath, iso, round } from "@/components/iso/iso";
import type { MapScene, NodeItem, RailWire } from "@/clusterMap/layout";
import type { Lod } from "@/clusterMap/lod";
import type { LinkTone } from "@/clusterMap/network";

/** 네트워크 층(계획 75)의 합친 경로 하나 */
export interface NetPiece {
  key: string;
  /**
   * `rail` 레일 띠, `wire` 포트 → 레일 선(색마다, 저하는 점선, 끊김은 레일 앞에서 끊김), `wireCross` 끊긴 선의 ×,
   * `bond` 본딩 테두리(색마다), `side`·`top` 포트 면(색마다), `shade` 포트 오른쪽 면 그늘, `hatch` 저하 포트 윗면 빗금,
   * `cross` 끊긴 포트 윗면 ×, `fault` 장애 포트·본딩 위의 작은 표시
   */
  kind:
    "rail" | "wire" | "wireCross" | "bond" | "side" | "top" | "shade" | "hatch" | "cross" | "fault";
  /** 선만 `unknown`(모름 — 회색 점선)이 있다 */
  tone: LinkTone | "unknown" | null;
  d: string;
  parts: number;
}

const LINK_TONES: readonly LinkTone[] = ["up", "degraded", "down"];
const WIRE_TONES: readonly (LinkTone | "unknown")[] = [...LINK_TONES, "unknown"];

/**
 * 포트·본딩 테두리·레일 선의 상태 색과 레일 띠 색(`colors.ts`의 `UPLINK_COLOR`·`RAIL_COLOR`와 같은 값 — 시험이 본다). 이
 * 파일과 그림 조각(`netUi.tsx`)은 지도 조각의 모듈을 실행 때 읽지 않는다: 읽으면 rolldown이 둘이 함께 쓰는 모듈을 공유 조각으로
 * 떼어 지도를 열 때 조각이 하나 더 온다.
 */
export const NET_COLOR: Record<LinkTone | "unknown", string> = {
  up: "var(--success)",
  degraded: "var(--warning)",
  down: "var(--danger)",
  unknown: "var(--muted-foreground)",
};
export const NET_RAIL = "var(--chart-1)";

/** 포트 줄을 그리는가(`layout.ts`의 `portRowShown`과 같은 셈 — 네트워크 레이어를 켰을 때) */
const rowShown = (n: NodeItem, lod: Lod) => lod !== "summary" && (n.net?.ports.length ?? 0) > 0;
/** 끊긴 선: 레일 앞 `CUT_AT`에서 `CUT_GAP`만큼 비우고 그 가운데 × */
const CUT_AT = 18;
const CUT_GAP = 10;
const CUT_X = 3;

const at = (x: number, y: number, z: number) => {
  const p = iso(x, y, z);
  return `${round(p.x)},${round(p.y)}`;
};

/** 선 하나: 포트 앞 → 틈으로 꺾음 → 레일(끊김이면 레일 앞에서 끊고 레일에 짧은 토막) */
function wirePath(w: RailWire): string {
  const bend = Math.min(w.to, w.y + 4);
  if (w.tone !== "down") {
    return `M${at(w.x, w.y, w.z)}L${at(w.lane, bend, w.z)}L${at(w.lane, w.to, w.z)}`;
  }
  const cut = Math.max(bend, w.to - CUT_AT);
  return (
    `M${at(w.x, w.y, w.z)}L${at(w.lane, bend, w.z)}L${at(w.lane, cut, w.z)}` +
    `M${at(w.lane, cut + CUT_GAP, w.z)}L${at(w.lane, w.to, w.z)}`
  );
}

function wireCross(w: RailWire): string {
  // 틈은 `wirePath`와 같은 자리(짧은 선은 꺾는 곳에 맞춰 옮겨짐)
  const cut = Math.max(Math.min(w.to, w.y + 4), w.to - CUT_AT);
  const c = iso(w.lane, cut + CUT_GAP / 2, w.z);
  const p = (dx: number, dy: number) => `${round(c.x + dx)},${round(c.y + dy)}`;
  return `M${p(-CUT_X, -CUT_X)}L${p(CUT_X, CUT_X)}M${p(CUT_X, -CUT_X)}L${p(-CUT_X, CUT_X)}`;
}

/** 장애 표시: 포트(본딩) 윗면 가운데 위 10에 아래를 가리키는 작은 세모 */
function faultMark(box: { x: number; y: number; z: number; w: number; d: number; h: number }) {
  const c = iso(box.x + box.w / 2, box.y + box.d / 2, box.z + box.h + 10);
  return `M${round(c.x - 4)},${round(c.y - 6)}L${round(c.x + 4)},${round(c.y - 6)}L${round(c.x)},${round(c.y)}Z`;
}

/**
 * 네트워크 층의 경로(색·면마다 하나 — 노드 수와 상관없이 많아야 스무 남짓): 레일 띠, 선(색마다), 끊김 ×, 그리고 요약이
 * 아니면 포트 줄(본딩 테두리·포트 면·그늘·빗금·×·장애 표시). 모두 바닥 높이라 판 바로 뒤에 한꺼번에 그린다(`layout.ts`).
 */
export function netPaths(scene: MapScene, lod: Lod): NetPiece[] {
  const out: NetPiece[] = [];
  const rails = [...scene.rails.values()];
  if (rails.length > 0) {
    out.push({
      key: "net:rail",
      kind: "rail",
      tone: null,
      d: boxesPath(
        rails.map((r) => r.box),
        "top",
      ),
      parts: rails.length,
    });
  }
  for (const tone of WIRE_TONES) {
    const list = scene.wires.filter((w) => w.tone === tone);
    if (list.length > 0) {
      out.push({
        key: `net:wire:${tone}`,
        kind: "wire",
        tone,
        d: list.map(wirePath).join(""),
        parts: list.length,
      });
    }
  }
  const cut = scene.wires.filter((w) => w.tone === "down");
  if (cut.length > 0) {
    out.push({
      key: "net:wire:cross",
      kind: "wireCross",
      tone: "down",
      d: cut.map(wireCross).join(""),
      parts: cut.length,
    });
  }
  const rows = scene.nodes.filter((n) => rowShown(n, lod));
  if (rows.length === 0) {
    return out;
  }
  const bonds = rows.flatMap((n) => n.net!.bonds);
  const ports = rows.flatMap((n) => n.net!.ports);
  for (const tone of LINK_TONES) {
    const list = bonds.filter((b) => b.tone === tone);
    if (list.length > 0) {
      out.push({
        key: `net:bond:${tone}`,
        kind: "bond",
        tone,
        d: boxesPath(
          list.map((b) => b.box),
          "top",
        ),
        parts: list.length,
      });
    }
  }
  for (const tone of LINK_TONES) {
    const list = ports.filter((p) => p.tone === tone);
    if (list.length > 0) {
      out.push({
        key: `net:side:${tone}`,
        kind: "side",
        tone,
        d: facesPath(list.flatMap((p) => [boxFace(p.box, "left"), boxFace(p.box, "right")])),
        parts: list.length * 2,
      });
    }
  }
  out.push({
    key: "net:shade",
    kind: "shade",
    tone: null,
    d: boxesPath(
      ports.map((p) => p.box),
      "right",
    ),
    parts: ports.length,
  });
  for (const tone of LINK_TONES) {
    const list = ports.filter((p) => p.tone === tone);
    if (list.length > 0) {
      out.push({
        key: `net:top:${tone}`,
        kind: "top",
        tone,
        d: boxesPath(
          list.map((p) => p.box),
          "top",
        ),
        parts: list.length,
      });
    }
  }
  const degraded = ports.filter((p) => p.tone === "degraded");
  if (degraded.length > 0) {
    out.push({
      key: "net:hatch",
      kind: "hatch",
      tone: "degraded",
      d: boxesPath(
        degraded.map((p) => p.box),
        "top",
      ),
      parts: degraded.length,
    });
  }
  const down = ports.filter((p) => p.tone === "down");
  if (down.length > 0) {
    out.push({
      key: "net:cross",
      kind: "cross",
      tone: "down",
      d: down.map((p) => crossPath(boxFace(p.box, "top"))).join(" "),
      parts: down.length,
    });
  }
  const faulted = [
    ...ports.filter((p) => p.fault !== null).map((p) => p.box),
    ...bonds.filter((b) => b.fault !== null).map((b) => b.box),
  ];
  if (faulted.length > 0) {
    out.push({
      key: "net:fault",
      kind: "fault",
      tone: "down",
      d: faulted.map(faultMark).join(""),
      parts: faulted.length,
    });
  }
  return out;
}

/**
 * 네트워크 층의 원소 수(`MapScene`이 그리는 것과 같은 셈): 경로 조각마다 하나, 레일마다 이름 글, 판마다 "+n 레일" 글,
 * 포트 줄을 그린 노드마다 "+n 포트" 글, 레일이 있으면 밝히기 선 경로 하나.
 */
export function netElements(scene: MapScene, lod: Lod): number {
  let n = netPaths(scene, lod).length + scene.rails.size;
  n += scene.slabs.filter((s) => s.railMore > 0).length;
  n += scene.nodes.filter((x) => rowShown(x, lod) && x.net!.more > 0).length;
  if (scene.rails.size > 0) {
    n += 1;
  }
  return n;
}
