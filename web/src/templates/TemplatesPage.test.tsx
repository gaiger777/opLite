import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Network, Principal, Vm, Volume } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
import { TemplatesPage } from "@/templates/TemplatesPage";
import { NODE, pool, task, vm, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmListPage } from "@/vms/VmListPage";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const VIEWER: Principal = {
  user: "00000000-0000-0000-0000-000000000003",
  name: "viewer",
  role: "viewer",
};

const lan = { id: "019a0f3c-0000-7000-8004-000000000001", name: "lan" } as Network;
const dmz = { id: "019a0f3c-0000-7000-8004-000000000002", name: "dmz" } as Network;

function withDisks(v: Vm, disks: string[], template = false): Vm {
  return {
    ...v,
    spec: {
      ...v.spec,
      template,
      disks: disks.map((d, i) => ({ volume: d, bus: "virtio-blk" as const, boot_order: i + 1 })),
      nics: [{ network: "lan", model: "virtio" }],
    },
  };
}

function cloneOf(name: string, base: Volume): Volume {
  const v = volume(name, base.spec.pool);
  return { ...v, spec: { ...v.spec, base: base.id } };
}

const root = volume("tmpl-root", "local");
const tmpl = withDisks(vm("tmpl", "stopped"), ["tmpl-root"], true);
const spare = withDisks(vm("spare", "stopped"), ["spare-root"], true);
const web1 = withDisks(vm("web-1", "running"), ["web-1-root"]);

it("lists templates with their disks, pool and linked clones", async () => {
  mockLists({
    vm: [tmpl, spare, web1, vm("db", "stopped")],
    volume: [root, volume("spare-root", "local"), cloneOf("web-1-root", root)],
    node: [NODE],
  });
  renderPage(<TemplatesPage />);
  const table = await screen.findByRole("table", { name: "Templates" });
  const row = (await within(table).findByRole("link", { name: "tmpl" })).closest("tr")!;
  expect(within(table).queryByText("db")).not.toBeInTheDocument();
  const cells = within(row).getAllByRole("cell");
  expect(cells[1]).toHaveTextContent("tmpl-root");
  expect(cells[2]).toHaveTextContent("local");
  expect(cells[3]).toHaveTextContent("1");
  // 연결 복제가 남은 템플릿은 되돌리거나 지울 수 없다.
  for (const name of ["Convert tmpl to a VM", "Delete tmpl"]) {
    expect(within(row).getByRole("button", { name })).toHaveAccessibleDescription(
      "Linked clones remain. Delete or flatten them first.",
    );
  }
  const other = within(table).getByRole("link", { name: "spare" }).closest("tr")!;
  expect(within(other).getAllByRole("cell")[3]).toHaveTextContent("0");
  expect(within(other).getByRole("button", { name: "Delete spare" })).not.toHaveAttribute(
    "aria-disabled",
  );
});

it("converts a template back and deletes one after the untemplate task", async () => {
  mockLists({ vm: [spare], volume: [volume("spare-root", "local")], node: [NODE] });
  const done = { ...task("t-un", "vm-untemplate", spare.id) };
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/vms/${spare.id}/template`), () =>
      HttpResponse.json(done, { status: 202 }),
    ),
    // 되돌리기 Task가 끝난 목록
    http.get(apiUrl("/tasks"), () =>
      HttpResponse.json({
        revision: 2,
        items: [{ ...done, status: { state: "succeeded", error: null } }],
      }),
    ),
    http.delete(apiUrl(`/vms/${spare.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<TemplatesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Delete spare" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete template spare?" });
  await user.type(within(dialog).getByRole("textbox"), "spare");
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));
  await waitFor(() => expect(deleted).toBe(true));
});

it("hides template actions from viewers", async () => {
  mockLists({ vm: [tmpl], volume: [root], node: [NODE] });
  renderPage(<TemplatesPage />, { as: VIEWER });
  const table = await screen.findByRole("table", { name: "Templates" });
  await within(table).findByRole("link", { name: "tmpl" });
  expect(within(table).queryByRole("button")).not.toBeInTheDocument();
});

/** 보낸 배포 요청의 본문을 모은다. `refuse`번째 요청부터는 409로 거절한다. */
function captureDeploy(refuse = Infinity) {
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${tmpl.id}/clone`), async ({ request }) => {
      bodies.push(await request.json());
      if (bodies.length >= refuse) {
        return HttpResponse.json({ code: "name_taken", message: "name taken" }, { status: 409 });
      }
      return HttpResponse.json(task(`t${bodies.length}`, "vm-clone", tmpl.id), { status: 202 });
    }),
  );
  return bodies;
}

async function openDeploy() {
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Deploy from tmpl" }));
  const dialog = await screen.findByRole("dialog", { name: "Deploy from tmpl" });
  return { user, dialog };
}

it("deploys several VMs from a name pattern with overrides", async () => {
  mockLists({
    vm: [tmpl, web1],
    volume: [root, cloneOf("web-1-root", root)],
    node: [NODE],
    network: [lan, dmz],
    "storage-pool": [pool("local")],
  });
  const bodies = captureDeploy();
  renderPage(<TemplatesPage />);
  const { user, dialog } = await openDeploy();
  // 디렉터리 풀의 디스크는 연결 복제다.
  expect(await within(dialog).findByText("Linked clone")).toBeInTheDocument();
  const name = within(dialog).getByLabelText("New VM name");
  await user.clear(name);
  await user.type(name, "web-{{n}");
  await user.clear(within(dialog).getByLabelText("Count"));
  await user.type(within(dialog).getByLabelText("Count"), "3");
  await user.clear(within(dialog).getByLabelText("First number"));
  await user.type(within(dialog).getByLabelText("First number"), "2");
  expect(within(dialog).getByText("VMs: web-2, web-3, web-4")).toBeInTheDocument();
  await user.type(within(dialog).getByLabelText("vCPUs"), "4");
  await user.selectOptions(within(dialog).getByLabelText("nic0 network"), "dmz");
  await user.type(within(dialog).getByLabelText("disk0 new size (GiB, grow only)"), "8");
  // 여러 VM에 같은 호스트 이름은 거절한다.
  await user.click(within(dialog).getByLabelText("Set up the guest on first boot (cloud-init)"));
  await user.type(within(dialog).getByLabelText("Host name"), "web");
  await user.click(within(dialog).getByLabelText("Start after deploying"));
  await user.click(within(dialog).getByRole("button", { name: "Deploy" }));
  expect(within(dialog).getByText("Enter a size larger than the current size.")).toBeVisible();
  expect(
    within(dialog).getByText(
      "Several VMs cannot share one host name. Leave it empty to use the VM names.",
    ),
  ).toBeVisible();
  expect(bodies).toEqual([]);

  await user.clear(within(dialog).getByLabelText("Host name"));
  await user.clear(within(dialog).getByLabelText("disk0 new size (GiB, grow only)"));
  await user.type(within(dialog).getByLabelText("disk0 new size (GiB, grow only)"), "32");
  await user.type(within(dialog).getByLabelText("User"), "ops");
  await user.click(within(dialog).getByRole("button", { name: "Deploy" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(bodies).toEqual(
    ["web-2", "web-3", "web-4"].map((n) => ({
      name: n,
      vcpus: 4,
      nics: ["dmz"],
      disks: [32 * 1024 ** 3],
      cloud_init: { user: "ops" },
      start: true,
    })),
  );
  expect(await screen.findByText("Deploy web-2 from tmpl")).toBeInTheDocument();
});

it("refuses linked clones a disk cannot have and stops after a refused deploy", async () => {
  const san = pool("san", { backend: { kind: "lvm-shared", vg: "vg0" } });
  const sanRoot = {
    ...volume("tmpl-root", "san"),
    spec: { pool: "san", size_bytes: 16 * 1024 ** 3, format: "raw" as const },
  };
  mockLists({
    vm: [tmpl],
    volume: [sanRoot],
    node: [{ ...NODE, status: { ...NODE.status, pools: {} } }],
    network: [lan],
    "storage-pool": [san],
  });
  const bodies = captureDeploy(2);
  renderPage(<TemplatesPage />);
  const { user, dialog } = await openDeploy();
  expect(await within(dialog).findByText(/Cannot be linked — full copy/)).toBeInTheDocument();
  await user.selectOptions(within(dialog).getByLabelText("Disk copies"), "linked");
  await user.clear(within(dialog).getByLabelText("Count"));
  await user.type(within(dialog).getByLabelText("Count"), "3");
  await user.click(within(dialog).getByRole("button", { name: "Deploy" }));
  expect(within(dialog).getByText("Some disks cannot be linked clones.")).toBeVisible();

  await user.selectOptions(within(dialog).getByLabelText("Disk copies"), "full");
  await user.click(within(dialog).getByRole("button", { name: "Deploy" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  // 둘째가 거절되면 셋째는 보내지 않는다.
  expect(bodies).toEqual([
    { name: "tmpl-1", linked: false },
    { name: "tmpl-2", linked: false },
  ]);
  expect(await screen.findByText(/Stopped: could not deploy tmpl-2 \(1 more/)).toBeInTheDocument();
});

it("shows templates in the VM list and blocks starting them", async () => {
  mockLists({ vm: [tmpl, web1], volume: [root], node: [NODE] });
  // "템플릿만 보기" 단추는 뺐다(계획 77-6, 템플릿 화면이 따로) — 주소 거름은 그대로다.
  renderPage(<VmListPage />, { url: "/?template=1" });
  const row = (await screen.findByRole("link", { name: "tmpl" })).closest("tr")!;
  expect(within(row).getByText("Template")).toBeInTheDocument();
  const user = userEvent.setup();
  expect(screen.queryByRole("link", { name: "web-1" })).not.toBeInTheDocument();
  await user.click(within(row).getByRole("button", { name: "Actions for tmpl" }));
  expect(await screen.findByRole("menuitem", { name: /^Start/ })).toHaveAccessibleDescription(
    "A template cannot be started, migrated or deleted.",
  );
  expect(screen.getByRole("menuitem", { name: "Deploy" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Convert to VM" })).toBeInTheDocument();
});

it("converts a stopped VM to a template from its page", async () => {
  const db = withDisks(vm("db", "stopped"), ["db-root"]);
  mockLists({ vm: [db], volume: [volume("db-root", "local")], node: [NODE] });
  let converted = false;
  server.use(
    http.post(apiUrl(`/vms/${db.id}/template`), () => {
      converted = true;
      return HttpResponse.json({ ...db, spec: { ...db.spec, template: true } });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Actions for db" }));
  await user.click(screen.getByRole("menuitem", { name: "Convert to template" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Convert db to a template?" });
  await user.click(within(dialog).getByRole("button", { name: "Convert to template" }));
  await waitFor(() => expect(converted).toBe(true));
  expect(await screen.findByText("Converted db to a template.")).toBeInTheDocument();
});

it("explains why a running VM cannot become a template", async () => {
  mockLists({ vm: [web1], volume: [cloneOf("web-1-root", root), root], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web-1" });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for web-1" }));
  expect(screen.getByRole("menuitem", { name: "Convert to template" })).toHaveAccessibleDescription(
    "Only a stopped VM can become a template.",
  );
  // 연결 복제 디스크의 기반
  expect(await screen.findByText("disk0: linked clone of tmpl-root")).toBeInTheDocument();
});

/** 리뷰 2 웹 I1: vCPU를 덮어쓰면 소켓 1이라 템플릿의 게스트 NUMA 노드 둘은 틀린다 — 보내지 않고 알린다. */
it("checks the template's NUMA nodes against an overridden vCPU count", async () => {
  const numa = {
    ...tmpl,
    spec: {
      ...tmpl.spec,
      cpu: { ...tmpl.spec.cpu, sockets: 2, cores: 2 },
      numa: { nodes: 2 },
    },
  };
  mockLists({
    vm: [numa],
    volume: [root],
    node: [NODE],
    network: [lan, dmz],
    "storage-pool": [pool("local")],
  });
  const bodies = captureDeploy();
  renderPage(<TemplatesPage />);
  const { user, dialog } = await openDeploy();
  await user.type(within(dialog).getByLabelText("vCPUs"), "4");
  expect(
    await within(dialog).findByText(
      "Choose a node count that divides the number of sockets (1–8).",
    ),
  ).toBeVisible();
  await user.click(within(dialog).getByRole("button", { name: "Deploy" }));
  expect(bodies).toEqual([]);
  // 노드를 하나로 고르면 보낸다.
  await user.selectOptions(within(dialog).getByLabelText("Guest NUMA nodes"), "1 NUMA node");
  await user.click(within(dialog).getByRole("button", { name: "Deploy" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toMatchObject({ vcpus: 4, numa: { nodes: 1 } });
});

it("shows how to get a first template when there is none", async () => {
  mockLists({ vm: [web1], volume: [], node: [NODE] });
  renderPage(<TemplatesPage />);
  expect(await screen.findByText("No templates yet")).toBeInTheDocument();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Get one from the image library" })).toHaveAttribute(
    "href",
    "/images",
  );
  expect(screen.getByRole("link", { name: "Turn a VM into a template" })).toHaveAttribute(
    "href",
    "/vms",
  );
  expect(screen.getByRole("button", { name: "Learn more" })).toBeInTheDocument();
});

it("shows no template buttons to a viewer", async () => {
  mockLists({ vm: [], volume: [], node: [NODE] });
  renderPage(<TemplatesPage />, { as: VIEWER });
  expect(await screen.findByText("No templates yet")).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Get one from the image library" })).toBeNull();
  expect(screen.getByRole("button", { name: "Learn more" })).toBeInTheDocument();
});
