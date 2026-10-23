import { setupServer } from "msw/node";

/** 테스트마다 `server.use(...)`로 API 응답을 정한다. 정하지 않은 요청은 실패한다. */
export const server = setupServer();

/** 테스트에서 API 경로의 전체 URL */
export function apiUrl(path: string) {
  return `${window.location.origin}/api/v1${path}`;
}
