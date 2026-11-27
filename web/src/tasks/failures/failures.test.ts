import { readFileSync } from "node:fs";
import { join } from "node:path";

import type { Task } from "@/api/types";
import i18n from "@/i18n";
import mainEn from "@/i18n/en.json";
import { failureLinks, failureText, failureTitle } from "@/tasks/failures/failure";
import en from "@/tasks/failures/i18n/en.json";
import ko from "@/tasks/failures/i18n/ko.json";

const crates = join(import.meta.dirname, "../../../../crates");

/** 서버의 원인 코드와 서버가 다는 값 이름(`orbivisor-types/src/task_failure.rs`의 `codes!`) */
function serverCodes(): Map<string, string[]> {
  const text = readFileSync(join(crates, "orbivisor-types/src/task_failure.rs"), "utf8");
  const start = text.indexOf("codes! {");
  const block = text.slice(start, text.indexOf("\n}\n", start));
  // 줄바꿈·띄어쓰기가 달라도 읽는다. 항목 수(`= "`)와 읽은 수가 같아야 한다 — 다른 모양으로 쓴 줄이 조용히 빠지지 않게.
  const entries = [
    ...block.matchAll(/[A-Z_]+\s*=\s*"([a-z_]+)"\s*,\s*(?:true|false)\s*,\s*\[([^\]]*)\]\s*;/g),
  ];
  expect(entries.length, "codes! entries that could not be read").toBe(
    block.match(/=\s*"/g)?.length ?? 0,
  );
  return new Map(entries.map((m) => [m[1]!, [...m[2]!.matchAll(/"(\w+)"/g)].map((p) => p[1]!)]));
}

/** 서버의 경보 조건 종류(`AlertCondition`의 변형 이름을 snake_case로) */
function alertConditions(): string[] {
  const text = readFileSync(join(crates, "orbivisor-types/src/alert.rs"), "utf8");
  const start = text.indexOf("pub enum AlertCondition {");
  const block = text.slice(start, text.indexOf("\n}\n", start));
  return [...block.matchAll(/^ {4}([A-Z][A-Za-z]*)\b/gm)].map((m) =>
    m[1]!.replace(/[A-Z]/g, (c, i: number) => (i ? "_" : "") + c.toLowerCase()),
  );
}

const placeholders = (s: string) => [...s.matchAll(/\{\{(\w+)\}\}/g)].map((m) => m[1]).sort();

it("has a ko and en entry for every server failure code, and nothing else", () => {
  const params = serverCodes();
  const codes = [...params.keys()];
  expect(codes.length).toBeGreaterThanOrEqual(20);
  expect(Object.keys(en.taskErrors).sort()).toEqual([...codes].sort());
  expect(Object.keys(ko.taskErrors).sort()).toEqual([...codes].sort());
  for (const code of codes) {
    const e = en.taskErrors[code as keyof typeof en.taskErrors];
    const k = ko.taskErrors[code as keyof typeof ko.taskErrors];
    for (const field of ["title", "cause", "fix"] as const) {
      expect(e[field].trim(), `${code}.${field}`).not.toBe("");
      expect(k[field].trim(), `${code}.${field}`).not.toBe("");
      // 두 언어가 같은 값을 쓴다.
      expect(placeholders(k[field]), `${code}.${field}`).toEqual(placeholders(e[field]));
      // 서버가 다는 값만 쓴다(`need_bytes`는 사람 단위 `need`로도).
      const known = params
        .get(code)!
        .flatMap((p) => (p.endsWith("_bytes") ? [p, p.slice(0, -"_bytes".length)] : [p]));
      for (const name of placeholders(e[field])) {
        expect(known, `${code}.${field} uses {{${name}}}`).toContain(name);
      }
    }
  }
});

it("has a cause and fix for every alert condition", () => {
  const conditions = alertConditions();
  expect(conditions).toContain("node_offline");
  expect(Object.keys(en.alertRunbook).sort()).toEqual([...conditions].sort());
  expect(Object.keys(ko.alertRunbook).sort()).toEqual([...conditions].sort());
  expect(Object.keys(ko.failureUi).sort()).toEqual(Object.keys(en.failureUi).sort());
  // 첫 화면 묶음에는 없다(늦게 받는 조각).
  for (const top of Object.keys(en)) {
    expect(Object.keys(mainEn)).not.toContain(top);
  }
});

function failed(failure: NonNullable<Task["status"]["failure"]>): Task {
  return {
    id: "0193f1d2-0000-7000-8000-000000000001",
    name: "t",
    revision: 1,
    labels: {},
    spec: { op: { kind: "vm-migrate", node: "n2" }, target: "vm1", node: "n1" },
    status: { state: "failed", error: "raw", failure },
  } as Task;
}

it("fills the dictionary with the failure values", async () => {
  await i18n.changeLanguage("ko");
  const task = failed({
    code: "pool_full",
    params: { pool: "local", node: "svc02", need_bytes: "21474836480", free_bytes: "3221225472" },
  });
  const text = failureText(task, i18n.t)!;
  expect(text.title).toBe("노드 svc02의 저장소 풀 local에 공간이 모자랍니다.");
  expect(text.cause).toBe("필요한 공간은 20 GiB인데 남은 공간은 3 GiB입니다.");
  expect(text.fix).toHaveLength(1);
  expect(failureTitle(task, i18n.t)).toBe(text.title);
  // 여러 줄 해결은 단계로
  expect(failureText(failed({ code: "task_timeout" }), i18n.t)!.fix).toHaveLength(2);
  // 모르는 코드·성공한 Task·코드 없는 실패는 원문만
  expect(failureText(failed({ code: "from_the_future" }), i18n.t)).toBeNull();
  expect(failureText({ ...task, status: { state: "failed", error: "x" } }, i18n.t)).toBeNull();
  // 값이 빠졌으면 자리를 "?"로
  expect(failureText(failed({ code: "node_offline" }), i18n.t)!.title).toContain("노드 ?");
  await i18n.changeLanguage("en");
});

it("links to the pages the failure values name", () => {
  expect(failureLinks({ code: "pool_full", params: { pool: "local", node: "svc02" } })).toEqual([
    { kind: "storage-pool", name: "local", href: "/storage-pools?q=local" },
    { kind: "node", name: "svc02", href: "/nodes/svc02" },
  ]);
  // 노드 이름을 몰라 id만 있으면 링크하지 않는다. 코드마다 고칠 곳은 덧붙인다.
  expect(
    failureLinks({
      code: "node_not_member",
      params: { node: "0193f1d2-0000-7000-8000-000000000001" },
    }),
  ).toEqual([]);
  expect(failureLinks({ code: "backup_store_denied", params: { store: "s3" } })).toEqual([
    { kind: "backup-store", href: "/backup-stores" },
  ]);
  // 적용 단계 실패는 만들다 실패한(아직 없는) 항목의 화면을 가리키지 않는다(리뷰 I6).
  expect(
    failureLinks({
      code: "apply_step_failed",
      params: { step: "1/1", action: "create", kind: "BackupStore", name: "s3", cause: "x" },
    }),
  ).toEqual([]);
  // 내려받기 공간은 풀이 아니라 그 노드의 업로드 디렉터리(리뷰 I3)
  expect(
    failureLinks({ code: "upload_dir_full", params: { node: "svc01", dir: "/var/lib/x/uploads" } }),
  ).toEqual([{ kind: "node", name: "svc01", href: "/nodes/svc01" }]);
});
