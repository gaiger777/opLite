import { Permissions } from "@/auth/permissions";
import { createItems } from "@/layout/create";

it("offers import only with root vm.create", () => {
  // OVA·ESXi 가져오기는 루트 vm.create다. 폴더의 vm.create로는 VM만 만든다(리뷰 2).
  const A = "019a0f3c-0000-7000-8009-00000000000a";
  const folderUser = new Permissions({
    all: false,
    root: [],
    root_contents: [],
    folders: { [A]: ["vm.create"] },
    contents: { [A]: ["vm.create"] },
    objects: [],
  });
  expect(createItems(folderUser).map((i) => i.id)).toEqual(["vm"]);
  expect(createItems(Permissions.fromRole("operator")).map((i) => i.id)).toContain("import");
});
