import { builtinPrivileges, kindPrivilege, Permissions, PRIVILEGES } from "@/auth/permissions";

const A = "019a0f3c-0000-7000-8009-00000000000a";
const B = "019a0f3c-0000-7000-8009-00000000000b";
const VM = "019a0f3c-0000-7000-8000-000000000001";

const mine = {
  all: false,
  root: [],
  root_contents: [],
  folders: { [A]: ["system.read", "inventory.folder", "inventory.move", "permission.modify"] },
  contents: { [A]: ["system.read", "vm.create", "vm.power", "inventory.move"] },
  objects: [{ kind: "vm", id: VM, privileges: ["system.read", "vm.console"] }],
};

it("mirrors the server's builtin roles and kind privileges", () => {
  expect(builtinPrivileges("admin")).toEqual(PRIVILEGES);
  expect(builtinPrivileges("none")).toEqual([]);
  expect(builtinPrivileges("viewer")).toEqual(["system.read", "cluster.read", "alert.read"]);
  expect(kindPrivilege("write", "vm")).toBe("vm.config");
  expect(kindPrivilege("read", "folder")).toBe("system.read");
  expect(kindPrivilege("read", "group")).toBe("cluster.admin");
  expect(kindPrivilege("read", "node")).toBe("cluster.read");
  // VMware 이전·VM 내보내기(계획 72): vm.export는 마지막이고 operator에 없다.
  expect(PRIVILEGES.at(-1)).toBe("vm.export");
  expect(builtinPrivileges("operator")).not.toContain("vm.export");
  expect(kindPrivilege("read", "vmware-source")).toBe("cluster.read");
  expect(kindPrivilege("write", "vmware-source")).toBe("cluster.admin");
  expect(kindPrivilege("read", "vmware-migration")).toBe("system.read");
  expect(kindPrivilege("read", "vmware-vm")).toBe("system.read");
  expect(kindPrivilege("read", "vm-export")).toBe("system.read");
  expect(kindPrivilege("write", "vm-export")).toBe("vm.export");
  expect(kindPrivilege("operate", "vm-export")).toBe("vm.export");
  // 선언형 적용 묶음(계획 73): 읽기 system.read, 쓰기는 기본(cluster.admin — 적용이 단계마다 본다)
  expect(kindPrivilege("read", "apply-set")).toBe("system.read");
  expect(kindPrivilege("write", "apply-set")).toBe("cluster.admin");
});

it("answers per folder and per object from /permissions/mine", () => {
  const p = new Permissions(mine);
  // 루트에는 아무것도 없다(폴더 사용자).
  expect(p.has("cluster.read")).toBe(false);
  expect(p.reads("node")).toBe(false);
  // 객체마다 거르는 종류는 늘 읽는다(서버가 거른다).
  expect(p.reads("vm")).toBe(true);
  expect(p.has("inventory.folder", { type: "folder", id: A })).toBe(true);
  expect(p.has("inventory.folder", { type: "folder", id: B })).toBe(false);
  expect(p.has("vm.power", { type: "object", kind: "vm", id: VM, folder: A })).toBe(true);
  expect(p.has("vm.power", { type: "object", kind: "vm", id: VM, folder: null })).toBe(false);
  // 객체 grant는 폴더와 상관없이 더해진다.
  expect(p.has("vm.console", { type: "object", kind: "vm", id: VM, folder: null })).toBe(true);
  expect(p.has("vm.console", { type: "object", kind: "vm", id: "other", folder: A })).toBe(false);
  // 새로 둘 객체(id 없음)는 그 폴더 안의 권한이다.
  expect(p.has("vm.create", { type: "object", kind: "vm", folder: A })).toBe(true);
  expect(p.anywhere("vm.create")).toBe(true);
  expect(p.anywhere("vm.delete")).toBe(false);
  expect(p.foldersWith("vm.create")).toEqual([A]);
});

it("treats a folder it does not know (made after reading) like the root contents", () => {
  const p = new Permissions({ ...mine, root_contents: ["system.read", "vm.power"] });
  expect(p.has("vm.power", { type: "object", kind: "vm", id: VM, folder: B })).toBe(true);
});

it("gives everything to a global admin and the role's privileges everywhere otherwise", () => {
  const admin = new Permissions({ ...mine, all: true });
  expect(admin.has("cluster.admin")).toBe(true);
  expect(admin.has("vm.delete", { type: "folder", id: B })).toBe(true);
  const operator = Permissions.fromRole("operator");
  expect(operator.has("vm.power", { type: "object", kind: "vm", id: VM, folder: A })).toBe(true);
  expect(operator.can("write", "network")).toBe(false);
  expect(operator.can("write", "vm")).toBe(true);
  expect(Permissions.fromRole("none").anywhere("system.read")).toBe(false);
});
