import { http, HttpResponse } from "msw";

import { ApiError, NetworkError, api, call, setUnauthorizedHandler } from "@/api/client";
import { apiUrl, server } from "@/test/server";

afterEach(() =>
  setUnauthorizedHandler(
    () => {},
    () => 0,
  ),
);

it("turns error bodies into ApiError", async () => {
  server.use(
    http.get(apiUrl("/vms/web-01"), () =>
      HttpResponse.json({ code: "not_found", message: "no VM web-01" }, { status: 404 }),
    ),
  );
  const error = await call(api.GET("/vms/{key}", { params: { path: { key: "web-01" } } })).catch(
    (e: unknown) => e,
  );
  expect(error).toBeInstanceOf(ApiError);
  expect(error).toMatchObject({ status: 404, code: "not_found", message: "no VM web-01" });
});

it("reports 401 except for the login request itself", async () => {
  const handler = vi.fn();
  setUnauthorizedHandler(handler, () => 3);
  const denied = HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 });
  server.use(
    http.get(apiUrl("/vms"), () => denied.clone()),
    http.post(apiUrl("/auth/login"), () => denied.clone()),
  );
  await call(api.POST("/auth/login", { body: { username: "a", password: "b" } })).catch(() => {});
  expect(handler).not.toHaveBeenCalled();
  await call(api.GET("/vms")).catch(() => {});
  // 요청을 보낼 때의 인증 세대를 넘긴다.
  expect(handler).toHaveBeenCalledExactlyOnceWith(3);
});

it("treats proxy error pages as network errors", async () => {
  server.use(
    http.get(
      apiUrl("/vms"),
      () =>
        new HttpResponse("<html><body>502 Bad Gateway</body></html>", {
          status: 502,
          headers: { "Content-Type": "text/html" },
        }),
    ),
  );
  const error = await call(api.GET("/vms")).catch((e: unknown) => e);
  expect(error).toMatchObject({ status: 502, code: "network" });
});

it("ignores error bodies with the wrong field types", async () => {
  server.use(
    http.get(apiUrl("/vms"), () => HttpResponse.json({ code: 1, message: null }, { status: 500 })),
  );
  const error = await call(api.GET("/vms")).catch((e: unknown) => e);
  expect(error).toMatchObject({ status: 500, code: "internal", message: "HTTP 500" });
});

it("wraps fetch failures in NetworkError", async () => {
  server.use(http.get(apiUrl("/vms"), () => HttpResponse.error()));
  const error = await call(api.GET("/vms")).catch((e: unknown) => e);
  expect(error).toBeInstanceOf(NetworkError);
});

it("leaves aborted requests alone", async () => {
  server.use(http.get(apiUrl("/vms"), () => new Promise<never>(() => {})));
  const controller = new AbortController();
  const request = call(api.GET("/vms", { signal: controller.signal })).catch((e: unknown) => e);
  controller.abort();
  const error = await request;
  expect(error).not.toBeInstanceOf(NetworkError);
  expect(error).toMatchObject({ name: "AbortError" });
});

it("leaves requests aborted with a custom reason alone", async () => {
  server.use(http.get(apiUrl("/vms"), () => new Promise<never>(() => {})));
  const controller = new AbortController();
  const request = call(api.GET("/vms", { signal: controller.signal })).catch((e: unknown) => e);
  // 이유가 AbortError가 아니어도 취소다.
  controller.abort(new Error("left the page"));
  const error = await request;
  expect(error).not.toBeInstanceOf(NetworkError);
  expect(error).toMatchObject({ message: "left the page" });
});

it("wraps a connection dropped while reading the body in NetworkError", async () => {
  server.use(
    http.get(
      apiUrl("/vms"),
      () =>
        new HttpResponse(
          new ReadableStream({
            start(controller) {
              controller.enqueue(new TextEncoder().encode('{"revision":'));
              controller.error(new TypeError("terminated"));
            },
          }),
          { headers: { "Content-Type": "application/json" } },
        ),
    ),
  );
  const error = await call(api.GET("/vms")).catch((e: unknown) => e);
  expect(error).toBeInstanceOf(NetworkError);
  expect(error).toMatchObject({ message: "terminated" });
});
