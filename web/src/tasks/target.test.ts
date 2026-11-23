import type { Task } from "@/api/types";
import { alertHref, type TargetLookup, taskTarget } from "@/tasks/target";
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
  expect(alertHref({ ...s, kind: "vm" })).toBe("/vms/a%20b");
  expect(alertHref({ ...s, kind: "pool" })).toBe("/storage-pools?q=a%20b");
  expect(alertHref({ ...s, kind: "backup_job" })).toBe("/backup-jobs");
  expect(alertHref({ ...s, kind: "task" })).toBe("/tasks");
  expect(alertHref({ ...s, kind: "sdn" })).toBe("/sdn");
  expect(alertHref({ ...s, kind: "router" })).toBe("/routers?open=x");
  expect(alertHref({ ...s, kind: "load_balancer" })).toBe("/load-balancers?open=x");
  expect(alertHref({ ...s, kind: "backup" })).toBe("/backups?q=a%20b");
  expect(alertHref({ ...s, kind: "backup_store" })).toBe("/backup-stores?open=x");
});
