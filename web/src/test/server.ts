import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

/** 테스트에서 API 경로의 전체 URL */
export function apiUrl(path: string) {
  return `${window.location.origin}/api/v1${path}`;
}

/**
 * 테스트마다 `server.use(...)`로 API 응답을 정한다. 정하지 않은 요청은 실패한다. 다만 Task 목록은
 * 모든 화면 바깥의 Task 색인(`renderPage`)이 읽으므로 기본으로 빈 목록이다(`server.use`가 앞선다).
 * 구독 상태도 대시보드 배너가 읽으므로 기본으로 배너가 없는 상태(오래 남은 active)다. 관리 Ceph도 노드·풀
 * 화면이 읽으므로 기본으로 없다. 게스트 정보도 VM 요약이 읽으므로 기본으로 에이전트가 없다. 대시보드 위젯이 읽는
 * 클러스터 지표 모으기와 활성 경보도 기본으로 비어 있다. 내 권한(`/permissions/mine`)은 기본으로 없어(404)
 * 화면이 전역 역할로 본다. 폴더 목록도 기본으로 비어 있다. CPU 질의(계획 63)는 VM 폼·상세·노드·클러스터·설정 화면이
 * 읽으므로 기본으로 `host`·`max`만 있고 기준·카탈로그가 없는 클러스터다. NUMA 질의(계획 64)도 노드 화면이 읽으므로 기본으로
 * 토폴로지를 알린 노드가 없다.
 */
export const server = setupServer(
  http.get(apiUrl("/permissions/mine"), () =>
    HttpResponse.json({ code: "not_found", message: "not found" }, { status: 404 }),
  ),
  http.get(apiUrl("/folders"), () => HttpResponse.json({ revision: 1, items: [] })),
  http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
  http.get(apiUrl("/ceph"), () =>
    HttpResponse.json({ code: "not_found", message: "no Ceph cluster is set up" }, { status: 404 }),
  ),
  // SDN(계획 65): 설정 전 클러스터의 `GET /sdn`(서버 `SdnView::new(None, …)` — `phase` 등 빈 칸은 빠지고 노드마다
  // chassis 행이 있지만 노드 이름은 테스트마다 다르므로 비운다). 네트워크 만들기 대화상자가 읽는다.
  http.get(apiUrl("/sdn"), () => HttpResponse.json({ centrals: [], chassis: [], tasks: [] })),
  http.get(apiUrl("/vms/:key/guest"), () => HttpResponse.json({ state: "no_agent" })),
  // CPU(계획 63): x86_64 노드 하나(카탈로그를 아직 알리지 않음), 기준 끔인 클러스터의 관리자 응답 모양이다. 기준이
  // 없으면 `cluster`에 `current`가 없다(리뷰 1 웹 #1 — 있으면 새 VM이 `cluster`로 시작한다). 다만 노드마다의 목록
  // (`/cpu/models`·`/vms/{key}/cpu`의 `nodes`)은 비워 두었다 — 노드 이름은 테스트마다 다르므로 "켤 수 없는 노드"를
  // 보는 테스트는 제 응답을 둔다(리뷰 2 웹 #6). `/nodes/{key}/cpu`의 `node`는 서버와 달리 경로 키를 그대로 돌려준다.
  http.get(apiUrl("/cpu/models"), () =>
    HttpResponse.json({
      arch: "x86_64",
      nodes: 1,
      models: [
        { name: "host", kind: "basic", usable: 1 },
        { name: "max", kind: "basic", usable: 1 },
        { name: "cluster", kind: "cluster", usable: 1 },
        { name: "x86-64-v2", kind: "level", usable: 0 },
        { name: "x86-64-v2-aes", kind: "level", usable: 0 },
        { name: "x86-64-v3", kind: "level", usable: 0 },
        { name: "x86-64-v4", kind: "level", usable: 0 },
      ],
    }),
  ),
  http.get(apiUrl("/cpu/flags"), () =>
    HttpResponse.json({ arch: "x86_64", nodes: 0, turn_on_restricted: true, flags: [] }),
  ),
  http.get(apiUrl("/cpu/baseline"), () =>
    HttpResponse.json({
      mode: "off",
      auto_flags: true,
      arches: [{ arch: "x86_64", pinned: false, cluster_vms: 0 }],
    }),
  ),
  http.get(apiUrl("/nodes/:key/cpu"), ({ params }) =>
    HttpResponse.json({ node: String(params.key), arch: "x86_64", models: [], flags: [] }),
  ),
  http.get(apiUrl("/vms/:key/cpu"), () =>
    HttpResponse.json({
      spec: { sockets: 1, cores: 1, model: "host" },
      resolved: { model: "host", source: "spec" },
      nodes: [],
    }),
  ),
  // NUMA(계획 64): 토폴로지를 알린 노드가 없는 클러스터(macOS·옛 노드와 같은 모양). 노드 화면·VM 폼이 읽는다.
  http.get(apiUrl("/topology/usage"), () => HttpResponse.json({ nodes: [] })),
  http.get(apiUrl("/nodes/:key/topology"), ({ params }) =>
    HttpResponse.json({ node: String(params.key), host_nodes: [], vms: [], warnings: [] }),
  ),
  http.get(apiUrl("/vms/:key/numa"), () =>
    HttpResponse.json({ spec: { policy: "shared" }, wanted: "none", nodes: [] }),
  ),
  http.get(apiUrl("/metrics/cluster"), () =>
    HttpResponse.json({
      tier: "raw5s",
      step_ms: 5000,
      start: 0,
      columns: {},
      nodes: [],
      node_count: 0,
      top: [],
      pools: [],
      unreachable: [],
      unsupported: [],
    }),
  ),
  http.get(apiUrl("/alerts"), () =>
    HttpResponse.json({ alerts: [], unreachable: [], unsupported: [] }),
  ),
  http.get(apiUrl("/subscription"), () =>
    HttpResponse.json({
      state: "active",
      id: "sub-1",
      customer: "Example",
      level: "standard",
      nodes: 10,
      used_nodes: 1,
      expires_at: 4102444800,
    }),
  ),
);
