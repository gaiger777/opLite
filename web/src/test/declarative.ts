// 설정 기록·선언 묶음·인증서 화면(계획 73) 시험의 고정 값(서버 `HistoryEntry`·`PlanView`·`ApplySet`·ACME 보기와 같은 꼴)
import type { AcmeAccount, AcmeDns, Certificate } from "@/certs/api";
import type { ApplySet, PlanView } from "@/declarative/api";
import type { HistoryDetail, HistoryEntry } from "@/history/api";

export const WEB_ID = "019a0f3c-0000-7000-8073-000000000001";
export const SET_ID = "019a0f3c-0000-7000-8073-0000000000a1";

/** 2026-11-24 15:02(지역 시각 — 날짜 묶음이 시간대에 따라 갈리지 않게)과 그 앞날 */
export const AT = new Date(2026, 10, 24, 15, 2).getTime();
const DAY = 86_400_000;

export function entry(revision: number, extra: Partial<HistoryEntry> = {}): HistoryEntry {
  return {
    revision,
    kind: "vm",
    id: WEB_ID,
    name: "web",
    op: "update",
    at_ms: AT,
    actor: { user: "admin", auth: { kind: "session" } },
    source: "web",
    changed: ["memory_mib"],
    ...extra,
  };
}

export const TIMELINE: HistoryEntry[] = [
  entry(81, { reason: "more memory for the sale" }),
  entry(70, {
    at_ms: AT - DAY,
    source: "apply",
    actor: { user: "ci", auth: { kind: "token", name: "gitops" } },
    changed: ["vcpus", "labels.tier", "nics", "disks", "cloud_init.password"],
    values: [
      { path: "vcpus", op: "replace", old: 2, new: 4 },
      { path: "labels.tier", op: "add", new: "web" },
    ],
  }),
  entry(60, { at_ms: AT - DAY - 3_600_000, op: "create", changed: [], source: "cli" }),
];

export function detail(row: HistoryEntry, extra: Partial<HistoryDetail> = {}): HistoryDetail {
  return {
    compare: "prev",
    against: row.revision - 11,
    diff: [
      { path: "memory_mib", op: "replace", old: 4096, new: 8192 },
      {
        path: "cloud_init.password",
        op: "replace",
        old: { $secret: "set" },
        new: { $secret: "changed" },
      },
    ],
    record: {
      ...row,
      spec: { memory_mib: 8192, cloud_init: { password: { $secret: "changed" } } },
    },
    ...extra,
  };
}

export function plan(extra: Partial<PlanView> = {}): PlanView {
  return {
    plan: "019a0f3c-0000-7000-8073-0000000000f1",
    base_revision: 90,
    summary: { create: 0, update: 1, delete: 0, unchanged: 0, blocked: 0 },
    steps: [
      {
        n: 1,
        kind: "Vm",
        name: "web",
        action: "update",
        diff: [{ path: "memory_mib", op: "replace", old: 8192, new: 4096 }],
        problems: [
          {
            code: "secret_not_restored",
            level: "warning",
            message: "secrets are not restored; the current password stays",
          },
        ],
      },
    ],
    ...extra,
  };
}

export function applySet(name: string, extra: Partial<ApplySet["status"]> = {}): ApplySet {
  return {
    id: SET_ID,
    name,
    revision: 3,
    labels: {},
    spec: { protect_data: true, description: "web tier" },
    status: {
      applied_at: AT - DAY,
      applied_by: "ci",
      applied_revision: 70,
      last_error: null as unknown as string,
      objects: [
        {
          kind: "vm",
          id: WEB_ID,
          name: "web",
          revision: 70,
          declared: {
            metadata: { labels: { tier: "web" } },
            spec: { memory_mib: 4096, vcpus: 2, cloud_init: { password: { $secret: "set" } } },
            state: "running",
          },
        },
        {
          kind: "network",
          id: "019a0f3c-0000-7000-8073-000000000002",
          name: "lan",
          revision: 65,
          declared: { spec: { bridge: "br0" } },
        },
      ],
      drift: { checked_at: AT, revision: 90, items: [] },
      ...extra,
    },
  };
}

export const DRIFTED = applySet("web-prod", {
  drift: {
    checked_at: AT,
    revision: 90,
    items: [
      {
        kind: "vm",
        id: WEB_ID,
        name: "web",
        state: "modified",
        paths: ["memory_mib", "vcpus"],
        by: {
          revision: 81,
          at_ms: AT,
          actor: { user: "admin" },
          source: "web",
          reason: "more memory for the sale",
        },
      },
    ],
  },
});

/** `DRIFTED`·`applySet()`의 리소스가 지금 가진 매니페스트(web은 메모리·vCPU를 화면에서 바꿈) */
export const LIVE_MANIFESTS = [
  {
    apiVersion: "orbivisor.io/v1",
    kind: "Vm",
    metadata: { name: "web", labels: { tier: "web", "orbivisor.io/apply-set": "web-prod" } },
    spec: {
      memory_mib: 8192,
      vcpus: 4,
      cloud_init: { password: { secretRef: "web-pw" } },
      nics: [{ network: "lan", mac: "52:54:00:00:00:01" }],
    },
    state: "running",
  },
  {
    apiVersion: "orbivisor.io/v1",
    kind: "Network",
    metadata: { name: "lan" },
    spec: { bridge: "br0" },
  },
] as const;

export function account(name: string, extra: Partial<AcmeAccount["status"]> = {}): AcmeAccount {
  return {
    id: `019a0f3c-0000-7000-8074-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { directory: "letsencrypt", email: "ops@example.com", agree_tos: true },
    status: {
      url: "https://acme.example/acct/1",
      registered_at: 1,
      profiles: ["classic", "shortlived"],
      terms: "",
      error: null as unknown as string,
      ...extra,
    },
  };
}

export function dnsProvider(name: string): AcmeDns {
  return {
    id: `019a0f3c-0000-7000-8075-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { provider: { kind: "cloudflare" } as never, zones: ["example.com"] },
    status: {},
  };
}

/** 지금부터 `days`일 뒤 끝나는 인증서 */
export function certificate(
  name: string,
  days: number,
  extra: Partial<Certificate["status"]> = {},
): Certificate {
  const now = Math.floor(Date.now() / 1000);
  return {
    id: `019a0f3c-0000-7000-8076-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      account: "le",
      names: ["api.example.com"],
      challenge: { kind: "dns01", dns: "cf" },
      scope: { kind: "shared" },
      precheck: true,
    },
    status: {
      challenges: [],
      failures: 0,
      orders: {},
      nodes: {},
      node_failures: {},
      next_attempt_at: null as unknown as number,
      error: null as unknown as string,
      shared: {
        names: ["api.example.com"],
        chain_pem: "",
        serial: "0a",
        not_before: now - 86_400,
        // 한 시간 더(시험이 도는 사이 남은 날이 하루 줄지 않게)
        not_after: now + days * 86_400 + 3_600,
        renew_at: now + Math.max(0, days - 30) * 86_400,
      },
      ...extra,
    },
  };
}
