import { builderCtx, NODE_A, NODE_B, perms, TEMPLATE } from "@/test/builder";
import { folderId } from "@/test/fixtures";
import i18n from "@/i18n";
import { accepts, type Thing } from "@/vmBuilder/model/accepts";
import { drop, moveBlock, PACKAGES_USER_DATA, removeBlock } from "@/vmBuilder/model/drop";
import { type Part, parts } from "@/vmBuilder/model/parts";
import { areaOf, blocking, problems, worst } from "@/vmBuilder/model/problems";
import { blocks, slotOf, slots } from "@/vmBuilder/model/slots";
import {
  type BuilderCtx,
  type BuilderState,
  CARRIED_KEYS,
  DRAWN_KEYS,
  formOnly,
  fromValues,
  initialState,
  record,
  setName,
  undo,
  UNDO_LIMIT,
} from "@/vmBuilder/model/state";
import { DEFAULT_NET } from "@/vms/cloudInit";
import { emptyForm, MAX_NICS } from "@/vms/form";

const ctx = builderCtx();

function part(id: string, s: BuilderState = initialState("q35"), c: BuilderCtx = ctx): Part {
  const found = parts(s, c).find((p) => p.id === id);
  if (!found) {
    throw new Error(`no part ${id}`);
  }
  return found;
}

/** 부품 id들을 칸에 차례로 놓는다(`[부품 id, 칸]`). */
function build(steps: [string, string][], c: BuilderCtx = ctx, from = initialState("q35")) {
  return steps.reduce((s, [id, at]) => {
    const next = drop(s, part(id, s, c), at, c);
    if (next === s) {
      throw new Error(`could not drop ${id} on ${at}`);
    }
    return next;
  }, from);
}

const named = (s: BuilderState, name = "web") => setName(s, name);

describe("state", () => {
  it("starts as the form's empty VM with an empty chassis", () => {
    const s = initialState("q35", "f1");
    expect(s.values).toEqual({ ...emptyForm("q35", "f1"), disks: [], nics: [] });
    expect(s).toMatchObject({
      base: { kind: "blank" },
      zone: null,
      autoNames: [],
      preset: "blank",
    });
  });

  it("renames new disks that follow the VM name, not renamed or existing ones", () => {
    let s = build([
      ["pool:ceph", "disks"],
      ["volume:spare", "disks"],
      ["pool:ceph", "disks"],
    ]);
    s = named(s);
    expect(s.values.disks.map((d) => d.volume)).toEqual(["web-disk0", "spare", "web-disk2"]);
    s = { ...s, autoNames: [false, false, true] };
    s = named(s, "db");
    expect(s.values.disks.map((d) => d.volume)).toEqual(["web-disk0", "spare", "db-disk2"]);
  });

  it("takes over form values and guesses which disk names follow", () => {
    const v = { ...emptyForm("q35"), name: "web" };
    v.disks = [
      {
        kind: "new",
        volume: "web-disk0",
        pool: "ceph",
        sizeGib: "8",
        bus: "virtio-blk",
        bootOrder: "1",
      },
      { kind: "new", volume: "data", pool: "ceph", sizeGib: "8", bus: "virtio-blk", bootOrder: "" },
    ];
    expect(fromValues(v).autoNames).toEqual([true, false]);
  });

  it("undoes up to the limit", () => {
    let h = { past: [] as BuilderState[], present: initialState("q35") };
    const first = h.present;
    for (let i = 0; i < UNDO_LIMIT + 5; i++) {
      h = record(h, named(h.present, `vm${i}`));
    }
    expect(h.past).toHaveLength(UNDO_LIMIT);
    expect(record(h, h.present)).toBe(h);
    let back = h;
    for (let i = 0; i < UNDO_LIMIT; i++) {
      back = undo(back);
    }
    expect(back.past).toEqual([]);
    expect(back.present.values.name).toBe("vm4");
    expect(undo(back)).toBe(back);
    expect(back.present).not.toBe(first);
  });

  it("knows every form field as drawn or carried (design §13)", () => {
    const all = Object.keys(emptyForm("q35")).sort();
    expect([...DRAWN_KEYS, ...CARRIED_KEYS].sort()).toEqual(all);
    expect(DRAWN_KEYS.filter((k) => (CARRIED_KEYS as readonly string[]).includes(k))).toEqual([]);
  });

  it("lists the carried fields changed in the form", () => {
    const v = emptyForm("q35");
    expect(formOnly(v)).toEqual([]);
    expect(formOnly({ ...v, restart: "on-failure", cpuFlags: ["+aes"] })).toEqual([
      "cpuFlags",
      "restart",
    ]);
  });
});

describe("parts", () => {
  it("lists the groups in order", () => {
    const groups = [...new Set(parts(initialState("q35"), ctx).map((p) => p.group))];
    expect(groups).toEqual([
      "size",
      "os",
      "cpu",
      "disk",
      "media",
      "template",
      "network",
      "cloudInit",
      "placement",
      "protection",
      "folder",
    ]);
  });

  it("tells whether a size fits the chosen node's free memory", () => {
    const fits = (node: string) =>
      parts({ ...initialState("q35"), values: { ...initialState("q35").values, node } }, ctx)
        .filter((p) => p.kind === "size")
        .map((p) => (p.kind === "size" ? p.fit : null));
    // node-b: 8192 − 6144 = 2048 MiB 남음
    expect(fits("n2")).toEqual(["fits", "over", "over", "over"]);
    // 자동: 가장 여유 있는 node-a(12288 MiB)
    expect(fits("")).toEqual(["fits", "fits", "fits", "over"]);
    // node-a에 메모리가 더 차면 작게도 빠듯하다.
    const busy = builderCtx({
      loads: [{ ...ctx.loads![0]!, mem_allocated_mib: 16384 - 1200 }],
    });
    expect(parts(initialState("q35"), busy).find((p) => p.id === "size:s")).toMatchObject({
      fit: "tight",
    });
  });

  it("keeps basic, level and usable named CPU models", () => {
    const names = parts(initialState("q35"), ctx).flatMap((p) =>
      p.kind === "cpu" ? [p.name] : [],
    );
    expect(names).toEqual(["host", "max", "x86-64-v3"]);
  });

  it("shows unusable parts dimmed with a reason", () => {
    const s = initialState("q35");
    const viewer = builderCtx({ perms: perms(["vm.create", "system.read"]) });
    expect(part("pool:ceph", s, viewer).blocked).toEqual({
      key: "vmBuilder.reason.permPool",
      values: { name: "ceph" },
    });
    expect(part("network:lan", s, viewer).blocked?.key).toBe("vmBuilder.reason.permNetwork");
    expect(part("network:user", s, viewer).blocked).toBeUndefined();
    expect(part("zone:a", s, viewer).blocked?.key).toBe("vmBuilder.reason.permRules");
    // node-b는 TPM이 없다.
    const onB = { ...s, values: { ...s.values, node: "n2" } };
    expect(part("os:windows", onB).blocked?.key).toBe("vmForm.tpmNodeOff");
    expect(part("ha", onB).blocked?.key).toBe("vmForm.haNodeOff");
    // 고른 node-a와 템플릿 아키텍처는 같다. aarch64 노드면 다르다.
    expect(part("template:" + TEMPLATE.id, onB).blocked).toBeUndefined();
    const onArm = { ...s, values: { ...s.values, node: "n3" } };
    expect(part("template:" + TEMPLATE.id, onArm).blocked?.key).toBe("vmBuilder.reason.arch");
    const offline = builderCtx({
      nodes: [{ ...NODE_B, status: { ...NODE_B.status, online: false } }],
    });
    expect(part("node:n2", s, offline).blocked?.key).toBe("vmBuilder.reason.nodeOffline");
    expect(part("pool:local", s, offline).blocked?.key).toBe("vmBuilder.reason.poolNowhere");
  });

  it("offers free volumes only and drops the ones already placed", () => {
    const ids = (s: BuilderState) =>
      parts(s, ctx).flatMap((p) => (p.kind === "volume" ? [p.id] : []));
    expect(ids(initialState("q35"))).toEqual(["volume:spare"]);
    expect(ids(build([["volume:spare", "disks"]]))).toEqual([]);
  });

  it("offers zones by node label, the root folder and folder paths", () => {
    const all = parts(initialState("q35"), ctx);
    expect(
      all.filter((p) => p.kind === "zone").map((p) => [p.name, p.kind === "zone" && p.nodes]),
    ).toEqual([
      ["a", 1],
      ["b", 1],
    ]);
    expect(all.filter((p) => p.kind === "folder").map((p) => p.name)).toEqual([
      "/",
      "/team",
      "/team/prod",
    ]);
  });

  it("leaves out placement when nodes cannot be read", () => {
    const noNodes = builderCtx({ nodes: null });
    expect(parts(initialState(""), noNodes).some((p) => p.group === "placement")).toBe(false);
  });
});

describe("slots", () => {
  it("ids look like form error keys", () => {
    const s = build([
      ["pool:ceph", "disks"],
      ["network:lan", "nics"],
      ["network:lan", "nics"],
    ]);
    expect(slots(s).map((x) => x.id)).toEqual([
      "vm",
      "compute",
      "placement",
      "protection",
      "cloudInit",
      "disks",
      "disks.0",
      "cdroms",
      "nics",
      "nics.0",
      "nics.1",
      "folder",
      "trash",
    ]);
    expect(slotOf("nics.1")).toEqual({ id: "nics.1", kind: "nic", index: 1 });
    expect(slotOf("nics.x")).toBeNull();
    expect(slotOf("disks.0.pool")).toBeNull();
    expect(slotOf("nope")).toBeNull();
    expect(blocks(s).map((b) => b.id)).toEqual(["block:disks.0", "block:nics.0", "block:nics.1"]);
  });
});

describe("accepts", () => {
  const base = build([
    ["pool:ceph", "disks"],
    ["volume:spare", "disks"],
    ["network:lan", "nics"],
    ["network:user", "nics"],
    ["network:tenant", "nics"],
  ]);
  const verdict = (id: string, at: string, s = base, c = ctx) => {
    const v = accepts(part(id, s, c), at, s, c);
    return v.ok ? (v.warn ? `warn:${v.warn.key}` : "ok") : v.reason ? `no:${v.reason.key}` : "no";
  };
  it.each([
    ["size:m", "compute", "ok"],
    ["size:m", "disks", "no"],
    ["cpu:x86-64-v3", "compute", "ok"],
    ["os:windows", "vm", "ok"],
    ["pool:ceph", "disks", "ok"],
    ["pool:local", "disks.0", "ok"],
    // 있는 볼륨에는 풀을 바꿔 놓지 않는다.
    ["pool:ceph", "disks.1", "no"],
    ["pool:ceph", "nics", "no"],
    ["iso:alpine", "cdroms", "ok"],
    ["network:lan", "nics", "ok"],
    ["network:lan", "nics.1", "ok"],
    ["sg:web", "nics.0", "ok"],
    ["sg:web", "nics.1", "no:vmBuilder.reason.userNoSg"],
    ["sg:web", "nics", "no"],
    ["addr:static", "nics.0", "ok"],
    ["addr:static", "nics.2", "no:vmBuilder.reason.sdnStatic"],
    ["addr:dhcp", "nics.2", "ok"],
    ["ci:user", "cloudInit", "ok"],
    ["auto", "placement", "ok"],
    ["node:n1", "placement", "ok"],
    ["zone:a", "placement", "ok"],
    ["ha", "protection", "ok"],
    ["haGroup:pair", "protection", "ok"],
    [`folder:${folderId("team")}`, "folder", "ok"],
    [`template:${TEMPLATE.id}`, "vm", "ok"],
  ])("%s on %s → %s", (id, at, expected) => {
    expect(verdict(id, at)).toBe(expected);
  });

  it("blocks by count", () => {
    let s = base;
    while (s.values.nics.length < MAX_NICS) {
      s = drop(s, part("network:lan", s), "nics", ctx);
    }
    expect(verdict("network:lan", "nics", s)).toBe("no:vmBuilder.reason.maxNics");
    expect(verdict("network:lan", "nics.0", s)).toBe("ok");
    s = build(Array.from({ length: 4 }, () => ["iso:alpine", "cdroms"] as [string, string]));
    expect(verdict("iso:alpine", "cdroms", s)).toBe("no:vmBuilder.reason.maxCdroms");
    s = build(
      [
        ["sg:web", "nics.0"],
        ["sg:ssh", "nics.0"],
      ],
      ctx,
      base,
    );
    expect(s.values.nics[0]!.securityGroups).toEqual(["web", "ssh"]);
    const many = builderCtx({
      securityGroups: ["a", "b", "c", "d", "e", "f", "g", "h", "i"].map((n) => ({
        ...ctx.securityGroups[0]!,
        name: n,
      })),
    });
    let full = base;
    for (const n of ["a", "b", "c", "d", "e", "f", "g", "h"]) {
      full = drop(full, part(`sg:${n}`, full, many), "nics.0", many);
    }
    expect(full.values.nics[0]!.securityGroups).toHaveLength(8);
    expect(verdict("sg:i", "nics.0", full, many)).toBe("no:vmBuilder.reason.maxSgs");
    // 이미 건 그룹은 다시 놓아도 된다(그대로).
    expect(verdict("sg:a", "nics.0", full, many)).toBe("ok");
  });

  it("blocks SATA ports of a Windows VM on q35", () => {
    let s = build([["os:windows", "vm"]]);
    for (let i = 0; i < 3; i++) {
      s = drop(s, part("pool:ceph", s), "disks", ctx);
    }
    s = build(
      [
        ["iso:alpine", "cdroms"],
        ["iso:alpine", "cdroms"],
      ],
      ctx,
      s,
    );
    // SATA 디스크 3 + CD 2 = 5: cloud-init 씨앗이 들어가면 6
    expect(verdict("ci:user", "cloudInit", s)).toBe("ok");
    expect(verdict("iso:alpine", "cdroms", s)).toBe("ok");
    s = drop(s, part("iso:alpine", s), "cdroms", ctx);
    expect(verdict("ci:user", "cloudInit", s)).toBe("no:vmBuilder.reason.sataPorts");
    expect(verdict("pool:ceph", "disks", s)).toBe("no:vmBuilder.reason.sataPorts");
  });

  it("blocks by place: the chosen node", () => {
    const onB = build([["node:n2", "placement"]], ctx, base);
    expect(verdict("pool:local", "disks", onB)).toBe("no:vmBuilder.reason.poolNotOnNode");
    expect(verdict("cpu:x86-64-v3", "compute", onB)).toBe("no:vmBuilder.reason.cpuCannotRun");
    expect(verdict("ha", "protection", onB)).toBe("no:vmForm.haNodeOff");
    const ha = build([["ha", "protection"]], ctx, base);
    expect(verdict("node:n2", "placement", ha)).toBe("no:vmForm.haNodeOff");
    expect(verdict("iso:local-iso", "cdroms", ha)).toBe("no:vmBuilder.reason.isoLocalHa");
    const withLocal = build([["iso:local-iso", "cdroms"]], ctx, base);
    expect(verdict("ha", "protection", withLocal)).toBe("no:vmBuilder.reason.isoLocalHa");
    const win = build([["os:windows", "vm"]]);
    expect(verdict("node:n3", "placement", win)).toBe("no:vmBuilder.reason.arch");
    expect(verdict("node:n2", "placement", win)).toBe("no:vmForm.tpmNodeOff");
    expect(verdict("os:windows", "vm", build([["node:n3", "placement"]]))).toBe(
      "no:vmBuilder.reason.windowsArch",
    );
  });

  it("blocks a node that hard rules keep the VM off", () => {
    const c = builderCtx({
      rules: [
        {
          ...ctx.vms[0]!,
          name: "no-a",
          spec: { kind: "node-anti-affinity", vms: ["web"], nodes: ["node-a"], enabled: true },
          status: {},
        } as never,
      ],
    });
    const s = named(initialState("q35"));
    expect(verdict("node:n1", "placement", s, c)).toBe("no:vmBuilder.reason.hardRule");
    expect(verdict("node:n2", "placement", s, c)).toBe("ok");
  });

  it("warns, but lets go, when capacity looks short", () => {
    const big = { ...base, values: { ...base.values, memory: "4096" } };
    expect(verdict("node:n2", "placement", big)).toBe("warn:vmBuilder.warn.nodeMemory");
    const onB = build([["node:n2", "placement"]]);
    expect(verdict("size:xl", "compute", onB)).toBe("warn:vmBuilder.warn.sizeMemory");
    const tiny = builderCtx({
      nodes: [
        {
          ...NODE_A,
          status: {
            ...NODE_A.status,
            pools: {
              ...NODE_A.status.pools,
              local: { ...NODE_A.status.pools.local!, available_bytes: 1024 ** 3 },
            },
          },
        },
      ],
    });
    expect(verdict("pool:local", "disks", initialState("q35"), tiny)).toBe(
      "warn:vmBuilder.warn.poolFull",
    );
    const quota = builderCtx({
      usage: {
        folder: "f",
        path: "/team",
        binding: {},
        usage: { vms: 1, vcpus: 7, memory_mib: 1024, disk_bytes: 0 },
        limits: [
          {
            folder: "f",
            path: "/team",
            quota: { vcpus: 8 },
            usage: { vms: 1, vcpus: 7, memory_mib: 1024, disk_bytes: 0 },
          },
        ],
      },
    });
    expect(verdict("size:m", "compute", initialState("q35"), quota)).toBe(
      "warn:vmBuilder.warn.quotaVcpus",
    );
    expect(verdict("size:s", "compute", initialState("q35"), quota)).toBe("ok");
  });

  it("keeps the devices of a template deployment", () => {
    const t = build([[`template:${TEMPLATE.id}`, "vm"]]);
    expect(verdict("pool:ceph", "disks", t)).toBe("no:vmBuilder.reason.templateFixed");
    expect(verdict("network:lan", "nics", t)).toBe("no:vmBuilder.reason.templateFixed");
    expect(verdict("network:tenant", "nics.0", t)).toBe("ok");
    expect(verdict("iso:alpine", "cdroms", t)).toBe("no:vmBuilder.reason.templateFixed");
    expect(verdict("os:linux", "vm", t)).toBe("no:vmBuilder.reason.templateFixed");
    expect(verdict("size:l", "compute", t)).toBe("ok");
    const block: Thing = { kind: "block", id: "block:disks.0", list: "disks", index: 0 };
    expect(accepts(block, "trash", t, ctx)).toEqual({
      ok: false,
      reason: { key: "vmBuilder.reason.templateFixed", values: undefined },
    });
    // 배포 요청은 HA를 싣지 못한다.
    expect(verdict("ha", "protection", t)).toBe("no:vmBuilder.reason.templateHa");
  });

  it("refuses a held block whose place is gone (undo while held)", () => {
    const s = build([
      ["pool:ceph", "disks"],
      ["pool:ceph", "disks"],
    ]);
    const gone: Thing = { kind: "block", id: "block:disks.2", list: "disks", index: 2 };
    expect(accepts(gone, "disks.0", s, ctx).ok).toBe(false);
    expect(accepts(gone, "disks", s, ctx).ok).toBe(false);
    expect(accepts(gone, "trash", s, ctx).ok).toBe(false);
    expect(drop(s, gone, "disks.0", ctx)).toBe(s);
  });

  it("moves blocks within their own list and to the trash", () => {
    const block = (list: "disks" | "nics", index: number): Thing => ({
      kind: "block",
      id: `block:${list}.${index}`,
      list,
      index,
    });
    expect(accepts(block("disks", 0), "disks.1", base, ctx).ok).toBe(true);
    expect(accepts(block("disks", 0), "disks", base, ctx).ok).toBe(true);
    expect(accepts(block("disks", 1), "disks", base, ctx).ok).toBe(false);
    expect(accepts(block("disks", 0), "disks.0", base, ctx).ok).toBe(false);
    expect(accepts(block("disks", 0), "nics.0", base, ctx).ok).toBe(false);
    expect(accepts(block("nics", 2), "trash", base, ctx).ok).toBe(true);
    expect(accepts(block("nics", 2), "compute", base, ctx).ok).toBe(false);
  });
});

describe("drop", () => {
  it("applies sizes, OS presets and CPU models", () => {
    let s = build([["size:l", "compute"]]);
    expect(s.values).toMatchObject({ sockets: 1, cores: "4", memory: "8192" });
    s = build(
      [
        ["network:lan", "nics"],
        ["pool:ceph", "disks"],
        ["os:windows", "vm"],
      ],
      ctx,
      s,
    );
    expect(s.preset).toBe("windows");
    expect(s.values).toMatchObject({ tpm: true, secureBoot: true, machine: "q35", memory: "4096" });
    expect(s.values.disks[0]!.bus).toBe("sata");
    expect(s.values.nics[0]!.model).toBe("e1000e");
    // Windows에서 더한 장치도 Windows 장치
    s = build([["network:lan", "nics"]], ctx, s);
    expect(s.values.nics[1]!.model).toBe("e1000e");
    // Linux로 바꾸면 장치를 돌리고 cloud-init과 게스트 에이전트를 켠다.
    s = build([["os:linux", "vm"]], ctx, s);
    expect(s.values.disks[0]!.bus).toBe("virtio-blk");
    expect(s.values.nics.map((n) => n.model)).toEqual(["virtio", "virtio"]);
    expect(s.values).toMatchObject({ guestAgent: true, cloudInit: { enabled: true } });
    expect(build([["cpu:x86-64-v3", "compute"]]).values.model).toBe("x86-64-v3");
  });

  it("adds new disks named after the VM and changes their pool", () => {
    let s = named(build([["pool:ceph", "disks"]]));
    expect(s.values.disks[0]).toEqual({
      kind: "new",
      volume: "web-disk0",
      pool: "ceph",
      sizeGib: "20",
      bus: "virtio-blk",
      bootOrder: "",
    });
    s = build([["pool:local", "disks.0"]], ctx, s);
    expect(s.values.disks[0]!.pool).toBe("local");
    s = build([["volume:spare", "disks"]], ctx, s);
    expect(s.values.disks[1]).toMatchObject({ kind: "existing", volume: "spare" });
    expect(s.autoNames).toEqual([true, false]);
  });

  it("adds CD drives and swaps their medium", () => {
    let s = build([["iso:alpine", "cdroms"]]);
    expect(s.values.cdroms).toEqual([{ iso: "alpine", bootOrder: "" }]);
    s = build([["iso:local-iso", "cdroms.0"]], ctx, s);
    expect(s.values.cdroms).toEqual([{ iso: "local-iso", bootOrder: "" }]);
  });

  it("adds NICs, changes their network and clears the old addresses", () => {
    let s = build([
      ["network:tenant", "nics"],
      ["sg:web", "nics.0"],
      ["sg:web", "nics.0"],
      ["addr:dhcp", "nics.0"],
    ]);
    s = { ...s, values: { ...s.values, nics: [{ ...s.values.nics[0]!, ips: "10.10.0.5" }] } };
    expect(s.values.nics[0]!.securityGroups).toEqual(["web"]);
    expect(s.values.cloudInit.networks).toEqual([DEFAULT_NET]);
    s = build([["network:lan", "nics.0"]], ctx, s);
    expect(s.values.nics[0]).toMatchObject({ network: "lan", ips: "", securityGroups: ["web"] });
    s = build([["addr:static", "nics.0"]], ctx, s);
    expect(s.values.cloudInit.networks[0]!.ipv4).toBe("static");
    s = build([["network:user", "nics.0"]], ctx, s);
    expect(s.values.nics[0]!.securityGroups).toEqual([]);
    expect(s.values.cloudInit.networks[0]).toEqual(DEFAULT_NET);
  });

  it("turns cloud-init on with snippets and adds the guest agent package once", () => {
    let s = build([["ci:packages", "cloudInit"]]);
    expect(s.values.cloudInit).toMatchObject({ enabled: true, userData: PACKAGES_USER_DATA });
    expect(s.values.guestAgent).toBe(true);
    expect(accepts(part("ci:packages", s), "cloudInit", s, ctx)).toEqual({
      ok: true,
      warn: { key: "vmBuilder.warn.userData", values: undefined },
    });
    s = build([["ci:packages", "cloudInit"]], ctx, s);
    expect(s.values.cloudInit.userData).toBe(PACKAGES_USER_DATA);
  });

  it("places on a node, a zone or anywhere", () => {
    let s = build([["node:n3", "placement"]], ctx, initialState("q35"));
    // 머신은 노드 아키텍처를 따른다.
    expect(s.values).toMatchObject({ node: "n3", machine: "virt" });
    s = build([["zone:b", "placement"]], ctx, s);
    expect(s.values.node).toBe("");
    expect(s.zone).toEqual({ key: "zone", value: "b", hard: true });
    s = build([["node:n1", "placement"]], ctx, s);
    expect(s.zone).toBeNull();
    s = { ...s, values: { ...s.values, pinned: true } };
    s = build([["auto", "placement"]], ctx, s);
    expect(s.values).toMatchObject({ node: "", pinned: false });
  });

  it("turns HA on, sets a group and a folder", () => {
    const s = build([
      ["ha", "protection"],
      ["haGroup:pair", "protection"],
      [`folder:${folderId("prod")}`, "folder"],
    ]);
    expect(s.values).toMatchObject({
      ha: true,
      haState: "started",
      haGroup: "pair",
      folder: folderId("prod"),
    });
  });

  it("switches to a template deployment with the template's devices", () => {
    const s = named(
      build([
        ["pool:ceph", "disks"],
        [`template:${TEMPLATE.id}`, "vm"],
      ]),
    );
    expect(s.base).toEqual({ kind: "template", id: TEMPLATE.id, linked: null, pool: "" });
    expect(s.values.disks).toEqual([
      {
        kind: "existing",
        volume: "tmpl-disk0",
        pool: "",
        sizeGib: "",
        bus: "virtio-blk",
        bootOrder: "",
      },
    ]);
    expect(s.values.nics.map((n) => n.network)).toEqual(["lan"]);
    expect(s.values.machine).toBe("q35");
    // 템플릿 디스크 이름은 VM 이름을 따르지 않는다.
    expect(s.values.disks[0]!.volume).toBe("tmpl-disk0");
  });

  it("leaves the state alone when the drop is refused", () => {
    const s = initialState("q35");
    expect(drop(s, part("pool:ceph"), "nics", ctx)).toBe(s);
  });

  it("reorders and removes blocks, with NIC addresses following their NIC", () => {
    let s = named(
      build([
        ["pool:ceph", "disks"],
        ["volume:spare", "disks"],
        ["pool:local", "disks"],
        ["network:lan", "nics"],
        ["network:user", "nics"],
        ["addr:static", "nics.0"],
      ]),
    );
    s = drop(s, { kind: "block", id: "block:disks.2", list: "disks", index: 2 }, "disks.0", ctx);
    expect(s.values.disks.map((d) => d.volume)).toEqual(["web-disk0", "web-disk1", "spare"]);
    expect(s.values.disks.map((d) => d.pool)).toEqual(["local", "ceph", ""]);
    s = moveBlock(s, "nics", 0, 1);
    expect(s.values.nics.map((n) => n.network)).toEqual(["user", "lan"]);
    expect(s.values.cloudInit.networks.map((n) => n.ipv4)).toEqual(["dhcp", "static"]);
    s = drop(s, { kind: "block", id: "block:nics.0", list: "nics", index: 0 }, "trash", ctx);
    expect(s.values.nics.map((n) => n.network)).toEqual(["lan"]);
    expect(s.values.cloudInit.networks.map((n) => n.ipv4)).toEqual(["static"]);
    s = removeBlock(s, "disks", 0);
    expect(s.values.disks.map((d) => d.volume)).toEqual(["web-disk0", "spare"]);
    expect(s.autoNames).toEqual([true, false]);
    expect(moveBlock(s, "disks", 0, 5)).toBe(s);
    expect(moveBlock(s, "disks", 5, 0)).toBe(s);
    expect(moveBlock(s, "disks", -1, 0)).toBe(s);
  });

  it("turns off what a template deployment cannot carry (HA, pinning, the agent flag)", () => {
    let s = build([["ha", "protection"]]);
    s = { ...s, values: { ...s.values, pinned: true, guestAgent: false } };
    s = build([[`template:${TEMPLATE.id}`, "vm"]], ctx, s);
    expect(s.values).toMatchObject({ ha: false, haGroup: "", pinned: false });
    s = build([["ci:packages", "cloudInit"]], ctx, s);
    expect(s.values.cloudInit.userData).toBe(PACKAGES_USER_DATA);
    expect(s.values.guestAgent).toBe(false);
  });
});

describe("problems", () => {
  it.each([
    ["name", "name"],
    ["cores", "compute"],
    ["maxMemory", "compute"],
    ["disks.0.size", "disks.0"],
    ["disks.12.pool", "disks.12"],
    ["cdroms", "cdroms"],
    ["cdroms.1.bootOrder", "cdroms.1"],
    ["nics.1.securityGroups", "nics.1"],
    ["nics.0.qos.out", "nics.0"],
    ["ci.networks.0.ipv4Address", "cloudInit"],
    ["ci.user", "cloudInit"],
    ["tpm", "vm"],
    ["secureBoot", "vm"],
    ["schedule.keep", "vm"],
    ["replication.0.node", "vm"],
    ["haPriority", "protection"],
    ["guestWatchSecs", "protection"],
  ])("maps the form error %s to %s", (key, at) => {
    expect(areaOf(key)).toBe(at);
  });

  it("starts with a name error and a boot warning", () => {
    const list = problems(initialState("q35"), ctx);
    expect(list).toEqual([
      { at: "name", key: "vmForm.errors.name", level: "error", source: "browser" },
      { at: "disks", key: "vmBuilder.warn.noBoot", level: "warn", source: "browser" },
    ]);
    expect(blocking(list)?.at).toBe("name");
  });

  it("finds a taken name and what changed in the world after the drop", () => {
    let s = named(
      build([
        ["pool:local", "disks"],
        ["network:lan", "nics"],
        ["sg:web", "nics.0"],
      ]),
      "taken",
    );
    const gone = builderCtx({ pools: [], networks: [], securityGroups: [] });
    const list = problems(s, gone);
    expect(list.map((p) => [p.at, p.key])).toEqual([
      ["name", "vmBuilder.problem.nameTaken"],
      ["disks.0", "vmBuilder.reason.gone"],
      ["nics.0", "vmBuilder.reason.gone"],
      ["nics.0", "vmBuilder.reason.gone"],
    ]);
    expect(worst(list, "nics")).toBe("error");
    expect(worst(list, "compute")).toBeNull();
    // 노드를 고른 뒤 그 노드가 유지보수에 들어갔다.
    s = build([["node:n1", "placement"]], ctx, named(s));
    const maint = builderCtx({
      nodes: [{ ...NODE_A, spec: { ...NODE_A.spec, maintenance: true } }, NODE_B],
    });
    expect(problems(s, maint).map((p) => p.key)).toContain("vmBuilder.reason.nodeMaintenance");
  });

  it("checks the grow-only sizes of a template deployment", () => {
    let s = named(build([[`template:${TEMPLATE.id}`, "vm"]]));
    expect(problems(s, ctx).filter((p) => p.level === "error")).toEqual([]);
    s = { ...s, values: { ...s.values, disks: [{ ...s.values.disks[0]!, sizeGib: "8" }] } };
    expect(problems(s, ctx).map((p) => [p.at, p.key])).toEqual([
      ["disks.0", "volumes.errors.grow"],
    ]);
    s = { ...s, values: { ...s.values, disks: [{ ...s.values.disks[0]!, sizeGib: "x" }] } };
    expect(problems(s, ctx).map((p) => p.key)).toEqual(["vmForm.errors.size"]);
  });

  it("adds server problems and drops duplicates", () => {
    const s = named(build([["pool:ceph", "disks"]]));
    const list = problems(s, ctx, {
      ok: false,
      problems: [
        { at: "quota", code: "quota_exceeded", message: "over", level: "error" },
        { at: "disks.0", code: "pool_space", message: "tight", level: "warn" },
        { at: "quota", code: "quota_exceeded", message: "again", level: "error" },
      ],
      ips: [],
    });
    expect(list.filter((p) => p.source === "server")).toEqual([
      { at: "quota", key: "quota_exceeded", message: "over", level: "error", source: "server" },
      { at: "disks.0", key: "pool_space", message: "tight", level: "warn", source: "server" },
    ]);
    expect(worst(list, "disks")).toBe("warn");
  });
});

// 성질 시험: 아무 부품을 아무 칸에 차례로 놓아도 — 놓을 수 없는 놓기는 상태를 그대로 두고, 놓은 뒤 상태는 놓기 규칙을
// 어기지 않으며(개수 상한), 문제의 자리는 늘 있는 칸이고, 되돌리기는 앞 상태를 그대로 돌려준다.
describe("properties over random drops", () => {
  function rng(seed: number) {
    let x = seed;
    return () => {
      x = (x * 1103515245 + 12345) % 2 ** 31;
      return x / 2 ** 31;
    };
  }
  const SPECIAL = new Set(["name", "quota", "spec", "vm"]);

  it.each(Array.from({ length: 60 }, (_, i) => i + 1))("seed %i", (seed) => {
    const r = rng(seed);
    let h = { past: [] as BuilderState[], present: named(initialState("q35")) };
    for (let i = 0; i < 40; i++) {
      const s = h.present;
      const things: Thing[] = [...parts(s, ctx), ...blocks(s)];
      const thing = things[Math.floor(r() * things.length)]!;
      const ids = slots(s).map((x) => x.id);
      const at = ids[Math.floor(r() * ids.length)]!;
      const ok = accepts(thing, at, s, ctx).ok;
      const next = drop(s, thing, at, ctx);
      if (!ok) {
        expect(next).toBe(s);
        continue;
      }
      expect(next.values.nics.length).toBeLessThanOrEqual(MAX_NICS);
      expect(next.values.cdroms.length).toBeLessThanOrEqual(4);
      expect(next.autoNames.length).toBeLessThanOrEqual(Math.max(next.values.disks.length, 0) || 0);
      const known = new Set(slots(next).map((x) => x.id));
      for (const p of problems(next, ctx)) {
        expect(known.has(p.at) || SPECIAL.has(p.at)).toBe(true);
      }
      const before = h;
      h = record(h, next);
      expect(undo(h).present).toBe(before.present);
    }
  });
});

it("has every reason and warning key in both languages", async () => {
  const keys = new Set<string>();
  const s = initialState("q35");
  for (const p of parts(s, ctx)) {
    if (p.blocked) {
      keys.add(p.blocked.key);
    }
  }
  const src = import.meta.glob("./*.ts", { query: "?raw", import: "default", eager: true });
  for (const text of Object.values(src) as string[]) {
    for (const m of text.matchAll(/(?:no|warn)\("([a-zA-Z]+)"/g)) {
      keys.add(`vmBuilder.${text.includes(`no("${m[1]}"`) ? "reason" : "warn"}.${m[1]}`);
    }
    for (const m of text.matchAll(/"(vmBuilder\.[a-zA-Z.]+[a-zA-Z])"/g)) {
      keys.add(m[1]!);
    }
  }
  for (const lng of ["en", "ko"]) {
    for (const key of keys) {
      expect(i18n.exists(key, { lng }), `${lng} ${key}`).toBe(true);
    }
  }
});
