// VM 만들기의 첫 단계 "용도"(계획 77-5, 진단 §4.3): 카드 하나가 처음 틀을 채운다 — OS 프리셋·CPU·메모리·디스크·NIC·
// CD. 카드는 디스크에 OS를 넣지 않는다 — 리눅스·Windows 카드는 고른 직후 "OS 고르기"(템플릿·ISO)를 연다. 값은 빌더의 놓기(`drop`)를 차례로 얹어 만들므로 놓기 규칙·이름 짓기·Windows 장치가 같다. 화면 없이 시험한다.
import { accepts } from "@/vmBuilder/model/accepts";
import { drop } from "@/vmBuilder/model/drop";
import { type Part, parts, USER_NETWORK } from "@/vmBuilder/model/parts";
import type { BuilderCtx, BuilderState, Missed, Reason } from "@/vmBuilder/model/state";
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
  web: { cores: 1, memory: 2048, disks: [16] },
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

/** 고른 직후 그림 위에 "OS 고르기" 카드를 여는 용도(리눅스는 템플릿·ISO, Windows는 설치·드라이버 ISO) */
export function picksOs(p: Purpose | null): p is "linux" | "web" | "database" | "windows" {
  return p !== null && OS[p] !== undefined;
}

/**
 * 카드를 쓸 수 없는 까닭(흐린 카드). 템플릿이 없거나, OS 프리셋을 놓을 수 없다 — 놓기 규칙(`accepts`)과 같은 검사다
 * (Windows는 TPM이 없거나 Arm(virt) 노드뿐인 클러스터).
 */
export function purposeBlocked(p: Purpose, s: BuilderState, ctx: BuilderCtx): Reason | undefined {
  if (p === "template" && !ctx.vms.some(isTemplate)) {
    return { key: "vmBuilder.purpose.noTemplates" };
  }
  const os = OS[p];
  const part = os && parts(s, ctx).find((x) => x.id === `os:${os}`);
  if (part) {
    const v = accepts(part, "vm", s, ctx);
    if (!v.ok) {
      return v.reason?.key === "vmBuilder.reason.windowsArch"
        ? { key: "vmBuilder.purpose.windowsArm" }
        : v.reason;
    }
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

/** 새 디스크를 못 붙이는 까닭(카드의 크기 줄): 풀이 있으나 권한이 없음, 쓸 풀이 없음. 붙일 수 있으면 undefined */
export function noDiskReason(s: BuilderState, ctx: BuilderCtx): "perm" | "none" | undefined {
  if (defaultPool(s, ctx)) {
    return undefined;
  }
  return parts(s, ctx).some(
    (p) => p.kind === "pool" && p.blocked?.key === "vmBuilder.reason.permPool",
  )
    ? "perm"
    : "none";
}

/** 웹 서버 카드가 NIC에 거는 보안 그룹: 들어오는 tcp 80이나 443을 받는 첫 그룹(이름순) */
export function webGroup(ctx: BuilderCtx): string | undefined {
  const opens = (ports: string | undefined, port: number) => {
    if (ports === undefined) {
      return true;
    }
    const [lo = "", hi = lo] = ports.split("-");
    return Number(lo) <= port && port <= Number(hi);
  };
  return [...ctx.securityGroups]
    .sort((a, b) => a.name.localeCompare(b.name))
    .find((g) =>
      (g.spec.rules ?? []).some(
        (r) =>
          r.direction === "in" &&
          r.action === "accept" &&
          (r.protocol === undefined || r.protocol === "tcp") &&
          (opens(r.ports, 80) || opens(r.ports, 443)),
      ),
    )?.name;
}

function put(s: BuilderState, ctx: BuilderCtx, part: Part | undefined, at: string): BuilderState {
  return part ? drop(s, part, at, ctx) : s;
}

const miss = (s: BuilderState, m: Missed): BuilderState => ({
  ...s,
  missed: [...(s.missed ?? []), m],
});

/**
 * 용도 카드를 처음 상태 `start`에 얹는다: OS 프리셋 → 크기 → 새 디스크(기본 풀) → NIC(기본 네트워크), ISO 설치와
 * Windows는 빈 CD 드라이브(먼저 부팅 — 설치 ISO를 넣는 곳), 웹 서버는 80·443을 여는 보안 그룹. OS 프리셋을 놓을 수
 * 없으면 나머지를 얹지 않고 `missed: ["os"]`로 돌려준다(Windows가 아닌 VM을 Windows라 부르지 않게). 디스크·NIC를
 * 약속대로 붙이지 못하면 `missed`에 적는다(검사 경고). 템플릿은 고른 템플릿을 놓는다(`template`), 이미지는 값을 바꾸지 않는다.
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
    if (s.preset !== os) {
      return miss(start, "os");
    }
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
    } else if (!s.missed?.includes("disk")) {
      s = miss(s, "disk");
    }
  }
  if (p === "iso" || p === "windows") {
    s = { ...s, values: { ...s.values, cdroms: [{ iso: "", bootOrder: "1" }] } };
  }
  if (p !== "blank") {
    const net = defaultNetwork(s, ctx);
    s = put(s, ctx, net, "nics");
    if (!net || net.name === USER_NETWORK) {
      s = miss(s, "nic");
    }
  }
  const group = p === "web" ? webGroup(ctx) : undefined;
  const sg = group && parts(s, ctx).find((x) => x.id === `sg:${group}`);
  if (sg && s.values.nics.length > 0) {
    s = put(s, ctx, sg, "nics.0");
  }
  return s;
}

/**
 * 올린 디스크 이미지로 만들기(계획 77-8 — 업로드가 끝나면 "이 이미지로 VM 만들기", 주소 `?purpose=image&disk=`): ISO 카드의
 * 크기(vCPU·메모리)에 그 볼륨을 첫 디스크로(먼저 부팅), 기본 NIC. 볼륨을 붙일 수 없으면(없음·다른 VM이 씀) 디스크 없이
 * `missed: ["disk"]`.
 */
export function withImage(start: BuilderState, ctx: BuilderCtx, volume: string): BuilderState {
  const size = PURPOSE_SIZE.iso!;
  let s: BuilderState = {
    ...start,
    values: { ...start.values, sockets: 1, cores: String(size.cores), memory: String(size.memory) },
  };
  const before = s.values.disks.length;
  s = put(
    s,
    ctx,
    parts(s, ctx).find((x) => x.id === `volume:${volume}`),
    "disks",
  );
  if (s.values.disks.length === before) {
    s = miss(s, "disk");
  }
  const net = defaultNetwork(s, ctx);
  s = put(s, ctx, net, "nics");
  if (!net || net.name === USER_NETWORK) {
    s = miss(s, "nic");
  }
  return s;
}

/**
 * OS 고르기 카드에서 ISO를 넣는다: 빈 CD 드라이브가 있으면 그것(`which`가 "drivers"면 둘째 드라이브 — 없으면 더함),
 * 없으면 먼저 부팅하는 드라이브를 더한다. 놓기 규칙이 거절하면 그대로다.
 */
export function insertIso(
  s: BuilderState,
  ctx: BuilderCtx,
  iso: string,
  which: "install" | "drivers" = "install",
): BuilderState {
  const part = parts(s, ctx).find((x) => x.id === `iso:${iso}`);
  if (!part) {
    return s;
  }
  const cds = s.values.cdroms;
  const index = which === "install" ? 0 : 1;
  if (cds.length > index) {
    return drop(s, part, `cdroms.${index}`, ctx);
  }
  const next = drop(s, part, "cdroms", ctx);
  if (next === s || which === "drivers") {
    return next;
  }
  return {
    ...next,
    values: {
      ...next.values,
      cdroms: next.values.cdroms.map((c, i) =>
        i === next.values.cdroms.length - 1 ? { ...c, bootOrder: "1" } : c,
      ),
    },
  };
}

/** 템플릿 카드 안의 고를 템플릿(이름순) */
export function templates(ctx: BuilderCtx) {
  return ctx.vms.filter(isTemplate).sort((a, b) => a.name.localeCompare(b.name));
}
