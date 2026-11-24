// VM 틀의 칸(계획 71 설계 §7.3). 칸 id는 폼의 오류 열쇠와 같은 꼴(`disks.0`, `nics.1`)이라 브라우저·서버 문제(`at`)가
// 같은 칸에 붙는다.
import type { BuilderState } from "@/vmBuilder/model/state";

export type ListKind = "disks" | "cdroms" | "nics";

export type Slot =
  | {
      id: string;
      kind:
        | "vm"
        | "compute"
        | "disks"
        | "cdroms"
        | "nics"
        | "cloudInit"
        | "placement"
        | "protection"
        | "folder"
        | "trash";
    }
  | { id: string; kind: "disk" | "cdrom" | "nic"; index: number };

/** 틀 블록(디스크·CD·NIC 한 줄) — 같은 칸 안에서 순서를 바꾸거나 `trash`로 뺀다. */
export interface Block {
  kind: "block";
  id: string;
  list: ListKind;
  index: number;
}

const ITEM: Record<ListKind, "disk" | "cdrom" | "nic"> = {
  disks: "disk",
  cdroms: "cdrom",
  nics: "nic",
};
const LIST: Record<string, ListKind> = { disk: "disks", cdrom: "cdroms", nic: "nics" };

/** 정해진 칸(차례는 화면의 차례: 틀 전체, 계산, 배치, 보호, cloud-init, 디스크·CD·NIC, 폴더, 빼기) */
export const FIXED = [
  "vm",
  "compute",
  "placement",
  "protection",
  "cloudInit",
  "disks",
  "cdroms",
  "nics",
  "folder",
  "trash",
] as const;

/** 칸 id를 읽는다(모르는 id는 null). */
export function slotOf(id: string): Slot | null {
  const [head = "", n, ...rest] = id.split(".");
  if (n === undefined) {
    return (FIXED as readonly string[]).includes(head) ? ({ id, kind: head } as Slot) : null;
  }
  const kind = (Object.keys(LIST) as string[]).find((k) => LIST[k] === head);
  return kind && rest.length === 0 && /^\d+$/.test(n)
    ? { id, kind: kind as "disk" | "cdrom" | "nic", index: Number(n) }
    : null;
}

/** 칸의 목록 종류와 자리(목록 칸이 아니면 null) */
export function listOf(slot: Slot): { list: ListKind; index: number | null } | null {
  if ("index" in slot) {
    return { list: LIST[slot.kind]!, index: slot.index };
  }
  return slot.kind === "disks" || slot.kind === "cdroms" || slot.kind === "nics"
    ? { list: slot.kind, index: null }
    : null;
}

/** 지금 틀의 칸 */
export function slots(s: BuilderState): Slot[] {
  const out: Slot[] = [];
  for (const id of FIXED) {
    out.push({ id, kind: id } as Slot);
    if (id === "disks" || id === "cdroms" || id === "nics") {
      s.values[id].forEach((_, index) => {
        out.push({ id: `${id}.${index}`, kind: ITEM[id], index });
      });
    }
  }
  return out;
}

/** 틀의 블록(디스크·CD·NIC 줄) */
export function blocks(s: BuilderState): Block[] {
  return (["disks", "cdroms", "nics"] as const).flatMap((list) =>
    s.values[list].map((_, index) => ({
      kind: "block" as const,
      id: `block:${list}.${index}`,
      list,
      index,
    })),
  );
}
