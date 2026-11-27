import { existsSync, readdirSync, readFileSync } from "node:fs";

import type { AuditEntry } from "@/api/types";
import { auditOutcome, auditTarget, describeAudit, routeParams } from "@/audit/describe";
import i18n from "@/i18n";

const BASE: AuditEntry = {
  id: "019a0f3c-8a10-7000-8000-000000000002",
  time: 0,
  node: "n",
  user: "u",
  user_id: null,
  role: null,
  auth: "session",
  ip: "",
  method: "POST",
  path: "",
  action: "",
  target: null,
  status: 200,
  duration_ms: 0,
  detail: null,
};

const entry = (action: string, path: string): AuditEntry => ({
  ...BASE,
  method: action.split(" ")[0]!,
  action,
  path,
});

it("fills the route's slots from the real path", () => {
  expect(routeParams("/vms/{key}/snapshots/{name}", "/vms/web%2001/snapshots/s1")).toEqual({
    key: "web 01",
    name: "s1",
  });
  const t = i18n.getFixedT("ko");
  expect(
    describeAudit(entry("POST /vms/{key}/actions/{action}", "/vms/web/actions/start"), t),
  ).toBe("VM web 켜기");
  expect(
    describeAudit(
      entry("POST /vms/{key}/snapshots/{name}/revert", "/vms/web/snapshots/s1/revert"),
      t,
    ),
  ).toBe("VM web을(를) 스냅샷 s1(으)로 되돌리기");
  // 모르는 VM 동작은 그 낱말 그대로, 이름에 든 꺾쇠는 글 그대로
  expect(
    describeAudit(
      entry("POST /vms/{key}/actions/{action}", "/vms/<b>/actions/zap"),
      i18n.getFixedT("en"),
    ),
  ).toBe("zap VM <b>");
  expect(describeAudit(entry("POST /nope", "/nope"), t)).toBeNull();
  // id로 부른 VM은 아는 이름으로
  const id = "019a0f3c-0000-7000-8000-000000000001";
  expect(
    describeAudit(entry("GET /vms/{key}/console", `/vms/${id}/console`), t, new Map([[id, "web"]])),
  ).toBe("VM web 콘솔 열기");
});

it("names every route that changes something (OpenAPI) and the console", () => {
  // 생성된 API 형(schema.d.ts)의 경로마다 바꾸는 메서드 — 새 라우트를 더하면 이름 표에도 더한다.
  const schema = readFileSync("src/api/schema.d.ts", "utf8");
  const paths = schema.slice(0, schema.indexOf("export type webhooks"));
  const missing: string[] = [];
  for (const m of paths.matchAll(/^ {4}"(\/[^"]*)": \{\n([\s\S]*?)^ {4}\};/gm)) {
    for (const method of ["put", "post", "delete", "patch"]) {
      if (new RegExp(`^ {8}${method}: operations`, "m").test(m[2]!)) {
        const action = `${method.toUpperCase()} ${m[1]}`;
        if (!i18n.exists(`auditOps.route.${action}`, { lng: "ko" })) missing.push(action);
        if (!i18n.exists(`auditOps.route.${action}`, { lng: "en" })) missing.push(`en ${action}`);
      }
    }
  }
  expect(missing).toEqual([]);
  expect(i18n.exists("auditOps.route.GET /vms/{key}/console")).toBe(true);
});

/** 저장소의 Rust 소스(시험 밖)에서 노드가 스스로 남기는 감사 항목의 동작 이름을 모은다. */
function systemActions(): string[] {
  const found = new Set<string>();
  const walk = (dir: string) => {
    for (const name of readdirSync(dir, { withFileTypes: true })) {
      const path = `${dir}/${name.name}`;
      if (name.isDirectory()) {
        walk(path);
      } else if (name.name.endsWith(".rs")) {
        const text = readFileSync(path, "utf8").split("#[cfg(test)]")[0]!;
        if (!text.includes("system_entry(")) continue;
        // 노드 항목의 동작은 "<무엇>.<일>"(소문자·-) — 파일 이름 같은 다른 글(audit.log)은 뺀다
        for (const m of text.matchAll(/"([a-z]+(?:-[a-z]+)*\.[a-z]+(?:-[a-z]+)*)"/g)) {
          if (m[1] !== "audit.log") found.add(m[1]!);
        }
        // 선언형 적용 단계(`format!("apply.{}", verb(...))`): verb가 돌려주는 낱말마다
        if (text.includes('format!("apply.{}"')) {
          const verbs = /fn verb\([^)]*\)[^{]*\{([\s\S]*?)\n\}/.exec(text)![1]!;
          for (const v of verbs.matchAll(/=> "([a-z]+)"/g)) found.add(`apply.${v[1]}`);
        }
      }
    }
  };
  for (const crate of readdirSync("../crates")) {
    if (existsSync(`../crates/${crate}/src`)) walk(`../crates/${crate}/src`);
  }
  // write_dropped·suppressed_entry는 system_entry를 거치지 않는다
  found.add("audit.dropped").add("audit.suppressed");
  return [...found].sort();
}

it("names every entry the nodes write on their own (system_entry in the server)", () => {
  const actions = systemActions();
  // 모으기가 제대로 도는지(모두 빠지면 시험이 헛돈다)
  expect(actions).toEqual(
    expect.arrayContaining(["apply.update", "acme.order", "vmware-vm.cutover", "node.bond-down"]),
  );
  const missing = actions.flatMap((a) =>
    ["ko", "en"]
      .filter((lng) => !i18n.exists(`auditOps.system.${a}`, { lng }))
      .map((l) => `${l} ${a}`),
  );
  expect(missing).toEqual([]);
});

it("tells results apart: preview, started, failed SSO and plain success", () => {
  const t = i18n.getFixedT("ko");
  const outcome = (extra: Partial<AuditEntry>) => auditOutcome({ ...BASE, ...extra }, t)?.text;
  expect(outcome({ action: "POST /apply", status: 200 })).toBe("미리 보기(바꾼 것 없음)");
  expect(outcome({ action: "POST /vms", status: 200, outcome: "dry_run" })).toBe(
    "미리 보기(바꾼 것 없음)",
  );
  expect(outcome({ action: "POST /apply", status: 202 })).toBe("시작함");
  expect(auditOutcome({ ...BASE, status: 202, task: "t1" }, t)?.task).toBe("t1");
  const sso = { action: "GET /auth/oidc/callback", status: 302 };
  expect(outcome({ ...sso, detail: "realm=okta error=no_role" })).toBe(
    "실패(로그인 화면으로 돌려보냄)",
  );
  expect(outcome({ ...sso, detail: "realm=okta username=a", outcome: "failed" })).toBe(
    "실패(로그인 화면으로 돌려보냄)",
  );
  expect(outcome({ ...sso, detail: "realm=okta username=a" })).toBe("성공");
  expect(outcome({ status: 0 })).toBeUndefined();
});

it("shows the target only when the action's name does not already say it", () => {
  const e = { ...BASE, target: { kind: "folders", key: "f1" } };
  expect(auditTarget(e, "폴더 지우기")).toBe("folders/f1");
  expect(auditTarget(e, "폴더 지우기", new Map([["f1", "prod"]]))).toBe("folders/prod");
  expect(auditTarget({ ...e, target: { kind: "vms", key: "web" } }, "VM web 켜기")).toBeNull();
  expect(auditTarget({ ...BASE, target: null }, null)).toBeNull();
});
