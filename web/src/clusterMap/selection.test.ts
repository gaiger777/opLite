import { layoutCluster } from "@/clusterMap/layout";
import { toModel } from "@/clusterMap/model";
import {
  focusOf,
  focusText,
  keyOfSelection,
  type MapSelection,
  parseFocus,
  selectionOf,
} from "@/clusterMap/selection";
import { mapNode, mapVm, physicalTopology } from "@/test/clusterMap";

const nodes = [mapNode(1, "n1"), mapNode(2, "n2")];
const vms = [mapVm(1, "web", nodes[0]!.id)];
const model = toModel({ nodes, vms, physical: physicalTopology(nodes) });
const scene = layoutCluster(model, { rails: true });
const n1 = nodes[0]!.id;

it("reads the focus text", () => {
  expect(parseFocus("node:n1")).toEqual({ kind: "node", node: "n1" });
  expect(parseFocus("vm:web:1")).toEqual({ kind: "vm", vm: "web:1" });
  expect(parseFocus("port:n1/eno1")).toEqual({ kind: "port", node: "n1", port: "eno1" });
  expect(parseFocus("port:a/b/eno1")).toEqual({ kind: "port", node: "a/b", port: "eno1" });
  expect(parseFocus("rail:vmbr0")).toEqual({ kind: "rail", rail: "vmbr0" });
  for (const bad of [null, "", "n1", "node:", "port:n1", "port:/eno1", "port:n1/", "disk:x"]) {
    expect(parseFocus(bad)).toBeNull();
  }
});

it("turns scene keys into selections and back", () => {
  const cases: [string, MapSelection][] = [
    [`node:${n1}`, { kind: "node", id: n1 }],
    [`vm:${vms[0]!.id}`, { kind: "vm", id: vms[0]!.id }],
    [`port:${n1}/eno2`, { kind: "port", node: n1, port: "eno2" }],
    [
      [...scene.rails.values()].find((r) => r.carrier === "vmbr0")!.key,
      { kind: "rail", id: "vmbr0" },
    ],
  ];
  for (const [key, sel] of cases) {
    expect(selectionOf(key, scene)).toEqual(sel);
    expect(keyOfSelection(sel, scene)).toBe(key);
  }
  expect(selectionOf(`more:node:${n1}`, scene)).toEqual({ kind: "node", id: n1 });
  expect(selectionOf("unplaced", scene)).toBeNull();
  // 줄에 없는 포트는 그 노드, 없는 것은 null
  expect(keyOfSelection({ kind: "port", node: n1, port: "eno4" }, scene)).toBe(`node:${n1}`);
  expect(keyOfSelection({ kind: "rail", id: "nope" }, scene)).toBeNull();
  expect(focusText({ kind: "port", node: n1, port: "eno1" })).toBe(`port:${n1}/eno1`);
  expect(focusText({ kind: "rail", id: "vmbr0" })).toBe("rail:vmbr0");
});

it("finds the focus by name or id", () => {
  expect(focusOf({ kind: "node", node: "n2" }, model, scene)).toEqual({
    kind: "node",
    key: `node:${nodes[1]!.id}`,
  });
  expect(focusOf({ kind: "node", node: n1 }, model, scene)).toEqual({
    kind: "node",
    key: `node:${n1}`,
  });
  expect(focusOf({ kind: "port", node: "n1", port: "eno3" }, model, scene)).toEqual({
    kind: "port",
    node: `node:${n1}`,
    key: `port:${n1}/eno3`,
  });
  // 줄에 없는 포트(남는 eno4) → 그 노드
  expect(focusOf({ kind: "port", node: "n1", port: "eno4" }, model, scene)).toEqual({
    kind: "node",
    key: `node:${n1}`,
  });
  expect(focusOf({ kind: "vm", vm: "web" }, model, scene)).toMatchObject({ kind: "tile" });
  expect(focusOf({ kind: "rail", rail: "underlay" }, model, scene)).toMatchObject({
    kind: "rail",
    node: `node:${n1}`,
  });
  expect(focusOf({ kind: "node", node: "nope" }, model, scene)).toBeNull();
});
