import createClient, { type Middleware } from "openapi-fetch";

import type { components, paths } from "@/api/schema";

/** API가 돌려준 오류(`ErrorBody`). 네트워크 오류는 그대로 `TypeError`다. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly details?: unknown,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

let onUnauthorized = () => {};

/** 로그인 밖의 요청이 401을 받으면(세션 만료, opld 재시작, 비밀번호 변경) 부른다. */
export function setUnauthorizedHandler(handler: () => void) {
  onUnauthorized = handler;
}

const unauthorized: Middleware = {
  onResponse({ request, response }) {
    if (response.status === 401 && !new URL(request.url).pathname.endsWith("/auth/login")) {
      onUnauthorized();
    }
  },
};

/** 같은 출처의 `/api/v1`. 세션 쿠키는 브라우저가 붙인다. */
export const api = createClient<paths>({
  baseUrl: `${window.location.origin}/api/v1`,
  // 부를 때마다 전역 fetch를 찾는다(테스트의 MSW가 나중에 바꾼다).
  fetch: (request) => globalThis.fetch(request),
});
api.use(unauthorized);

type ErrorBody = components["schemas"]["ErrorBody"];

function isErrorBody(body: unknown): body is ErrorBody {
  return typeof body === "object" && body !== null && "code" in body && "message" in body;
}

export function toApiError(status: number, body: unknown): ApiError {
  if (isErrorBody(body)) {
    return new ApiError(status, body.code, body.message, body.details);
  }
  return new ApiError(status, "internal", `HTTP ${status}`);
}

/** openapi-fetch의 결과에서 데이터를 꺼낸다. 오류 응답이면 `ApiError`를 던진다. */
export async function call<T>(
  request: Promise<{ data?: T; error?: unknown; response: Response }>,
): Promise<T> {
  const { data, error, response } = await request;
  if (!response.ok) {
    throw toApiError(response.status, error);
  }
  return data as T;
}
