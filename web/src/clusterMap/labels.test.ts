import type { AlertView } from "@/alerts/alerts";
import { layoutCluster } from "@/clusterMap/layout";
import { linkText, mapLabels, speedText } from "@/clusterMap/labels";
import { toModel } from "@/clusterMap/model";
import i18n from "@/i18n";
import { countReads } from "@/test/topology";
import { load, mapNode, mapVm, nodeId, physicalTopology, placement, vmId } from "@/test/clusterMap";

const n3 = mapNode(3, "n3", { labels: { zone: "rack-a" }, spec: { maintenance: true } });
const n5 = mapNode(5, "n5", { labels: { zone: "rack-a" } });
const w = mapNode(9, "w1", { labels: { zone: "rack-a" }, spec: { witness: true } });
const off = mapNode(7, "n7", { status: { online: false, shutdown: { action: "freeze", at: 1 } } });
const task = "019a0f3c-0000-7000-8c00-000000000001";

function input() {
  const alert = (severity: AlertView["severity"]): AlertView => ({
    rule: "r",
    rule_name: "r",
    condition: "c",
    severity,
    since: 0,
    state: "firing",
    subject: { kind: "node", id: n3.id, name: "n3" },
  });
  return {
    nodes: [n3, n5, w, off],
    vms: [
      mapVm(1, "web", n3.id, "migrating", {
        spec: { power: "running", ha: true },
        status: { node: n3.id, migration: { node: n5.id, task } },
      }),
      mapVm(2, "db", n3.id, "failed", { status: { rule_violations: ["apart"] } }),
      mapVm(3, "api", n3.id, "running"),
      mapVm(4, "old", off.id, "running"),
      mapVm(5, "new", null, "stopped"),
    ],
    placement: placement([
      load(n3, { cpu_use_pct: 724, mem_use_pct: 638, mem_allocated_mib: 18350, vms: 4 }),
      load(n5, { age_secs: undefined }),
    ]),
    cluster: { name: "prod", leader: n5.id },
    alerts: [alert("warning"), alert("critical")],
    progress: new Map([[task, { stage: "copying-disks", done_bytes: 62, total_bytes: 100 }]]),
    vmsLimited: true,
  };
}

it("names nodes, tiles and arrows for screen readers in English", () => {
  const model = toModel(input());
  const scene = layoutCluster(model);
  const labels = mapLabels(i18n.getFixedT("en"), model, scene);
  expect(labels.get(`node:${n3.id}`)).toBe(
    "Node n3, group rack-a, online, draining 1/2, CPU 72%, memory 64%, memory allocated 112%, " +
      "3 VMs (1 running, 1 changing, 1 failed), 2 you can't see, 2 alerts",
  );
  expect(labels.get(`node:${n5.id}`)).toBe(
    "Node n5, group rack-a, online, leader, load unknown, no VMs",
  );
  expect(labels.get(`node:${w.id}`)).toBe("Witness node w1, group rack-a, online");
  expect(labels.get(`node:${off.id}`)).toBe(
    "Node n7, no group, offline, frozen, load unknown, 1 VM (1 running)",
  );
  expect(labels.get(`vm:${vmId(1)}`)).toBe("VM web, Migrating, node n3, HA, moving to n5 62%");
  expect(labels.get(`vm:${vmId(2)}`)).toBe("VM db, Failed, node n3, breaks 1 placement rule");
  expect(labels.get(`vm:${vmId(4)}`)).toBe(
    "VM old, Running, node n7, node offline, last known place",
  );
  expect(labels.get(`vm:${vmId(5)}`)).toBe("VM new, Stopped, not placed");
  expect(labels.get(`ghost:${vmId(1)}`)).toBe("VM web arriving from n3");
  expect(labels.get(`hidden:node:${n3.id}`)).toBe("2 VMs on n3 you can't see");
  expect(labels.get("unplaced")).toBe("Not placed, 1 VM");
  expect(labels.get(`move:${vmId(1)}`)).toBe("web → n5 · 62%");
});

it("names the same things in Korean", () => {
  const model = toModel(input());
  const scene = layoutCluster(model);
  const labels = mapLabels(i18n.getFixedT("ko"), model, scene);
  expect(labels.get(`node:${n3.id}`)).toBe(
    "노드 n3, 판 rack-a, 온라인, 비우는 중 1/2, CPU 72%, 메모리 64%, 메모리 할당 112%, " +
      "VM 3개(켜짐 1, 바뀌는 중 1, 실패 1), 볼 수 없음 2, 경보 2",
  );
  expect(labels.get(`vm:${vmId(1)}`)).toBe(
    "VM web, 마이그레이션 중, 노드 n3, HA, n5(으)로 옮기는 중 62%",
  );
  expect(labels.get(`node:${off.id}`)).toBe(
    "노드 n7, 영역 없음, 오프라인, 얼림, 부하 모름, VM 1개(켜짐 1)",
  );
  expect(labels.get("unplaced")).toBe("배치 안 됨, VM 1개");
  expect(labels.get(`move:${vmId(1)}`)).toBe("web → n5 · 62%");
});

it("writes arrow labels for progress, stage and recommendations", () => {
  const t = i18n.getFixedT("en");
  const name = (key: string) => key.replace("node:", "");
  const base = {
    key: "k",
    kind: "migration" as const,
    vm: "v",
    vmName: "web",
    ends: ["node:a", "node:b"] as [string, string],
    from: { x: 0, y: 0 },
    to: { x: 1, y: 1 },
    lift: 0,
    progress: null,
    stage: null,
    task: null,
    recommendation: null,
  };
  expect(linkText(t, base, name)).toBe("web → b");
  // 메모리 단계는 번역한 글, 모르는 단계의 날 id는 보이지 않는다
  expect(linkText(t, { ...base, stage: "migrating-memory" }, name)).toBe(
    "web → b · Migrating memory",
  );
  expect(linkText(i18n.getFixedT("ko"), { ...base, stage: "migrating-memory" }, name)).toBe(
    "web → b · 메모리 옮기는 중",
  );
  expect(linkText(t, { ...base, stage: "disks" }, name)).toBe("web → b");
  expect(linkText(t, { ...base, kind: "recommendation" }, name)).toBe("Recommended: web a → b");
  expect(linkText(i18n.getFixedT("ko"), { ...base, kind: "recommendation" }, name)).toBe(
    "추천: web a → b",
  );
});

it("names more and hidden tiles past 64 cells", () => {
  const n = mapNode(1, "n1");
  const vms = Array.from({ length: 70 }, (_, i) =>
    mapVm(i, `v${String(i).padStart(2, "0")}`, n.id),
  );
  const model = toModel({
    nodes: [n],
    vms,
    placement: placement([load(n, { vms: 72 })]),
    vmsLimited: true,
  });
  const labels = mapLabels(i18n.getFixedT("en"), model, layoutCluster(model));
  expect(labels.get(`more:node:${nodeId(1)}`)).toBe("8 more VMs on n1");
  expect(labels.get(`hidden:node:${nodeId(1)}`)).toBe("2 VMs on n1 you can't see");
});

it("reads the VM list once per picture, not once per tile", () => {
  const n = mapNode(1, "n1");
  const vms = Array.from({ length: 2000 }, (_, i) => mapVm(i, `v${i}`, n.id));
  const model = toModel({ nodes: [n], vms });
  const scene = layoutCluster(model);
  const counted = countReads(model.vms);
  mapLabels(i18n.getFixedT("en"), { ...model, vms: counted.items }, scene);
  expect(counted.reads()).toBeLessThanOrEqual(2 * 2000);
});

it("names ports and rails in both languages", async () => {
  const nodes = [n3, n5];
  const model = toModel({
    nodes,
    vms: [],
    physical: physicalTopology(nodes, { n3: ["eno1", "eno2"], n5: ["eno3"] }),
  });
  const scene = layoutCluster(model, { rails: true });
  const rail = (carrier: string) =>
    [...scene.rails.values()].find((r) => r.carrier === carrier)!.key;
  const en = mapLabels(i18n.t, model, scene);
  expect(en.get(`port:${n3.id}/eno1`)).toBe(
    "Port eno1 on n3, down, bond bond0, 10 Gb/s, switch sw1 port Gi1/0/1, " +
      "cuts off networks 2, VMs 3",
  );
  expect(en.get(`port:${n5.id}/eno3`)).toBe(
    "Port eno3 on n5, down, 10 Gb/s, cuts off networks 1, VMs 2",
  );
  expect(en.get(rail("vmbr0"))).toBe("Rail vmbr0, 2 networks, 2 nodes, 1 down");
  expect(en.get(`node:${n3.id}`)).toMatch(/, 2 link faults$/);
  await i18n.changeLanguage("ko");
  const ko = mapLabels(i18n.t, model, scene);
  expect(ko.get(`port:${n3.id}/eno2`)).toBe(
    "노드 n3의 포트 eno2, 끊김, 본딩 bond0, 10 Gb/s, 네트워크 2개·VM 3개 끊김",
  );
  expect(ko.get(rail("underlay"))).toBe("레일 underlay(VXLAN), 네트워크 1개, 노드 2개, 끊김 1");
  expect(ko.get(`node:${n3.id}`)).toMatch(/, 링크 장애 2$/);
  expect([speedText(25000), speedText(100), speedText(0)]).toEqual(["25 Gb/s", "100 Mb/s", null]);
});
