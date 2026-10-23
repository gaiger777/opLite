import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { task, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmListPage } from "@/vms/VmListPage";

const web = vm("web", "running");
const db = vm("db", "stopped", { spec: { ...vm("db", "stopped").spec, memory_mib: 4096 } });

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
