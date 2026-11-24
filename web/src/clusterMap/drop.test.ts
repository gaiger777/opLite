// 지도 끌어 옮기기의 놓을 자리 판정(계획 71 설계 §10.2 표 — 줄마다 하나, 위에서부터 첫 것).
import type { Node, Task } from "@/api/types";
import { type DropCtx, dragBlock, dropCheck, toVerdict } from "@/clusterMap/drop";
import { levelOf } from "@/clusterMap/MapScene";
import { mapNode, mapVm } from "@/test/clusterMap";
import { haGroup, placementRule } from "@/test/fixtures";

const node = (n: number, name: string, extra: Parameters<typeof mapNode>[2] = {}): Node =>
  mapNode(n, name, {
    ...extra,
    status: { arch: "aarch64", ...extra.status },
  });

const N1 = node(1, "n1");
const N2 = node(2, "n2");
const WEB = mapVm(1, "web", N1.id, "running");
const CTX: DropCtx = { vms: [WEB], mayForce: false };

const key = (c: ReturnType<typeof dropCheck>) => [c.level, c.reason?.key.split(".").pop()];

it("blocks the VM's own node, then each node state migrationTargets rules out", () => {
  expect(key(dropCheck(WEB, N1, CTX))).toEqual(["blocked", "here"]);
  expect(key(dropCheck(WEB, node(3, "w", { spec: { witness: true } }), CTX))).toEqual([
    "blocked",
    "witness",
  ]);
  expect(key(dropCheck(WEB, node(3, "n3", { status: { online: false } }), CTX))).toEqual([
    "blocked",
    "offline",
  ]);
  const frozen = node(3, "n3", {
    status: { online: false, shutdown: { action: "freeze" } as never },
  });
  expect(key(dropCheck(WEB, frozen, CTX))).toEqual(["blocked", "frozen"]);
  expect(key(dropCheck(WEB, node(3, "n3", { spec: { maintenance: true } }), CTX))).toEqual([
    "blocked",
    "maintenance",
  ]);
  const x86 = dropCheck(WEB, node(3, "n3", { status: { arch: "x86_64" } }), CTX);
  expect(key(x86)).toEqual(["blocked", "arch"]);
  expect(x86.reason?.values).toEqual({ node: "n3", arch: "x86_64" });
});

it("blocks a node that can't run the VM's CPU or NUMA, and skips those rows until they are read", () => {
  expect(
    key(
      dropCheck(WEB, N2, {
        ...CTX,
        cpu: [{ node: "n2", can_run: false, reason: "missing avx2" }],
      }),
    ),
  ).toEqual(["blocked", "cpu"]);
  expect(
    dropCheck(WEB, N2, { ...CTX, cpu: [{ node: "n2", can_run: false, missing: ["avx2", "aes"] }] })
      .reason?.values,
  ).toEqual({ node: "n2", why: "avx2, aes" });
  expect(
    key(
      dropCheck(WEB, N2, { ...CTX, numa: [{ node: "n2", fits: false, reason: "no hugepages" }] }),
    ),
  ).toEqual(["blocked", "numa"]);
  expect(key(dropCheck(WEB, N2, { ...CTX, cpu: [{ node: "n2", can_run: true }] }))).toEqual([
    "ok",
    undefined,
  ]);
  expect(key(dropCheck(WEB, N2, CTX))).toEqual(["ok", undefined]);
});

it("blocks a hard placement rule for operators and warns admins who may override it", () => {
  const rule = placementRule("pin", 1, { kind: "node-affinity", vms: ["web"], nodes: ["n1"] });
  expect(key(dropCheck(WEB, N2, { ...CTX, rules: [rule] }))).toEqual(["blocked", "rules"]);
  const admin = dropCheck(WEB, N2, { ...CTX, rules: [rule], mayForce: true });
  expect(key(admin)).toEqual(["warn", "rulesForce"]);
  expect(admin.reason?.values).toEqual({ rules: "pin" });
  // 제한 HA 그룹도 서버처럼 규칙으로 본다
  const ha = mapVm(1, "web", N1.id, "running", { spec: { ha: { group: "g" } as never } });
  const group = haGroup("g", 1, { restricted: true, nodes: [{ node: "n1", priority: 1 }] });
  expect(key(dropCheck(ha, N2, { ...CTX, rules: [], groups: [group] }))).toEqual([
    "blocked",
    "rules",
  ]);
});

it("warns about little free memory, then about local disks to copy", () => {
  const tight = dropCheck(WEB, N2, { ...CTX, freeMib: new Map([[N2.id, 1024]]) });
  expect(key(tight)).toEqual(["warn", "memory"]);
  expect(tight.reason?.values).toEqual({ free: "1 GiB", need: "2 GiB" });
  expect(key(dropCheck(WEB, N2, { ...CTX, freeMib: new Map([[N2.id, 4096]]) }))).toEqual([
    "ok",
    undefined,
  ]);
  const copy = dropCheck(WEB, N2, { ...CTX, copyBytes: 16 * 1024 ** 3 });
  expect(key(copy)).toEqual(["warn", "copy"]);
  expect(copy.reason?.values).toEqual({ size: "16 GiB" });
});

it("maps checks to the drag kit's verdicts and highlight levels", () => {
  const reason = { key: "k" };
  expect(toVerdict({ level: "ok" })).toEqual({ ok: true, warn: undefined });
  expect(toVerdict({ level: "warn", reason })).toEqual({ ok: true, warn: reason });
  expect(toVerdict({ level: "blocked", reason })).toEqual({ ok: false, reason });
  expect(levelOf({ ok: true })).toBe("ok");
  expect(levelOf({ ok: true, warn: reason })).toBe("warn");
  expect(levelOf({ ok: false })).toBe("blocked");
});

it("lets only running VMs the user may migrate be dragged", () => {
  expect(dragBlock(WEB, [], true)).toBeNull();
  expect(dragBlock(mapVm(2, "off", N1.id, "stopped"), [], true)?.key).toBe(
    "clusterMap.drag.stopped",
  );
  expect(dragBlock(WEB, [], false)).toEqual({
    key: "clusterMap.drag.denied",
    values: { vm: "web" },
  });
  const moving = mapVm(3, "mig", N1.id, "migrating", {
    spec: { power: "running" },
    status: { node: N1.id, migration: { node: N2.id, task: "t" } },
  });
  expect(dragBlock(moving, [], true)?.key).toBe("vms.reason.migrating");
  const pending = [
    {
      id: "t",
      spec: { op: { kind: "vm-start" }, target: WEB.id, node: N1.id },
      status: { state: "running", error: null },
    } as Task,
  ];
  expect(dragBlock(WEB, pending, true)?.key).toBe("vms.reason.powerPending");
});
