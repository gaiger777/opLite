import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { useState } from "react";
import { toast } from "sonner";

import type { Node, Principal, Vm } from "@/api/types";
import { ClusterCpuSection } from "@/cpu/ClusterCpuSection";
import {
  archOf,
  cpuPending,
  type CpuModelEntry,
  describeResolved,
  flagProblem,
  modelGroups,
  normalizeFlag,
  toFlag,
} from "@/cpu/cpu";
import { cpuOverride } from "@/cpu/CpuChoice";
import { CpuFlagsInput } from "@/cpu/CpuFlagsInput";
import { CpuModelPicker } from "@/cpu/CpuModelPicker";
import { cpuPatch, CpuSettingsSection, cpuValues } from "@/cpu/CpuSettingsSection";
import { NodeCpuSection } from "@/cpu/NodeCpuSection";
import i18n from "@/i18n";
import { SETTINGS_KEY } from "@/api/live";
import { toRun, runForm } from "@/imports/imports";
import { describeDetail, describeTask, resetTasks } from "@/tasks/tasks";
import { hardware, NODE, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmCloneDialog } from "@/vms/VmCloneDialog";
import { BuilderPage } from "@/vmBuilder/BuilderPage";
import { VmListPage } from "@/vms/VmListPage";
import { VmMigrateDialog } from "@/vms/VmMigrateDialog";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

/** 만들기 화면(계획 77-5 — 빈 VM)과 고급 설정 서랍. CPU 모델·플래그·머신은 서랍에 있다. */
function openCreate() {
  renderPage(<BuilderPage />, { path: "/vms/-new", url: "/vms/-new?purpose=blank" });
}
async function advanced(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Advanced settings" }));
  await screen.findByRole("dialog", { name: "Advanced settings" });
}

const VIEWER: Principal = {
  user: "00000000-0000-0000-0000-000000000003",
  name: "viewer",
  role: "viewer",
};

const MODELS: CpuModelEntry[] = [
  { name: "host", kind: "basic", usable: 3 },
  { name: "max", kind: "basic", usable: 3 },
  { name: "cluster", kind: "cluster", usable: 3, current: "Skylake-Server-v5" },
  { name: "x86-64-v3", kind: "level", usable: 3 },
  { name: "Icelake-Server", kind: "qemu", alias_of: "Icelake-Server-v7", usable: 2 },
  {
    name: "Icelake-Server-v7",
    kind: "qemu",
    vendor: "GenuineIntel",
    usable: 2,
    nodes: [
      { node: "n1", can_run: true },
      { node: "n2", can_run: true },
      { node: "n4", can_run: false, missing: ["avx512vbmi", "gfni"] },
    ],
  },
  { name: "Skylake-Server-v5", kind: "qemu", vendor: "GenuineIntel", usable: 3 },
];

const FLAGS = [
  { name: "aes", supported: 3, known: 3 },
  { name: "avx512vbmi", supported: 2, known: 3 },
  { name: "sse4.2", supported: 3, known: 3 },
  { name: "pcid", supported: 3, known: 3, masked: true },
];

function mockCpu() {
  server.use(
    http.get(apiUrl("/cpu/models"), () =>
      HttpResponse.json({ arch: "x86_64", nodes: 3, models: MODELS }),
    ),
    http.get(apiUrl("/cpu/flags"), () =>
      HttpResponse.json({ arch: "x86_64", nodes: 3, flags: FLAGS }),
    ),
  );
}

describe("pure helpers", () => {
  it("normalizes flag names like the server", () => {
    expect(normalizeFlag("AVX512_VBMI")).toBe("avx512-vbmi");
    expect(normalizeFlag("sse4_2")).toBe("sse4.2");
    expect(normalizeFlag("sse4-1")).toBe("sse4.1");
    expect(normalizeFlag("-bad")).toBeNull();
    expect(normalizeFlag("a b")).toBeNull();
    expect(toFlag("avx512vbmi")).toBe("-avx512vbmi");
    expect(toFlag("+AES")).toBe("+aes");
    expect(toFlag("aes", true)).toBe("+aes");
  });

  it("refuses unknown, repeated and too many flags", () => {
    const known = new Set(["aes", "avx512vbmi"]);
    expect(flagProblem("avx512vbmi", [], known)).toBeNull();
    expect(flagProblem("nope", [], known)).toBe("cpu.errors.unknownFlag");
    expect(flagProblem("nope", [], undefined)).toBeNull();
    expect(flagProblem("+aes", ["-aes"], known)).toBe("cpu.errors.duplicateFlag");
    expect(flagProblem("x y", [], undefined)).toBe("cpu.errors.flagName");
    const many = Array.from({ length: 64 }, (_, i) => `-f${i}`);
    expect(flagProblem("aes", many, known)).toBe("cpu.errors.tooManyFlags");
  });

  it("groups models, hides aliases and finds versioned models by their alias", () => {
    const groups = modelGroups(MODELS, "", "host");
    expect(groups.map((g) => [g.kind, g.models.map((m) => m.name)])).toEqual([
      ["basic", ["host", "max"]],
      ["cluster", ["cluster"]],
      ["level", ["x86-64-v3"]],
      ["qemu", ["Icelake-Server-v7", "Skylake-Server-v5"]],
    ]);
    // 별칭 이름으로 찾으면 버전 모델이 나온다. 지금 값은 찾기와 상관없이 남는다.
    expect(
      modelGroups(MODELS, "icelake-server", "max").flatMap((g) => g.models.map((m) => m.name)),
    ).toEqual(["max", "Icelake-Server-v7"]);
  });

  it("describes CPUs and finds the pending CPU change", () => {
    expect(
      describeResolved({ model: "Icelake-Server-v7", flags: { pcid: true, avx512vbmi: false } }),
    ).toBe("Icelake-Server-v7 -avx512vbmi +pcid");
    expect(archOf("q35")).toBe("x86_64");
    expect(archOf("")).toBeUndefined();
    const pending = vm("web", "running", {
      status: {
        ...vm("web", "running").status,
        hardware: hardware({
          pending: [
            {
              field: "cpu",
              running: "1x2 host",
              wanted: "1x2 host -avx512vbmi",
              reason: "restart",
            },
          ],
        }),
      },
    });
    expect(cpuPending(pending)?.wanted).toBe("1x2 host -avx512vbmi");
    expect(cpuPending(vm("db", "running"))).toBeUndefined();
  });

  it("names the cpu-baseline and node-cpu-probe tasks", () => {
    const t = i18n.t;
    const baseline = task("t1", "cpu-baseline", "c1");
    baseline.spec.op = {
      kind: "cpu-baseline",
      arch: "x86_64",
      mode: "auto",
      previous: "Icelake-Server-v7",
      current: "Skylake-Server-v5",
      limited_by: ["n4"],
      running_vms: 3,
    };
    expect(describeTask(baseline, t, "cluster")).toBe(
      "CPU baseline for x86_64: Icelake-Server-v7 → Skylake-Server-v5",
    );
    expect(describeDetail(baseline, t)).toBe(
      "Limited by n4. 3 running VMs change at their next power cycle.",
    );
    expect(describeTask(task("t2", "node-cpu-probe", "n1"), t, "node-a")).toBe(
      "Re-read CPU models on node-a",
    );
  });

  it("turns settings into a patch of only the changed fields", () => {
    const current = cpuValues({ cpu: { auto_flags: true, mask: ["avx512vbmi"] } });
    expect(current).toEqual({
      mode: "off",
      pinned: {},
      autoFlags: true,
      mask: ["avx512vbmi"],
      stored: "off",
    });
    expect(cpuPatch(current, current)).toEqual({});
    expect(cpuPatch({ ...current, mask: ["avx512vbmi", "pcid"] }, current)).toEqual({
      mask: ["avx512vbmi", "pcid"],
    });
    const pinned = {
      ...current,
      mode: "pinned" as const,
      pinned: { x86_64: { model: "Haswell-v4", flags: ["-hle"] } },
    };
    expect(cpuPatch(pinned, current, true)).toEqual({
      pinned: [{ arch: "x86_64", model: "Haswell-v4", flags: ["-hle"] }],
      force: true,
    });
    expect(
      cpuValues({
        cpu: {
          auto_flags: true,
          baseline: "off",
          pinned: [{ arch: "x86_64", model: "Haswell-v4" }],
        },
      }).mode,
    ).toBe("pinned");
    // `auto` + 고정(CLI로 둔 설정)에서 "끔"을 고르면 `baseline: off`도 보낸다(리뷰 1 웹 #5).
    const both = cpuValues({
      cpu: {
        auto_flags: true,
        baseline: "auto",
        pinned: [{ arch: "x86_64", model: "Haswell-v4" }],
      },
    });
    expect(both.mode).toBe("pinned");
    expect(cpuPatch({ ...both, mode: "off" }, both)).toEqual({ baseline: "off", pinned: [] });
    // 리뷰 2 웹 #1: x86_64 자동 + aarch64 고정은 "고정"으로 보이지만 고치지 않으면 보낼 것이 없고, 숨김 목록만 고치면
    // `baseline`을 건드리지 않는다(x86_64 자동 기준이 사라지지 않게).
    const mixed = cpuValues({
      cpu: {
        auto_flags: true,
        baseline: "auto",
        pinned: [{ arch: "aarch64", model: "neoverse-n1" }],
      },
    });
    expect(cpuPatch(mixed, mixed)).toEqual({});
    expect(cpuPatch({ ...mixed, mask: ["avx512vbmi"] }, mixed)).toEqual({ mask: ["avx512vbmi"] });
    expect(cpuPatch({ ...mixed, mode: "auto" }, mixed)).toEqual({ pinned: [] });
  });

  it("sends only the CPU fields a clone or deploy changed, and an import's model and flags", () => {
    const from = { model: "host", flags: ["-avx512vbmi"] };
    expect(cpuOverride(null, from)).toEqual({});
    expect(cpuOverride(from, from)).toEqual({});
    expect(cpuOverride({ ...from, model: "cluster" }, from)).toEqual({ cpu_model: "cluster" });
    expect(cpuOverride({ ...from, flags: [] }, from)).toEqual({ cpu_flags: [] });
    const imp = { name: "web", status: {}, spec: {} } as never;
    const form = runForm(imp);
    expect(toRun(form)).not.toHaveProperty("cpu_model");
    expect(toRun({ ...form, cpuModel: "cluster", cpuFlags: ["-aes"] })).toMatchObject({
      cpu_model: "cluster",
      cpu_flags: ["-aes"],
    });
  });
});

function PickerHarness() {
  const [model, setModel] = useState("host");
  const [flags, setFlags] = useState<string[]>(["-avx512vbmi"]);
  return (
    <>
      <CpuModelPicker id="m" label="CPU model" value={model} onChange={setModel} arch="x86_64" />
      <CpuFlagsInput
        id="f"
        label="CPU flags"
        flags={flags}
        onChange={setFlags}
        arches={["x86_64"]}
        signed
      />
      <output>{[model, ...flags].join(" ")}</output>
    </>
  );
}

it("picks a model, shows the nodes that cannot run it and edits flags", async () => {
  mockCpu();
  renderPage(<PickerHarness />);
  const select = await screen.findByLabelText("CPU model");
  await screen.findByRole("option", { name: "Icelake-Server-v7 — runs on 2/3 nodes" });
  // 별칭은 고르지 않는다.
  expect(screen.queryByRole("option", { name: /^Icelake-Server —/ })).not.toBeInTheDocument();
  expect(screen.getByRole("option", { name: /cluster \(Skylake-Server-v5\)/ })).toBeInTheDocument();
  await userEvent.selectOptions(select, "Icelake-Server-v7");
  expect(screen.getByText("n4 cannot run it (missing avx512vbmi, gfni)")).toBeInTheDocument();

  const input = screen.getByLabelText("CPU flags", { selector: "input" });
  // 모르는 이름은 받지 않는다.
  await userEvent.type(input, "nonexistent{Enter}");
  expect(screen.getByText("No node's QEMU knows this flag.")).toBeInTheDocument();
  await userEvent.clear(input);
  // Enter는 숨기기다.
  await userEvent.type(input, "SSE4_2{Enter}");
  await userEvent.type(input, "aes");
  await userEvent.click(screen.getByRole("button", { name: "Turn on" }));
  expect(screen.getByRole("status")).toHaveTextContent(
    "Icelake-Server-v7 -avx512vbmi -sse4.2 +aes",
  );
  // 칩을 누르면 켜기·숨기기가 바뀌고, ×로 뺀다.
  // 칩의 이름에 지금 상태가 있다(리뷰 1 웹 #11).
  await userEvent.click(
    screen.getByRole("button", { name: "avx512vbmi is hidden — turn it on instead" }),
  );
  await userEvent.click(screen.getByRole("button", { name: "Remove sse4.2" }));
  expect(screen.getByRole("status")).toHaveTextContent("Icelake-Server-v7 +avx512vbmi +aes");
  // 클러스터 전체에서 숨긴 플래그를 켜면 그렇다고 말한다.
  await userEvent.type(input, "pcid");
  await userEvent.click(screen.getByRole("button", { name: "Turn on" }));
  expect(screen.getByText("(hidden cluster-wide)")).toBeInTheDocument();
});

it("falls back to host and max when the models cannot be read", async () => {
  server.use(
    http.get(apiUrl("/cpu/models"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<PickerHarness />);
  const select = await screen.findByLabelText("CPU model");
  expect(
    within(select)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["host", "max"]);
});

const PENDING_VM: Vm = vm("web", "running", {
  status: {
    ...vm("web", "running").status,
    hardware: hardware({
      pending: [
        {
          field: "cpu",
          running: "1x2 host",
          wanted: "1x2 host -avx512vbmi",
          reason: "restart",
          detail: "-avx512vbmi; power cycle required",
        },
      ],
    }),
  },
});

it("filters the VM list to VMs waiting for a CPU power cycle", async () => {
  mockLists({ vm: [PENDING_VM, vm("db", "running")], node: [NODE] });
  renderPage(<VmListPage />, { path: "/vms", url: "/vms?cpu=pending" });
  expect(await screen.findByRole("link", { name: "web" })).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "db" })).not.toBeInTheDocument();
  expect(
    screen.getByText("CPU change pending", { selector: "[data-slot=badge]" }),
  ).toBeInTheDocument();
});

function cpuNode(id: string, name: string): Node {
  return { ...NODE, id, name, status: { ...NODE.status, arch: "aarch64" } };
}

it("disables migration targets that cannot run the VM's CPU and shows the host warning", async () => {
  const web = vm("web", "running");
  mockLists({
    vm: [web],
    node: [cpuNode("n1", "node-a"), cpuNode("n2", "node-b"), cpuNode("n3", "node-c")],
  });
  server.use(
    http.get(apiUrl("/vms/:key/cpu"), () =>
      HttpResponse.json({
        spec: { cores: 2, model: "host" },
        nodes: [
          {
            node: "node-b",
            can_run: false,
            reason: 'node "node-b" cannot run CPU model X (missing a)',
          },
          {
            node: "node-c",
            can_run: true,
            warnings: ['target node "node-c" lacks avx512vbmi that the guest may be using'],
          },
        ],
      }),
    ),
  );
  renderPage(<VmMigrateDialog vm={web} onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  const b = await within(dialog).findByRole("option", { name: "node-b (cannot run the VM's CPU)" });
  expect(b).toBeDisabled();
  // 켤 수 있는 첫 노드를 고른다.
  expect(within(dialog).getByLabelText("Target node")).toHaveValue("n3");
  expect(
    within(dialog).getByText('target node "node-c" lacks avx512vbmi that the guest may be using'),
  ).toBeInTheDocument();
});

it("shows the node CPU and re-reads it as a task", async () => {
  let refreshed = 0;
  server.use(
    http.get(apiUrl("/nodes/:key/cpu"), () =>
      HttpResponse.json({
        node: "node-a",
        arch: "x86_64",
        cpu: {
          catalog: "c82d",
          qemu: "11.1.2",
          accel: "kvm",
          vendor: "GenuineIntel",
          model_id: "Intel Xeon",
          probed_at: 1_763_500_000,
        },
        baseline_problem: "cannot run CPU model Icelake-Server-v7 (missing avx512vbmi)",
        flags: [{ name: "aes", supported: true }],
        models: [
          {
            name: "Icelake-Server-v7",
            usable: false,
            migration_safe: true,
            missing: ["avx512vbmi"],
          },
          { name: "Skylake-Server-v5", usable: true, migration_safe: true },
        ],
      }),
    ),
    http.post(apiUrl("/nodes/:key/cpu/refresh"), () => {
      refreshed += 1;
      return HttpResponse.json(task("t9", "node-cpu-probe", "n1"), { status: 202 });
    }),
  );
  mockLists({ node: [NODE] });
  renderPage(<NodeCpuSection node={NODE} />);
  const section = await screen.findByRole("region", { name: "CPU models" });
  expect(await within(section).findByText("11.1.2")).toBeInTheDocument();
  expect(within(section).getByText("Intel Xeon")).toBeInTheDocument();
  expect(within(section).getByText(/does not take cluster-baseline VMs/)).toBeInTheDocument();
  expect(within(section).getByText("1 of 2 usable · 1 flags")).toBeInTheDocument();
  await userEvent.type(within(section).getByLabelText("Search CPU models"), "sky");
  expect(within(section).queryByText("Icelake-Server-v7")).not.toBeInTheDocument();
  await userEvent.click(within(section).getByRole("button", { name: "Re-read CPU models" }));
  await waitFor(() => expect(refreshed).toBe(1));
});

it("hides the node and cluster CPU views without cluster.read and the refresh without admin", async () => {
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: ["vm.power"],
        folders: {},
        contents: {},
        objects: [],
      }),
    ),
  );
  renderPage(
    <>
      <p>page</p>
      <NodeCpuSection node={NODE} />
      <ClusterCpuSection />
    </>,
    { as: VIEWER },
  );
  await screen.findByText("page");
  await waitFor(() =>
    expect(screen.queryByRole("region", { name: "CPU models" })).not.toBeInTheDocument(),
  );
  expect(screen.queryByRole("region", { name: "CPU baseline" })).not.toBeInTheDocument();
});

it("shows the cluster baseline per architecture", async () => {
  server.use(
    http.get(apiUrl("/cpu/baseline"), () =>
      HttpResponse.json({
        mode: "auto",
        auto_flags: true,
        mask: ["avx512vbmi"],
        arches: [
          {
            arch: "x86_64",
            pinned: false,
            cluster_vms: 4,
            nodes: ["n1", "n2", "n4"],
            limited_by: ["n4"],
            baseline: {
              model: "Skylake-Server-v5",
              flags: { pcid: true },
              mode: "auto",
              previous: { model: "Icelake-Server-v7" },
              since: 1_763_500_000,
              warnings: [],
            },
          },
        ],
      }),
    ),
  );
  mockCpu();
  renderPage(<ClusterCpuSection />);
  const section = await screen.findByRole("region", { name: "CPU baseline" });
  expect(await within(section).findByText("Skylake-Server-v5 +pcid")).toBeInTheDocument();
  expect(within(section).getByText("Limited by: n4")).toBeInTheDocument();
  expect(within(section).getByText("Hidden from every VM: avx512vbmi")).toBeInTheDocument();
  expect(within(section).getByText(/before: Icelake-Server-v7/)).toBeInTheDocument();
});

const SETTINGS = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 3,
  labels: {},
  spec: { updates: { channel: "stable" } },
  status: {},
};

it("saves the hide list and links to the VMs waiting for a power cycle", async () => {
  const sent: unknown[] = [];
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({
        ...SETTINGS,
        revision: 4,
        spec: { ...SETTINGS.spec, cpu: { auto_flags: true, mask: ["avx512vbmi"] } },
        affected_running_vms: 2,
      });
    }),
  );
  renderPage(<CpuSettingsSection write />, { as: ADMIN });
  const input = await screen.findByLabelText("Hide from every VM", { selector: "input" });
  await waitFor(() =>
    // 자동 완성 목록(이름을 거르는 데도 쓴다)이 찼다.
    expect(
      document.querySelector('#settings-cpu-mask-names option[value="avx512vbmi"]'),
    ).not.toBeNull(),
  );
  await userEvent.type(input, "avx512vbmi{Enter}");
  await userEvent.click(screen.getByRole("button", { name: "Save CPU settings" }));
  expect(await screen.findByText(/2 running VMs keep their CPU/)).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Show them" })).toHaveAttribute(
    "href",
    "/vms?cpu=pending",
  );
  expect(sent).toEqual([{ cpu: { mask: ["avx512vbmi"] } }]);
  // 보류는 각 노드의 검사 회차가 쓰므로 목록이 몇 초 늦게 찬다고 말한다(리뷰 1 웹 #6).
  expect(screen.getByText(/fills in within a few seconds/)).toBeInTheDocument();
});

it("explains nodes that cannot run a pinned baseline and pins anyway with force", async () => {
  const sent: unknown[] = [];
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { cpu: { force?: boolean } };
      sent.push(body);
      if (!body.cpu.force) {
        return HttpResponse.json(
          {
            code: "cpu_baseline_unsupported",
            message: "node n4 cannot run",
            details: {
              nodes: ["n4"],
              warnings: ['node "n4" cannot run CPU model Icelake-Server-v7 (missing gfni)'],
            },
          },
          { status: 409 },
        );
      }
      return HttpResponse.json({ ...SETTINGS, revision: 4, affected_running_vms: 0 });
    }),
  );
  renderPage(<CpuSettingsSection write />, { as: ADMIN });
  await userEvent.selectOptions(await screen.findByLabelText("Cluster CPU baseline"), "pinned");
  const model = await screen.findByLabelText("Model");
  await screen.findByRole("option", { name: /Icelake-Server-v7/ });
  await userEvent.selectOptions(model, "Icelake-Server-v7");
  await userEvent.click(screen.getByRole("button", { name: "Save CPU settings" }));
  const alert = await screen.findByRole("alert");
  expect(alert).toHaveTextContent("Some nodes cannot run the pinned model: n4");
  expect(alert).toHaveTextContent("missing gfni");
  await userEvent.click(within(alert).getByRole("button", { name: "Pin anyway" }));
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent[1]).toEqual({
    cpu: { pinned: [{ arch: "x86_64", model: "Icelake-Server-v7" }], force: true },
  });
});

it("defaults a new VM to the cluster model and sends its CPU flags", async () => {
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl("/vms"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(vm("web", "stopped"), { status: 201 });
    }),
  );
  openCreate();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await advanced(user);
  // 클러스터 기준이 있으면 새 VM은 `cluster`로 시작한다.
  await waitFor(() => expect(screen.getByLabelText("CPU model")).toHaveValue("cluster"));
  expect(screen.getByText(/The cluster baseline is now Skylake-Server-v5/)).toBeInTheDocument();
  const flags = screen.getByLabelText("CPU flags", { selector: "input" });
  await waitFor(() =>
    expect(document.querySelector('#vm-cpu-flags-names option[value="aes"]')).not.toBeNull(),
  );
  await user.type(flags, "avx512vbmi{Enter}");
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toMatchObject({
    spec: { cpu: { model: "cluster", flags: ["-avx512vbmi"] } },
  });
});

it("offers a power cycle for a pending CPU change and says a guest reboot does not apply it", async () => {
  const actions: string[] = [];
  mockLists({ vm: [PENDING_VM], node: [NODE] });
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      actions.push(String(params.action));
      return HttpResponse.json(task("t5", "vm-reboot", PENDING_VM.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByText(/Power cycle required/)).toBeInTheDocument();
  expect(
    screen.getByRole("heading", { name: "Applies at the next power cycle" }),
  ).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Power cycle now" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Reboot web?" });
  expect(dialog).toHaveTextContent("a reboot inside the guest would not apply them");
  await userEvent.click(within(dialog).getByRole("button", { name: "Reboot" }));
  await waitFor(() => expect(actions).toEqual(["reboot"]));
});

// ---- 리뷰 1 (웹)

it("keeps host for a new VM when the cluster has no baseline", async () => {
  // 기본 msw 응답은 서버 그대로다: 기준이 없으면 `cluster`에 `current`가 없다.
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  openCreate();
  await advanced(userEvent.setup());
  const model = await screen.findByLabelText("CPU model");
  await screen.findByRole("option", { name: /^cluster/ });
  await new Promise((r) => setTimeout(r, 50));
  expect(model).toHaveValue("host");
  await userEvent.selectOptions(model, "cluster");
  expect(screen.getByText(/No cluster baseline is set/)).toBeInTheDocument();
});

it("honors the sign typed before a flag, and suggests each name once", async () => {
  mockCpu();
  server.use(
    http.get(apiUrl("/cpu/flags"), ({ request }) =>
      HttpResponse.json({
        arch: new URL(request.url).searchParams.get("arch"),
        nodes: 1,
        flags: FLAGS,
      }),
    ),
  );
  function Harness() {
    const [flags, setFlags] = useState<string[]>([]);
    return (
      <>
        <CpuFlagsInput
          id="f"
          label="CPU flags"
          flags={flags}
          onChange={setFlags}
          arches={["x86_64", "aarch64"]}
          signed
        />
        <output>{flags.join(" ")}</output>
      </>
    );
  }
  renderPage(<Harness />);
  await waitFor(() =>
    expect(document.querySelectorAll('#f-names option[value="aes"]')).toHaveLength(1),
  );
  const input = screen.getByLabelText("CPU flags", { selector: "input" });
  await userEvent.type(input, "+aes{Enter}");
  await userEvent.type(input, "+sse4.2");
  await userEvent.click(screen.getByRole("button", { name: "Hide" }));
  await userEvent.type(input, "-avx512vbmi");
  await userEvent.click(screen.getByRole("button", { name: "Turn on" }));
  expect(screen.getByRole("status")).toHaveTextContent("+aes +sse4.2 -avx512vbmi");
  expect(screen.getByRole("button", { name: "aes is on — hide it instead" })).toBeInTheDocument();
});

it("saves only what was edited and shows a concurrent settings change", async () => {
  const sent: { body: unknown; revision: string | null }[] = [];
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      sent.push({ body: await request.json(), revision: request.headers.get("If-Match") });
      return HttpResponse.json({ code: "revision_mismatch", message: "stale" }, { status: 412 });
    }),
  );
  const { queryClient } = renderPage(<CpuSettingsSection write />, { as: ADMIN });
  const input = await screen.findByLabelText("Hide from every VM", { selector: "input" });
  await waitFor(() =>
    expect(
      document.querySelector('#settings-cpu-mask-names option[value="avx512vbmi"]'),
    ).not.toBeNull(),
  );
  await userEvent.type(input, "avx512vbmi{Enter}");
  // 그 사이 다른 관리자가 자동 플래그를 껐다(watch가 새 설정을 넣는다).
  queryClient.setQueryData(SETTINGS_KEY, {
    ...SETTINGS,
    revision: 7,
    spec: { ...SETTINGS.spec, cpu: { auto_flags: false } },
  });
  await userEvent.click(screen.getByRole("button", { name: "Save CPU settings" }));
  const alert = await screen.findByRole("alert");
  // 고친 칸만, 고치기 시작할 때의 revision으로 보낸다 — 남의 auto_flags를 되돌리지 않는다.
  expect(sent).toEqual([{ body: { cpu: { mask: ["avx512vbmi"] } }, revision: "3" }]);
  expect(alert).toHaveTextContent("Someone else changed it first");
  await userEvent.click(
    within(alert).getByRole("button", { name: "Discard my changes and reload" }),
  );
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(screen.queryByText("avx512vbmi", { selector: "li span" })).not.toBeInTheDocument();
});

it("pre-fills a pinned baseline with the current model and its flags", async () => {
  const sent: unknown[] = [];
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({
        ...SETTINGS,
        spec: { ...SETTINGS.spec, cpu: { baseline: "auto", auto_flags: true, mask: ["gfni"] } },
      }),
    ),
    http.get(apiUrl("/cpu/baseline"), () =>
      HttpResponse.json({
        mode: "auto",
        auto_flags: true,
        mask: ["gfni"],
        arches: [
          {
            arch: "x86_64",
            pinned: false,
            cluster_vms: 1,
            baseline: {
              model: "Skylake-Server-v5",
              flags: { pcid: true, gfni: false, hle: false },
              mode: "auto",
              since: 1,
            },
          },
        ],
      }),
    ),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({ ...SETTINGS, revision: 4, affected_running_vms: 0 });
    }),
  );
  renderPage(<CpuSettingsSection write />, { as: ADMIN });
  await screen.findByText(/Now on x86_64/);
  await userEvent.selectOptions(screen.getByLabelText("Cluster CPU baseline"), "pinned");
  await userEvent.click(screen.getByRole("button", { name: "Save CPU settings" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  // 숨김 목록이 끈 gfni는 고정에 넣지 않는다.
  expect(sent[0]).toEqual({
    cpu: {
      baseline: "off",
      pinned: [{ arch: "x86_64", model: "Skylake-Server-v5", flags: ["+pcid", "-hle"] }],
    },
  });
});

it("re-reports a failed node CPU refresh only once", async () => {
  server.use(
    http.post(apiUrl("/nodes/:key/cpu/refresh"), () =>
      HttpResponse.json(task("t9", "node-cpu-probe", "n1"), { status: 202 }),
    ),
  );
  mockLists({
    node: [NODE],
    task: [
      {
        ...task("t9", "node-cpu-probe", "n1"),
        status: { state: "failed", error: "timed out waiting for the CPU probe" },
      } as never,
    ],
  });
  renderPage(<NodeCpuSection node={NODE} />);
  const section = await screen.findByRole("region", { name: "CPU models" });
  const button = within(section).getByRole("button", { name: "Re-read CPU models" });
  await userEvent.click(button);
  await waitFor(() => expect(button).toBeEnabled());
  expect(screen.queryByText(/Could not re-read CPU models/)).not.toBeInTheDocument();
});

it("resets an x86 model and flags when the machine type switches the architecture", async () => {
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  openCreate();
  const user = userEvent.setup();
  await advanced(user);
  const machine = await screen.findByLabelText("Virtual motherboard (advanced)");
  await user.selectOptions(machine, "q35");
  await screen.findAllByRole("option", { name: /Skylake-Server-v5/ });
  await user.selectOptions(screen.getByLabelText("CPU model"), "Skylake-Server-v5");
  const flags = screen.getByLabelText("CPU flags", { selector: "input" });
  await waitFor(() =>
    expect(document.querySelector('#vm-cpu-flags-names option[value="aes"]')).not.toBeNull(),
  );
  await user.type(flags, "aes{Enter}");
  await user.selectOptions(machine, "virt");
  await waitFor(() => expect(screen.getByLabelText("CPU model")).toHaveValue("host"));
  expect(screen.queryByRole("button", { name: /aes is hidden/ })).not.toBeInTheDocument();
  expect(screen.getByText(/so the CPU model and flags were reset/)).toBeInTheDocument();
});

it("does not migrate before the VM's CPU check has loaded", async () => {
  const web = vm("web", "running");
  mockLists({ vm: [web], node: [cpuNode("n1", "node-a"), cpuNode("n2", "node-b")] });
  let answer: () => void = () => {};
  const gate = new Promise<void>((r) => (answer = r));
  server.use(
    http.get(apiUrl("/vms/:key/cpu"), async () => {
      await gate;
      return HttpResponse.json({
        spec: { sockets: 1, cores: 2, model: "host" },
        nodes: [{ node: "node-b", can_run: true }],
      });
    }),
  );
  renderPage(<VmMigrateDialog vm={web} onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog", { name: "Migrate web" });
  await within(dialog).findByRole("option", { name: "node-b" });
  const submit = within(dialog).getByRole("button", { name: "Migrate" });
  await new Promise((r) => setTimeout(r, 50));
  expect(submit).toBeDisabled();
  answer();
  await waitFor(() => expect(submit).toBeEnabled());
});

it("renders the model × node table only when it is opened", async () => {
  server.use(
    http.get(apiUrl("/cpu/baseline"), () =>
      HttpResponse.json({
        mode: "off",
        auto_flags: true,
        arches: [{ arch: "x86_64", pinned: false, cluster_vms: 0 }],
      }),
    ),
  );
  mockCpu();
  renderPage(<ClusterCpuSection />);
  const section = await screen.findByRole("region", { name: "CPU baseline" });
  const summary = await within(section).findByText(/models × nodes|model/i, {
    selector: "summary",
  });
  expect(within(section).queryByRole("table")).not.toBeInTheDocument();
  await userEvent.click(summary);
  expect(await within(section).findByRole("table")).toBeInTheDocument();
  expect(within(section).getAllByText("✓").length).toBeGreaterThan(0);
});

it("returns focus to Power cycle now when the confirmation closes", async () => {
  mockLists({ vm: [PENDING_VM], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const button = await screen.findByRole("button", { name: "Power cycle now" });
  await userEvent.click(button);
  const dialog = await screen.findByRole("alertdialog", { name: "Reboot web?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(button).toHaveFocus());
});

it("rebases the CPU draft on another settings save and reloads on discard", async () => {
  const sent: (string | null)[] = [];
  let reads = 0;
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  server.use(
    http.get(apiUrl("/settings"), () => {
      reads += 1;
      return HttpResponse.json(SETTINGS);
    }),
    http.patch(apiUrl("/settings"), ({ request }) => {
      sent.push(request.headers.get("If-Match"));
      return HttpResponse.json({ code: "revision_mismatch", message: "stale" }, { status: 412 });
    }),
  );
  const { queryClient } = renderPage(<CpuSettingsSection write />, { as: ADMIN });
  const input = await screen.findByLabelText("Hide from every VM", { selector: "input" });
  await waitFor(() =>
    expect(
      document.querySelector('#settings-cpu-mask-names option[value="avx512vbmi"]'),
    ).not.toBeNull(),
  );
  await userEvent.type(input, "avx512vbmi{Enter}");
  // 같은 페이지의 다른 절이 저장했다: CPU 칸은 그대로라 새 revision으로 보낸다(리뷰 2 웹 #2).
  queryClient.setQueryData(SETTINGS_KEY, { ...SETTINGS, revision: 9 });
  await userEvent.click(screen.getByRole("button", { name: "Save CPU settings" }));
  await waitFor(() => expect(sent).toEqual(["9"]));
  // "버리고 다시 읽기"는 설정을 다시 읽는다(리뷰 2 웹 #4).
  const alert = await screen.findByRole("alert");
  const before = reads;
  await userEvent.click(
    within(alert).getByRole("button", { name: "Discard my changes and reload" }),
  );
  await waitFor(() => expect(reads).toBeGreaterThan(before));
});

it("pre-fills a pinned baseline without flags the stored hide list turned off", async () => {
  const sent: unknown[] = [];
  mockCpu();
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({
        ...SETTINGS,
        spec: { ...SETTINGS.spec, cpu: { baseline: "auto", auto_flags: true, mask: ["sse4.2"] } },
      }),
    ),
    http.get(apiUrl("/cpu/baseline"), () =>
      HttpResponse.json({
        mode: "auto",
        auto_flags: true,
        mask: ["sse4.2"],
        arches: [
          {
            arch: "x86_64",
            pinned: false,
            cluster_vms: 1,
            baseline: {
              model: "Skylake-Server-v5",
              flags: { pcid: true, "sse4.2": false },
              mode: "auto",
              since: 1,
            },
          },
        ],
      }),
    ),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json({ ...SETTINGS, revision: 4, affected_running_vms: 0 });
    }),
  );
  renderPage(<CpuSettingsSection write />, { as: ADMIN });
  await screen.findByText(/Now on x86_64/);
  // 저장 전에 숨김 목록에서 뺀 뒤 고정으로 바꿔도 기준의 -sse4.2는 고정에 넣지 않는다(리뷰 2 웹 #7).
  await userEvent.click(await screen.findByRole("button", { name: "Remove sse4.2" }));
  await userEvent.selectOptions(screen.getByLabelText("Cluster CPU baseline"), "pinned");
  await userEvent.click(screen.getByRole("button", { name: "Save CPU settings" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({
    cpu: {
      baseline: "off",
      pinned: [{ arch: "x86_64", model: "Skylake-Server-v5", flags: ["+pcid"] }],
      mask: [],
    },
  });
});

it("lets users without cluster.admin turn on only the allowed flags and says why", async () => {
  server.use(
    http.get(apiUrl("/cpu/flags"), () =>
      HttpResponse.json({
        arch: "x86_64",
        nodes: 1,
        turn_on_restricted: false,
        flags: [
          { name: "aes", supported: 1, known: 1 },
          { name: "avx512vbmi", supported: 1, known: 1, restricted: true },
          { name: "invtsc", supported: 1, known: 1, restricted: true, blocks_migration: true },
        ],
      }),
    ),
  );
  function Harness() {
    const [flags, setFlags] = useState<string[]>(["-invtsc"]);
    return (
      <>
        <CpuFlagsInput
          id="f"
          label="CPU flags"
          flags={flags}
          onChange={setFlags}
          arches={["x86_64"]}
          signed
        />
        <output>{flags.join(" ")}</output>
      </>
    );
  }
  renderPage(<Harness />);
  expect(await screen.findByText(/You can turn on only aes\./)).toBeInTheDocument();
  await waitFor(() =>
    expect(document.querySelector('#f-names option[value="+aes"]')).not.toBeNull(),
  );
  expect(document.querySelector('#f-names option[value="+avx512vbmi"]')).toBeNull();
  const input = screen.getByLabelText("CPU flags", { selector: "input" });
  await userEvent.type(input, "+avx512vbmi{Enter}");
  expect(
    screen.getByText("Only a cluster administrator can turn on avx512vbmi. You can hide any flag."),
  ).toBeInTheDocument();
  await userEvent.clear(input);
  await userEvent.type(input, "avx512vbmi{Enter}");
  await userEvent.type(input, "+aes{Enter}");
  expect(screen.getByRole("status")).toHaveTextContent("-invtsc -avx512vbmi +aes");
  // 숨긴 제한 플래그는 켜기로 바꾸지 못한다.
  expect(
    screen.getByRole("button", {
      name: "invtsc is hidden — only a cluster administrator can turn it on",
    }),
  ).toBeDisabled();
});

it("marks a turned-on flag that blocks live migration", async () => {
  server.use(
    http.get(apiUrl("/cpu/flags"), () =>
      HttpResponse.json({
        arch: "x86_64",
        nodes: 1,
        turn_on_restricted: true,
        flags: [
          { name: "invtsc", supported: 1, known: 1, restricted: true, blocks_migration: true },
        ],
      }),
    ),
  );
  renderPage(
    <CpuFlagsInput
      id="f"
      label="CPU flags"
      flags={["+invtsc"]}
      onChange={() => {}}
      arches={["x86_64"]}
      signed
    />,
  );
  expect(await screen.findByText("(blocks live migration)")).toBeInTheDocument();
  expect(screen.queryByText(/You can turn on only/)).not.toBeInTheDocument();
});

it("resets an automatically chosen cluster model when the architecture changes", async () => {
  server.use(
    http.get(apiUrl("/cpu/models"), ({ request }) => {
      const arch = new URL(request.url).searchParams.get("arch");
      return HttpResponse.json({
        arch: arch ?? "x86_64",
        nodes: 1,
        models: [
          { name: "host", kind: "basic", usable: 1 },
          { name: "max", kind: "basic", usable: 1 },
          arch === "aarch64"
            ? { name: "cluster", kind: "cluster", usable: 1 }
            : { name: "cluster", kind: "cluster", usable: 1, current: "Skylake-Server-v5" },
        ],
      });
    }),
  );
  mockLists({ node: [{ ...NODE, status: { ...NODE.status, arch: "x86_64" } }] });
  openCreate();
  const user = userEvent.setup();
  await advanced(user);
  const machine = await screen.findByLabelText("Virtual motherboard (advanced)");
  await user.selectOptions(machine, "q35");
  await waitFor(() => expect(screen.getByLabelText("CPU model")).toHaveValue("cluster"));
  // aarch64에는 기준이 없다: 효과가 고른 cluster를 host로 돌린다(리뷰 2 웹 #5).
  await user.selectOptions(machine, "virt");
  await waitFor(() => expect(screen.getByLabelText("CPU model")).toHaveValue("host"));
  // x86_64로 돌아오면 다시 고른다.
  await user.selectOptions(machine, "q35");
  await waitFor(() => expect(screen.getByLabelText("CPU model")).toHaveValue("cluster"));
});

it("clones with another CPU model only when it was changed", async () => {
  const bodies: unknown[] = [];
  const db = vm("db", "stopped", {});
  mockCpu();
  mockLists({ vm: [db], snapshot: [], "storage-pool": [], volume: [], node: [NODE] });
  server.use(
    http.post(apiUrl("/vms/:key/clone"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-clone", "db-copy"), { status: 202 });
    }),
  );
  renderPage(<VmCloneDialog vm={db} onClose={() => {}} />);
  const dialog = await screen.findByRole("dialog", { name: /db/ });
  await userEvent.type(within(dialog).getByLabelText("New VM name"), "db-copy");
  await userEvent.click(within(dialog).getByText("CPU model and flags"));
  const model = await within(dialog).findByLabelText("CPU model");
  await within(dialog).findAllByRole("option", { name: /Skylake-Server-v5/ });
  await userEvent.selectOptions(model, "Skylake-Server-v5");
  await userEvent.click(within(dialog).getByRole("button", { name: "Clone" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toEqual({
    name: "db-copy",
    snapshot: null,
    pool: null,
    cpu_model: "Skylake-Server-v5",
  });
});
