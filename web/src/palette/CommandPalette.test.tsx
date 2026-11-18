import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { List } from "@/api/live";
import type { Node, Principal, Vm } from "@/api/types";
import { AppLayout } from "@/layout/AppLayout";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { NODE, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const VIEWER: Principal = {
  user: "00000000-0000-0000-0000-000000000003",
  name: "viewer",
  role: "viewer",
};

function node(id: string, name: string, address: string): Node {
  return {
    ...NODE,
    id,
    name,
    spec: { ...NODE.spec, address },
    status: { ...NODE.status, arch: "aarch64" },
  };
}

const NODES = [node("n1", "node1", "10.0.9.1:8006"), node("n2", "node2", "10.0.9.2:8006")];
const web: Vm = vm("web", "running", {
  spec: {
    ...vm("web", "running").spec,
    cloud_init: { networks: [{ ipv4: { mode: "static", address: "10.0.0.21/24" } }] },
    nics: [{ model: "virtio", network: "lan", mac: "52:54:00:aa:bb:cc" }],
  },
});
const webDb = vm("web-db", "stopped");
const app = vm("app", "stopped");

let actions: string[];

beforeEach(() => {
  actions = [];
  mockLists({ vm: [web, webDb, app], node: NODES, "storage-pool": [pool("local")] });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      actions.push(`${String(params.action)}:${String(params.key)}`);
      return HttpResponse.json(task(`t-${actions.length}`, "vm-start", String(params.key)), {
        status: 202,
      });
    }),
  );
});

afterEach(() => {
  localStorage.clear();
  toast.dismiss();
  resetTasks();
});

function shell(url = "/", as = ADMIN) {
  return renderPage(<AppLayout />, { path: "*", url, as });
}

/** ⌘K로 팔레트를 열고 질의를 친다. 결과가 나올 때까지 기다린다. */
async function search(query: string, ready: RegExp | string) {
  const user = userEvent.setup();
  await user.keyboard("{Meta>}k{/Meta}");
  const box = await screen.findByRole("combobox", { name: "Command palette" });
  if (query) {
    await user.type(box, query);
  }
  await screen.findAllByRole("option", { name: ready });
  return { user, box };
}

it("opens with ⌘K or Ctrl+K and gives focus back on Escape", async () => {
  const user = userEvent.setup();
  shell();
  const account = await screen.findByRole("button", { name: "Account (admin)" });
  account.focus();
  await user.keyboard("{Control>}k{/Control}");
  const dialog = await screen.findByRole("dialog", { name: "Command palette" });
  const box = within(dialog).getByRole("combobox");
  expect(box).toHaveFocus();
  expect(box).toHaveAttribute("aria-expanded", "true");
  expect(box).toHaveAttribute("aria-controls", within(dialog).getByRole("listbox").id);
  await user.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(account).toHaveFocus();

  await user.click(screen.getByRole("button", { name: "Search or run a command" }));
  expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
  // ⌘K를 다시 누르면 닫는다.
  await user.keyboard("{Meta>}k{/Meta}");
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
});

it("moves with the arrow keys and opens a VM with Enter", async () => {
  const { router } = shell();
  const { user, box } = await search("web", /^web-db/);
  const options = screen.getAllByRole("option");
  // 이름 그대로 친 VM의 동작들이 앞이고, 그 뒤 VM 결과다.
  const vms = within(screen.getByRole("group", { name: "Virtual machines" })).getAllByRole(
    "option",
  );
  expect(vms.map((o) => o.textContent)).toEqual([
    "webRunning · node1 · 10.0.0.21",
    "web-dbStopped · node1",
  ]);
  expect(options[0]).toHaveAttribute("aria-selected", "true");
  expect(box).toHaveAttribute("aria-activedescendant", options[0]!.id);
  await user.keyboard("{ArrowUp}");
  expect(box).toHaveAttribute("aria-activedescendant", options[options.length - 1]!.id);
  await user.keyboard("{ArrowDown}");
  await user.clear(box);
  await user.type(box, "web-");
  await screen.findByRole("option", { name: /^web-db/ });
  await user.keyboard("{Enter}");
  await waitFor(() => expect(router.state.location.pathname).toBe("/vms/web-db"));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("finds a VM by its static IP, MAC and CIDR, and a node by its address", async () => {
  shell();
  const { user, box } = await search("10.0.0.21", /^web/);
  expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual([
    "webRunning · node1 · 10.0.0.21",
  ]);
  await user.clear(box);
  await user.type(box, "52:54:00:aa");
  await screen.findByRole("option", { name: /^web/ });
  await user.clear(box);
  await user.type(box, "10.0.0.0/24");
  await screen.findByRole("option", { name: /^web/ });
  expect(screen.queryByRole("option", { name: /^node/ })).not.toBeInTheDocument();
  await user.clear(box);
  await user.type(box, "10.0.9.2");
  await screen.findByRole("option", { name: /^node2/ });
});

it("opens the migrate dialog filled with the node instead of migrating", async () => {
  let migrated = false;
  server.use(
    http.post(apiUrl("/vms/:key/migrate"), () => {
      migrated = true;
      return HttpResponse.json(task("m", "vm-migrate", web.id), { status: 202 });
    }),
  );
  shell();
  const { user } = await search("migrate web to node2", "Migrate web to node2…");
  // 대상이 접두사로 맞는 VM마다 하나(web-db는 멈춰 있어 흐리다)
  expect(screen.getByRole("option", { name: /^Migrate web-db to node2…/ })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await user.keyboard("{Enter}");
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  expect(within(dialog).getByLabelText("Target node")).toHaveValue("n2");
  expect(migrated).toBe(false);
});

it("asks before stopping but starts right away", async () => {
  shell();
  let { user } = await search("web 정지", "Stop web…");
  await user.keyboard("{Enter}");
  const confirm = await screen.findByRole("alertdialog", { name: "Stop web?" });
  expect(actions).toEqual([]);
  await user.click(within(confirm).getByRole("button", { name: "Stop" }));
  await waitFor(() => expect(actions).toEqual([`stop:${web.id}`]));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());

  ({ user } = await search("start app", "Start app"));
  await user.keyboard("{Enter}");
  await waitFor(() => expect(actions).toEqual([`stop:${web.id}`, `start:${app.id}`]));
  expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
});

it("asks before starting a VM whose name only starts with the typed text", async () => {
  shell();
  const { user } = await search("start ap", "Start app…");
  await user.keyboard("{Enter}");
  const confirm = await screen.findByRole("alertdialog", { name: "Start app?" });
  expect(actions).toEqual([]);
  await user.click(within(confirm).getByRole("button", { name: "Start" }));
  await waitFor(() => expect(actions).toEqual([`start:${app.id}`]));
});

it("blocks the stop confirm when the VM stops while the dialog is open", async () => {
  const { queryClient } = shell();
  const { user } = await search("web 정지", "Stop web…");
  await user.keyboard("{Enter}");
  const confirm = await screen.findByRole("alertdialog", { name: "Stop web?" });
  expect(within(confirm).getByRole("button", { name: "Stop" })).toBeEnabled();
  // 그 사이 다른 사람이 VM을 껐다.
  queryClient.setQueryData<List<Vm>>(["list", "vm"], {
    revision: 99,
    items: [vm("web", "stopped"), webDb, app],
  });
  await waitFor(() => expect(within(confirm).getByRole("button", { name: "Stop" })).toBeDisabled());
  expect(confirm).toHaveTextContent("The VM is stopped.");
  expect(actions).toEqual([]);
});

it("ignores the Enter that finishes Korean input", async () => {
  shell();
  const { box } = await search("start app", "Start app");
  fireEvent.keyDown(box, { key: "Enter", isComposing: true });
  expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
  expect(actions).toEqual([]);
  // Safari: 조합을 끝낸 Enter는 isComposing이 false이고 keyCode가 229다.
  fireEvent.keyDown(box, { key: "Enter", keyCode: 229 });
  expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
  expect(actions).toEqual([]);
  fireEvent.keyDown(box, { key: "Enter" });
  await waitFor(() => expect(actions).toEqual([`start:${app.id}`]));
});

it("dims an action the VM cannot do and says why", async () => {
  shell();
  const { user } = await search("start web", /^Start web/);
  // 접두사가 맞는 web-db도 있다. 정확히 맞는 web이 앞이다.
  const option = screen.getAllByRole("option", { name: /^Start web/ })[0]!;
  expect(option).toHaveAttribute("aria-disabled", "true");
  expect(option).toHaveTextContent("The VM is running.");
  await user.keyboard("{Enter}");
  expect(screen.getByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
  expect(actions).toEqual([]);
});

it("lists the actions of a VM typed by name and shows them after Tab", async () => {
  shell();
  const { user, box } = await search("ap", /^app/);
  expect(screen.queryByRole("group", { name: "Actions" })).not.toBeInTheDocument();
  await user.keyboard("{Tab}");
  expect(box).toHaveValue("app");
  const group = await screen.findByRole("group", { name: "Actions" });
  expect(
    within(group)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual([
    "Start app",
    expect.stringMatching(/^Stop app…/),
    expect.stringMatching(/^Reboot app…/),
    expect.stringMatching(/^Force stop app…/),
    expect.stringMatching(/^Migrate app…/),
    "Take a snapshot of app…",
    expect.stringMatching(/^Open the console of app/),
  ]);
});

it("gives viewers no actions", async () => {
  shell("/", VIEWER);
  const { user, box } = await search("web", /^web-db/);
  expect(screen.queryByRole("group", { name: "Actions" })).not.toBeInTheDocument();
  await user.clear(box);
  await user.type(box, "start app");
  expect(await screen.findByText("No matches.")).toBeInTheDocument();
  await user.clear(box);
  // 만들기 명령도 없다.
  await user.type(box, "create");
  expect(await screen.findByText("No matches.")).toBeInTheDocument();
});

it("goes to every sub-screen, including views", async () => {
  const { router } = shell();
  const { user } = await search("cluster", "Infrastructure › Cluster");
  await user.click(screen.getByRole("option", { name: "Infrastructure › Cluster" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/cluster"));
});

it("runs create, theme and help commands", async () => {
  const { router } = shell();
  let { user } = await search("create storage", "Create: Storage pool");
  await user.keyboard("{Enter}");
  await waitFor(() =>
    expect(`${router.state.location.pathname}${router.state.location.search}`).toBe(
      "/storage-pools?create=1",
    ),
  );
  ({ user } = await search("theme dark", "Theme: Dark"));
  await user.keyboard("{Enter}");
  expect(document.documentElement).toHaveClass("dark");
  ({ user } = await search("shortcuts", "Keyboard shortcuts"));
  await user.keyboard("{Enter}");
  expect(await screen.findByRole("dialog", { name: "Keyboard shortcuts" })).toBeInTheDocument();
});

it("shows favorites first and recent items on an empty query, without deleted ones", async () => {
  localStorage.setItem(
    `opl.${ADMIN.user}.pins`,
    JSON.stringify([{ kind: "vm", id: app.id, name: "app" }]),
  );
  localStorage.setItem(
    `opl.${ADMIN.user}.recent`,
    JSON.stringify([
      { kind: "vm", id: "gone", name: "gone" },
      { kind: "node", id: "n2", name: "old-name" },
      { kind: "vm", id: app.id, name: "app" },
    ]),
  );
  shell();
  await search("", /^node2/);
  const groups = screen.getAllByRole("group").map((g) => g.getAttribute("aria-labelledby"));
  expect(document.getElementById(groups[0]!)).toHaveTextContent("Favorites");
  expect(
    within(screen.getByRole("group", { name: "Favorites" }))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["appStopped · node1"]);
  // 즐겨찾기에 있는 것은 최근에서 빼고, 지운 VM은 없다.
  expect(
    within(screen.getByRole("group", { name: "Recent" }))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["node210.0.9.2:8006"]);
});

it("has no axe violations when open", async () => {
  shell();
  await search("web", /^web-db/);
  await expectNoAxeViolations(document.body);
});
