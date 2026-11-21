// 토폴로지 시험의 응답(계획 67) — 배치 시험과 화면 시험이 함께 쓴다.
import type { Topology } from "@/topology/topology";

export const topoId = (kind: string, n: number) =>
  `019a0f3c-0000-7000-8${kind}-${String(n).padStart(12, "0")}`;

/**
 * 서버 `NetworkTopology` 모양(빈 `folder`·`mtu`·`physnet`·`vlan`·`migration`·`external`·`node`·`vip`·`vm`은 빠진다,
 * `hidden`은 늘 다섯 칸): 공급자 ext, 라우터 r1(t1·t2, 바깥 ext), 라우터 없는 오버레이 t3, 단순 lan(t1로 옮기는 중), VM 셋, LB 하나,
 * 유동 IP 하나.
 */
export function sampleGraph(): Topology {
  const EXT = topoId("001", 1);
  const T1 = topoId("001", 2);
  const T2 = topoId("001", 3);
  const T3 = topoId("001", 4);
  const LAN = topoId("001", 5);
  const WEB = topoId("002", 1);
  const DB = topoId("002", 2);
  const OLD = topoId("002", 3);
  return {
    networks: [
      { id: T2, name: "t2", kind: "sdn", cidrs: ["10.20.0.0/24"] },
      { id: EXT, name: "ext", kind: "provider", cidrs: ["203.0.113.0/24"], physnet: "ext" },
      { id: T1, name: "t1", kind: "sdn", cidrs: ["10.10.0.0/24"], mtu: 1442 },
      { id: T3, name: "t3", kind: "sdn", cidrs: [] },
      {
        id: LAN,
        name: "lan",
        kind: "bridge",
        cidrs: [],
        migration: {
          id: topoId("00d", 1),
          name: "lan-to-t1",
          target: T1,
          phase: "running",
          moved: 1,
          total: 2,
        },
      },
    ],
    routers: [
      {
        id: topoId("00a", 1),
        name: "r1",
        interfaces: [T1, T2],
        external: { network: EXT, ips: ["203.0.113.2"] },
        gateway: { node: "node-a", state: "active" },
      },
    ],
    load_balancers: [
      {
        id: topoId("00c", 1),
        name: "web-lb",
        network: T1,
        vip: "10.10.0.50",
        external_ip: "203.0.113.130",
        members: [{ vm: WEB, ip: "10.10.0.2", health: "online" }],
      },
    ],
    floating_ips: [
      {
        id: topoId("00b", 1),
        address: "203.0.113.120",
        network: EXT,
        vm: DB,
        state: "distributed",
      },
    ],
    vms: [
      {
        id: WEB,
        name: "web",
        power: "running",
        node: "node-a",
        nics: [
          { network: T1, ips: ["10.10.0.2"] },
          { network: T2, ips: ["10.20.0.2"] },
        ],
      },
      { id: DB, name: "db", power: "stopped", nics: [{ network: T1, ips: ["10.10.0.3"] }] },
      { id: OLD, name: "old", power: "running", node: "node-a", nics: [{ network: LAN, ips: [] }] },
    ],
    hidden: { networks: 0, routers: 0, load_balancers: 0, floating_ips: 0, vms: 0 },
  };
}

/**
 * 배열을 감싸 원소 읽기(번호로 읽기 — `for…of`·`map`·`filter`도 이것으로 읽는다)를 센다. 성능 시험이 벽시계 대신 일의 양을
 * 센다(리뷰 2 웹 #4 — 시험 기계의 부하에 흔들리지 않게).
 */
export function countReads<T>(items: T[]): { items: T[]; reads: () => number } {
  let reads = 0;
  const proxy = new Proxy(items, {
    get(target, key, receiver) {
      if (typeof key === "string" && /^\d+$/.test(key)) {
        reads += 1;
      }
      return Reflect.get(target, key, receiver) as unknown;
    },
  });
  return { items: proxy, reads: () => reads };
}
