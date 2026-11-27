// 실패한 Task의 "무엇이 / 왜 / 이렇게 하세요"(계획 77-2, 진단 §3.4). 서버가 단 `status.failure`(코드·값)를 사전
// `taskErrors.<code>`로 풀고, 값에서 관련 화면 링크를 만든다. 코드가 없거나(옛 노드·코드 없는 실패) 사전에 없는 코드면 null —
// 그때는 지금처럼 원문만 보인다.
import type { TFunction } from "i18next";

import type { Task, TaskFailure } from "@/api/types";
import { formatBytes } from "@/lib/format";
import { kindHref } from "@/tasks/target";

export interface FailureText {
  code: string;
  title: string;
  cause: string;
  /** 해결 단계(사전의 줄마다 하나) */
  fix: string[];
}

/** 사전 문구에 넣을 값: `*_bytes`는 사람 단위로(`need_bytes` → `need`) */
function values(failure: TaskFailure): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(failure.params ?? {})) {
    if (key.endsWith("_bytes") && /^\d+$/.test(value)) {
      out[key.slice(0, -"_bytes".length)] = formatBytes(Number(value));
    }
    out[key] = value;
  }
  return out;
}

/** 실패의 사람 말 설명(사전에 있는 코드만) */
export function failureText(task: Task, t: TFunction): FailureText | null {
  const failure = task.status.failure;
  if (task.status.state !== "failed" || !failure) {
    return null;
  }
  const base = `taskErrors.${failure.code}`;
  if (!t(`${base}.title`, { defaultValue: "" })) {
    return null;
  }
  // 값이 없는 자리는 비우지 않고 "?"로(옛 서버가 값을 덜 보냈을 때)
  const opts = { ...values(failure), interpolation: { escapeValue: false }, defaultValue: "" };
  const fill = (key: string) => t(`${base}.${key}`, opts).replace(/\{\{\w+\}\}/g, "?");
  return {
    code: failure.code,
    title: fill("title"),
    cause: fill("cause"),
    fix: fill("fix")
      .split("\n")
      .filter((line) => line.trim()),
  };
}

/** 목록 한 줄 요약(사전의 제목). 사전이 없으면 null */
export function failureTitle(task: Task, t: TFunction): string | null {
  return failureText(task, t)?.title ?? null;
}

/** 값의 이름 → 화면 종류 */
const PARAM_KINDS: Record<string, string> = {
  node: "node",
  pool: "storage-pool",
  vm: "vm",
  volume: "volume",
  network: "network",
};

/** 코드마다 고칠 곳(값으로 못 만드는 화면) */
const CODE_LINKS: Record<string, { kind: string; href: string }> = {
  backup_store_unavailable: { kind: "backup-store", href: "/backup-stores" },
  backup_store_denied: { kind: "backup-store", href: "/backup-stores" },
  backup_store_full: { kind: "backup-store", href: "/backup-stores" },
  backup_corrupt: { kind: "backup-store", href: "/backup-stores" },
  backup_key_mismatch: { kind: "backup-store", href: "/backup-stores" },
  ceph_no_quorum: { kind: "ceph", href: "/ceph" },
  sdn_not_ready: { kind: "sdn", href: "/sdn" },
  vmware_unreachable: { kind: "vmware-migration", href: "/vmware-migrations" },
  vmware_auth: { kind: "vmware-migration", href: "/vmware-migrations" },
};

export interface FailureLink {
  /** 화면 종류(`tasks.drawer.kinds.*` 이름) */
  kind: string;
  /** 보일 이름(값), 화면 자체면 없음 */
  name?: string;
  href: string;
}

/** 원인 값에서 관련 화면 링크(노드·풀·VM·볼륨·네트워크 + 코드마다 고칠 곳). 노드 id만 아는 값은 링크하지 않는다. */
export function failureLinks(failure: TaskFailure): FailureLink[] {
  const out: FailureLink[] = [];
  for (const [key, value] of Object.entries(failure.params ?? {})) {
    const kind = PARAM_KINDS[key];
    // 노드 이름을 몰라 id를 단 값은 화면이 없다.
    if (!kind || (kind === "node" && /^[0-9a-f-]{36}$/.test(value))) {
      continue;
    }
    const href = kindHref(kind, "", value);
    if (href) {
      out.push({ kind, name: value, href });
    }
  }
  const extra = CODE_LINKS[failure.code];
  if (extra && !out.some((l) => l.kind === extra.kind)) {
    out.push(extra);
  }
  return out;
}
