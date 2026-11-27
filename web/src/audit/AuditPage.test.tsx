import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { AuditEntry } from "@/api/types";
import { AuditPage } from "@/audit/AuditPage";
import { expectNoAxeViolations } from "@/test/axe";
import { task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const START: AuditEntry = {
  id: "019a0f3c-8a10-7000-8000-000000000002",
  time: 1761192217104,
  node: "node-a",
  user: "alice",
  user_id: "00000000-0000-0000-0000-000000000002",
  role: "operator",
  auth: "token",
  ip: "10.0.0.5",
  method: "POST",
  path: "/vms/web-1/actions/start",
  action: "POST /vms/{key}/actions/{action}",
  target: { kind: "vms", key: "web-1" },
  status: 202,
  duration_ms: 12,
  detail: null,
};

const LOGIN: AuditEntry = {
  id: "019a0f3c-8a10-7000-8000-000000000001",
  time: 1761192200000,
  node: "node-b",
  user: null,
  user_id: null,
  role: null,
  auth: "none",
  ip: "10.0.0.9",
  method: "POST",
  path: "/auth/login",
  action: "POST /auth/login",
  target: null,
  status: 401,
  duration_ms: 3,
  detail: "username=mallory",
};

/** 감사 API의 응답을 정하고, 받은 쿼리를 모은다. */
function mockAudit(
  body: {
    entries: AuditEntry[];
    unreachable?: string[];
    disabled?: string[];
    truncated?: boolean;
  } = {
    entries: [START, LOGIN],
  },
) {
  const queries: URLSearchParams[] = [];
  server.use(
    http.get(apiUrl("/audit"), ({ request }) => {
      queries.push(new URL(request.url).searchParams);
      return HttpResponse.json({ unreachable: [], disabled: [], truncated: false, ...body });
    }),
  );
  return queries;
}

it("lists entries in plain words: who, what, result, node and IP", async () => {
  mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  const rows = within(screen.getByRole("table", { name: "Audit log" })).getAllByRole("row");
  expect(rows).toHaveLength(3);
  // "POST /vms/{key}/actions/{action}" 대신 동작 이름(계획 77-7) — 원문은 줄을 열어 본다
  const cells = within(rows[1]!).getAllByRole("cell");
  expect(cells[1]).toHaveTextContent("alice");
  expect(cells[2]).toHaveTextContent(/^Start VM web-1$/);
  // 대상은 이름에 이미 있으니 따로 쓰지 않는다
  expect(cells[3]).toHaveTextContent(/^$/);
  // 202는 "성공"이 아니라 "시작함"(일의 결과는 작업에)
  expect(cells[4]).toHaveTextContent(/^Started$/);
  expect(cells[5]).toHaveTextContent("node-a");
  expect(cells[6]).toHaveTextContent("10.0.0.5");
  expect(rows[1]).not.toHaveTextContent("POST");
  expect(rows[1]!.querySelector("time")).toHaveAttribute("datetime", "2025-10-23T04:03:37.104Z");
  // 인증 전 실패: 사용자가 없고, detail을 동작 밑에 보이고, 결과는 까닭과 함께 경고 색이다.
  const login = within(rows[2]!).getAllByRole("cell");
  expect(login[1]).toHaveTextContent("—");
  expect(login[2]).toHaveTextContent("Sign inusername=mallory");
  expect(login[4]).toHaveTextContent("Failed — wrong sign-in (401)");
  expect(login[4]!.firstElementChild).toHaveClass("text-destructive");
  expect(cells[4]!.firstElementChild).not.toHaveClass("text-destructive");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("puts the range and the applied filters in the query", async () => {
  const queries = mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  // 기본은 지난 24시간이고, 거르기는 넣지 않는다.
  expect(queries).toHaveLength(1);
  const day = Date.now() - Number(queries[0]!.get("since"));
  expect(day).toBeGreaterThanOrEqual(86_400_000);
  expect(day).toBeLessThan(86_400_000 + 60_000);
  expect([...queries[0]!.keys()]).toEqual(["since"]);

  await userEvent.selectOptions(screen.getByLabelText("Time range"), "Last hour");
  await vi.waitFor(() => expect(queries).toHaveLength(2));
  const hour = Date.now() - Number(queries[1]!.get("since"));
  expect(hour).toBeGreaterThanOrEqual(3_600_000);
  expect(hour).toBeLessThan(3_600_000 + 60_000);

  // 입력만 해서는 읽지 않는다. 적용해야 쿼리에 들어간다.
  await userEvent.type(screen.getByLabelText("User"), " alice ");
  await userEvent.type(screen.getByLabelText("Action"), "actions/start");
  expect(queries).toHaveLength(2);
  await userEvent.click(screen.getByRole("button", { name: "Apply" }));
  await vi.waitFor(() => expect(queries).toHaveLength(3));
  expect(queries[2]!.get("user")).toBe("alice");
  expect(queries[2]!.get("action")).toBe("actions/start");
  expect(Date.now() - Number(queries[2]!.get("since"))).toBeLessThan(3_600_000 + 60_000);
  expect(queries[2]!.has("limit")).toBe(false);
});

it("reads again on refresh", async () => {
  const queries = mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
  await vi.waitFor(() => expect(queries).toHaveLength(2));
});

it("warns about unreachable nodes and says when the list was cut", async () => {
  mockAudit({ entries: [START], unreachable: ["node-b", "node-c"], truncated: true });
  renderPage(<AuditPage />);
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "No audit entries from these nodes (unreachable): node-b, node-c",
  );
  expect(screen.getByRole("status")).toHaveTextContent(
    "Showing the newest 1 entries. More match; narrow the range or the filters.",
  );
});

it("says which nodes have the audit log turned off", async () => {
  mockAudit({ entries: [{ ...START, target: null }], disabled: ["node-c", "node-d"] });
  renderPage(<AuditPage />);
  expect(await screen.findByRole("status")).toHaveTextContent(
    "The audit log is turned off on these nodes: node-c, node-d",
  );
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("keeps the entries and says when reading again failed", async () => {
  let calls = 0;
  server.use(
    http.get(apiUrl("/audit"), () => {
      calls += 1;
      return calls === 1
        ? HttpResponse.json({ entries: [START], unreachable: [], disabled: [], truncated: false })
        : HttpResponse.json({ code: "invalid_request", message: "bad since" }, { status: 400 });
    }),
  );
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Could not read the audit log again. bad since",
  );
  expect(screen.getByText("alice")).toBeInTheDocument();
});

it("opens an entry in plain words with the raw request and the whole JSON", async () => {
  mockAudit();
  renderPage(<AuditPage />);
  const login = await screen.findByRole("button", { name: "Sign in" });
  await userEvent.click(login.closest("tr")!.querySelector("td")!);
  const dialog = await screen.findByRole("dialog", { name: "Sign in" });
  expect(dialog).toHaveTextContent("ResultFailed — wrong sign-in (401)");
  expect(dialog).toHaveTextContent("Whonot signed in");
  expect(dialog).toHaveTextContent("From10.0.0.9 · node node-b");
  expect(dialog).toHaveTextContent("Took3 ms");
  expect(dialog).toHaveTextContent("Detailsusername=mallory");
  expect(dialog).toHaveTextContent("Raw requestPOST /api/v1/auth/login");
  expect(within(dialog).getByRole("button", { name: "Copy the raw request" })).toBeInTheDocument();
  // 전체 JSON은 접어 둔다
  const json = within(dialog).getByText("Whole entry (JSON)").closest("details")!;
  expect(json).not.toHaveAttribute("open");
  expect(json).toHaveTextContent('"node": "node-b"');
  await expectNoAxeViolations(dialog, { disable: ["region"] });
  await userEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  await vi.waitFor(() => expect(login).toHaveFocus());
});

it("keeps the raw action for routes it has no name for", async () => {
  mockAudit({
    entries: [{ ...START, action: "POST /brand-new/{key}", path: "/brand-new/x", target: null }],
  });
  renderPage(<AuditPage />);
  expect(await screen.findByRole("button", { name: "POST /brand-new/{key}" })).toHaveClass(
    "font-mono",
  );
});

it("says why the log could not be read", async () => {
  server.use(
    http.get(apiUrl("/audit"), () =>
      HttpResponse.json({ code: "invalid_request", message: "bad since" }, { status: 400 }),
    ),
  );
  renderPage(<AuditPage />);
  expect(await screen.findByText("bad since")).toBeInTheDocument();
});

it("says empty ranges are empty", async () => {
  mockAudit({ entries: [] });
  renderPage(<AuditPage />);
  expect(await screen.findByText("No audit entries in this range.")).toBeInTheDocument();
});

it("does not read the log for other roles", async () => {
  const audit: string[] = [];
  const seen = ({ request }: { request: Request }) => {
    if (new URL(request.url).pathname === "/api/v1/audit") audit.push(request.url);
  };
  server.events.on("request:start", seen);
  try {
    renderPage(<AuditPage />, { as: { ...ADMIN, role: "operator" } });
    expect(
      await screen.findByText("Only administrators can read the audit log."),
    ).toBeInTheDocument();
    expect(audit).toEqual([]);
  } finally {
    server.events.removeListener("request:start", seen);
  }
});

it("names the balancer's automatic moves and shows what moved", async () => {
  // 리더가 스스로 남긴 항목: 사용자·IP·메서드가 없다.
  const MOVE: AuditEntry = {
    ...LOGIN,
    id: "019a0f3c-8a10-7000-8000-000000000003",
    node: "node-a",
    ip: "",
    method: "",
    path: "",
    action: "placement.move",
    target: { kind: "vms", key: "web-1" },
    status: 0,
    detail: "auto: web-1 node-a -> node-b (balance), imbalance 21.0% -> 12.5%, cost 4096 MiB",
  };
  mockAudit({ entries: [START, MOVE] });
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  const rows = within(screen.getByRole("table", { name: "Audit log" })).getAllByRole("row");
  const cells = within(rows[2]!).getAllByRole("cell");
  expect(cells[1]).toHaveTextContent("The node itself (automatic)");
  expect(cells[2]).toHaveTextContent(
    "Automatic move by the balancer — VM web-1auto: web-1 node-a -> node-b (balance), imbalance 21.0% -> 12.5%, cost 4096 MiB",
  );
  expect(cells[4]).toHaveTextContent("—");
});

const TASK_ID = "019a0f3c-0000-7000-8000-0000000000aa";

it("tells a preview, a started task (with its result) and a failed SSO redirect from a success", async () => {
  const entry = (n: number, extra: Partial<AuditEntry>): AuditEntry => ({
    ...START,
    id: `019a0f3c-8a10-7000-8000-00000000010${n}`,
    time: START.time - n,
    ...extra,
  });
  const history = { action: "POST /history/{kind}/{key}/restore", path: "/history/vm/web/restore" };
  const sso = { method: "GET", action: "GET /auth/oidc/callback", path: "/auth/oidc/callback" };
  mockAudit({
    entries: [
      // 미리 보기: 서버가 적은 결과, 옛 노드는 미리 보기가 있는 라우트의 200
      entry(1, { action: "POST /apply", path: "/apply", status: 200, outcome: "dry_run" }),
      entry(2, { ...history, status: 200 }),
      // 시작함: 서버가 적은 작업과 그 지금 상태
      entry(3, { ...history, status: 202, task: TASK_ID }),
      // SSO 콜백은 실패도 302 — 서버가 적은 결과, 옛 노드는 detail의 error=
      entry(4, { ...sso, status: 302, outcome: "failed", detail: "realm=okta error=no_role" }),
      entry(5, { ...sso, status: 302, detail: "realm=<unknown> error=sso_failed" }),
      entry(6, { ...sso, status: 302, detail: "realm=okta username=alice" }),
    ],
  });
  mockLists({
    task: [{ ...task(TASK_ID, "apply", "x"), status: { state: "failed", error: "boom" } }],
  });
  renderPage(<AuditPage />);
  const table = await screen.findByRole("table", { name: "Audit log" });
  const results = () =>
    within(table)
      .getAllByRole("row")
      .slice(1)
      .map((row) => within(row).getAllByRole("cell")[4]!.textContent);
  await vi.waitFor(() =>
    expect(results()).toEqual([
      "Preview (nothing changed)",
      "Preview (nothing changed)",
      "Started · Task Failed",
      "Failed (sent back to the sign-in page)",
      "Failed (sent back to the sign-in page)",
      "Succeeded",
    ]),
  );
  expect(within(table).getByRole("link", { name: "Task Failed" })).toHaveAttribute(
    "href",
    expect.stringContaining(`task=${TASK_ID}`),
  );
});

it("shows the target when the action's name does not say it, and names the nodes' own entries", async () => {
  const FOLDER_ID = "019a0f3c-0000-7000-8062-000000000001";
  const node = (n: number, action: string, kind: string, key: string): AuditEntry => ({
    ...LOGIN,
    id: `019a0f3c-8a10-7000-8000-00000000020${n}`,
    ip: "",
    method: "",
    path: "",
    action,
    target: { kind, key },
    status: 0,
    detail: null,
  });
  mockAudit({
    entries: [
      // 이름에 대상이 없는 라우트: 대상 열에 — id는 아는 이름으로
      {
        ...START,
        method: "DELETE",
        action: "DELETE /folders/{id}",
        path: `/folders/${FOLDER_ID}`,
        target: { kind: "folders", key: FOLDER_ID },
        status: 204,
      },
      node(1, "apply.update", "vm", "web"),
      node(2, "vmware-vm.cutover", "vmware-vm", "legacy-db"),
      node(3, "node.bond-down", "nodes", "node-b"),
      // 표에 없는 노드 항목: 원문과 대상
      node(4, "brand.new", "things", "t1"),
    ],
  });
  mockLists({ folder: [{ id: FOLDER_ID, name: FOLDER_ID, spec: { name: "prod", kind: "vm" } }] });
  renderPage(<AuditPage />);
  const table = await screen.findByRole("table", { name: "Audit log" });
  const cells = (i: number) =>
    within(within(table).getAllByRole("row")[i + 1]!).getAllByRole("cell");
  await vi.waitFor(() => expect(cells(0)[3]).toHaveTextContent("folders/prod"));
  expect(cells(0)[2]).toHaveTextContent("Delete a folder");
  expect(cells(1)[2]).toHaveTextContent("YAML apply: change virtual machine web");
  expect(cells(1)[3]).toHaveTextContent(/^$/);
  expect(cells(2)[2]).toHaveTextContent("Cut over VMware VM legacy-db");
  expect(cells(3)[2]).toHaveTextContent("Node node-b: all bonded ports went down");
  expect(cells(4)[2]).toHaveTextContent("brand.new");
  expect(cells(4)[3]).toHaveTextContent("things/t1");
});

it("explains an empty range", async () => {
  mockAudit({ entries: [] });
  renderPage(<AuditPage />, { as: ADMIN });
  const empty = (await screen.findByText("No audit entries in this range.")).closest<HTMLElement>(
    "[data-slot=empty-state]",
  )!;
  expect(empty).toHaveTextContent("Widen the time range");
  expect(within(empty).getByRole("button", { name: "Learn more" })).toBeInTheDocument();
});

it("pages long logs fifty lines at a time", async () => {
  const many = Array.from({ length: 60 }, (_, i) => ({
    ...LOGIN,
    id: `019a0f3c-8a10-7000-8000-${String(i).padStart(12, "0")}`,
    time: LOGIN.time - i * 1000,
  }));
  mockAudit({ entries: many });
  renderPage(<AuditPage />);
  const table = await screen.findByRole("table", { name: "Audit log" });
  await within(table).findAllByText("Sign in");
  // 머리 줄 + 50줄
  expect(within(table).getAllByRole("row")).toHaveLength(51);
  expect(screen.getByRole("navigation", { name: "Pages" })).toBeInTheDocument();
});
