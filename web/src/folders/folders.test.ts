import { folderPath, folderTree, membersOf, ROOT_ID, subtree } from "@/folders/folders";
import { folder, folderId, vm } from "@/test/fixtures";

const folders = [
  folder("teamA"),
  folder("prod", "vm", "teamA"),
  folder("dev", "vm", "teamA"),
  folder("teamB"),
  folder("lan", "network"),
];

it("builds one tree per kind, sorted, with paths", () => {
  const tree = folderTree(folders, "vm");
  expect(tree.folders.map((f) => f.spec.name)).toEqual(["dev", "prod", "teamA", "teamB"]);
  expect(tree.children.get(ROOT_ID)?.map((f) => f.spec.name)).toEqual(["teamA", "teamB"]);
  expect(tree.children.get(folderId("teamA"))?.map((f) => f.spec.name)).toEqual(["dev", "prod"]);
  expect(folderPath(tree, folderId("prod"))).toBe("/teamA/prod");
  expect(folderPath(tree, null)).toBe("/");
  expect([...subtree(tree, folderId("teamA"))].sort()).toEqual(
    [folderId("teamA"), folderId("prod"), folderId("dev")].sort(),
  );
});

it("puts a folder whose parent cannot be seen at the top", () => {
  // 부모(teamA)를 볼 수 없는 사용자
  const tree = folderTree([folder("prod", "vm", "teamA")], "vm");
  expect(tree.children.get(ROOT_ID)?.map((f) => f.spec.name)).toEqual(["prod"]);
  expect(folderPath(tree, folderId("prod"))).toBe("/prod");
});

it("lists the members of a folder, with or without subfolders", () => {
  const tree = folderTree(folders, "vm");
  const at = (name: string, where?: string) =>
    vm(
      name,
      "stopped",
      where ? { spec: { ...vm(name, "stopped").spec, folder: folderId(where) } } : {},
    );
  const items = [at("web", "prod"), at("db", "teamA"), at("top"), at("hidden", "unknown")];
  const names = (list: typeof items) => list.map((v) => v.name);
  expect(names(membersOf(tree, items, folderId("teamA"), false))).toEqual(["db"]);
  expect(names(membersOf(tree, items, folderId("teamA"), true))).toEqual(["web", "db"]);
  // 볼 수 없는 폴더의 객체는 맨 위에 있는 것으로 보인다.
  expect(names(membersOf(tree, items, ROOT_ID, false))).toEqual(["top", "hidden"]);
  expect(names(membersOf(tree, items, ROOT_ID, true))).toHaveLength(4);
});
