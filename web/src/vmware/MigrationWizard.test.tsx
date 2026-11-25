import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { expectNoAxeViolations } from "@/test/axe";
import { NODE, pool } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { inventoryOf, invVm, migration, migrationItem, vmwareSource } from "@/test/vmware";
import { MigrationWizard } from "@/vmware/MigrationWizard";

afterEach(() => toast.dismiss());

const VC1 = vmwareSource("vc1");
const WEB = invVm("web01", 1, { folder: "DC1/vm/Prod/Web", networks: ["VM Network", "dvpg-web"] });
const DB = invVm("db01", 2, { folder: "DC1/vm/Prod", power: "powered-off", datastores: ["ds2"] });
const APP = invVm("app03", 3, {
  folder: "DC1/vm/Prod",
  networks: ["isolated"],
  datastores: ["ds9"],
});
const WAVE = migration("wave1", 1, { status: { phase: "draft", counts: { draft: 2 } } });

function setup() {
  mockLists({
    network: [
      { id: "net1", name: "lan" },
      { id: "net2", name: "VM Network" },
    ],
    "storage-pool": [pool("fast"), pool("slow")],
    node: [NODE],
  });
  const calls: { method: string; path: string; body: unknown }[] = [];
  const record = async (request: Request) => {
    const url = new URL(request.url);
    calls.push({
      method: request.method,
      path: url.pathname.replace("/api/v1", "") + url.search,
      body: request.method === "GET" ? null : await request.json().catch(() => null),
    });
  };
  server.use(
    http.get(apiUrl("/vmware-sources"), () => HttpResponse.json({ revision: 1, items: [VC1] })),
    http.get(apiUrl("/vmware-migrations"), () => HttpResponse.json({ revision: 1, items: [] })),
    http.get(apiUrl("/vmware-sources/:key/inventory"), async ({ request }) => {
      await record(request);
      return HttpResponse.json(
        inventoryOf(
          [WEB, DB, APP],
          [{ instance_uuid: "5003-3", migration: "old", item: "old-app03" }],
        ),
      );
    }),
    http.post(apiUrl("/vmware-migrations"), async ({ request }) => {
      await record(request);
      return HttpResponse.json(
        {
          migration: WAVE,
          vms: [
            migrationItem("web01", 1, { state: "draft" }),
            migrationItem("db01", 2, { state: "draft" }),
          ],
        },
        { status: 201 },
      );
    }),
    http.post(apiUrl("/vmware-migrations/:key/preflight"), async ({ request }) => {
      await record(request);
      return HttpResponse.json({
        fetched_at: 1,
        vms: [
          {
            item: "wave1-web01",
            ref: "vm-1",
            name: "web01",
            level: "ok",
            problems: [],
            node: "node-a",
          },
          {
            item: "wave1-db01",
            ref: "vm-2",
            name: "db01",
            level: "blocked",
            problems: [
              {
                at: "source",
                code: "vmware_snapshots",
                level: "error",
                message: "consolidate or delete snapshots first",
              },
            ],
          },
        ],
      });
    }),
    http.patch(apiUrl("/vmware-migrations/:key"), async ({ request }) => {
      await record(request);
      return HttpResponse.json({ migration: WAVE, vms: [] });
    }),
    http.post(apiUrl("/vmware-migrations/:key/actions/start"), async ({ request }) => {
      await record(request);
      return HttpResponse.json({
        migration: { ...WAVE, status: { ...WAVE.status, phase: "running" } },
        vms: [],
      });
    }),
  );
  return calls;
}

it("walks source → tree → mapping → check and starts without the blocked VM", async () => {
  const user = userEvent.setup();
  const calls = setup();
  const { container, router } = renderPage(<MigrationWizard />, {
    path: "/vmware-migrations/-new",
  });
  expect(await screen.findByRole("heading", { name: "New VMware migration" })).toBeInTheDocument();
  await expectNoAxeViolations(container);

  // ① 원본·이름
  await user.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("Choose a source.")).toBeInTheDocument();
  await user.type(screen.getByLabelText("Name"), "wave1");
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Source" }),
    "vc1 (vc1.example.com)",
  );
  await user.click(screen.getByRole("button", { name: "Next" }));

  // ② 나무 + 표: 폴더를 고르면 그 아래만, 찾기, 이미 다른 계획에 든 VM은 고를 수 없다
  const table = await screen.findByRole("table", { name: "Source VMs" });
  const tree = screen.getByRole("navigation", { name: "Inventory" });
  await user.click(within(tree).getByRole("button", { name: /^Web/ }));
  expect(within(table).getAllByRole("row")).toHaveLength(2);
  expect(within(table).getByRole("checkbox", { name: "web01" })).toBeInTheDocument();
  await user.click(within(tree).getByRole("button", { name: /^All VMs/ }));
  await user.type(screen.getByLabelText("Search"), "app");
  const app = within(table).getByRole("checkbox", { name: "app03" });
  expect(app).toBeDisabled();
  expect(app).toHaveAccessibleDescription("Already in migration old");
  await user.clear(screen.getByLabelText("Search"));
  // 보이는 VM 모두 고르기는 고를 수 있는 것만
  await user.click(within(table).getByRole("checkbox", { name: "Select all shown VMs" }));
  expect(screen.getByText("2 selected · 3 shown")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Next" }));

  // ③ 매핑: 고른 VM이 쓰는 네트워크·데이터스토어만, 같은 이름은 미리 고름
  const networks = screen.getByRole("table", { name: "Networks" });
  expect(
    within(networks)
      .getAllByRole("row")
      .map((r) => r.textContent),
  ).toEqual([
    "VMware networkOrbivisor network",
    expect.stringContaining("VM Network"),
    expect.stringContaining("dvpg-web"),
  ]);
  expect(within(networks).queryByText("isolated")).not.toBeInTheDocument();
  expect(screen.getByLabelText("VM Network")).toHaveValue("VM Network");
  expect(screen.getByLabelText("dvpg-web")).toHaveValue("");
  const datastores = screen.getByRole("table", { name: "Datastores" });
  expect(within(datastores).getByText("ds1")).toBeInTheDocument();
  expect(within(datastores).getByText("ds2")).toBeInTheDocument();
  expect(within(datastores).queryByText("ds9")).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getAllByText("Choose a pool or a default pool.")).toHaveLength(2);
  await user.selectOptions(screen.getByLabelText("dvpg-web"), "lan");
  await user.selectOptions(screen.getByLabelText("ds1"), "fast");
  await user.selectOptions(screen.getByLabelText("Default pool"), "slow");
  await user.click(screen.getByRole("button", { name: "Next" }));

  // ④ 한도·전환 정책
  expect(screen.getByLabelText("VMs at a time")).toHaveValue("4");
  await user.click(screen.getByRole("button", { name: "Next" }));

  // ⑤ 미리 확인: 초안을 만들고 VM마다 칩
  const results = await screen.findByRole("table", { name: "Check results" });
  expect(within(results).getByRole("row", { name: /web01/ })).toHaveTextContent("OK");
  const db = within(results).getByRole("row", { name: /db01/ });
  expect(db).toHaveTextContent("Blocked");
  await user.click(within(db).getByText("1 problem"));
  expect(within(db).getByText(/consolidate or delete snapshots first/)).toBeVisible();
  expect(screen.getByText("1 OK · 0 with warnings · 1 blocked")).toBeInTheDocument();
  expect(screen.getByText(/1 blocked VM is left out/)).toBeInTheDocument();
  const create = calls.find((c) => c.method === "POST" && c.path === "/vmware-migrations");
  expect(create?.body).toMatchObject({
    name: "wave1",
    source: VC1.id,
    mode: "warm",
    // 표 차례(이름)대로
    vms: [{ ref: "vm-2" }, { ref: "vm-1" }],
    rules: {
      networks: [
        { from: "VM Network", to: "VM Network" },
        { from: "dvpg-web", to: "lan" },
      ],
      datastores: [{ from: "ds1", to: "fast" }],
      default_pool: "slow",
    },
  });

  await user.click(screen.getByRole("button", { name: "Start 1 VM" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/vmware-migrations/wave1"));
  const tail = calls.slice(-2);
  expect(tail[0]).toMatchObject({ method: "PATCH", body: { remove_vms: ["wave1-db01"] } });
  expect(tail[1]?.path).toBe(`/vmware-migrations/${WAVE.id}/actions/start`);
});

it("updates the draft instead of making another when the user goes back", async () => {
  const user = userEvent.setup();
  const calls = setup();
  renderPage(<MigrationWizard />, { path: "/vmware-migrations/-new" });
  await user.type(await screen.findByLabelText("Name"), "wave1");
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Source" }),
    "vc1 (vc1.example.com)",
  );
  await user.click(screen.getByRole("button", { name: "Next" }));
  const table = await screen.findByRole("table", { name: "Source VMs" });
  await user.click(within(table).getByRole("checkbox", { name: "web01" }));
  await user.click(within(table).getByRole("checkbox", { name: "db01" }));
  await user.click(screen.getByRole("button", { name: "Next" }));
  await user.selectOptions(screen.getByLabelText("Default pool"), "fast");
  await user.click(screen.getByRole("button", { name: "Next" }));
  await user.click(screen.getByRole("button", { name: "Next" }));
  await screen.findByRole("table", { name: "Check results" });
  // 뒤로 가서 db01을 빼고 다시 확인하면 초안을 바꾼다(새로 만들지 않는다).
  server.use(
    http.get(apiUrl("/vmware-vms"), () =>
      HttpResponse.json({ revision: 1, items: [migrationItem("web01", 1, { state: "draft" })] }),
    ),
  );
  for (let i = 0; i < 3; i++) {
    await user.click(screen.getByRole("button", { name: "Back" }));
  }
  await user.click(
    within(await screen.findByRole("table", { name: "Source VMs" })).getByRole("checkbox", {
      name: "db01",
    }),
  );
  for (let i = 0; i < 3; i++) {
    await user.click(screen.getByRole("button", { name: "Next" }));
  }
  await screen.findByRole("table", { name: "Check results" });
  expect(calls.filter((c) => c.method === "POST" && c.path === "/vmware-migrations")).toHaveLength(
    1,
  );
  const patch = calls.find((c) => c.method === "PATCH");
  expect(patch?.body).toMatchObject({ remove_vms: ["vm-2"] });
});

it("says what is wrong when the inventory cannot be read", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.get(apiUrl("/vmware-sources/:key/inventory"), () =>
      HttpResponse.json({ code: "vmware_unreachable", message: "timed out" }, { status: 502 }),
    ),
  );
  renderPage(<MigrationWizard />, { path: "/vmware-migrations/-new" });
  await user.type(await screen.findByLabelText("Name"), "wave1");
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Source" }),
    "vc1 (vc1.example.com)",
  );
  await user.click(screen.getByRole("button", { name: "Next" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The VMware server cannot be reached: timed out",
  );
});

/** web01·db01을 골라 ⑤ 미리 확인까지 간다(초안이 생긴다). */
async function toCheck(user: ReturnType<typeof userEvent.setup>) {
  renderPage(<MigrationWizard />, { path: "/vmware-migrations/-new" });
  await user.type(await screen.findByLabelText("Name"), "wave1");
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Source" }),
    "vc1 (vc1.example.com)",
  );
  await user.click(screen.getByRole("button", { name: "Next" }));
  const table = await screen.findByRole("table", { name: "Source VMs" });
  await user.click(within(table).getByRole("checkbox", { name: "web01" }));
  await user.click(within(table).getByRole("checkbox", { name: "db01" }));
  await user.click(screen.getByRole("button", { name: "Next" }));
  await user.selectOptions(screen.getByLabelText("Default pool"), "fast");
  await user.click(screen.getByRole("button", { name: "Next" }));
  await user.click(screen.getByRole("button", { name: "Next" }));
  await screen.findByRole("table", { name: "Check results" });
}

it("retries a failed start without removing the blocked VM again", async () => {
  const user = userEvent.setup();
  const calls = setup();
  let starts = 0;
  server.use(
    http.post(apiUrl("/vmware-migrations/:key/actions/start"), async ({ request }) => {
      calls.push({
        method: "POST",
        path: new URL(request.url).pathname.replace("/api/v1", ""),
        body: null,
      });
      starts++;
      return starts === 1
        ? HttpResponse.json({ code: "vmware_unreachable", message: "timed out" }, { status: 502 })
        : HttpResponse.json({
            migration: { ...WAVE, status: { ...WAVE.status, phase: "running" } },
            vms: [],
          });
    }),
  );
  await toCheck(user);
  await user.click(screen.getByRole("button", { name: "Start 1 VM" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("timed out");
  const mark = calls.length;
  await user.click(screen.getByRole("button", { name: "Start 1 VM" }));
  await waitFor(() => expect(starts).toBe(2));
  // 이미 뺀 db01을 또 빼지 않는다(서버는 400 vmware_vm_not_found로 답한다).
  expect(calls.slice(mark).map((c) => c.method)).toEqual(["POST"]);
});

it("unselects the blocked VM it removed, even when the start then fails", async () => {
  const user = userEvent.setup();
  setup();
  server.use(
    http.post(apiUrl("/vmware-migrations/:key/actions/start"), () =>
      HttpResponse.json({ code: "vmware_unreachable", message: "timed out" }, { status: 502 }),
    ),
  );
  await toCheck(user);
  await user.click(screen.getByRole("button", { name: "Start 1 VM" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("timed out");
  // 초안에서 뺀 db01은 고른 것에서도 빠져 있다 — 고른 수와 다시 확인한 결과가 어긋나지 않는다.
  for (let i = 0; i < 3; i++) {
    await user.click(screen.getByRole("button", { name: "Back" }));
  }
  const table = await screen.findByRole("table", { name: "Source VMs" });
  expect(within(table).getByRole("checkbox", { name: "db01" })).not.toBeChecked();
  expect(within(table).getByRole("checkbox", { name: "web01" })).toBeChecked();
  expect(screen.getByText("1 selected · 3 shown")).toBeInTheDocument();
});

it("lets the user uncheck the draft's own VMs after reading the inventory again", async () => {
  const user = userEvent.setup();
  setup();
  await toCheck(user);
  // 초안이 생긴 뒤의 인벤토리는 초안의 VM도 "끝나지 않은 계획에 든 VM"으로 알린다.
  server.use(
    http.get(apiUrl("/vmware-sources/:key/inventory"), () =>
      HttpResponse.json(
        inventoryOf(
          [WEB, DB, APP],
          [
            { instance_uuid: "5003-1", migration: "wave1", item: "wave1-web01" },
            { instance_uuid: "5003-2", migration: "wave1", item: "wave1-db01" },
            { instance_uuid: "5003-3", migration: "old", item: "old-app03" },
          ],
        ),
      ),
    ),
  );
  for (let i = 0; i < 3; i++) {
    await user.click(screen.getByRole("button", { name: "Back" }));
  }
  await user.click(screen.getByRole("button", { name: "Read again" }));
  const table = await screen.findByRole("table", { name: "Source VMs" });
  await waitFor(() =>
    expect(within(table).getByRole("checkbox", { name: "app03" })).toBeDisabled(),
  );
  const db = within(table).getByRole("checkbox", { name: "db01" });
  expect(db).toBeEnabled();
  expect(within(table).queryByText("Already in migration wave1")).not.toBeInTheDocument();
  await user.click(db);
  expect(db).not.toBeChecked();
});

it("unselects chosen VMs that are gone from the inventory read again", async () => {
  const user = userEvent.setup();
  setup();
  renderPage(<MigrationWizard />, { path: "/vmware-migrations/-new" });
  await user.type(await screen.findByLabelText("Name"), "wave1");
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Source" }),
    "vc1 (vc1.example.com)",
  );
  await user.click(screen.getByRole("button", { name: "Next" }));
  const table = await screen.findByRole("table", { name: "Source VMs" });
  await user.click(within(table).getByRole("checkbox", { name: "web01" }));
  await user.click(within(table).getByRole("checkbox", { name: "db01" }));
  expect(screen.getByText("2 selected · 3 shown")).toBeInTheDocument();
  server.use(
    http.get(apiUrl("/vmware-sources/:key/inventory"), () =>
      HttpResponse.json(inventoryOf([DB, APP])),
    ),
  );
  await user.click(screen.getByRole("button", { name: "Read again" }));
  expect(
    await screen.findByText("1 chosen VM is no longer in the inventory and was unselected."),
  ).toBeInTheDocument();
  expect(screen.getByText("1 selected · 2 shown")).toBeInTheDocument();
});
