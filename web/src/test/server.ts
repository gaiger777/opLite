import { http, HttpResponse } from "msw";
import { setupServer } from "msw/node";

/** 테스트에서 API 경로의 전체 URL */
export function apiUrl(path: string) {
  return `${window.location.origin}/api/v1${path}`;
}

/**
 * 테스트마다 `server.use(...)`로 API 응답을 정한다. 정하지 않은 요청은 실패한다. 다만 Task 목록은
 * 모든 화면 바깥의 Task 색인(`renderPage`)이 읽으므로 기본으로 빈 목록이다(`server.use`가 앞선다).
 */
export const server = setupServer(
  http.get(apiUrl("/tasks"), () => HttpResponse.json({ revision: 1, items: [] })),
);
