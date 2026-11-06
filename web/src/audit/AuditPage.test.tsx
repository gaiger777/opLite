import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { AuditEntry } from "@/api/types";
import { AuditPage } from "@/audit/AuditPage";
import { ADMIN, renderPage } from "@/test/render";
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

it("lists entries with their node, user, action, target and status", async () => {
  mockAudit();
  renderPage(<AuditPage />);
  await screen.findByText("alice");
  const rows = within(screen.getByRole("table", { name: "Audit log" })).getAllByRole("row");
  expect(rows).toHaveLength(3);
  for (const text of [
    "node-a",
    "alice",
    "10.0.0.5",
    "POST /vms/{key}/actions/{action}",
    "vms/web-1",
    "202",
  ]) {
    expect(rows[1]).toHaveTextContent(text);
  }
  expect(rows[1]!.querySelector("time")).toHaveAttribute("datetime", "2025-10-23T04:03:37.104Z");
  // 인증 전 실패: 사용자가 없고, 대상이 없으니 detail을 보이고, 상태는 경고 색이다.
  const cells = within(rows[2]!).getAllByRole("cell");
  expect(cells[2]).toHaveTextContent("—");
  expect(cells[5]).toHaveTextContent("username=mallory");
  expect(cells[6]).toHaveTextContent("401");
  expect(cells[6]).toHaveClass("text-destructive");
  expect(within(rows[1]!).getAllByRole("cell")[6]).not.toHaveClass("text-destructive");
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
  // 대상도 detail도 없으면 줄표
  const row = within(screen.getByRole("table", { name: "Audit log" })).getAllByRole("row")[1]!;
  expect(within(row).getAllByRole("cell")[5]).toHaveTextContent("—");
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

it("shows an entry as JSON when its row is clicked", async () => {
  mockAudit();
  renderPage(<AuditPage />);
  const login = await screen.findByRole("button", { name: "POST /auth/login" });
  await userEvent.click(login.closest("tr")!.querySelector("td")!);
  const dialog = await screen.findByRole("dialog", { name: "Audit entry" });
  expect(dialog).toHaveTextContent('"detail": "username=mallory"');
  expect(dialog).toHaveTextContent('"node": "node-b"');
  await userEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  await vi.waitFor(() => expect(login).toHaveFocus());
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
