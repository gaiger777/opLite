// 계획 77-5: 용도 카드가 처음 틀을 채우는 값과 놓일 노드·여유 막대(화면 없이).
import { CEPH, NODE_A, NODE_ARM, NODE_B, builderCtx, perms, TEMPLATE } from "@/test/builder";
import { securityGroup, vm } from "@/test/fixtures";
import { inDrawer, needsOs, problems } from "@/vmBuilder/model/problems";
import {
  applyPurpose,
  insertIso,
  noDiskReason,
  purposeBlocked,
  PURPOSES,
  webGroup,
  withImage,
} from "@/vmBuilder/model/purposes";
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
    // 설치 ISO를 넣을 빈 CD 드라이브(먼저 부팅)
    expect(win.values.cdroms).toEqual([{ iso: "", bootOrder: "1" }]);
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
    // 말없이 빼지 않는다: 검사 경고가 둘(디스크·NAT)
    expect(bare.missed).toEqual(["disk", "nic"]);
    const ctx = builderCtx({ networks: [], perms: perms(["vm.create"]) });
    expect(
      problems(bare, ctx)
        .filter((x) => /warn\.missed/.test(x.key))
        .map((x) => [x.at, x.key]),
    ).toEqual([
      ["disks", "vmBuilder.warn.missedDisk"],
      ["nics", "vmBuilder.warn.missedNic"],
    ]);
    // 카드의 크기 줄: 풀이 있으나 권한이 없음 / 풀이 하나도 없음
    expect(noDiskReason(start(), ctx)).toBe("perm");
    expect(noDiskReason(start(), builderCtx({ pools: [] }))).toBe("none");
    expect(noDiskReason(start(), builderCtx())).toBeUndefined();
  });

  it("block Windows on an Arm-only cluster with the same rule as dropping, and apply nothing then", () => {
    const arm = builderCtx({ nodes: [NODE_ARM] });
    const virt = initialState("virt");
    expect(purposeBlocked("windows", virt, arm)).toEqual({ key: "vmBuilder.purpose.windowsArm" });
    expect(purposeBlocked("linux", virt, arm)).toBeUndefined();
    // 주소로 열어도 Windows가 아닌 VM을 Windows라 부르지 않는다: 아무것도 얹지 않고 알린다.
    const s = applyPurpose(virt, "windows", arm);
    expect(s.values.disks).toEqual([]);
    expect(s.preset).toBe("blank");
    expect(s.missed).toEqual(["os"]);
    expect(problems(s, arm).map((x) => x.key)).toContain("vmBuilder.warn.missedOs");
  });

  it("give a web server a smaller size and a security group that opens 80/443", () => {
    const open = securityGroup("http");
    open.spec.rules = [{ direction: "in", action: "accept", protocol: "tcp", ports: "80" }];
    const ctx = builderCtx({ securityGroups: [securityGroup("ssh"), open] });
    expect(webGroup(ctx)).toBe("http");
    const web = applyPurpose(start(), "web", ctx);
    expect(web.values).toMatchObject({ cores: "1", memory: "2048" });
    expect(web.values.nics[0]?.securityGroups).toEqual(["http"]);
    // 그런 그룹이 없으면 걸지 않는다.
    expect(applyPurpose(start(), "web", builderCtx()).values.nics[0]?.securityGroups).toEqual([]);
  });

  it("put an ISO in a CD drive that boots first, and a Windows driver ISO in a second drive", () => {
    const ctx = builderCtx();
    const linux = applyPurpose(start(), "linux", ctx);
    expect(needsOs(linux)).toBe(true);
    const withIso = insertIso(linux, ctx, "alpine");
    expect(withIso.values.cdroms).toEqual([{ iso: "alpine", bootOrder: "1" }]);
    expect(needsOs(withIso)).toBe(false);
    expect(problems(withIso, ctx).map((x) => x.key)).not.toContain("vmBuilder.warn.noOs");
    const win = applyPurpose(start(), "windows", ctx);
    const both = insertIso(insertIso(win, ctx, "alpine"), ctx, "alpine", "drivers");
    expect(both.values.cdroms).toEqual([
      { iso: "alpine", bootOrder: "1" },
      { iso: "alpine", bootOrder: "" },
    ]);
  });

  it("start from an uploaded image: that volume as the first disk and a NIC (plan 77-8)", () => {
    const ctx = builderCtx();
    const s = withImage(start(), ctx, "spare");
    expect(s.values).toMatchObject({ cores: "2", memory: "4096" });
    expect(s.values.disks).toEqual([
      expect.objectContaining({ kind: "existing", volume: "spare" }),
    ]);
    expect(s.values.nics).toEqual([expect.objectContaining({ network: "lan" })]);
    expect(s.missed ?? []).toEqual([]);
    // 다른 VM이 쓰는 볼륨은 붙이지 않고 알린다.
    const used = withImage(start(), ctx, "used");
    expect(used.values.disks).toEqual([]);
    expect(used.missed).toContain("disk");
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

describe("fields fixed only in the advanced drawer", () => {
  it("are those the picture does not draw", () => {
    expect(
      ["tpm", "secureBoot", "schedule.keep", "replication.0.node", "maxVcpus", "haPriority"].every(
        inDrawer,
      ),
    ).toBe(true);
    expect(["disks.0.bootOrder", "nics.1.qos.rate", "cdroms.0.bootOrder"].every(inDrawer)).toBe(
      true,
    );
    expect(
      ["name", "cores", "memory", "disks.0.volume", "nics.0.mac", "cdroms", "ci.user"].some(
        inDrawer,
      ),
    ).toBe(false);
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
    // 서버가 답했는데 노드가 없으면 어림으로 떨어지지 않는다: 놓을 노드 없음(그 문제), 다른 오류 때문
    const noNode = { at: "placement", code: "no_node", level: "error" as const, message: "x" };
    expect(targetNode(s, ctx, check({ ok: false, problems: [noNode] }))).toEqual({
      node: null,
      how: "none",
      problem: noNode,
    });
    expect(targetNode(s, ctx, check({}))).toEqual({ node: null, how: "none" });
    const other = { at: "disks", code: "volume_in_use", level: "error" as const, message: "x" };
    expect(targetNode(s, ctx, check({ ok: false, problems: [other] }))).toMatchObject({
      how: "none",
      other: true,
    });
  });

  it("adds this VM to what the node already holds, counting like the server (VMs set to run there)", () => {
    const placed = { ...vm("a", "running"), status: { ...vm("a", "running").status, node: "n1" } };
    // 꺼진 VM은 서버 배치(`placed_on`)도 메모리 할당도 세지 않는다.
    const off = vm("b", "stopped");
    const ctx = builderCtx({ vms: [placed, off, TEMPLATE] });
    const s = applyPurpose(start(), "database", ctx);
    const room = nodeRoom(s, ctx, NODE_A);
    expect(room.cpu).toEqual({ used: 2, add: 4, total: NODE_A.status.cpus });
    expect(room.memory).toEqual({ used: 4096, add: 16384, total: 16384 });
    expect(percent(room.memory!)).toBe(125);
    // 새 디스크(32 + 100 GiB, ceph)
    expect(room.disk).toMatchObject({ pool: "ceph", add: 132 * 1024 ** 3, others: 0 });
    // 데이터 디스크를 다른 풀에 두면 "외 1개 풀"
    const split = {
      ...s,
      values: {
        ...s.values,
        disks: s.values.disks.map((d, i) => (i === 0 ? { ...d, pool: "local" } : d)),
      },
    };
    expect(nodeRoom(split, ctx, NODE_A).disk).toMatchObject({ pool: CEPH.name, others: 1 });
  });
});
