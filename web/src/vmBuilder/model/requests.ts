// 보낼 요청(계획 71 설계 §7.6): 만들기와 JSON 미리 보기가 같은 함수를 쓴다. 빈 VM 바탕은 새 디스크마다 `POST /volumes`
// → 영역이면 `POST /placement-rules` → `POST /vms`(폼의 `toSpec`), 템플릿 바탕은 영역 규칙 → `POST /vms/{id}/clone`
// (배포 대화상자와 같은 `cloneBody`). 서버 미리 확인 본문(`POST /vm-checks`)도 여기서 짠다.
import type { components } from "@/api/schema";
import type { VmSpec } from "@/api/types";
import { sdnNetworkNamed, splitList } from "@/sdn/ips";
import { poolFormats } from "@/storage/pools";
import { cloneBody } from "@/templates/templates";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import { PASSWORD_MASK, toCloudInit } from "@/vms/cloudInit";
import { gibToBytes, toSpec, type VmFormValues, volumeRequest } from "@/vms/form";

type Schemas = components["schemas"];

export type RuleRequest = { name: string; spec: Schemas["PlacementRuleSpec"] };
export type VolumeRequest = ReturnType<typeof volumeRequest>;

export type BuilderRequest =
  | { method: "POST"; path: "/volumes"; body: VolumeRequest }
  | { method: "POST"; path: "/placement-rules"; body: RuleRequest }
  | { method: "POST"; path: "/vms"; body: { name: string; spec: VmSpec } }
  | { method: "POST"; path: "/vms/{key}/clone"; key: string; body: Schemas["CloneRequest"] };

/** `POST /vm-checks` 본문·응답·문제 하나(설계 §9.1 — 스테이지 1의 서버 타입) */
export type VmCheckRequest = Schemas["VmCheckRequest"];
export type VmCheck = Schemas["VmCheck"];
export type VmCheckProblem = Schemas["VmCheckProblem"];

const MIB = 1024 ** 2;

/** NIC마다 SDN 네트워크인지(주소 칸·cloud-init 주소) */
export function sdnOf(s: BuilderState, ctx: Pick<BuilderCtx, "networks">): boolean[] {
  return s.values.nics.map((n) => sdnNetworkNamed(ctx.networks, n.network));
}

/** 영역 규칙(VM보다 먼저 만듦): `<vm>-zone`, hard면 node-affinity, 아니면 soft */
export function zoneRule(s: BuilderState): RuleRequest | null {
  const z = s.zone;
  if (!z) {
    return null;
  }
  return {
    name: `${s.values.name}-zone`,
    spec: {
      kind: z.hard ? "node-affinity" : "soft-node-affinity",
      vms: [s.values.name],
      node_tag: `${z.key}=${z.value}`,
      enabled: true,
    },
  };
}

/** 새 디스크마다의 볼륨 요청(형식은 풀 종류의 기본) */
export function volumeRequests(s: BuilderState, ctx: Pick<BuilderCtx, "pools">): VolumeRequest[] {
  return s.values.disks
    .filter((d) => d.kind === "new")
    .map((d) => volumeRequest(d, poolFormats(ctx.pools.find((p) => p.name === d.pool))[0]));
}

/** 템플릿 바탕의 배포 본문: 템플릿과 다른 칸만 덮어쓴다. */
function deployRequest(s: BuilderState, ctx: BuilderCtx, id: string): Schemas["CloneRequest"] {
  const v = s.values;
  const t = ctx.vms.find((x) => x.id === id);
  const spec = t?.spec;
  const vcpus = v.sockets * Number(v.cores);
  const sdn = sdnOf(s, ctx);
  const ci = toCloudInit(v.cloudInit, v.nics.length, sdn);
  const ips = v.nics.map((n, i) => (sdn[i] ? splitList(n.ips) : []));
  const base = s.base.kind === "template" ? s.base : null;
  return cloneBody(v.name, {
    mode: base?.linked == null ? "auto" : base.linked ? "linked" : "full",
    pool: base?.pool ?? "",
    node: v.node,
    ...(spec && vcpus !== (spec.cpu.sockets ?? 1) * spec.cpu.cores && { vcpus }),
    ...(spec && Number(v.memory) !== spec.memory_mib && { memoryMib: Number(v.memory) }),
    nics: v.nics.map((n, i) => (n.network !== spec?.nics?.[i]?.network ? n.network : "")),
    ...(ips.some((a) => a.length > 0) && { ips }),
    // 크기는 1 MiB 단위로 올려 보낸다(배포 대화상자와 같다). 비면 그대로다.
    disks: v.disks.map((d) => {
      const bytes = d.sizeGib.trim() === "" ? undefined : gibToBytes(d.sizeGib.trim());
      return bytes === undefined ? null : Math.ceil(bytes / MIB) * MIB;
    }),
    cloudInit: ci,
    start: v.start,
    folder: v.folder,
    cpu: {
      ...(v.model !== (spec?.cpu.model ?? "host") && { cpu_model: v.model }),
      ...(v.cpuFlags.join(" ") !== (spec?.cpu.flags ?? []).join(" ") && { cpu_flags: v.cpuFlags }),
    },
    numa: {},
  });
}

/** 만들 요청(차례대로). 만들기와 JSON 미리 보기가 함께 쓴다. */
export function requests(s: BuilderState, ctx: BuilderCtx): BuilderRequest[] {
  const rule = zoneRule(s);
  const rules: BuilderRequest[] = rule
    ? [{ method: "POST", path: "/placement-rules", body: rule }]
    : [];
  if (s.base.kind === "template") {
    return [
      ...rules,
      {
        method: "POST",
        path: "/vms/{key}/clone",
        key: s.base.id,
        body: deployRequest(s, ctx, s.base.id),
      },
    ];
  }
  const v = s.values;
  return [
    ...volumeRequests(s, ctx).map((body) => ({
      method: "POST" as const,
      path: "/volumes" as const,
      body,
    })),
    ...rules,
    {
      method: "POST",
      path: "/vms",
      body: { name: v.name, spec: toSpec(v, v.start ? "running" : "stopped", sdnOf(s, ctx)) },
    },
  ];
}

/** 비밀번호 평문을 뺀 값(확인 요청 — 비밀번호 검사는 브라우저 `cloudInitErrors`가 한다) */
function withoutPassword(v: VmFormValues): VmFormValues {
  return { ...v, cloudInit: { ...v.cloudInit, password: "", passwordSet: false } };
}

/**
 * 서버 미리 확인 본문. 템플릿 바탕은 확인하지 않는다(설계 §2 — 배포의 미리 확인은 나중). `made`가 참인 이름의 영역
 * 규칙은 이미 만들었다(지난 만들기 — 서버에 있으니 "만들 규칙"으로 보내지 않음).
 */
export function checkBody(
  s: BuilderState,
  ctx: BuilderCtx,
  made: (rule: string) => boolean = () => false,
): VmCheckRequest | null {
  if (s.base.kind === "template") {
    return null;
  }
  const v = withoutPassword(s.values);
  const rule = zoneRule(s);
  return {
    name: v.name,
    labels: {},
    spec: toSpec(v, v.start ? "running" : "stopped", sdnOf(s, ctx)),
    volumes: volumeRequests(s, ctx),
    rules: rule && !made(rule.name) ? [rule] : [],
  };
}

/** 요청의 cloud-init 비밀번호를 가린다(화면·복사에 평문을 남기지 않음). 가렸으면 참 */
export function masked(r: BuilderRequest): { request: BuilderRequest; masked: boolean } {
  const ci =
    r.path === "/vms"
      ? r.body.spec.cloud_init
      : r.path === "/vms/{key}/clone"
        ? r.body.cloud_init
        : undefined;
  if (!ci?.password || ci.password === PASSWORD_MASK) {
    return { request: r, masked: false };
  }
  const hidden = { ...ci, password: PASSWORD_MASK };
  if (r.path === "/vms") {
    return {
      request: { ...r, body: { ...r.body, spec: { ...r.body.spec, cloud_init: hidden } } },
      masked: true,
    };
  }
  if (r.path === "/vms/{key}/clone") {
    return { request: { ...r, body: { ...r.body, cloud_init: hidden } }, masked: true };
  }
  return { request: r, masked: false };
}

/** 요청의 주소(`/api/v1` 아래, 템플릿 id를 채움) */
export function requestPath(r: BuilderRequest): string {
  return `/api/v1${r.path === "/vms/{key}/clone" ? `/vms/${encodeURIComponent(r.key)}/clone` : r.path}`;
}

/**
 * JSON 미리 보기: 요청마다 `# POST 주소` 줄과 본문(2칸 들여쓰기). 비밀번호를 가렸으면 맨 위에 바꾸라는 주석(`note`)을 둔다.
 */
export function jsonText(reqs: readonly BuilderRequest[], note: string): string {
  const out = reqs.map(masked);
  const blocks = out.map(
    ({ request }) =>
      `# ${request.method} ${requestPath(request)}\n${JSON.stringify(request.body, null, 2)}`,
  );
  return [...(out.some((o) => o.masked) ? [`# ${note}`] : []), ...blocks].join("\n\n");
}
