import type { components } from "@/api/schema";
import type { BackupJob, Weekday } from "@/api/types";
import { validName } from "@/vms/form";

type Spec = BackupJob["spec"];
type Keep = components["schemas"]["Keep"];
export type BackupJobPatch = components["schemas"]["BackupJobPatch"];

/** 월요일부터 */
export const WEEKDAYS: readonly Weekday[] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

export const KEEP_UNITS = ["last", "daily", "weekly", "monthly"] as const;
type KeepUnit = (typeof KEEP_UNITS)[number];

/** 서버의 u32 상한 */
const U32_MAX = 4_294_967_295;

/** 유닉스 초를 `2026-11-04 03:00 UTC`처럼. 작업 일정이 UTC라 화면도 UTC로 보인다. */
export function formatUtc(seconds: number): string {
  const iso = new Date(seconds * 1000).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
}

/** 쉼표로 나눈 `KEY=VALUE`. 하나라도 잘못되면 null. 값에는 `=`가 있어도 된다. */
export function parseLabels(text: string): Record<string, string> | null {
  const labels: Record<string, string> = {};
  for (const part of text.split(",").map((p) => p.trim())) {
    if (part === "") {
      continue;
    }
    const at = part.indexOf("=");
    const key = at < 0 ? "" : part.slice(0, at).trim();
    if (key === "") {
      return null;
    }
    labels[key] = part.slice(at + 1).trim();
  }
  return labels;
}

function parseNames(text: string): string[] {
  return text
    .split(",")
    .map((n) => n.trim())
    .filter((n) => n !== "");
}

/** 0이 아닌 보존 단위와 개수. 비면 지우지 않는 작업이다. */
export function keepParts(keep: Keep | undefined): [KeepUnit, number][] {
  return KEEP_UNITS.filter((unit) => (keep?.[unit] ?? 0) > 0).map((unit) => [unit, keep![unit]]);
}

/** 만들기·바꾸기 폼의 값. 숫자도 입력 그대로 문자열로 둔다. */
export interface JobForm {
  name: string;
  pool: string;
  target: "all" | "names" | "labels";
  /** 쉼표로 나눈 VM 이름 */
  names: string;
  /** 쉼표로 나눈 `KEY=VALUE` */
  labels: string;
  at: string;
  days: Weekday[];
  keep: Record<KeepUnit, string>;
  enabled: boolean;
  /** 사본을 보낼 원격. 비면 복제하지 않는다. */
  replicateRemote: string;
  /** 원격의 백업 풀 */
  replicatePool: string;
}

export function emptyForm(pool: string): JobForm {
  return {
    name: "",
    pool,
    target: "all",
    names: "",
    labels: "",
    at: "",
    days: [],
    keep: { last: "", daily: "", weekly: "", monthly: "" },
    enabled: true,
    replicateRemote: "",
    replicatePool: "",
  };
}

export function formOf(job: BackupJob): JobForm {
  const vms = job.spec.vms;
  const keep = job.spec.keep;
  return {
    name: job.name,
    pool: job.spec.pool,
    target: "all" in vms ? "all" : "names" in vms ? "names" : "labels",
    names: "names" in vms ? vms.names.join(", ") : "",
    labels:
      "labels" in vms
        ? Object.entries(vms.labels)
            .map(([k, v]) => `${k}=${v}`)
            .join(", ")
        : "",
    at: job.spec.schedule.at,
    days: job.spec.schedule.days ?? [],
    keep: {
      last: String(keep?.last ?? 0),
      daily: String(keep?.daily ?? 0),
      weekly: String(keep?.weekly ?? 0),
      monthly: String(keep?.monthly ?? 0),
    },
    enabled: job.spec.enabled,
    replicateRemote: job.spec.replicate?.remote ?? "",
    replicatePool: job.spec.replicate?.pool ?? "",
  };
}

/** 서버와 같은 검사. 오류는 필드별 번역 키다. `taken`은 다른 작업의 이름이고 바꾸기면 null(이름 없음). */
export function checkForm(form: JobForm, taken: string[] | null): Record<string, string> {
  const found: Record<string, string> = {};
  if (taken !== null) {
    if (!validName(form.name)) {
      found.name = "forms.name";
    } else if (taken.includes(form.name)) {
      found.name = "backupJobs.errors.exists";
    }
  }
  if (form.pool === "") {
    found.pool = "backups.errors.pool";
  }
  if (form.replicateRemote !== "" && form.replicatePool === "") {
    found.replicatePool = "backups.errors.pool";
  }
  if (form.target === "names" && parseNames(form.names).length === 0) {
    found.names = "backupJobs.errors.names";
  }
  if (form.target === "labels") {
    const labels = parseLabels(form.labels);
    if (labels === null || Object.keys(labels).length === 0) {
      found.labels = "backupJobs.errors.labels";
    }
  }
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(form.at)) {
    found.at = "backupJobs.errors.at";
  }
  if (
    KEEP_UNITS.some((unit) => {
      const n = form.keep[unit].trim();
      return n !== "" && !(/^\d+$/.test(n) && Number(n) <= U32_MAX);
    })
  ) {
    found.keep = "backupJobs.errors.keep";
  }
  return found;
}

/** 검사한 폼의 스펙. 빈 보존 칸은 0이다. */
export function specOf(form: JobForm): Spec {
  const vms: Spec["vms"] =
    form.target === "all"
      ? { all: true }
      : form.target === "names"
        ? { names: parseNames(form.names) }
        : { labels: parseLabels(form.labels) ?? {} };
  const count = (unit: KeepUnit) => Number(form.keep[unit].trim() || "0");
  return {
    vms,
    pool: form.pool,
    schedule: { at: form.at, days: WEEKDAYS.filter((d) => form.days.includes(d)) },
    keep: {
      last: count("last"),
      daily: count("daily"),
      weekly: count("weekly"),
      monthly: count("monthly"),
    },
    enabled: form.enabled,
    // 복제하지 않으면 필드를 빼서 보낸다.
    ...(form.replicateRemote !== "" && {
      replicate: { remote: form.replicateRemote, pool: form.replicatePool },
    }),
  };
}

/**
 * 작업에서 바뀐 필드만(`PATCH` 본문). 일정과 보존 규칙은 통째로 보낸다. 요일은 집합으로 비교한다. 복제를
 * 끄면 `replicate: null`이다(빼면 그대로 둔다).
 */
export function patchOf(job: BackupJob, spec: Spec): BackupJobPatch {
  const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
  const keep = (k: Keep | undefined) => KEEP_UNITS.map((unit) => k?.[unit] ?? 0);
  const patch: BackupJobPatch = {};
  if (!same(job.spec.vms, spec.vms)) {
    patch.vms = spec.vms;
  }
  if (job.spec.pool !== spec.pool) {
    patch.pool = spec.pool;
  }
  const days = (d: Weekday[] | undefined) => WEEKDAYS.filter((w) => d?.includes(w));
  const schedule = { at: job.spec.schedule.at, days: days(job.spec.schedule.days) };
  if (!same(schedule, { at: spec.schedule.at, days: days(spec.schedule.days) })) {
    patch.schedule = spec.schedule;
  }
  if (!same(keep(job.spec.keep), keep(spec.keep))) {
    patch.keep = spec.keep;
  }
  if (job.spec.enabled !== spec.enabled) {
    patch.enabled = spec.enabled;
  }
  const target = (r: Spec["replicate"]) => (r ? [r.remote, r.pool] : null);
  if (!same(target(job.spec.replicate), target(spec.replicate))) {
    patch.replicate = spec.replicate ?? null;
  }
  return patch;
}
