// 빌더 화면의 이름 짓기(계획 71 설계 §11): 부품·칸·블록·문제의 이름과 부품의 작은 정보 줄. 모델은 데이터만 주고
// (정해진 부품은 `name`이 빈 글) 이름은 여기서 문구(`vmBuilder.*`)로 붙인다.
import type { Item } from "@/dnd/types";
import { formatBytes, formatMib } from "@/lib/format";
import { SIZES, type Part } from "@/vmBuilder/model/parts";
import type { Problem } from "@/vmBuilder/model/problems";
import { type Block, slotOf } from "@/vmBuilder/model/slots";
import type { BuilderState } from "@/vmBuilder/model/state";

export type T = (key: string, values?: Record<string, unknown>) => string;

/** 끌 것: 부품이나 블록에 알림 줄 이름을 붙인 것 */
export type Thing = (Part | Block) & Item;

export function partLabel(t: T, p: Part): string {
  switch (p.kind) {
    case "size":
      return t(`vmBuilder.part.size.${p.size}`);
    case "os":
      return t(`vmBuilder.part.os.${p.os}`);
    case "pool":
      return t("vmBuilder.part.pool", { name: p.pool });
    case "ci":
      return t(`vmBuilder.part.ci.${p.snippet}`);
    case "addr":
      return t(`vmBuilder.part.addr.${p.mode}`);
    case "auto":
      return t("vmBuilder.part.auto");
    case "zone":
      return t("vmBuilder.part.zone", { name: p.value });
    case "ha":
      return t("vmBuilder.part.ha");
    default:
      return p.name;
  }
}

/** 부품의 작은 회색 정보(남은 용량·쓸 수 있는지). 막혔으면 까닭 */
export function partInfo(t: T, p: Part): string {
  if (p.blocked) {
    return t(p.blocked.key, p.blocked.values);
  }
  const i = (key: string, values?: Record<string, unknown>) => t(`vmBuilder.info.${key}`, values);
  switch (p.kind) {
    case "size": {
      const s = SIZES[p.size];
      const size = i("size", { cores: s.cores, memory: s.memory / 1024 });
      return p.fit ? `${size} · ${i(p.fit)}` : size;
    }
    case "cpu":
      return p.usable === undefined ? "" : i("usable", { count: p.usable });
    case "pool":
      return [
        i(p.shared ? "shared" : "local"),
        p.free !== undefined && i("free", { free: formatBytes(p.free) }),
      ]
        .filter(Boolean)
        .join(" · ");
    case "volume":
      return `${formatBytes(p.size)} · ${p.pool}`;
    case "iso":
      return `${p.pool} · ${i(p.shared ? "shared" : "local")}`;
    case "template":
      return i("template", { vcpus: p.vcpus, memory: formatMib(p.memory), count: p.disks });
    case "network":
      return p.sdn ? "SDN" : "";
    case "sg":
      return i("rules", { count: p.rules });
    case "node":
      return p.free === undefined ? "" : i("free", { free: formatMib(p.free) });
    case "zone":
      return i("nodes", { count: p.nodes });
    default:
      return "";
  }
}

/** 칸 이름(알림 줄·`aria-label`) */
export function slotLabel(t: T, id: string, s: BuilderState): string {
  const slot = slotOf(id);
  if (!slot) {
    return id;
  }
  if ("index" in slot) {
    const v = s.values;
    const name =
      slot.kind === "disk"
        ? v.disks[slot.index]?.volume
        : slot.kind === "cdrom"
          ? v.cdroms[slot.index]?.iso
          : v.nics[slot.index]?.network;
    return t(`vmBuilder.slot.${slot.kind}`, { n: slot.index + 1, name: name || "—" });
  }
  return t(`vmBuilder.slot.${slot.kind}`);
}

export function blockLabel(t: T, b: Block, s: BuilderState): string {
  return slotLabel(t, `${b.list}.${b.index}`, s);
}

/** 문제 글: 서버 문제는 서버의 설명, 브라우저 문제는 번역 키 */
export function problemText(t: T, p: Problem): string {
  return p.message ?? t(p.key, p.values);
}

/** 문제가 붙는 칸: 아는 칸이나 이름 칸, 그 밖(`quota`·`spec`·`replication`)은 틀 전체 */
export function problemSlot(at: string): string {
  return at === "name" || (at !== "trash" && slotOf(at)) ? at : "vm";
}
