import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { REFRESH_ON } from "@/api/live";
import { expectNoAxeViolations } from "@/test/axe";
import { vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { migration, migrationItem, vmwareSource } from "@/test/vmware";
import type { MigrationItem } from "@/vmware/api";
import { MigrationPage } from "@/vmware/MigrationPage";

afterEach(() => toast.dismiss());

const WAVE = migration("wave1", 1, {
  spec: {
    cutover: { shutdown: "guest", shutdown_timeout_s: 300, protect_source: true } as never,
  },
  status: { phase: "running", counts: { syncing: 1, copying: 1, done: 1 } },
});
const TARGET = vm("db01", "running");
const disk = (copied: number, delta?: number) => ({
  key: 2000,
  file: "[ds1] web01/web01-flat.vmdk",
  capacity_bytes: 4 * 1024 ** 3,
  copied_bytes: copied,
  rounds: delta === undefined ? 0 : 2,
  ...(delta === undefined ? {} : { last_delta_bytes: delta }),
});
const WEB = migrationItem("web01", 1, {
  state: "syncing",
  rounds: 2,
  estimate_s: 252,
  next_sync_at: 1_793_764_800,
  disks: [disk(4 * 1024 ** 3, 96 * 1024 ** 2)],
  history: [{ at: 1_793_761_200, event: "round 2: 96 MiB changed" }],
});
const APP = migrationItem("app01", 2, { state: "copying", disks: [disk(1024 ** 3)] });
const DB = migrationItem("db01", 3, {
  state: "done",
  rounds: 3,
  target_vm: TARGET.id,
  source: {
    vm: {
      name: "db01",
      vcpus: 2,
      cores_per_socket: 1,
      memory_mib: 4096,
      firmware: "bios",
      guest_os: "ubuntu64Guest",
      disks: [],
      nics: [],
    },
    host: "esx1.example.com",
    power: "powered-off",
  },
  source_changes: {
    annotation_added: true,
    nics_disconnected: [4000],
    old_annotation: "",
    powered_off_by_us: true,
  },
});

function setup(items: MigrationItem[] = [WEB, APP, DB]) {
  mockLists({ vm: [TARGET] });
  const posted: { path: string; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/vmware-migrations/:key"), () => HttpResponse.json(WAVE)),
    http.get(apiUrl("/vmware-vms"), () => HttpResponse.json({ revision: 1, items })),
    http.get(apiUrl("/vmware-sources"), () =>
      HttpResponse.json({ revision: 1, items: [vmwareSource("vc1")] }),
    ),
    http.post(apiUrl("/vmware-vms/:key/:action"), async ({ request, params }) => {
      posted.push({
        path: `${String(params.key)}/${String(params.action)}`,
        body: await request.json().catch(() => null),
      });
      return HttpResponse.json({ item: items[0] });
    }),
  );
  return posted;
}

const open = (as = ADMIN) =>
  renderPage(<MigrationPage />, {
    path: "/vmware-migrations/:name",
    url: "/vmware-migrations/wave1",
    as,
  });

it("shows each VM's state, round, copy bar, last delta and estimated downtime", async () => {
  setup();
  const { container } = open();
  const table = await screen.findByRole("table", { name: "wave1" });
  const web = await within(table).findByRole("row", { name: /web01/ });
  expect(web).toHaveTextContent("Syncing");
  expect(web).toHaveTextContent("96 MiB");
  expect(web).toHaveTextContent("4m 12s");
  const app = within(table).getByRole("row", { name: /app01/ });
  expect(within(app).getByRole("progressbar", { name: "Copy of app01" })).toHaveAttribute(
    "aria-valuenow",
    "25",
  );
  expect(within(table).getByRole("row", { name: /db01/ })).toHaveTextContent("Done");
  expect(within(table).getByRole("link", { name: "db01" })).toHaveAttribute("href", "/vms/db01");
  expect(screen.getByText("Warm migration from vc1")).toBeInTheDocument();
  await expectNoAxeViolations(container);
});

it("follows watch events: the item list is read again", async () => {
  setup();
  const { queryClient } = open();
  const table = await screen.findByRole("table", { name: "wave1" });
  await within(table).findByRole("row", { name: /web01/ });
  // `LiveProvider`는 vmware-vm 사건이 오면 이 키를 다시 읽는다.
  expect(REFRESH_ON["vmware-vm"]).toEqual([["vmware-vms"]]);
  server.use(
    http.get(apiUrl("/vmware-vms"), () =>
      HttpResponse.json({
        revision: 2,
        items: [
          {
            ...WEB,
            revision: 2,
            status: { ...WEB.status, state: "cutting-over", cutover_step: "final-sync" },
          },
          APP,
          DB,
        ],
      }),
    ),
  );
  await act(() => queryClient.invalidateQueries({ queryKey: REFRESH_ON["vmware-vm"]![0] }));
  await waitFor(() =>
    expect(within(table).getByRole("row", { name: /web01/ })).toHaveTextContent(
      "Cutting over: final sync",
    ),
  );
});

it("cuts over the chosen VMs after showing the downtime and what happens to the source", async () => {
  const user = userEvent.setup();
  const posted = setup();
  open();
  const table = await screen.findByRole("table", { name: "wave1" });
  await user.click(await within(table).findByRole("checkbox", { name: "Select web01" }));
  await user.click(screen.getByRole("button", { name: "Cut over" }));
  const dialog = screen.getByRole("alertdialog", { name: "Cut over 1 VM?" });
  expect(dialog).toHaveTextContent("about 4m 12s down · last delta 96 MiB");
  expect(dialog).toHaveTextContent("shut down through VMware Tools (waits up to 300 s)");
  expect(dialog).toHaveTextContent("Source NICs stay disconnected at power-on");
  await user.click(within(dialog).getByRole("button", { name: "Cut over" }));
  await waitFor(() => expect(posted).toEqual([{ path: `${WEB.id}/cutover`, body: null }]));
});

it("cannot cut over a VM that is still copying", async () => {
  const user = userEvent.setup();
  setup();
  open();
  const table = await screen.findByRole("table", { name: "wave1" });
  await user.click(await within(table).findByRole("checkbox", { name: "Select app01" }));
  expect(screen.getByRole("button", { name: "Cut over" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
});

it("reverts a cut-over VM only after its name is typed", async () => {
  const user = userEvent.setup();
  const posted = setup();
  open();
  await user.click(await screen.findByRole("button", { name: "Open db01" }));
  const sheet = screen.getByRole("dialog", { name: "db01" });
  expect(sheet).toHaveTextContent("Disconnected at power-on (protected)");
  await user.click(within(sheet).getByRole("button", { name: "Revert…" }));
  const dialog = screen.getByRole("alertdialog", { name: "Revert db01?" });
  expect(dialog).toHaveTextContent(
    "Data written on the new VM since the cutover is not copied back.",
  );
  const confirm = within(dialog).getByRole("button", { name: "Revert" });
  expect(confirm).toBeDisabled();
  await user.type(within(dialog).getByRole("textbox"), "db01");
  await user.click(confirm);
  await waitFor(() =>
    expect(posted).toEqual([{ path: `${DB.id}/revert`, body: { confirm: "db01" } }]),
  );
});

it("shows history, disks and the round in the drawer and retries a failed VM", async () => {
  const user = userEvent.setup();
  const failed = migrationItem("old01", 4, { state: "failed", error: "vmware_stall: no progress" });
  const posted = setup([WEB, failed]);
  open();
  await user.click(await screen.findByRole("button", { name: "Open web01" }));
  const sheet = screen.getByRole("dialog", { name: "web01" });
  expect(within(sheet).getByText("round 2: 96 MiB changed")).toBeInTheDocument();
  expect(within(sheet).getByRole("table", { name: "Disks" })).toHaveTextContent(
    "[ds1] web01/web01-flat.vmdk",
  );
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Open old01" }));
  const other = screen.getByRole("dialog", { name: "old01" });
  expect(within(other).getByRole("alert")).toHaveTextContent("vmware_stall: no progress");
  await user.click(within(other).getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(posted).toEqual([{ path: `${failed.id}/retry`, body: null }]));
});

it("shows no actions to users who may not create VMs", async () => {
  setup();
  open({ ...ADMIN, role: "viewer" });
  const table = await screen.findByRole("table", { name: "wave1" });
  await within(table).findByRole("row", { name: /web01/ });
  expect(within(table).queryByRole("checkbox")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Cut over" })).not.toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole("button", { name: "Open db01" }));
  expect(screen.queryByRole("button", { name: "Revert…" })).not.toBeInTheDocument();
});
