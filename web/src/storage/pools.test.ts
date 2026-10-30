import {
  cephNameError,
  cephUserError,
  lvmNameError,
  nfsServerError,
  nfsExportError,
  nfsOptionsError,
  poolCapacity,
  poolLocation,
  poolFormats,
  poolNodes,
  poolsUsableOn,
  poolUsableSomewhere,
  rbdConfError,
  zfsDatasetError,
} from "@/storage/pools";
import { NODE, pool, poolState } from "@/test/fixtures";

const GIB = 1024 ** 3;
const nfs = (server: string) =>
  pool("nas", { backend: { kind: "nfs", server, export: "/srv/vm" }, shared: true });
const lvm = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
const zfs = pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } });
const rbd = (namespace?: string) =>
  pool("ceph", { backend: { kind: "rbd", pool: "rbd", ...(namespace && { namespace }) } });
const lvmShared = (iscsi?: { portal: string; target: string }) =>
  pool("san", { backend: { kind: "lvm-shared", vg: "san", ...(iscsi && { iscsi }) } });
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
  expect(poolLocation(lvm)).toBe("vg0/tp");
  expect(poolLocation(zfs)).toBe("tank/opl");
  expect(poolLocation(rbd())).toBe("rbd");
  expect(poolLocation(rbd("opl"))).toBe("rbd/opl");
  expect(poolLocation(lvmShared())).toBe("san");
  expect(poolLocation(lvmShared({ portal: "10.0.0.5:3260", target: "iqn.2003-01.org:opl" }))).toBe(
    "san @ 10.0.0.5:3260 iqn.2003-01.org:opl",
  );
});

it("checks LVM names like the server", () => {
  for (const ok of ["vg0", "VG_data", "a+b.c-d", "x".repeat(127), "...", "my_snapshot", "tdata"]) {
    expect(lvmNameError(ok)).toBeUndefined();
  }
  for (const bad of ["", "-vg", "vg 0", "vg/tp", "볼륨", "x".repeat(128)]) {
    expect(lvmNameError(bad)).toBe("pools.errors.lvmName");
  }
  // LVM이 받지 않는 이름: `.`·`..`, `snapshot`·`pvmove`로 시작, 내부 LV의 접미어 포함
  for (const reserved of [
    ".",
    "..",
    "snapshot1",
    "pvmove0",
    "data_tdata",
    "x_tmeta",
    "a_cdata",
    "a_cmeta",
    "a_corig",
    "a_mimage_0",
    "a_mlog",
    "a_pmspare",
    "a_rimage",
    "a_rmeta",
    "a_vorigin",
    "a_vdata",
  ]) {
    expect(lvmNameError(reserved)).toBe("pools.errors.lvmName");
  }
});

it("checks ZFS datasets like the server", () => {
  // zvol 스냅샷 `{dataset}/opl-{uuid}@{uuid}`가 ZFS 이름 상한 255자를 넘지 않게 177자까지다.
  const long = `t/${"a".repeat(175)}`;
  for (const ok of ["tank", "tank/opl", "Tank0/a_b.c:d-e/f", "t/x-", "t/.x", "t/9", long]) {
    expect(zfsDatasetError(ok)).toBeUndefined();
  }
  for (const bad of [
    "",
    "/tank",
    "tank/",
    "tank//opl",
    // 풀 이름은 문자로 시작한다.
    "0tank",
    "_tank",
    "-tank",
    ".tank",
    // `-`로 시작하는 구성 요소는 명령 옵션으로 읽힐 수 있다.
    "tank/-opl",
    "tank/opl@snap",
    "tank/opl#bm",
    "tank/my opl",
    "tank/opl+",
    "tank/데이터",
    "tank/.",
    "tank/..",
    `${long}a`,
  ]) {
    expect(zfsDatasetError(bad)).toBe("pools.errors.zfsDataset");
  }
});

it("checks Ceph names like the server", () => {
  for (const ok of ["rbd", "Opl_vm.1-a", "9", "a".repeat(63)]) {
    expect(cephNameError(ok)).toBeUndefined();
  }
  // `-`로 시작하면 명령 옵션으로 읽힐 수 있다.
  for (const bad of [
    "",
    "-rbd",
    "rbd/opl",
    "rbd@s",
    "a,b",
    "a:b",
    "a=b",
    "my rbd",
    "데이터",
    "a".repeat(64),
  ]) {
    expect(cephNameError(bad)).toBe("pools.errors.rbdName");
  }
});

it("checks Ceph users like the server, without the client. prefix", () => {
  expect(cephUserError("opl")).toBeUndefined();
  expect(cephUserError("clients")).toBeUndefined();
  expect(cephUserError("client.opl")).toBe("pools.errors.cephUser");
  expect(cephUserError("client.admin")).toBe("pools.errors.cephUser");
  expect(cephUserError("-opl")).toBe("pools.errors.rbdName");
});

it("checks ceph.conf paths like the server", () => {
  expect(rbdConfError("/etc/ceph/ceph.conf")).toBeUndefined();
  expect(rbdConfError("/etc/ceph/my ceph.conf")).toBeUndefined();
  // conf는 QEMU 옵션 문자열(`conf=...`)에 들어간다.
  for (const bad of [
    "",
    "ceph.conf",
    "/etc/ceph/a,b.conf",
    "/etc/ceph/a:b.conf",
    "/etc/ceph/a=b.conf",
    "/etc/ceph/\nceph.conf",
  ]) {
    expect(rbdConfError(bad)).toBe("pools.errors.rbdConf");
  }
});

it("offers the volume formats a pool holds, the default first", () => {
  expect(poolFormats(pool("local"))).toEqual(["qcow2", "raw"]);
  expect(poolFormats(nfs("nas.lan"))).toEqual(["qcow2", "raw"]);
  // LVM-thin 풀은 raw 볼륨만 둔다(서버가 qcow2를 거절한다).
  expect(poolFormats(lvm)).toEqual(["raw"]);
  // ZFS 풀도 raw zvol만 둔다.
  expect(poolFormats(zfs)).toEqual(["raw"]);
  // RBD 풀도 raw 이미지만 둔다.
  expect(poolFormats(rbd())).toEqual(["raw"]);
  // 풀을 고르기 전에는 파일 풀처럼 둘 다다.
  expect(poolFormats(undefined)).toEqual(["qcow2", "raw"]);
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
  // RBD 풀은 `shared`가 없어도 늘 공유다(서버의 `is_shared`).
  const ceph = (id: string, name: string) => ({
    ...NODE,
    id,
    name,
    status: { ...NODE.status, pools: { ceph: poolState("ceph", { available_bytes: GIB }) } },
  });
  expect(poolCapacity(rbd(), [ceph("n1", "node-a"), ceph("n2", "node-b")])).toEqual([
    { node: null, free: GIB, total: 128 * GIB },
  ]);
  // 공유 LVM 풀도 그렇다.
  const san = (id: string, name: string) => ({
    ...NODE,
    id,
    name,
    status: { ...NODE.status, pools: { san: poolState("san", { available_bytes: GIB }) } },
  });
  expect(poolCapacity(lvmShared(), [san("n1", "node-a"), san("n2", "node-b")])).toEqual([
    { node: null, free: GIB, total: 128 * GIB },
  ]);
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

  // LVM-thin 풀도 NFS 풀처럼 준비됐다고 알린 노드에서만 쓴다.
  const fast = (state: Parameters<typeof poolState>[1]) => ({
    ...NODE,
    status: { ...NODE.status, pools: { fast: poolState("fast", state) } },
  });
  expect(poolsUsableOn([lvm], [unknown], "n1")).toEqual([]);
  expect(poolsUsableOn([lvm], [fast({})], "n1")).toEqual([lvm]);
  expect(poolsUsableOn([lvm], [fast({ ready: false, error: "x" })], "n1")).toEqual([]);
});
