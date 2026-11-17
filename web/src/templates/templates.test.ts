import type { Vm, Volume } from "@/api/types";
import {
  convertReason,
  deployNames,
  linkableDisks,
  poolSupportsLinkedClones,
  revertReason,
  templateNode,
  templateRows,
} from "@/templates/templates";
import { NODE, pool, poolState, snapshot, vm, volume } from "@/test/fixtures";

function withDisks(v: Vm, disks: string[], template = false): Vm {
  return {
    ...v,
    spec: {
      ...v.spec,
      template,
      disks: disks.map((d) => ({ volume: d, bus: "virtio-blk" as const })),
    },
  };
}

function cloneOf(name: string, base: Volume): Volume {
  const v = volume(name, base.spec.pool);
  return { ...v, spec: { ...v.spec, base: base.id } };
}

const root = volume("tmpl-root", "local");
const data = {
  ...volume("tmpl-data", "fast"),
  spec: { ...volume("x", "fast").spec, pool: "fast" },
};

it("lists templates with disks, pools and linked clones", () => {
  const tmpl = withDisks(vm("tmpl", "stopped"), ["tmpl-root", "tmpl-data"], true);
  const volumes = [root, data, cloneOf("web-1-root", root), cloneOf("web-2-root", root)];
  const rows = templateRows([vm("web-1", "running"), tmpl], volumes);
  expect(rows).toHaveLength(1);
  expect(rows[0]).toMatchObject({
    disks: ["tmpl-root", "tmpl-data"],
    pools: ["local", "fast"],
  });
  expect(rows[0]!.clones.map((c) => c.name)).toEqual(["web-1-root", "web-2-root"]);
  expect(revertReason(tmpl, volumes)).toBe("templates.reason.inUse");
  expect(revertReason(tmpl, [root, data])).toBeNull();
  expect(revertReason(tmpl, undefined)).toBe("templates.reason.clonesUnknown");
});

it("knows which pools hold linked clones like the server", () => {
  expect(poolSupportsLinkedClones(pool("local"), "qcow2")).toBe(true);
  const zfs = pool("z", { backend: { kind: "zfs", dataset: "tank/vm" } });
  expect(poolSupportsLinkedClones(zfs, "raw")).toBe(true);
  expect(poolSupportsLinkedClones(zfs, "qcow2")).toBe(false);
  const san = pool("san", { backend: { kind: "lvm-shared", vg: "vg0" } });
  expect(poolSupportsLinkedClones(san, "raw")).toBe(false);
  // 다른 풀에는 연결 복제를 둘 수 없다.
  const pools = [pool("local"), san];
  expect(linkableDisks([root, undefined], pools, "")).toEqual([true, false]);
  expect(linkableDisks([root], pools, "san")).toEqual([false]);
  expect(linkableDisks([root], pools, "local")).toEqual([true]);
});

it("makes deploy names from a pattern", () => {
  expect(deployNames("web-{n}", 3, 1)).toEqual({ names: ["web-1", "web-2", "web-3"] });
  expect(deployNames("ci-{n}-x", 2, 9)).toEqual({ names: ["ci-9-x", "ci-10-x"] });
  // 하나면 `{n}`은 첫 번호
  expect(deployNames("web-{n}", 1, 5)).toEqual({ names: ["web-5"] });
  expect(deployNames("web", 1, 1)).toEqual({ names: ["web"] });
  expect(deployNames("web", 2, 1)).toEqual({ error: "templates.errors.pattern" });
  expect(deployNames("web-{n}", 0, 1)).toEqual({ error: "templates.errors.count" });
  expect(deployNames("web-{n}", 101, 1)).toEqual({ error: "templates.errors.count" });
  expect(deployNames("web-{n}", 2, -1)).toEqual({ error: "templates.errors.first" });
});

it("explains why a VM cannot become a template", () => {
  const db = withDisks(vm("db", "stopped"), ["tmpl-root"]);
  expect(convertReason(db, [], [root])).toBeNull();
  expect(convertReason(withDisks(vm("db", "running"), []), [], [])).toBe(
    "templates.reason.notStopped",
  );
  expect(convertReason(db, [snapshot("s1", db, 1)], [root])).toBe("templates.reason.snapshots");
  expect(convertReason({ ...db, spec: { ...db.spec, ha: true } }, [], [])).toBe(
    "templates.reason.ha",
  );
  expect(
    convertReason(
      { ...db, spec: { ...db.spec, snapshot_schedule: { every: "daily", keep: 3 } } },
      [],
      [],
    ),
  ).toBe("templates.reason.schedule");
  const linked = withDisks(vm("db", "stopped"), ["web-1-root"]);
  expect(convertReason(linked, [], [cloneOf("web-1-root", root)])).toBe(
    "templates.reason.linkedDisk",
  );
  // 목록을 모르면 그것으로는 막지 않는다.
  expect(convertReason(linked, undefined, undefined)).toBeNull();
});

it("picks the deploy task node like the server's template_node", () => {
  const n = (id: string, online = true, pools: string[] = []) => ({
    ...NODE,
    id,
    status: {
      ...NODE.status,
      online,
      pools: Object.fromEntries(pools.map((p) => [p, poolState(p)])),
    },
  });
  const san = pool("san", { backend: { kind: "lvm-shared", vg: "vg0" } });
  const pools = [pool("local"), san];
  const nodes = [n("n1", false, ["san"]), n("n3", true, ["san"]), n("n2", true, ["san"])];
  const shared = volume("tmpl-root", "san", { node: "n1" });
  const local = volume("tmpl-root", "local", { node: "n1" });
  const tmpl = withDisks(vm("tmpl", "stopped"), ["tmpl-root"], true);
  // 지난 노드 n1이 오프라인이면 공유 풀은 쓸 수 있는 온라인 노드 가운데 id가 가장 작은 노드다.
  expect(templateNode(tmpl, [shared], pools, nodes, "n3")).toBe("n2");
  // 로컬 풀은 볼륨이 있는 노드(오프라인이어도)다.
  expect(templateNode(tmpl, [local], pools, nodes, "n3")).toBe("n1");
  // 지난 노드가 온라인이면 그 노드다.
  expect(templateNode(tmpl, [shared], pools, [n("n1"), n("n2", true, ["san"])], "n3")).toBe("n1");
  // 지난 노드와 볼륨의 노드가 모두 클러스터에서 빠졌으면 접속한 노드다.
  expect(templateNode(tmpl, [local], pools, [n("n2")], "n3")).toBe("n3");
  expect(templateNode(tmpl, undefined, pools, nodes, "n3")).toBeUndefined();
  expect(templateNode(tmpl, [shared], pools, undefined, "n3")).toBeUndefined();
});
