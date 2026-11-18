import {
  candidateScore,
  EXACT,
  inCidr,
  matchScore,
  PREFIX,
  rank,
  SUBSTRING,
  WORD,
  type Candidate,
} from "@/palette/rank";

it.each([
  ["web", "web", EXACT],
  ["WEB", "web", EXACT],
  ["we", "web", PREFIX],
  ["db", "web-db", WORD],
  ["nodes", "Infrastructure › Nodes", WORD],
  ["eb", "web", SUBSTRING],
  ["x", "web", 0],
  ["create pool", "Create: Storage pool", WORD],
  ["infra nodes", "Infrastructure › Nodes", WORD],
  ["create ceph", "Create: Storage pool", 0],
  ["", "web", 0],
])("scores %j against %j", (query, text, score) => {
  expect(matchScore(query, text)).toBe(score);
});

it.each([
  ["10.0.0.21", "10.0.0.0/24", true],
  ["10.0.0.21/24", "10.0.0.0/24", true],
  ["10.0.1.21", "10.0.0.0/24", false],
  ["10.0.1.21", "10.0.0.0/16", true],
  ["192.168.1.1", "0.0.0.0/0", true],
  ["10.0.0.21", "10.0.0.21/32", true],
  ["fe80::1", "10.0.0.0/8", false],
  ["10.0.0.1", "10.0.0.0/33", false],
])("checks %s in %s", (ip, cidr, inside) => {
  expect(inCidr(ip, cidr)).toBe(inside);
});

const item = (label: string, extra: Partial<Candidate> = {}): Candidate => ({
  id: label,
  group: "vms",
  label,
  ...extra,
});

it("ranks exact over prefix over word prefix over substring", () => {
  const items = [item("my-web"), item("aweb"), item("webserver"), item("web")];
  expect(rank(items, "web").map((i) => i.label)).toEqual(["web", "webserver", "my-web", "aweb"]);
});

it("puts favorites and then recent items first among equal scores", () => {
  const items = [
    item("web-1"),
    item("web-2", { recent: 3 }),
    item("web-3", { recent: 0 }),
    item("web-4", { pinned: true }),
  ];
  expect(rank(items, "web").map((i) => i.label)).toEqual(["web-4", "web-3", "web-2", "web-1"]);
  // 점수가 먼저다.
  expect(rank([item("aweb", { pinned: true }), item("web")], "web").map((i) => i.label)).toEqual([
    "web",
    "aweb",
  ]);
});

it("keeps at most eight per group and leaves other groups alone", () => {
  const items = [
    ...Array.from({ length: 12 }, (_, i) => item(`vm${i}`)),
    item("vm-node", { group: "nodes" }),
  ];
  const ranked = rank(items, "vm");
  expect(ranked.filter((i) => i.group === "vms")).toHaveLength(8);
  expect(ranked.filter((i) => i.group === "nodes")).toHaveLength(1);
});

it("shows everything in order for an empty query", () => {
  const items = [item("b"), item("a", { pinned: true })];
  expect(rank(items, " ").map((i) => i.label)).toEqual(["a", "b"]);
});

it("finds by IP prefix, MAC and CIDR but not by a stray IP substring", () => {
  const web = item("web", { addresses: ["10.0.0.21", "52:54:00:12:34:56"] });
  const db = item("db", { addresses: ["10.0.1.5"] });
  expect(candidateScore("10.0.0.21", web)).toBe(EXACT);
  expect(candidateScore("10.0.0", web)).toBe(PREFIX);
  expect(candidateScore("52:54:00:12", web)).toBe(PREFIX);
  expect(candidateScore("0.0.21", web)).toBe(0);
  expect(rank([web, db], "10.0.0.0/24").map((i) => i.label)).toEqual(["web"]);
  expect(rank([web, db], "10.0.0.0/16").map((i) => i.label)).toEqual(["web", "db"]);
});

it("matches keywords at most as a word prefix", () => {
  expect(candidateScore("node-a", item("web", { keywords: ["node-a"] }))).toBe(WORD);
});
