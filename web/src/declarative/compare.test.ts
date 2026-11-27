import type { Manifest } from "@/declarative/api";
import { compareDeclared, covers, liveOf, manifestKind } from "@/declarative/compare";

const live = (extra: Partial<Manifest>): Manifest =>
  ({ apiVersion: "orbivisor.io/v1", kind: "Vm", metadata: { name: "web" }, ...extra }) as Manifest;

it("compares only the fields written in the file, skipping secrets and server-filled fields", () => {
  const rows = compareDeclared(
    {
      metadata: { folder: "/prod", labels: { tier: "web" } },
      spec: {
        memory_mib: 4096,
        cpu: { cores: 1 },
        nics: [{ network: "lan" }],
        password: { $secret: "set" },
      },
      state: "running",
    },
    live({
      metadata: { name: "web", labels: { tier: "db" } },
      spec: {
        memory_mib: 4096,
        cpu: { cores: 2, sockets: 1 },
        // 서버가 채운 MAC은 보지 않는다
        nics: [{ network: "lan", mac: "52:54:00:00:00:01" }],
        password: { secretRef: "pw" },
        description: "not in the file",
      },
      state: "running",
    }),
  );
  expect(rows.map((r) => [r.path, r.differs, r.secret])).toEqual([
    // 폴더가 없는 리소스는 뿌리에 있다
    ["folder", true, false],
    ["labels.tier", true, false],
    ["cpu.cores", true, false],
    ["memory_mib", false, false],
    ["nics", false, false],
    ["password", false, true],
    ["state", false, false],
  ]);
  expect(rows[0]!.live).toBe("/");
});

it("says everything differs when the resource is gone", () => {
  const rows = compareDeclared({ spec: { size_bytes: 1 } }, undefined);
  expect(rows).toEqual([
    { path: "size_bytes", file: 1, live: undefined, secret: false, differs: true },
  ]);
});

it("matches lists by length and order and treats null as absent", () => {
  expect(covers([{ a: 1 }], [{ a: 1, b: 2 }])).toBe(true);
  expect(covers([{ a: 1 }], [{ a: 1 }, { a: 2 }])).toBe(false);
  expect(covers({ a: null }, {})).toBe(true);
  expect(covers({ a: { b: 1 } }, { a: { b: 1, c: 2 } })).toBe(true);
});

it("finds an object's manifest by kind and name, folders by their parent too", () => {
  expect(manifestKind("storage-pool")).toBe("StoragePool");
  expect(manifestKind("floating-ip")).toBe("FloatingIp");
  const docs = [
    live({}),
    { ...live({}), kind: "Folder", metadata: { name: "web", folder: "/a" } } as Manifest,
    { ...live({}), kind: "Folder", metadata: { name: "web", folder: "/b" } } as Manifest,
  ];
  const obj = (kind: string, folder?: string) => ({
    kind: kind as "vm",
    id: "x",
    name: "web",
    revision: 1,
    declared: folder ? { metadata: { folder } } : {},
  });
  // 옛 서버(id 없음): 종류·이름으로
  expect(liveOf(obj("vm"), { documents: docs })).toBe(docs[0]);
  expect(liveOf(obj("folder", "/b"), { documents: docs })).toBe(docs[2]);
  expect(liveOf(obj("folder", "/c"), { documents: docs })).toBeUndefined();
  // 드리프트가 알린 지금 이름이 있으면 그 이름으로
  expect(liveOf({ ...obj("vm"), name: "old" }, { documents: docs }, "web")).toBe(docs[0]);
});

it("pairs by the recorded id when the server gives ids, even if two resources swapped names", () => {
  const a = live({ metadata: { name: "b" }, spec: { memory_mib: 1 } });
  const b = live({ metadata: { name: "a" }, spec: { memory_mib: 2 } });
  // 묶음은 a(id-a)·b(id-b)로 적용했다 — 밖에서 둘이 이름을 맞바꿨다
  const set = { documents: [a, b], ids: ["id-a", "id-b"] };
  const obj = (id: string, name: string) => ({ kind: "vm" as const, id, name, declared: {} });
  expect(liveOf(obj("id-a", "a"), set)).toBe(a);
  expect(liveOf(obj("id-b", "b"), set)).toBe(b);
  expect(liveOf(obj("id-c", "a"), set)).toBeUndefined();
  // 이름으로 짝지었다면 거꾸로 짝지었을 것
  expect(liveOf(obj("id-a", "a"), { documents: [a, b] })).toBe(b);
});
