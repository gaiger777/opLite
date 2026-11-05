import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

/** 테스트에서 API 경로의 전체 URL */
export function apiUrl(path: string) {
  return `${window.location.origin}/api/v1${path}`;
}

/**
 * 테스트마다 `server.use(...)`로 API 응답을 정한다. 정하지 않은 요청은 실패한다. 다만 Task 목록은
 * 모든 화면 바깥의 Task 색인(`renderPage`)이 읽으므로 기본으로 빈 목록이다(`server.use`가 앞선다).
 * 구독 상태도 대시보드 배너가 읽으므로 기본으로 배너가 없는 상태(오래 남은 active)다.
 */
export const server = setupServer(
  http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
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
