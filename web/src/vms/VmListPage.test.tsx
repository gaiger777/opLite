import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { List } from "@/api/live";
import { putResource } from "@/api/optimistic";
import type { Task } from "@/api/types";
import { snapshot, task, NODE, vm } from "@/test/fixtures";
import { expectNoAxeViolations } from "@/test/axe";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { resetTasks } from "@/tasks/tasks";
import { apiUrl, server } from "@/test/server";
import { VmListPage } from "@/vms/VmListPage";

const web = vm("web", "running");
const db = vm("db", "stopped", { spec: { ...vm("db", "stopped").spec, memory_mib: 4096 } });

// 알림과 지켜보는 Task는 모듈 전역이므로 테스트마다 비운다.
afterEach(() => {
  toast.dismiss();
  resetTasks();
  window.localStorage.clear();
});

/** 일괄 막대의 동작을 눌러 확인 대화상자를 연다. */
async function openBulk(action: string) {
  const bar = screen.getByRole("region", { name: "Bulk actions" });
  await userEvent.click(within(bar).getByRole("button", { name: new RegExp(`^${action} \\(`) }));
  return screen.findByRole("dialog", { name: new RegExp(`^${action} \\d+ VMs?$`) });
}

/** 확인 대화상자에서 동작을 누른다. */
async function confirmBulk(dialog: HTMLElement, action: string) {
  await userEvent.click(within(dialog).getByRole("button", { name: action }));
}

function rowOf(name: string) {
  return screen.getByRole("link", { name }).closest("tr")!;
}

it("lists, searches and sorts VMs", async () => {
  mockLists({ vm: [web, db], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  const row = rowOf("db");
  expect(row).toHaveTextContent("Stopped");
  expect(row).toHaveTextContent("4 GiB");
  expect(rowOf("web")).toHaveTextContent("node-a");

  await userEvent.click(screen.getByRole("button", { name: /Name/ }));
  const names = screen.getAllByRole("link").map((a) => a.textContent);
  expect(names.filter((n) => n === "db" || n === "web")).toEqual(["db", "web"]);

  await userEvent.type(screen.getByLabelText("Search by name"), "we");
  expect(screen.queryByRole("link", { name: "db" })).not.toBeInTheDocument();
});

it("starts a VM from its menu and selected VMs together", async () => {
  mockLists({ vm: [web, db, vm("app", "stopped")], node: [NODE] });
  const started: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      started.push(`${String(params.action)}:${String(params.key)}`);
      return HttpResponse.json(task(`t-${started.length}`, "vm-start", String(params.key)), {
        status: 202,
      });
    }),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Start" }));
  expect(await screen.findByText("Start db")).toBeInTheDocument();
  expect(started).toEqual([`start:${db.id}`]);

  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  const dialog = await openBulk("Start");
  // 실행 중인 web은 까닭과 함께 보이고 건너뛴다.
  expect(within(dialog).getByText("Possible: 2 · Not possible: 1")).toBeInTheDocument();
  expect(within(dialog).getByText("The VM is running.")).toBeInTheDocument();
  await confirmBulk(dialog, "Start");
  await waitFor(() => expect(started).toHaveLength(3));
  expect(started.slice(1).sort()).toEqual(
    [`start:${db.id}`, `start:${vm("app", "stopped").id}`].sort(),
  );
});

it("force stops a VM while a snapshot is being taken but not while one is reverted", async () => {
  const running = (id: string, kind: Task["spec"]["op"]["kind"]) => ({
    ...task(id, kind, web.id),
    status: { state: "running" as const, error: null },
  });
  mockLists({ vm: [web], node: [NODE], task: [running("t-create", "snapshot-create")] });
  const sent: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ request }) => {
      sent.push(new URL(request.url).search);
      return HttpResponse.json(task("t-stop", "vm-stop", web.id), { status: 202 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  // Task 목록을 읽은 뒤에 메뉴를 연다.
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  // 서버는 만들기 중 강제 정지를 받는다(만들기가 실패로 끝난다).
  await userEvent.click(await screen.findByRole("menuitem", { name: "Force stop" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Force stop" }),
  );
  await waitFor(() => expect(sent).toEqual(["?mode=force"]));

  // 되돌리기 중에는 막고 까닭을 보인다.
  queryClient.setQueryData<List<Task>>(["list", "task"], {
    revision: 2,
    items: [running("t-revert", "snapshot-revert")],
  });
  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  const item = await screen.findByRole("menuitem", { name: "Force stop" });
  expect(item).toHaveAttribute("aria-disabled", "true");
  expect(item).toHaveAccessibleDescription("Not available while a snapshot is being reverted.");
  expect(screen.getByRole("menuitem", { name: "Stop" })).not.toHaveAttribute("aria-disabled");
});

it("blocks starting a VM whose start is still pending, alone or with others", async () => {
  const app = vm("app", "stopped");
  const starting = task("t-start", "vm-start", db.id);
  mockLists({ vm: [web, db, app], node: [NODE], task: [starting] });
  const started: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      started.push(String(params.key));
      return HttpResponse.json(task(`t-p${started.length}`, "vm-start", String(params.key)), {
        status: 202,
      });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  // Task 목록을 읽은 뒤에 메뉴를 연다.
  await waitFor(() => expect(queryClient.getQueryData(["list", "task"])).toBeDefined());
  await userEvent.click(screen.getByRole("button", { name: "Actions for db" }));
  const start = await screen.findByRole("menuitem", { name: "Start" });
  expect(start).toHaveAttribute("aria-disabled", "true");
  expect(start).toHaveAccessibleDescription("A power action is still in progress.");
  // 골라도 시작하지 않는다.
  await userEvent.click(start);
  await userEvent.keyboard("{Escape}");

  // 여럿을 시작할 때도 db는 건너뛴다.
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  const dialog = await openBulk("Start");
  expect(within(dialog).getByText("A power action is still in progress.")).toBeInTheDocument();
  await confirmBulk(dialog, "Start");
  await waitFor(() => expect(started).toEqual([app.id]));
});

it("deletes only after the name is typed", async () => {
  mockLists({ vm: [db], node: [NODE] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  expect(confirm).toBeDisabled();
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  // 스냅샷이 없으면 고를 것이 없다.
  expect(within(dialog).queryByRole("checkbox")).not.toBeInTheDocument();
  await userEvent.click(confirm);
  expect(await screen.findByText("Deleted db.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("deletes the snapshots one by one before the VM", async () => {
  const first = snapshot("before", db, 1);
  const second = snapshot("after", db, 2, { spec: { parent: first.id } });
  mockLists({ vm: [db], node: [NODE], snapshot: [first, second, snapshot("x", web, 3)] });
  const removed: string[] = [];
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/snapshots/:name`), ({ params }) => {
      removed.push(String(params.name));
      return HttpResponse.json(task(`t-${String(params.name)}`, "snapshot-delete", db.id), {
        status: 202,
      });
    }),
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  const succeed = (...ids: string[]) =>
    queryClient.setQueryData<List<Task>>(["list", "task"], {
      revision: 2,
      items: ids.map((id) => ({
        ...task(`t-${id}`, "snapshot-delete", db.id),
        status: { state: "succeeded", error: null },
      })),
    });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  // 스냅샷이 남으면 서버가 지우지 않으므로 고르기 전에는 막고 까닭을 보인다.
  expect(confirm).toBeDisabled();
  expect(dialog).toHaveTextContent("A VM with snapshots cannot be deleted.");
  await userEvent.click(
    within(dialog).getByRole("checkbox", { name: "Also delete all 2 of its snapshots" }),
  );
  expect(dialog).not.toHaveTextContent("A VM with snapshots cannot be deleted.");
  await userEvent.click(confirm);

  // 앞 스냅샷의 Task가 끝나야 다음을 지우고, 모두 끝나야 VM을 지운다.
  await waitFor(() => expect(removed).toEqual([first.id]));
  expect(await screen.findByText("Delete snapshot before of db")).toBeInTheDocument();
  // 다음 DELETE를 함께 보내지 않았다.
  expect(removed).toEqual([first.id]);
  expect(dialog).toHaveTextContent("This cannot be cancelled once started.");
  // 지운 스냅샷이 목록에서 빠져도 누를 때의 수를 그대로 보인다.
  queryClient.setQueryData(["list", "snapshot"], { revision: 3, items: [second] });
  await new Promise((r) => setTimeout(r, 20));
  expect(dialog).toHaveTextContent("Also delete all 2 of its snapshots");
  succeed(first.id);
  await waitFor(() => expect(removed).toEqual([first.id, second.id]));
  expect(deleted).toBe(false);
  succeed(first.id, second.id);
  expect(await screen.findByText("Deleted db.")).toBeInTheDocument();
  expect(deleted).toBe(true);
  await waitFor(() => expect(screen.getByRole("button", { name: "Actions for db" })).toHaveFocus());
});

it("waits for a snapshot being taken before deleting", async () => {
  const taking = snapshot("taking", db, 1, { status: { phase: "pending", created_at: null } });
  mockLists({ vm: [db], node: [NODE], snapshot: [taking] });
  const sent: string[] = [];
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/*`), ({ request }) => {
      sent.push(request.url);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Also delete its snapshot" }));
  expect(dialog).toHaveTextContent("A snapshot of this VM is still being taken.");
  expect(within(dialog).getByRole("button", { name: "Delete" })).toBeDisabled();
  expect(sent).toEqual([]);
});

it("keeps the VM when a snapshot cannot be deleted", async () => {
  const only = snapshot("before", db, 1);
  mockLists({ vm: [db], node: [NODE], snapshot: [only] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/snapshots/${only.id}`), () =>
      HttpResponse.json(task("t-s", "snapshot-delete", db.id), { status: 202 }),
    ),
    http.delete(apiUrl(`/vms/${db.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Also delete its snapshot" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  await waitFor(() => expect(queryClient.getQueryCache().hasListeners()).toBe(true));
  queryClient.setQueryData<List<Task>>(["list", "task"], {
    revision: 2,
    items: [
      { ...task("t-s", "snapshot-delete", db.id), status: { state: "failed", error: "disk busy" } },
    ],
  });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("disk busy");
  expect(deleted).toBe(false);
  expect(dialog).toBeVisible();
});

it("deletes nothing when the VM has started since the dialog opened", async () => {
  const only = snapshot("before", db, 1);
  mockLists({ vm: [db], node: [NODE], snapshot: [only] });
  const sent: string[] = [];
  server.use(
    http.delete(apiUrl(`/vms/${db.id}/*`), ({ request }) => {
      sent.push(request.url);
      return new HttpResponse(null, { status: 204 });
    }),
    http.delete(apiUrl(`/vms/${db.id}`), ({ request }) => {
      sent.push(request.url);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { queryClient } = renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Also delete its snapshot" }));
  // 대화상자가 열린 동안 다른 곳에서 VM을 시작했다.
  queryClient.setQueryData(["list", "vm"], { revision: 3, items: [vm("db", "running")] });
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("Stop the VM first.");
  expect(sent).toEqual([]);
  expect(dialog).toBeVisible();
});

it("waits for the snapshots before offering a delete", async () => {
  mockLists({ vm: [db], node: [NODE] });
  server.use(
    http.get(apiUrl("/snapshots"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VmListPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type db to confirm"), "db");
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(within(dialog).getByRole("button", { name: "Delete" })).toBeDisabled();
});

it("hides changes from viewers", async () => {
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmListPage />, { as: { user: "u", name: "v", role: "viewer" } });
  await screen.findByRole("link", { name: "web" });
  expect(screen.queryByRole("link", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for web" })).not.toBeInTheDocument();
  expect(screen.queryByRole("checkbox", { name: "Select web" })).not.toBeInTheDocument();
});

it("says why the list could not be read", async () => {
  mockLists({ node: [NODE] });
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "permission denied" }, { status: 403 }),
    ),
  );
  renderPage(<VmListPage />);
  expect(await screen.findByText("You do not have permission to do this.")).toBeInTheDocument();
});

it("marks the sort order on header cells and ignores a bad saved sort", async () => {
  window.localStorage.setItem("opl.vms.sort", JSON.stringify([{ id: "secret", desc: 1 }]));
  mockLists({ vm: [web, db], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  const header = screen.getByRole("columnheader", { name: /Name/ });
  expect(header).toHaveAttribute("aria-sort", "none");
  await userEvent.click(within(header).getByRole("button"));
  expect(header).toHaveAttribute("aria-sort", "ascending");
  await userEvent.click(within(header).getByRole("button"));
  expect(header).toHaveAttribute("aria-sort", "descending");
  expect(JSON.parse(window.localStorage.getItem(`opl.${ADMIN.user}.cols.vms`)!)).toEqual({
    hidden: [],
    sort: "-name",
  });
  expect(screen.getByRole("columnheader", { name: "Actions" })).not.toHaveAttribute("aria-sort");
});

it("moves the old saved sort over", async () => {
  window.localStorage.setItem("opl.vms.sort", JSON.stringify([{ id: "memory", desc: true }]));
  mockLists({ vm: [web, db], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  expect(screen.getByRole("columnheader", { name: /Memory/ })).toHaveAttribute(
    "aria-sort",
    "descending",
  );
});

it("shows a dash for a VM without a node", async () => {
  const loose = vm("loose", "stopped", { spec: { ...vm("loose", "stopped").spec, node: null } });
  mockLists({ vm: [loose], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "loose" });
  expect(within(rowOf("loose")).getAllByRole("cell")[3]).toHaveTextContent(/^—$/);
});

it("acts only on selected VMs that the search still shows", async () => {
  const app = vm("app", "stopped");
  mockLists({ vm: [web, db, app], node: [NODE] });
  const started: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      started.push(String(params.key));
      return HttpResponse.json(task(`t-f${started.length}`, "vm-start", String(params.key)), {
        status: 202,
      });
    }),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  await userEvent.type(screen.getByLabelText("Search by name"), "d");
  // 가려진 행은 고르기에서 빠진다.
  expect(screen.getByText("1 selected")).toBeInTheDocument();
  const dialog = await openBulk("Start");
  await confirmBulk(dialog, "Start");
  await waitFor(() => expect(started).toEqual([db.id]));
  await userEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  await userEvent.clear(screen.getByLabelText("Search by name"));
  expect(screen.getByRole("checkbox", { name: "Select app" })).not.toBeChecked();
});

it("names the VM that failed and sums up a bulk action", async () => {
  const app = vm("app", "stopped");
  // db의 Task는 이미 끝났다(목록에 있으면 기다림이 곧 끝난다).
  const done = {
    ...task("t-bulk", "vm-start", db.id),
    status: { state: "succeeded", error: null },
  };
  mockLists({ vm: [web, db, app], node: [NODE], task: [done as Task] });
  const started: string[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      if (params.key === app.id) {
        return HttpResponse.json({ code: "conflict", message: "busy" }, { status: 409 });
      }
      started.push(String(params.key));
      return HttpResponse.json(task("t-bulk", "vm-start", String(params.key)), { status: 202 });
    }),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  const dialog = await openBulk("Start");
  await confirmBulk(dialog, "Start");
  // VM마다 결과를 보이고, 알림 하나로 묶는다. 실행 중인 web은 건너뛴다.
  expect(await within(dialog).findByText("Failed: busy")).toBeInTheDocument();
  expect(await within(dialog).findByText("Succeeded")).toBeInTheDocument();
  expect(
    await screen.findByText("Start: 1 of 2 succeeded, 1 failed. 1 VM skipped."),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "View tasks" })).toBeInTheDocument();
  expect(started).toEqual([db.id]);
});

it("shows why a VM pinned to a node in maintenance did not start", async () => {
  mockLists({ vm: [web, db], node: [NODE] });
  const message =
    'node "node-a" is in maintenance and no other node can run this VM; end maintenance first';
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), () =>
      HttpResponse.json({ code: "node_unavailable", message }, { status: 409 }),
    ),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  await userEvent.type(screen.getByLabelText("Search by name"), "d");
  const dialog = await openBulk("Start");
  await confirmBulk(dialog, "Start");
  expect(
    await within(dialog).findByText(`Failed: The node is unavailable: ${message}`),
  ).toBeInTheDocument();
});

it("marks HA VMs", async () => {
  const ha = vm("app", "running", { spec: { ...vm("app", "running").spec, ha: true } });
  mockLists({ vm: [web, ha], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "app" });
  expect(within(rowOf("app")).getByText("HA")).toBeInTheDocument();
  expect(within(rowOf("web")).queryByText("HA")).not.toBeInTheDocument();
});

it("has no accessibility violations", async () => {
  mockLists({ vm: [web, db], node: [NODE] });
  const { container } = renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  // 화면 하나만 그리므로 랜드마크(사이드바·main)는 셸 시험이 본다.
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("deletes several VMs only after their count is typed", async () => {
  const app = vm("app", "stopped");
  mockLists({ vm: [web, db, app], node: [NODE] });
  const deleted: string[] = [];
  server.use(
    http.delete(apiUrl("/vms/:key"), ({ params }) => {
      deleted.push(String(params.key));
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  const dialog = await openBulk("Delete");
  // 실행 중인 web은 영향 목록에 까닭과 함께 있고 지우지 않는다.
  expect(within(dialog).getByRole("table", { name: "Affected VMs" })).toHaveTextContent("web");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  expect(confirm).toBeDisabled();
  // 입력할 수는 고른 3이 아니라 실제로 지울 2다.
  expect(dialog).toHaveTextContent("2 of the 3 selected VMs will be affected.");
  await userEvent.type(within(dialog).getByLabelText("Type 2 to confirm"), "3");
  expect(confirm).toBeDisabled();
  await userEvent.clear(within(dialog).getByLabelText("Type 2 to confirm"));
  await userEvent.type(within(dialog).getByLabelText("Type 2 to confirm"), "2");
  await userEvent.click(confirm);
  await waitFor(() => expect(deleted.sort()).toEqual([db.id, app.id].sort()));
  expect(
    await screen.findByText("Delete: 2 of 2 succeeded, 0 failed. 1 VM skipped."),
  ).toBeVisible();
  // 고르기를 풀었다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.getByRole("checkbox", { name: "Select web" })).not.toBeChecked();
});

it("shows the live state of the chosen VMs while the bulk dialog is open", async () => {
  const app = vm("app", "stopped");
  mockLists({ vm: [db, app], node: [NODE] });
  const { queryClient } = renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "db" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  const dialog = await openBulk("Delete");
  await userEvent.type(within(dialog).getByLabelText("Type 2 to confirm"), "2");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  expect(confirm).toBeEnabled();

  // 대화상자가 열린 동안 누가 app을 켰다.
  act(() => putResource(queryClient, "vm", { ...vm("app", "running"), id: app.id, revision: 9 }));
  const row = within(within(dialog).getByRole("table", { name: "Affected VMs" }))
    .getByRole("cell", { name: "app" })
    .closest("tr")!;
  await waitFor(() => expect(within(row).getByTestId("vm-phase")).toHaveTextContent("Running"));
  expect(row).toHaveTextContent("The VM is running.");
  // 할 수가 1로 줄어 다시 입력해야 한다.
  expect(within(dialog).getByLabelText("Type 1 to confirm")).toHaveValue("2");
  expect(confirm).toBeDisabled();
});

it("sends at most four requests at once", async () => {
  const many = ["a1", "a2", "a3", "a4", "a5", "a6"].map((n) => vm(n, "stopped"));
  mockLists({ vm: many, node: [NODE] });
  let running = 0;
  let most = 0;
  let count = 0;
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), async ({ params }) => {
      running += 1;
      most = Math.max(most, running);
      await new Promise((resolve) => setTimeout(resolve, 20));
      running -= 1;
      count += 1;
      return HttpResponse.json(task(`t-c${count}`, "vm-start", String(params.key)), {
        status: 202,
      });
    }),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "a1" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  const dialog = await openBulk("Start");
  await confirmBulk(dialog, "Start");
  await waitFor(() => expect(count).toBe(6));
  expect(most).toBe(4);
  expect(within(dialog).getAllByText("Requested…")).toHaveLength(6);
});

it("migrates to the chosen node and snapshots with a bulk name", async () => {
  const other = { ...NODE, id: "n2", name: "node-b" };
  const app = vm("app", "running");
  mockLists({ vm: [web, app], node: [NODE, other] });
  const migrated: unknown[] = [];
  const snapped: { name: string; memory: boolean }[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/migrate"), async ({ request, params }) => {
      migrated.push({ key: params.key, ...((await request.json()) as object) });
      return HttpResponse.json(task(`t-m${migrated.length}`, "vm-migrate", String(params.key)), {
        status: 202,
      });
    }),
    http.post(apiUrl("/vms/:key/snapshots"), async ({ request, params }) => {
      snapped.push((await request.json()) as { name: string; memory: boolean });
      return HttpResponse.json(
        task(`t-s${snapped.length}`, "snapshot-create", String(params.key)),
        {
          status: 202,
        },
      );
    }),
  );
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "app" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  let dialog = await openBulk("Migrate");
  // 처음 고른 node-a에는 이미 있다.
  expect(within(dialog).getAllByText("Already on this node.")).toHaveLength(2);
  await userEvent.selectOptions(within(dialog).getByLabelText("Target node"), "n2");
  // 픽스처 노드는 아키텍처를 모르므로 virt VM의 대상이 아니다.
  expect(within(dialog).getAllByText(/Cannot run on this node/)).toHaveLength(2);
  // 취소하면 고른 채로 남는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  dialog = await openBulk("Snapshot");
  expect(
    within(dialog).getByText(/^Snapshot name: bulk-\d{8}-\d{4} \(disks only\)$/),
  ).toBeVisible();
  await confirmBulk(dialog, "Snapshot");
  await waitFor(() => expect(snapped).toHaveLength(2));
  expect(snapped[0]!.name).toMatch(/^bulk-\d{8}-\d{4}$/);
  expect(snapped[0]!.memory).toBe(false);
  expect(migrated).toEqual([]);
});

it("previews a VM in a drawer", async () => {
  mockLists({ vm: [web, db], node: [NODE] });
  renderPage(<VmListPage />);
  await screen.findByRole("link", { name: "web" });
  await userEvent.click(screen.getByRole("button", { name: "Preview web" }));
  const peek = await screen.findByRole("dialog", { name: "web" });
  expect(peek).toHaveTextContent("node-a");
  expect(within(peek).getByRole("link", { name: "Open" })).toHaveAttribute("href", "/vms/web");
  expect(within(peek).getByRole("link", { name: "Console" })).toBeInTheDocument();
  // 정지는 확인을 받는다.
  await userEvent.click(within(peek).getByRole("button", { name: "Stop" }));
  expect(await screen.findByRole("alertdialog", { name: "Stop web?" })).toBeInTheDocument();
});

it("shows the first steps while there are no VMs", async () => {
  mockLists({ node: [NODE] });
  renderPage(<VmListPage />);
  const steps = await screen.findByRole("region", { name: "First steps" });
  expect(within(steps).getByRole("link", { name: "Create a pool" })).toHaveAttribute(
    "href",
    "/storage-pools?create=1",
  );
});

it("puts a power button on each phone card", async () => {
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    ...original(query),
    matches: query === "(max-width: 767px)",
  })) as typeof window.matchMedia;
  try {
    mockLists({ vm: [web, db], node: [NODE] });
    renderPage(<VmListPage />);
    const list = await screen.findByRole("list", { name: "Virtual machines" });
    await within(list).findByRole("link", { name: "db" });
    const cards = within(list).getAllByRole("listitem");
    const card = (name: string) => cards.find((c) => within(c).queryByRole("link", { name }))!;
    expect(within(card("web")).getByRole("button", { name: "Stop web" })).toBeInTheDocument();
    expect(within(card("db")).getByRole("button", { name: "Start db" })).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "First steps" })).not.toBeInTheDocument();
  } finally {
    window.matchMedia = original;
  }
});

it("offers to create the first VM when there is none", async () => {
  mockLists({ vm: [], node: [NODE] });
  renderPage(<VmListPage />);
  expect(await screen.findByText("No virtual machines.")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Create your first VM" })).toHaveAttribute(
    "href",
    "/vms/-new",
  );
  expect(screen.getByRole("link", { name: "Quick start from a template" })).toHaveAttribute(
    "href",
    "/templates",
  );
  expect(screen.getByRole("button", { name: "Learn more" })).toBeInTheDocument();
});
