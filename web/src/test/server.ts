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
 * 읽으므로 기본으로 `host`·`max`만 있고 기준·카탈로그가 없는 클러스터다.
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
  http.get(apiUrl("/vms/:key/guest"), () => HttpResponse.json({ state: "no_agent" })),
  http.get(apiUrl("/cpu/models"), () =>
    HttpResponse.json({
      arch: "x86_64",
      nodes: 1,
      models: [
        { name: "host", kind: "basic", usable: 1 },
        { name: "max", kind: "basic", usable: 1 },
        { name: "cluster", kind: "cluster", usable: 1 },
      ],
    }),
  ),
  http.get(apiUrl("/cpu/flags"), () => HttpResponse.json({ arch: "x86_64", nodes: 1, flags: [] })),
  http.get(apiUrl("/cpu/baseline"), () =>
    HttpResponse.json({ mode: "off", auto_flags: true, arches: [] }),
  ),
  http.get(apiUrl("/nodes/:key/cpu"), ({ params }) =>
    HttpResponse.json({ node: String(params.key), flags: [], models: [] }),
  ),
  http.get(apiUrl("/vms/:key/cpu"), () => HttpResponse.json({ spec: { cores: 1 }, nodes: [] })),
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
