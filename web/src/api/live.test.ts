import { applyEvent, type List } from "@/api/live";

type Item = { id: string; revision: number; name: string };

const list: List<Item> = {
  revision: 10,
  items: [
    { id: "a", revision: 5, name: "a" },
    { id: "b", revision: 8, name: "b" },
  ],
};

it("applies newer changes and ignores ones the list already has", () => {
  const newer = applyEvent(list, {
    type: "put",
    kind: "vm",
    resource: { id: "a", revision: 11, name: "a2" } as Item,
  });
  expect(newer.items[0]).toMatchObject({ revision: 11, name: "a2" });
  const older = applyEvent(list, {
    type: "put",
    kind: "vm",
    resource: { id: "b", revision: 7, name: "old" } as Item,
  });
  expect(older).toBe(list);
});

it("adds new resources and removes deleted ones", () => {
  const added = applyEvent(list, {
    type: "put",
    kind: "vm",
    resource: { id: "c", revision: 12, name: "c" } as Item,
  });
  expect(added.items.map((i) => i.id)).toEqual(["a", "b", "c"]);
  const deleted = applyEvent(added, {
    type: "deleted",
    revision: 13,
    kind: "vm",
    resource: { id: "a", revision: 5 },
  });
  expect(deleted.items.map((i) => i.id)).toEqual(["b", "c"]);
});

it("keeps a resource that was recreated after an old deletion", () => {
  const stale = applyEvent(list, {
    type: "deleted",
    revision: 7,
    kind: "vm",
    resource: { id: "b", revision: 3 },
  });
  expect(stale).toBe(list);
});
