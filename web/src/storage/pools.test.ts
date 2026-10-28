import {
  nfsServerError,
  nfsExportError,
  nfsOptionsError,
  poolCapacity,
  poolLocation,
  poolNodes,
  poolsUsableOn,
  poolUsableSomewhere,
} from "@/storage/pools";
import { NODE, pool, poolState } from "@/test/fixtures";

const GIB = 1024 ** 3;
const nfs = (server: string) =>
  pool("nas", { backend: { kind: "nfs", server, export: "/srv/vm" }, shared: true });
const withPool = (
  id: string,
  name: string,
  state: Parameters<typeof poolState>[1],
  online = true,
) => ({
  ...NODE,
  id,
  name,
  status: {
    ...NODE.status,
    online,
    pools: { local: poolState("local", state), nas: poolState("nas", state) },
  },
});

it("shows where a pool keeps its files", () => {
  expect(poolLocation(pool("local"))).toBe("/srv/local");
  expect(poolLocation(nfs("nas.lan"))).toBe("nas.lan:/srv/vm");
  // IPv6 주소는 mount처럼 대괄호로 감싼다.
  expect(poolLocation(nfs("fd00::5"))).toBe("[fd00::5]:/srv/vm");
});

it("checks NFS servers like the server", () => {
  for (const ok of ["nas.lan", "10.0.0.5", "fd00::5", "NAS-01"]) {
    expect(nfsServerError(ok)).toBeUndefined();
  }
  for (const bad of ["", "nas lan", "a,b", "[fd00::5]", "-oremount", "a/b"]) {
    expect(nfsServerError(bad)).toBe("pools.errors.server");
  }
});

it("checks NFS exports like the server", () => {
  expect(nfsExportError("/srv/vm")).toBeUndefined();
  for (const bad of ["", "srv/vm", "/srv/\nvm", "/srv/\u007fvm"]) {
    expect(nfsExportError(bad)).toBe("pools.errors.export");
  }
});

it("refuses mount options that change what the mount is, like the server", () => {
  for (const ok of ["vers=4.2,hard", "nosuid,nodev,noexec", "soft,timeo=600", "nconnect=4"]) {
    expect(nfsOptionsError(ok)).toBeUndefined();
  }
  for (const bad of [
    "",
    "vers=4.2, hard",
    "hard\u0001",
    "bind",
    "hard,rbind",
    "remount,rw",
    "move",
    "suid",
    "DEV",
    "exec",
    "helper=foo",
    "x-systemd.automount",
    "X-mount.mkdir",
    "bg",
    "hard,BG",
  ]) {
    expect(nfsOptionsError(bad)).toBe("pools.errors.options");
  }
});

it("lists the nodes a pool may use", () => {
  const b = withPool("n2", "node-b", {});
  expect(poolNodes(pool("local"), [NODE, b]).map((n) => n.name)).toEqual(["node-a", "node-b"]);
  expect(poolNodes(pool("local", { nodes: ["n2"] }), [NODE, b]).map((n) => n.name)).toEqual([
    "node-b",
  ]);
  // 목록에 없는 노드 id는 id 끝자리로 보인다.
  expect(poolNodes(pool("local", { nodes: ["gone-node-12345678"] }), [NODE])).toEqual([
    { id: "gone-node-12345678", name: "12345678", state: undefined, offline: false },
  ]);
  // 꺼진 노드는 오프라인으로 보인다.
  const down = withPool("n3", "node-c", {}, false);
  expect(poolNodes(pool("local"), [down])[0]?.offline).toBe(true);
  // 같은 이름의 지워진 풀이 남긴 상태는 이 풀의 것이 아니다.
  const stale = withPool("n2", "node-b", { pool_id: "019a0f3c-0000-7000-8001-0000000000ff" });
  expect(poolNodes(pool("local"), [stale])[0]?.state).toBeUndefined();
});

it("takes the capacity from online nodes where the pool is ready", () => {
  const a = withPool("n1", "node-a", { ready: false, error: "mount failed" });
  const b = withPool("n2", "node-b", { available_bytes: 10 * GIB });
  const c = withPool("n3", "node-c", { available_bytes: 20 * GIB });
  const down = withPool("n4", "node-d", { available_bytes: 30 * GIB }, false);
  // 공유 풀은 첫 준비된 온라인 노드의 값 하나다.
  expect(poolCapacity(nfs("nas"), [down, a, b, c])).toEqual([
    { node: null, free: 10 * GIB, total: 128 * GIB },
  ]);
  // 로컬 풀은 준비된 노드마다 값이 다르다.
  expect(poolCapacity(pool("local"), [a, b, c, down])).toEqual([
    { node: "node-b", free: 10 * GIB, total: 128 * GIB },
    { node: "node-c", free: 20 * GIB, total: 128 * GIB },
  ]);
  expect(poolCapacity(pool("local"), [a])).toEqual([]);
});

it("offers the pools a node can use, like the server's pool_usable_on", () => {
  const nas = nfs("nas.lan");
  const unknown = { ...NODE, status: { ...NODE.status, pools: {} } };
  const pools = [pool("local"), pool("far", { nodes: ["n2"] }), nas];
  // 상태를 알리지 않은 노드에서도 디렉터리 풀은 쓸 수 있고, NFS 풀은 쓸 수 없다.
  expect(poolsUsableOn(pools, [unknown], "n1").map((p) => p.name)).toEqual(["local"]);
  // 준비됐다고 알리면 NFS 풀도, 준비되지 않았다고 알리면 디렉터리 풀도 그렇다.
  const ready = withPool("n1", "node-a", {});
  expect(poolsUsableOn(pools, [ready], "n1").map((p) => p.name)).toEqual(["local", "nas"]);
  const down = withPool("n1", "node-a", { ready: false, error: "x" });
  expect(poolsUsableOn(pools, [down], "n1")).toEqual([]);
  // 다른 풀(같은 이름)의 상태는 알리지 않은 것으로 본다.
  const stale = withPool("n1", "node-a", { pool_id: "019a0f3c-0000-7000-8001-0000000000ff" });
  expect(poolsUsableOn(pools, [stale], "n1").map((p) => p.name)).toEqual(["local"]);
  // 꺼진 노드, 모르는 노드에서는 아무 풀도 쓸 수 없다.
  expect(poolsUsableOn(pools, [withPool("n1", "node-a", {}, false)], "n1")).toEqual([]);
  expect(poolsUsableOn(pools, [ready], undefined)).toEqual([]);
  expect(poolsUsableOn(pools, [ready], "n9")).toEqual([]);

  expect(poolUsableSomewhere(nas, [unknown, withPool("n2", "node-b", {}, false)])).toBe(false);
  expect(poolUsableSomewhere(nas, [unknown, withPool("n2", "node-b", {})])).toBe(true);
});
