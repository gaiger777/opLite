import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { task, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { resetTasks } from "@/tasks/tasks";
import { apiUrl, server } from "@/test/server";
import { VmListPage } from "@/vms/VmListPage";

const web = vm("web", "running");
const db = vm("db", "stopped", { spec: { ...vm("db", "stopped").spec, memory_mib: 4096 } });

// 알림과 지켜보는 Task는 모듈 전역이므로 테스트마다 비운다.
afterEach(() => {
  toast.dismiss();
  resetTasks();
});

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
  await userEvent.click(within(screen.getByText("3 selected").parentElement!).getByText("Start"));
  // 실행 중인 web은 건너뛴다.
  expect(started.slice(1).sort()).toEqual(
    [`start:${db.id}`, `start:${vm("app", "stopped").id}`].sort(),
  );
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
  await userEvent.click(confirm);
  expect(await screen.findByText("Deleted db.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("hides changes from viewers", async () => {
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmListPage />, { as: { user: "u", name: "v", role: "viewer" } });
  await screen.findByRole("link", { name: "web" });
  expect(screen.queryByRole("link", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for web" })).not.toBeInTheDocument();
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
  expect(JSON.parse(window.localStorage.getItem("opl.vms.sort")!)).toEqual([
    { id: "name", desc: true },
  ]);
  expect(screen.getByRole("columnheader", { name: "Actions" })).not.toHaveAttribute("aria-sort");
  window.localStorage.removeItem("opl.vms.sort");
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
  await userEvent.click(within(screen.getByText("1 selected").parentElement!).getByText("Start"));
  expect(await screen.findByText("Start db")).toBeInTheDocument();
  expect(started).toEqual([db.id]);
});

it("names the VM that failed and sums up a bulk action", async () => {
  const app = vm("app", "stopped");
  mockLists({ vm: [web, db, app], node: [NODE] });
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
  await userEvent.click(within(screen.getByText("3 selected").parentElement!).getByText("Start"));
  expect(await screen.findByText(/^app: /)).toBeInTheDocument();
  // 실행 중인 web은 건너뛴다.
  expect(await screen.findByText("Start: 1 requested, 1 failed, 1 skipped.")).toBeInTheDocument();
  expect(started).toEqual([db.id]);
});
