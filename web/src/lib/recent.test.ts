import {
  alive,
  MAX_RECENT,
  pushRecent,
  readPins,
  readRecent,
  type ResourceRef,
  togglePin,
} from "@/lib/recent";

afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

const ref = (name: string, kind: ResourceRef["kind"] = "vm"): ResourceRef => ({
  kind,
  id: `id-${name}`,
  name,
});

it("keeps the newest twenty recent items without duplicates", () => {
  for (let i = 0; i < 25; i++) {
    pushRecent("u1", ref(`vm${i}`));
  }
  pushRecent("u1", ref("vm10"));
  const recent = readRecent("u1");
  expect(recent).toHaveLength(MAX_RECENT);
  expect(recent[0]?.name).toBe("vm10");
  expect(recent.filter((r) => r.name === "vm10")).toHaveLength(1);
  expect(recent.map((r) => r.name)).not.toContain("vm4");
});

it("keeps recent items and favorites per user", () => {
  pushRecent("u1", ref("web"));
  expect(togglePin("u1", ref("web"))).toBe(true);
  expect(readRecent("u2")).toEqual([]);
  expect(readPins("u2")).toEqual([]);
  expect(localStorage.getItem("opl.u1.pins")).toContain("web");
  expect(togglePin("u1", ref("web"))).toBe(false);
  expect(readPins("u1")).toEqual([]);
});

it("drops deleted resources and renames on read, keeping kinds not yet listed", () => {
  const refs = [ref("web"), ref("gone"), ref("node-a", "node")];
  expect(alive(refs, { vm: [{ id: "id-web", name: "web2" }] })).toEqual([
    { kind: "vm", id: "id-web", name: "web2" },
    ref("node-a", "node"),
  ]);
});

it("ignores broken or unavailable storage", () => {
  localStorage.setItem("opl.u1.recent", "{not json");
  expect(readRecent("u1")).toEqual([]);
  localStorage.setItem("opl.u1.pins", JSON.stringify([{ kind: "vm" }, ref("ok")]));
  expect(readPins("u1")).toEqual([ref("ok")]);
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new Error("quota");
  });
  expect(() => pushRecent("u1", ref("web"))).not.toThrow();
});
