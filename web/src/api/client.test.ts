import { http, HttpResponse } from "msw";

import { ApiError, api, call, setUnauthorizedHandler } from "@/api/client";
import { apiUrl, server } from "@/test/server";

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
  setUnauthorizedHandler(handler);
  const denied = HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 });
  server.use(
    http.get(apiUrl("/vms"), () => denied.clone()),
    http.post(apiUrl("/auth/login"), () => denied.clone()),
  );
  await call(api.POST("/auth/login", { body: { username: "a", password: "b" } })).catch(() => {});
  expect(handler).not.toHaveBeenCalled();
  await call(api.GET("/vms")).catch(() => {});
  expect(handler).toHaveBeenCalledOnce();
});
