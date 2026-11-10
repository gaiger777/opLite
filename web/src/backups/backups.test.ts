import {
  backupPools,
  chainBytes,
  chainOf,
  chainReplicas,
  newestFirst,
  storedBytes,
} from "@/backups/backups";
import { backup, pool, vm } from "@/test/fixtures";

const web = vm("web", "stopped");

it("adds up the stored size of a ready backup only", () => {
  const ready = backup("b1", web, 1, {
    status: {
      disks: [
        { name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 3 },
        { name: "disk1", file: "disk1.qcow2", virtual_bytes: 10, stored_bytes: 4 },
      ],
    },
  });
  expect(storedBytes(ready)).toBe(7);
  expect(storedBytes(backup("b2", web, 2, { status: { phase: "pending" } }))).toBeUndefined();
  expect(storedBytes(backup("b3", web, 3, { status: { phase: "failed" } }))).toBeUndefined();
});

it("offers only pools that store backups", () => {
  const pools = [pool("local"), pool("bk", { backups: true }), pool("off", { backups: false })];
  expect(backupPools(pools).map((p) => p.name)).toEqual(["bk"]);
});

it("puts the newest backup first", () => {
  const list = [backup("a", web, 1), backup("c", web, 3), backup("b", web, 2)];
  expect(newestFirst(list).map((b) => b.name)).toEqual(["c", "b", "a"]);
});

it("follows the chain of an incremental backup from its full backup", () => {
  const full = backup("full", web, 1);
  const inc1 = backup("inc1", web, 2, {
    spec: { incremental: true },
    status: {
      parent: full.id,
      disks: [{ name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 5 }],
    },
  });
  const inc2 = backup("inc2", web, 3, {
    spec: { incremental: true },
    status: {
      parent: inc1.id,
      disks: [{ name: "disk0", file: "disk0.qcow2", virtual_bytes: 10, stored_bytes: 7 }],
    },
  });
  const all = [inc2, full, inc1];
  expect(chainOf(inc2, all)?.map((b) => b.name)).toEqual(["full", "inc1", "inc2"]);
  expect(chainOf(full, all)?.map((b) => b.name)).toEqual(["full"]);
  // 체인 크기는 전체 백업과 증분 파일 크기의 합이다.
  expect(chainBytes(inc2, all)).toBe(1536 * 1024 ** 2 + 5 + 7);
  // 목록에 없는 조상이 있으면 체인을 모른다.
  expect(chainOf(inc2, [inc2, inc1])).toBeUndefined();
  expect(chainBytes(inc2, [inc2, inc1])).toBeUndefined();
  // 다 만들지 않은 백업이 체인에 있으면 크기를 모른다.
  const pending = backup("inc3", web, 4, { status: { phase: "pending", parent: inc2.id } });
  expect(chainBytes(pending, [...all, pending])).toBeUndefined();
});

it("counts the backups of the chain sent to each remote copy target", () => {
  const copy = (pool: string, state: "ready" | "copying" | "failed", backup?: string) => ({
    remote: "dr",
    pool,
    state,
    at: 1,
    ...(backup && { backup }),
  });
  const full = backup("full", web, 1, {
    status: {
      replicas: [
        copy("bk", "ready", "019a0f3c-0000-7000-8002-0000000000a1"),
        copy("other", "ready", "019a0f3c-0000-7000-8002-0000000000b1"),
      ],
    },
  });
  const inc1 = backup("inc1", web, 2, {
    status: {
      parent: full.id,
      replicas: [copy("bk", "ready", "019a0f3c-0000-7000-8002-0000000000a2")],
    },
  });
  const inc2 = backup("inc2", web, 3, {
    status: {
      parent: inc1.id,
      replicas: [
        copy("bk", "copying", "019a0f3c-0000-7000-8002-0000000000a3"),
        copy("other", "ready"),
      ],
    },
  });
  const all = [inc2, inc1, full];
  // (원격, 풀)은 전체 백업부터 처음 나온 차례이고, ready이고 사본 id가 있는 백업만 보낸 것으로 센다.
  expect(chainReplicas(inc2, all)).toEqual([
    { remote: "dr", pool: "bk", sent: 2, total: 3 },
    { remote: "dr", pool: "other", sent: 1, total: 3 },
  ]);
  // 자기 사본 기록이 없어도 앞 백업의 사본이 있으면 보인다.
  const inc3 = backup("inc3", web, 4, { status: { parent: inc2.id } });
  expect(chainReplicas(inc3, [...all, inc3])[0]).toEqual({
    remote: "dr",
    pool: "bk",
    sent: 2,
    total: 4,
  });
  // 이 백업의 실패 기록 하나뿐이어도 보인다(원격에 있는 백업 0개).
  const failed = backup("failed", web, 7, {
    status: { parent: full.id, replicas: [copy("third", "failed")] },
  });
  expect(chainReplicas(failed, [...all, failed])).toEqual([
    { remote: "dr", pool: "bk", sent: 1, total: 2 },
    { remote: "dr", pool: "other", sent: 1, total: 2 },
    { remote: "dr", pool: "third", sent: 0, total: 2 },
  ]);
  // 전체 백업, 사본이 없는 체인, 목록에 없는 조상이 있는 체인은 진행이 없다.
  expect(chainReplicas(full, all)).toEqual([]);
  const bare = backup("bare", web, 5, { status: { parent: backup("x", web, 6).id } });
  expect(chainReplicas(bare, [bare, backup("x", web, 6)])).toEqual([]);
  expect(chainReplicas(inc2, [inc2, inc1])).toEqual([]);
  // 잘못된 목록에서 고리를 이루면 사본 기록이 있어도 진행이 없다.
  const ring = { ...full, status: { ...full.status, parent: inc2.id } };
  expect(chainReplicas(inc2, [inc2, inc1, ring])).toEqual([]);
});
