// 계획 77-5: 용도 카드가 처음 틀을 채우는 값과 놓일 노드·여유 막대(화면 없이).
import { NODE_A, NODE_B, builderCtx, perms, TEMPLATE } from "@/test/builder";
import { vm } from "@/test/fixtures";
import { applyPurpose, purposeBlocked, PURPOSES } from "@/vmBuilder/model/purposes";
import { nodeRoom, percent, targetNode } from "@/vmBuilder/model/room";
import { initialState, setName } from "@/vmBuilder/model/state";
import type { VmCheck } from "@/vmBuilder/model/requests";

const start = () => initialState("q35");

describe("use cards", () => {
  it("fill a Linux server: preset, size, a disk in the first shared pool and a NIC on the first network", () => {
    const s = setName(applyPurpose(start(), "linux", builderCtx()), "web");
    expect(s.preset).toBe("linux");
    expect(s.values).toMatchObject({
      cores: "2",
      memory: "4096",
      guestAgent: true,
      cloudInit: { enabled: true },
    });
    expect(s.values.disks).toEqual([
      expect.objectContaining({ kind: "new", pool: "ceph", sizeGib: "20", volume: "web-disk0" }),
    ]);
    expect(s.values.nics).toEqual([expect.objectContaining({ network: "lan", model: "virtio" })]);
  });

  it("give a database a separate data disk and Windows its installer devices", () => {
    const db = applyPurpose(start(), "database", builderCtx());
    expect(db.values.disks.map((d) => d.sizeGib)).toEqual(["32", "100"]);
    expect(db.values).toMatchObject({ cores: "4", memory: "16384" });
    const win = applyPurpose(start(), "windows", builderCtx());
    expect(win.values).toMatchObject({ tpm: true, secureBoot: true, cores: "4", memory: "8192" });
    expect(win.values.disks[0]).toMatchObject({ bus: "sata", sizeGib: "64" });
    expect(win.values.nics[0]).toMatchObject({ model: "e1000e" });
  });

  it("boot an ISO install from an empty CD drive first, and leave a blank VM empty", () => {
    const iso = applyPurpose(start(), "iso", builderCtx());
    expect(iso.values.cdroms).toEqual([{ iso: "", bootOrder: "1" }]);
    expect(iso.values.disks).toHaveLength(1);
    expect(iso.preset).toBe("blank");
    const blank = applyPurpose(start(), "blank", builderCtx());
    expect(blank.values.disks).toEqual([]);
    expect(blank.values.nics).toEqual([]);
  });

  it("deploy the chosen template, and use the NAT network or no disk when nothing else is usable", () => {
    const t = applyPurpose(start(), "template", builderCtx(), TEMPLATE.id);
    expect(t.base).toMatchObject({ kind: "template", id: TEMPLATE.id });
    // 풀을 쓸 권한도 네트워크도 없으면: 디스크 없이, NIC는 user(NAT)
    const bare = applyPurpose(
      start(),
      "web",
      builderCtx({ networks: [], perms: perms(["vm.create"]) }),
    );
    expect(bare.values.disks).toEqual([]);
    expect(bare.values.nics).toEqual([expect.objectContaining({ network: "user" })]);
  });

  it("are blocked without templates, or for Windows without a TPM-capable node", () => {
    const ctx = builderCtx();
    expect(PURPOSES.filter((p) => purposeBlocked(p, start(), ctx))).toEqual([]);
    expect(purposeBlocked("template", start(), builderCtx({ vms: [] }))).toEqual({
      key: "vmBuilder.purpose.noTemplates",
    });
    const noTpm = builderCtx({ nodes: [NODE_B] });
    expect(purposeBlocked("windows", start(), noTpm)?.key).toMatch(/tpm|firmware/i);
  });
});

describe("where the VM goes and the room there", () => {
  const check = (extra: Partial<VmCheck>): VmCheck => ({
    ok: true,
    problems: [],
    node: null,
    machine: null,
    cpu_model: null,
    ips: [],
    ...extra,
  });

  it("is the chosen node, else the server's pick (running or stopped), else the node with most free memory", () => {
    const ctx = builderCtx();
    const s = start();
    expect(targetNode({ ...s, values: { ...s.values, node: "n2" } }, ctx, null)).toMatchObject({
      node: { id: "n2" },
      how: "chosen",
    });
    expect(targetNode(s, ctx, check({ node: "node-b" }))).toMatchObject({
      node: { id: "n2" },
      how: "server",
    });
    expect(targetNode(s, ctx, check({ start_node: "node-b" }))).toMatchObject({
      how: "server",
    });
    // node-a: 16 GiB 중 4 할당(12 남음), node-b: 8 중 6(2 남음)
    expect(targetNode(s, ctx, null)).toMatchObject({ node: { id: "n1" }, how: "guess" });
    expect(targetNode(s, builderCtx({ nodes: null }), null)).toBeNull();
  });

  it("adds this VM to what the node already holds", () => {
    const placed = { ...vm("a", "running"), status: { ...vm("a", "running").status, node: "n1" } };
    const ctx = builderCtx({ vms: [placed, TEMPLATE] });
    const s = applyPurpose(start(), "database", ctx);
    const room = nodeRoom(s, ctx, NODE_A);
    expect(room.cpu).toEqual({ used: 2, add: 4, total: NODE_A.status.cpus });
    expect(room.memory).toEqual({ used: 4096, add: 16384, total: 16384 });
    expect(percent(room.memory!)).toBe(125);
    // 새 디스크(32 + 100 GiB, ceph)
    expect(room.disk).toMatchObject({ pool: "ceph", add: 132 * 1024 ** 3 });
  });
});
