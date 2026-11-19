import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node, Vm } from "@/api/types";
import { builtinPrivileges, PRIVILEGES } from "@/auth/permissions";
import { cpuPending } from "@/cpu/cpu";
import { usageBars } from "@/folders/UsageBars";
import i18n from "@/i18n";
import { NodeTopologySection } from "@/numa/NodeTopologySection";
import {
  cpuGrid,
  cpuRoles,
  describeNuma,
  emptyNuma,
  formatCpuList,
  type NodeTopologyInfo,
  nodeCounts,
  numaErrors,
  numaFrom,
  numaOverride,
  numaSpec,
  parseCpuList,
  pinningRows,
  resourcesTo,
  roomOn,
} from "@/numa/numa";
import { NodesPage } from "@/nodes/NodesPage";
import { hardware, NODE, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { emptyForm, toSpec } from "@/vms/form";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmCreatePage } from "@/vms/VmFormPage";
import { VmMigrateDialog } from "@/vms/VmMigrateDialog";

afterEach(() => {
  toast.dismiss();
});

const t = i18n.t;

/** 2소켓 × 4코어 × 2스레드, 호스트 노드 둘(0: 0-7, 1: 8-15), 노드마다 L3 하나와 2M 512개 */
const TOPOLOGY: NodeTopologyInfo["topology"] = {
  sockets: 2,
  threads_per_core: 2,
  cpus: "0-15",
  nodes: [
    { id: 0, cpus: "0-7", memory_mib: 32768, hugepages: [{ size_kib: 2048, total: 512 }] },
    { id: 1, cpus: "8-15", memory_mib: 32768, hugepages: [{ size_kib: 2048, total: 512 }] },
  ],
  distances: [
    [10, 21],
    [21, 10],
  ],
  siblings: ["0,4", "1,5", "2,6", "3,7", "8,12", "9,13", "10,14", "11,15"],
  l3: ["0-7", "8-15"],
  page_sizes_kib: [2048, 1048576],
  pinning: true,
  cpuset: true,
  probed_at: 1,
  warnings: [],
};

const INFO: NodeTopologyInfo = {
  node: "node-a",
  topology: TOPOLOGY,
  host_resources: { reserved_cpus: "0", dedicated_cpus: "8-15" },
  sets: { reserved: "0", dedicated: "8-15", shared: "1-7" },
  host_nodes: [
    {
      id: 0,
      cpus: "0-7",
      memory_mib: 32768,
      dedicated: "",
      dedicated_used: "",
      hugepages: [{ size: "2M", total: 512, used: 0 }],
    },
    {
      id: 1,
      cpus: "8-15",
      memory_mib: 32768,
      dedicated: "8-15",
      dedicated_used: "8-9",
      hugepages: [{ size: "2M", total: 512, used: 128 }],
    },
  ],
  vms: [
    {
      vm: "db",
      host_nodes: [1],
      vcpus: [8, 9],
      siblings: "",
      emulator: "",
      page: "2M",
      pages: 128,
    },
  ],
  live: {
    affinity_ok: false,
    nodes: [
      { id: 0, memory_free_mib: 20000, hugepages: [{ size_kib: 2048, free: 500, surplus: 0 }] },
      { id: 1, memory_free_mib: 10000, hugepages: [{ size_kib: 2048, free: 384, surplus: 0 }] },
    ],
  },
  warnings: ["host processes may run on the dedicated CPUs 8-15; see the host preparation guide"],
};

/** 2 vCPU 전용·2M hugepages로 켠 VM(호스트 노드 1의 CPU 8·9) */
const DB: Vm = vm("db", "running", {
  spec: {
    ...vm("db", "running").spec,
    cpu: { sockets: 1, cores: 2, model: "host", policy: "dedicated" },
    hugepages: "2M",
  },
  status: {
    ...vm("db", "running").status,
    hardware: hardware({
      cpu: { sockets: 1, cores: 2, model: "host", policy: "dedicated" },
      layout: null,
      numa: {
        nodes: [{ sockets: [0], memory_mib: 2048 }],
        page: "2M",
        policy: "dedicated",
        thread_policy: "prefer",
        emulator: "shared",
        memory: "strict",
      },
      host_placement: { node: "n1", vcpus: [8, 9], host_nodes: [1], emulator: "" },
    }),
  },
});

// ---- 순수 함수

it("parses and formats CPU lists like the server", () => {
  expect(parseCpuList("0-3, 8,10-11")).toEqual([0, 1, 2, 3, 8, 10, 11]);
  expect(parseCpuList("")).toEqual([]);
  expect(parseCpuList("3-1")).toBeNull();
  expect(parseCpuList("a")).toBeNull();
  expect(parseCpuList("4096")).toBeNull();
  expect(formatCpuList([11, 0, 1, 2, 3, 8, 10, 3])).toBe("0-3,8,10-11");
  expect(formatCpuList([])).toBe("");
});

it("leaves a VM without plan-64 fields byte-identical", () => {
  const spec = toSpec(emptyForm("q35"), "stopped");
  expect(spec.cpu).toEqual({ sockets: 1, cores: 2, model: "host" });
  expect("numa" in spec).toBe(false);
  expect("hugepages" in spec).toBe(false);
  expect(numaSpec(emptyNuma())).toEqual({ cpu: {} });
});

it("round-trips the placement fields through the form", () => {
  const spec = {
    ...vm("x", "stopped").spec,
    cpu: {
      sockets: 2,
      cores: 4,
      model: "host",
      policy: "dedicated" as const,
      thread_policy: "isolate" as const,
      emulator: "isolate" as const,
    },
    numa: {
      nodes: [
        { cpus: 4, memory_mib: 1024 },
        { cpus: 4, memory_mib: 1024 },
      ],
      memory: "preferred" as const,
    },
    hugepages: "1G" as const,
  };
  const values = numaFrom(spec);
  expect(values.mode).toBe("list");
  expect(numaSpec(values)).toEqual({
    cpu: { policy: "dedicated", thread_policy: "isolate", emulator: "isolate" },
    numa: spec.numa,
    hugepages: "1G",
  });
  expect(describeNuma(t, spec)).toBe(
    "2 NUMA nodes, Dedicated (Isolate cores, Own dedicated CPU), hugepages 1G, Preferred",
  );
  expect(describeNuma(t, vm("y", "stopped").spec)).toBe("Shared, no NUMA");
});

it("checks node counts, node lists, require pairs and 1G with memory hotplug", () => {
  expect(nodeCounts(1)).toEqual([1]);
  expect(nodeCounts(4)).toEqual([1, 2, 4]);
  expect(nodeCounts(12)).toEqual([1, 2, 3, 4, 6]);
  const vmValues = { sockets: 2, cores: "3", maxVcpus: "", memory: "4096", maxMemory: "" };
  expect(numaErrors({ ...emptyNuma(), mode: "count", count: "3" }, vmValues)).toEqual({
    "numa.count": "numa.errors.count",
  });
  expect(
    numaErrors(
      { ...emptyNuma(), policy: "dedicated", threadPolicy: "require", hugepages: "1G" },
      { ...vmValues, maxMemory: "8192" },
    ),
  ).toEqual({
    "numa.threads": "numa.errors.requireOdd",
    "numa.hugepages": "numa.errors.hugepagesHotplug",
  });
  const list = (rows: [string, string][]) => ({
    ...emptyNuma(),
    mode: "list" as const,
    list: rows.map(([cpus, memory]) => ({ cpus, memory })),
  });
  expect(numaErrors(list([["2", "2048"]]), vmValues)).toEqual({
    "numa.list.0.cpus": "numa.errors.listCpus",
  });
  expect(numaErrors(list([["3", "2048"]]), vmValues)).toEqual({
    "numa.list": "numa.errors.listCpusSum",
  });
  expect(
    numaErrors(
      list([
        ["3", "2048"],
        ["3", "1024"],
      ]),
      vmValues,
    ),
  ).toEqual({ "numa.list": "numa.errors.listMemorySum" });
  expect(
    numaErrors(
      list([
        ["3", "2048"],
        ["3", "2048"],
      ]),
      vmValues,
    ),
  ).toEqual({});
});

it("sends only the changed placement fields as clone overrides", () => {
  const from = numaFrom(DB.spec);
  expect(numaOverride(null, from)).toEqual({});
  expect(numaOverride(from, from)).toEqual({});
  expect(numaOverride({ ...from, hugepages: "" }, from)).toEqual({ no_hugepages: true });
  expect(numaOverride({ ...from, policy: "shared" }, from)).toEqual({ cpu_policy: "shared" });
  expect(
    numaOverride({ ...from, threadPolicy: "isolate", mode: "count", count: "1" }, from),
  ).toEqual({ cpu_thread_policy: "isolate", numa: { nodes: 1 } });
  const isolated = { ...from, threadPolicy: "isolate" as const };
  expect(numaOverride({ ...isolated, threadPolicy: "" }, isolated)).toEqual({
    cpu_thread_policy: "prefer",
  });
  // 가져오기는 새 VM이라 지울 것이 없다.
  expect(numaOverride(emptyNuma(), emptyNuma(), false)).toEqual({});
  expect(
    numaOverride({ ...emptyNuma(), mode: "auto", memory: "strict" }, emptyNuma(), false),
  ).toEqual({ numa: { nodes: "auto", memory: "strict" } });
});

it("guesses which nodes have room for dedicated CPUs and hugepages", () => {
  const usage = [
    {
      node: "n1",
      pinning: true,
      dedicated_total: 4,
      dedicated_used: 2,
      hugepages: [{ size: "2M" as const, total_mib: 4096, used_mib: 0 }],
    },
    {
      node: "n2",
      pinning: true,
      dedicated_total: 8,
      dedicated_used: 0,
      hugepages: [{ size: "1G" as const, total_mib: 8192, used_mib: 0 }],
    },
    { node: "n3", pinning: false, dedicated_total: 0, dedicated_used: 0, hugepages: [] },
  ];
  expect(roomOn(usage, DB.spec)).toEqual(["n1"]);
  expect(roomOn(usage, { ...DB.spec, hugepages: "any" })).toEqual(["n1", "n2"]);
  expect(
    roomOn(usage, { ...DB.spec, cpu: { ...DB.spec.cpu, max_vcpus: 4 }, hugepages: undefined }),
  ).toEqual(["n2"]);
  expect(roomOn(usage, vm("x", "stopped").spec)).toEqual(["n1", "n2", "n3"]);
});

it("checks host resources and clears them with an empty object", () => {
  const online = parseCpuList("0-7")!;
  expect(resourcesTo({ reserved: "", dedicated: "", hugepages: [] }, online)).toEqual({
    errors: {},
    value: {},
  });
  expect(resourcesTo({ reserved: "0-1", dedicated: "1-3", hugepages: [] }, online).errors).toEqual({
    dedicated: "numa.resources.errors.overlap",
  });
  expect(resourcesTo({ reserved: "0", dedicated: "1-7", hugepages: [] }, online).errors).toEqual({
    dedicated: "numa.resources.errors.noShared",
  });
  expect(resourcesTo({ reserved: "9", dedicated: "x", hugepages: [] }, online).errors).toEqual({
    reserved: "numa.resources.errors.offline",
    dedicated: "numa.resources.errors.cpuList",
  });
  expect(
    resourcesTo(
      {
        reserved: "1,0",
        dedicated: "4-6",
        hugepages: [
          { size: "2M", count: "1024", hostNode: "0" },
          { size: "1G", count: "4", hostNode: "" },
        ],
      },
      online,
    ),
  ).toEqual({
    errors: {},
    value: {
      reserved_cpus: "0-1",
      dedicated_cpus: "4-6",
      hugepages: [
        { size: "2M", count: 1024, host_node: 0 },
        { size: "1G", count: 4 },
      ],
    },
  });
  expect(
    resourcesTo(
      {
        reserved: "",
        dedicated: "",
        hugepages: [
          { size: "2M", count: "", hostNode: "" },
          { size: "2M", count: "1", hostNode: "" },
        ],
      },
      online,
    ).errors,
  ).toEqual({
    "hugepages.0.count": "numa.resources.errors.count",
    "hugepages.1.count": "numa.resources.errors.duplicate",
  });
});

it("groups CPUs by host node, L3 and core, and colours them by set and VM", () => {
  const grid = cpuGrid(TOPOLOGY!);
  expect(grid.map((n) => n.node)).toEqual([0, 1]);
  expect(grid[0]!.l3[0]!.cores.slice(0, 2)).toEqual([
    [0, 4],
    [1, 5],
  ]);
  const roles = cpuRoles(INFO);
  expect(roles.get(0)).toEqual({ role: "reserved" });
  expect(roles.get(3)).toEqual({ role: "shared" });
  expect(roles.get(8)).toEqual({ role: "used", vm: "db" });
  expect(roles.get(10)).toEqual({ role: "free" });
});

it("lists the pinning of each vCPU slot from the record", () => {
  expect(pinningRows(DB.status.hardware!)).toEqual([
    { slot: 0, guestNode: 0, hostNode: 1, pcpu: 8 },
    { slot: 1, guestNode: 0, hostNode: 1, pcpu: 9 },
  ]);
});

it("treats a pending NUMA change as a power cycle, and keeps vm.dedicated out of operator", () => {
  const pending = {
    ...DB,
    status: {
      ...DB.status,
      hardware: {
        ...DB.status.hardware!,
        pending: [
          {
            field: "numa",
            running: "1 node",
            wanted: "2 nodes",
            reason: "restart" as const,
          },
        ],
      },
    },
  };
  expect(cpuPending(pending)?.field).toBe("numa");
  expect(PRIVILEGES).toContain("vm.dedicated");
  expect(builtinPrivileges("operator")).not.toContain("vm.dedicated");
});

it("shows the dedicated vCPU quota bar only when used or limited", () => {
  const view = {
    folder: "f",
    path: "/a",
    binding: {},
    limits: [],
    usage: { vcpus: 2, memory_mib: 1024, disk_bytes: 0, vms: 1 },
  };
  expect(usageBars(view).map((b) => b.dim)).toEqual(["vcpus", "memory_mib", "disk_bytes", "vms"]);
  expect(
    usageBars({ ...view, usage: { ...view.usage, dedicated_vcpus: 3 } }).map((b) => b.dim),
  ).toContain("dedicated_vcpus");
});

// ---- 화면

const X86: Node = { ...NODE, status: { ...NODE.status, arch: "x86_64" } };

it("saves CPU placement and NUMA from the VM form and shows where it fits", async () => {
  mockLists({ node: [X86] });
  const bodies: unknown[] = [];
  server.use(
    http.get(apiUrl("/topology/usage"), () =>
      HttpResponse.json({
        nodes: [
          {
            node: "node-a",
            pinning: true,
            dedicated_total: 8,
            dedicated_used: 0,
            hugepages: [{ size: "2M", total_mib: 8192, used_mib: 0 }],
          },
          { node: "node-b", pinning: false, dedicated_total: 0, dedicated_used: 0, hugepages: [] },
        ],
      }),
    ),
    http.post(apiUrl("/vms"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(vm("db", "stopped"), { status: 201 });
    }),
  );
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "db");
  await user.click(screen.getByText("CPU placement and NUMA"));
  // 소켓이 하나면 노드 수는 1뿐이고 그렇다고 말한다.
  expect(screen.getByText(/With one socket the guest has one NUMA node/)).toBeInTheDocument();
  await user.selectOptions(screen.getByLabelText("Sockets"), "2");
  await user.selectOptions(screen.getByLabelText("Guest NUMA nodes"), "2 NUMA nodes");
  await user.selectOptions(screen.getByLabelText("CPU policy"), "dedicated");
  await user.selectOptions(screen.getByLabelText("Emulator threads"), "isolate");
  await user.selectOptions(screen.getByLabelText("Hugepages"), "2M");
  expect(
    await screen.findByText("Room for this VM's dedicated CPUs and hugepages on: node-a"),
  ).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toMatchObject({
    spec: {
      cpu: { sockets: 2, cores: 2, policy: "dedicated", emulator: "isolate" },
      numa: { nodes: 2 },
      hugepages: "2M",
    },
  });
});

it("keeps dedicated CPUs and hugepages out of reach without vm.dedicated", async () => {
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: ["system.read", "cluster.read", "vm.create", "vm.config"],
        root_contents: ["system.read", "vm.create", "vm.config"],
        folders: {},
        contents: {},
        objects: [],
      }),
    ),
  );
  mockLists({ node: [X86] });
  renderPage(<VmCreatePage />, { path: "/vms/-new", as: { ...ADMIN, role: "operator" } });
  await userEvent.click(await screen.findByText("CPU placement and NUMA"));
  await waitFor(() => expect(screen.getByRole("option", { name: "Dedicated" })).toBeDisabled());
  expect(screen.getByRole("option", { name: "2 MiB" })).toBeDisabled();
  expect(screen.getAllByText(/need the vm.dedicated privilege/)).toHaveLength(2);
});

it("shows the CPU placement and the vCPU pinning of a running VM", async () => {
  mockLists({ vm: [DB], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  const section = await screen.findByRole("region", { name: "CPU placement" });
  expect(within(section).getByText("automatic NUMA, Dedicated, hugepages 2M")).toBeInTheDocument();
  expect(
    within(section).getByText("1 node (2 vCPU slots, 2048 MiB), Dedicated, hugepages 2M, Strict"),
  ).toBeInTheDocument();
  await userEvent.click(within(section).getByText("vCPU pinning"));
  const rows = within(within(section).getByRole("table", { name: "vCPU pinning" })).getAllByRole(
    "row",
  );
  expect(rows.map((r) => r.textContent)).toEqual([
    "vCPU slotGuest nodeHost nodeHost CPU",
    "0018",
    "1019",
  ]);
});

it("shows the node topology and saves host resources through a forced 409", async () => {
  const puts: { force: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/nodes/:key/topology"), () => HttpResponse.json(INFO)),
    http.put(apiUrl("/nodes/:key"), async ({ request }) => {
      const force = new URL(request.url).searchParams.get("force");
      puts.push({ force, body: await request.json() });
      if (!force) {
        return HttpResponse.json(
          {
            code: "host_cpus_in_use",
            message:
              'running VMs are pinned to CPUs outside the new dedicated CPUs of node "node-a": db (8-9)',
            details: { vms: ["db"] },
          },
          { status: 409 },
        );
      }
      return HttpResponse.json(NODE, {
        headers: { "orbivisor-warning": "db is pinned outside the dedicated CPUs" },
      });
    }),
  );
  const node = { ...NODE, spec: { ...NODE.spec, host_resources: INFO.host_resources } };
  mockLists({ node: [node] });
  renderPage(<NodeTopologySection node={node} />);
  const section = await screen.findByRole("region", { name: "NUMA topology" });
  expect(
    await within(section).findByText("2 sockets · 2 NUMA nodes · 16 CPUs · 2 threads per core", {
      exact: false,
    }),
  ).toBeInTheDocument();
  expect(
    within(section).getByText(/host processes may run on the dedicated CPUs/),
  ).toBeInTheDocument();
  expect(within(section).getByText(/does not run on the reserved CPUs/)).toBeInTheDocument();
  expect(
    within(section).getByRole("img", { name: "CPU 8: Dedicated, in use by db" }),
  ).toBeInTheDocument();
  expect(within(section).getByText("2M: 128 of 512 used")).toBeInTheDocument();
  expect(within(section).getByText("(384 free, 0 other)")).toBeInTheDocument();
  expect(within(section).getByRole("table", { name: "Distances" })).toHaveTextContent("21");

  const user = userEvent.setup();
  await user.click(within(section).getByRole("button", { name: "Host resources" }));
  const dialog = await screen.findByRole("dialog", { name: "Host resources of node-a" });
  expect(within(dialog).getByLabelText("Dedicated CPUs")).toHaveValue("8-15");
  // 격자를 누르면 전용에서 뺀다.
  await user.click(within(dialog).getByRole("button", { name: "CPU 8: Dedicated, in use by db" }));
  await user.click(within(dialog).getByRole("button", { name: "CPU 9: Dedicated, in use by db" }));
  expect(within(dialog).getByLabelText("Dedicated CPUs")).toHaveValue("10-15");
  await user.click(within(dialog).getByRole("button", { name: "Add reservation" }));
  await user.type(within(dialog).getByLabelText("Pages"), "1024");
  await user.selectOptions(within(dialog).getByLabelText("Host node"), "1");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByText("db")).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Save anyway" }));
  await waitFor(() => expect(puts).toHaveLength(2));
  expect(puts.map((p) => p.force)).toEqual([null, "true"]);
  expect(puts[1]!.body).toMatchObject({
    spec: {
      host_resources: {
        reserved_cpus: "0",
        dedicated_cpus: "10-15",
        hugepages: [{ size: "2M", count: 1024, host_node: 1 }],
      },
    },
  });
  expect(await screen.findByText("db is pinned outside the dedicated CPUs")).toBeInTheDocument();
});

it("says when a node has no topology", async () => {
  renderPage(<NodeTopologySection node={NODE} />);
  expect(await screen.findByText(/has not reported a NUMA topology/)).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Host resources" })).not.toBeInTheDocument();
});

it("lists dedicated CPUs and hugepages per node", async () => {
  server.use(
    http.get(apiUrl("/cluster"), () => HttpResponse.json({ name: "lab", node: "n1", nodes: [] })),
    http.get(apiUrl("/topology/usage"), () =>
      HttpResponse.json({
        nodes: [
          {
            node: "node-a",
            pinning: true,
            dedicated_total: 4,
            dedicated_used: 2,
            hugepages: [{ size: "2M", total_mib: 2048, used_mib: 512 }],
          },
        ],
      }),
    ),
  );
  mockLists({ node: [NODE] });
  renderPage(<NodesPage />, { path: "/nodes" });
  const row = (await screen.findByRole("link", { name: "node-a" })).closest("tr")!;
  expect(await within(row).findByText("2 of 4 used")).toBeInTheDocument();
  expect(within(row).getByText("2M: 512 MiB of 2 GiB")).toBeInTheDocument();
});

it("disables migration targets that cannot place the VM's dedicated CPUs", async () => {
  const node = (id: string, name: string): Node => ({
    ...NODE,
    id,
    name,
    status: { ...NODE.status, arch: "aarch64" },
  });
  mockLists({ vm: [DB], node: [node("n1", "node-a"), node("n2", "node-b"), node("n3", "node-c")] });
  server.use(
    http.get(apiUrl("/vms/:key/numa"), () =>
      HttpResponse.json({
        spec: { policy: "dedicated", hugepages: "2M" },
        wanted: "auto, dedicated, hugepages 2M",
        nodes: [
          {
            node: "node-b",
            fits: false,
            reason:
              'node "node-b" has 1 free dedicated CPUs on host NUMA node 0; the VM needs 2 on one',
          },
          { node: "node-c", fits: true, plan: { host_nodes: [0], pcpus: "2-3", page: "2M" } },
        ],
      }),
    ),
  );
  const { unmount } = renderPage(<VmMigrateDialog vm={DB} onClose={() => {}} />);
  let dialog = await screen.findByRole("dialog", { name: "Migrate db" });
  const b = await within(dialog).findByRole("option", {
    name: "node-b (cannot place the VM's CPUs or memory)",
  });
  expect(b).toBeDisabled();
  // 들어가는 첫 노드를 고른다.
  expect(within(dialog).getByLabelText("Target node")).toHaveValue("n3");
  unmount();
  // 팔레트가 못 들어가는 노드를 골랐으면 까닭을 보이고 보내지 않는다.
  renderPage(<VmMigrateDialog vm={DB} initialNode="n2" onClose={() => {}} />);
  dialog = await screen.findByRole("dialog", { name: "Migrate db" });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("has 1 free dedicated CPUs");
  expect(within(dialog).getByRole("button", { name: "Migrate" })).toBeDisabled();
});

it("clones a VM without its hugepages", async () => {
  const stopped = {
    ...DB,
    spec: { ...DB.spec, power: "stopped" as const },
    status: vm("db", "stopped").status,
  };
  mockLists({ vm: [stopped], node: [NODE] });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl("/vms/:key/clone"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-clone", stopped.id), { status: 202 });
    }),
  );
  renderPage(<VmCloneDialog vm={stopped} onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog", { name: "Clone db" });
  const user = userEvent.setup();
  await user.type(within(dialog).getByLabelText("New VM name"), "db2");
  await user.click(within(dialog).getByText("CPU placement and NUMA"));
  await user.selectOptions(within(dialog).getByLabelText("Hugepages"), "");
  await waitFor(() => expect(within(dialog).getByRole("button", { name: "Clone" })).toBeEnabled());
  await user.click(within(dialog).getByRole("button", { name: "Clone" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toEqual({ name: "db2", snapshot: null, pool: null, no_hugepages: true });
});
