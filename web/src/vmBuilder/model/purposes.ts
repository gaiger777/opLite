// VM 만들기의 첫 단계 "용도"(계획 77-5, 진단 §4.3): 카드 하나가 처음 틀을 채운다 — OS 프리셋·CPU·메모리·디스크·NIC·
// CD. 값은 빌더의 놓기(`drop`)를 차례로 얹어 만들므로 놓기 규칙·이름 짓기·Windows 장치가 같다. 화면 없이 시험한다.
import { drop } from "@/vmBuilder/model/drop";
import { type Part, parts, USER_NETWORK } from "@/vmBuilder/model/parts";
import type { BuilderCtx, BuilderState, Reason } from "@/vmBuilder/model/state";
import { isTemplate } from "@/templates/templates";

/** 카드 차례(화면의 차례) */
export const PURPOSES = [
  "linux",
  "web",
  "database",
  "windows",
  "iso",
  "template",
  "image",
  "blank",
] as const;
export type Purpose = (typeof PURPOSES)[number];

/** 카드의 기본 크기: vCPU, 메모리(MiB), 디스크(GiB — 차례대로) */
export interface PurposeSize {
  cores: number;
  memory: number;
  disks: number[];
}

export const PURPOSE_SIZE: Partial<Record<Purpose, PurposeSize>> = {
  linux: { cores: 2, memory: 4096, disks: [20] },
  web: { cores: 2, memory: 4096, disks: [20] },
  database: { cores: 4, memory: 16384, disks: [32, 100] },
  windows: { cores: 4, memory: 8192, disks: [64] },
  iso: { cores: 2, memory: 4096, disks: [32] },
  blank: { cores: 2, memory: 2048, disks: [] },
};

/** 카드의 OS 프리셋(놓기 부품 `os:*`) */
const OS: Partial<Record<Purpose, "linux" | "windows">> = {
  linux: "linux",
  web: "linux",
  database: "linux",
  windows: "windows",
};

/** 카드를 쓸 수 없는 까닭(흐린 카드). 템플릿이 없거나, Windows를 놓을 수 없다(TPM 없음). */
export function purposeBlocked(p: Purpose, s: BuilderState, ctx: BuilderCtx): Reason | undefined {
  if (p === "template" && !ctx.vms.some(isTemplate)) {
    return { key: "vmBuilder.purpose.noTemplates" };
  }
  if (p === "windows") {
    return parts(s, ctx).find((x) => x.id === "os:windows")?.blocked;
  }
  return undefined;
}

/** 새 디스크를 둘 풀: 첫 공유 풀(옮기기 쉬움), 없으면 첫 풀 — 만들 권한이 있는 것만 */
export function defaultPool(s: BuilderState, ctx: BuilderCtx): Part | undefined {
  const usable = parts(s, ctx).filter((p) => p.kind === "pool" && !p.blocked);
  return usable.find((p) => p.kind === "pool" && p.shared) ?? usable[0];
}

/** 새 NIC를 붙일 네트워크: 첫 네트워크(쓸 수 있는 것), 없으면 내장 `user`(NAT) */
export function defaultNetwork(s: BuilderState, ctx: BuilderCtx): Part | undefined {
  const nets = parts(s, ctx).filter((p) => p.kind === "network" && !p.blocked);
  return nets.find((p) => p.name !== USER_NETWORK) ?? nets[0];
}

function put(s: BuilderState, ctx: BuilderCtx, part: Part | undefined, at: string): BuilderState {
  return part ? drop(s, part, at, ctx) : s;
}

/**
 * 용도 카드를 처음 상태 `start`에 얹는다: OS 프리셋 → 크기 → 새 디스크(기본 풀) → NIC(기본 네트워크), ISO 설치는 빈 CD
 * 드라이브(먼저 부팅). 템플릿·이미지는 값을 바꾸지 않는다(템플릿은 고른 템플릿을 놓는다 — `template`).
 */
export function applyPurpose(
  start: BuilderState,
  p: Purpose,
  ctx: BuilderCtx,
  template?: string,
): BuilderState {
  if (p === "template") {
    const part = template
      ? parts(start, ctx).find((x) => x.id === `template:${template}`)
      : undefined;
    return put(start, ctx, part, "vm");
  }
  const size = PURPOSE_SIZE[p];
  if (!size) {
    return start;
  }
  let s = start;
  const os = OS[p];
  if (os) {
    s = put(
      s,
      ctx,
      parts(s, ctx).find((x) => x.id === `os:${os}`),
      "vm",
    );
  }
  s = {
    ...s,
    values: { ...s.values, sockets: 1, cores: String(size.cores), memory: String(size.memory) },
  };
  for (const gib of size.disks) {
    const before = s.values.disks.length;
    s = put(s, ctx, defaultPool(s, ctx), "disks");
    if (s.values.disks.length > before) {
      s = {
        ...s,
        values: {
          ...s.values,
          disks: s.values.disks.map((d, i) => (i === before ? { ...d, sizeGib: String(gib) } : d)),
        },
      };
    }
  }
  if (p === "iso") {
    s = { ...s, values: { ...s.values, cdroms: [{ iso: "", bootOrder: "1" }] } };
  }
  if (p !== "blank") {
    s = put(s, ctx, defaultNetwork(s, ctx), "nics");
  }
  return s;
}

/** 템플릿 카드 안의 고를 템플릿(이름순) */
export function templates(ctx: BuilderCtx) {
  return ctx.vms.filter(isTemplate).sort((a, b) => a.name.localeCompare(b.name));
}
