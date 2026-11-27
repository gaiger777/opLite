import type { Task } from "@/api/types";
import { alertHref, relatedLinks, type TargetLookup, taskTarget } from "@/tasks/target";
import { backup, NODE, task, vm, vmImport, volume } from "@/test/fixtures";

const web = vm("web", "running");
const disk = volume("web-disk0", "local");
const bk = backup("web-1", web, 1);
const imp = vmImport("esx-web", 1);

const lookup: TargetLookup = {
  vm: new Map([[web.id, web]]),
  volume: new Map([[disk.id, disk]]),
  backup: new Map([[bk.id, bk]]),
  import: new Map([[imp.id, imp]]),
  node: new Map([[NODE.id, NODE]]),
};

function op(kind: string, target: string, extra: object = {}): Task {
  const t = task("t1", "vm-start", target);
  return { ...t, spec: { ...t.spec, op: { kind, ...extra } } } as Task;
}

it("links VM tasks to the matching detail tab", () => {
  expect(taskTarget(op("vm-start", web.id), lookup)).toEqual({ name: "web", href: "/vms/web" });
  expect(taskTarget(op("snapshot-create", web.id), lookup).href).toBe("/vms/web?tab=snapshots");
  expect(taskTarget(op("vm-backup", web.id, { backup: bk.id }), lookup).href).toBe(
    "/vms/web?tab=backups",
  );
  expect(taskTarget(op("vm-firmware-reset", web.id), lookup).href).toBe("/vms/web?tab=hardware");
});

it("links other targets to their screens", () => {
  expect(taskTarget(op("node-upgrade", NODE.id), lookup)).toEqual({
    name: "node-a",
    href: "/nodes/node-a",
  });
  expect(taskTarget(op("volume-delete", disk.id), lookup)).toEqual({
    name: "web-disk0",
    href: "/volumes?q=web-disk0",
  });
  expect(taskTarget(op("backup-delete", bk.id), lookup)).toEqual({
    name: "web-1",
    href: "/backups?q=web-1",
  });
  expect(taskTarget(op("vm-import-delete", imp.id), lookup)).toEqual({
    name: imp.name,
    href: `/imports/${imp.name}`,
  });
  const gone = "019a0f3c-0000-7000-8000-00000000beef";
  expect(taskTarget(op("ceph-init", gone), lookup)).toEqual({ name: "0000beef", href: "/ceph" });
  expect(taskTarget(op("volume-delete", gone), lookup)).toEqual({ name: "0000beef" });
  // 복원의 새 VM은 끝나기 전에는 요청한 이름만 있다.
  expect(taskTarget(op("backup-restore", gone, { name: "web2" }), lookup)).toEqual({
    name: "web2",
  });
});

it("links alert subjects", () => {
  const s = { id: "x", name: "a b" };
  expect(alertHref({ ...s, kind: "node" })).toBe("/nodes/a%20b");
  expect(alertHref({ ...s, kind: "node", port: "eno1" })).toBe(
    "/cluster/map?focus=port%3Aa%20b%2Feno1",
  );
  expect(alertHref({ ...s, kind: "vm" })).toBe("/vms/a%20b");
  expect(alertHref({ ...s, kind: "pool" })).toBe("/storage-pools?q=a%20b");
  expect(alertHref({ ...s, kind: "backup_job" })).toBe("/backup-jobs");
  expect(alertHref({ ...s, kind: "task" })).toBe("/tasks");
  expect(alertHref({ ...s, kind: "sdn" })).toBe("/sdn");
  expect(alertHref({ ...s, kind: "router" })).toBe("/routers?open=x");
  expect(alertHref({ ...s, kind: "load_balancer" })).toBe("/load-balancers?open=x");
  expect(alertHref({ ...s, kind: "backup" })).toBe("/backups?q=a%20b");
  expect(alertHref({ ...s, kind: "backup_store" })).toBe("/backup-stores?open=x");
  // 계획 73: 인증서는 설정 › 보안 › 인증서, 적용 묶음은 그 묶음의 상세
  expect(alertHref({ ...s, kind: "certificate" })).toBe("/settings/certificates");
  expect(alertHref({ ...s, kind: "apply_set" })).toBe("/apply-sets/a%20b");
  // 계획 76: 복구 계획은 데이터 보호 › DR의 그 계획
  expect(alertHref({ ...s, kind: "recovery_plan" })).toBe("/dr-plans/a%20b");
});

it("links declarative apply and ACME order tasks (plan 73)", () => {
  const set = "019a0f3c-0000-7000-8073-0000000000a1";
  expect(taskTarget(op("apply", set, { plan: "p", set }), lookup)).toEqual({
    name: set.slice(-8),
    href: "/apply-sets",
  });
  expect(taskTarget(op("apply", "p", { plan: "p" }), lookup).href).toBe("/history");
  expect(taskTarget(op("acme-order", "c", { certificate: "c" }), lookup).href).toBe(
    "/settings/certificates",
  );
});

it("names targets from lists already read and from the server's name labels (plan 77)", () => {
  const store = "019a0f3c-0000-7000-8068-000000000001";
  const set = "019a0f3c-0000-7000-8073-0000000000a1";
  const named: TargetLookup = {
    ...lookup,
    named: new Map([
      [store, { name: "lab-store", kind: "backup-store" }],
      [set, { name: "p73", kind: "apply-set" }],
    ]),
  };
  expect(taskTarget(op("backup-store-gc", store), named)).toEqual({
    name: "lab-store",
    href: `/backup-stores?open=${store}`,
  });
  // 적용 Task는 그 묶음의 상세로 간다(목록이 아니라).
  expect(taskTarget(op("apply", set, { plan: "p", set }), named)).toEqual({
    name: "p73",
    href: "/apply-sets/p73",
  });
  // 목록에 없으면 만들 때 적은 이름. 지운 VM은 화면이 없다.
  const gone = "019a0f3c-0000-7000-8000-00000000beef";
  const labelled = (kind: string, name: string, k: string) => ({
    ...op(kind, gone),
    labels: { "target-name": name, "target-kind": k },
  });
  expect(taskTarget(labelled("vm-migrate", "web-01", "vm"), lookup)).toEqual({ name: "web-01" });
  expect(taskTarget(labelled("backup-store-gc", "lab-store", "backup-store"), lookup)).toEqual({
    name: "lab-store",
    href: `/backup-stores?open=${gone}`,
  });
  expect(taskTarget(labelled("apply", "p73", "apply-set"), lookup).href).toBe("/apply-sets/p73");
});

it("lists what a task is tied to besides its target", () => {
  expect(relatedLinks(op("vm-backup", web.id, { backup: bk.id }), lookup)).toEqual([
    { kind: "backup", name: "web-1", href: "/backups?q=web-1" },
  ]);
  expect(relatedLinks(op("backup-restore", "x", { backup: bk.id, name: "w2" }), lookup)).toEqual([
    { kind: "sourceBackup", name: "web-1", href: "/backups?q=web-1" },
  ]);
  expect(relatedLinks(op("backup-replicate", bk.id, { remote: "dr", pool: "p" }), lookup)).toEqual([
    { kind: "remote", name: "dr", href: "/remotes/dr" },
  ]);
  const set = "019a0f3c-0000-7000-8073-0000000000a1";
  const applied = {
    ...op("apply", set, { plan: "019a0f3c-0000-7000-8073-0000000000b2", set }),
    labels: { "target-name": "p73", "target-kind": "apply-set" },
  };
  // 묶음은 대상 줄에 이미 있으므로 계획만
  expect(relatedLinks(applied, lookup)).toEqual([
    { kind: "plan", name: "000000b2", href: "/history" },
  ]);
  expect(relatedLinks(op("vm-start", web.id), lookup)).toEqual([]);
});
