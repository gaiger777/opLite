import { readFileSync } from "node:fs";

import type { AuditEntry } from "@/api/types";
import { describeAudit, routeParams } from "@/audit/describe";
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
