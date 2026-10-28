import { QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { createQueryClient } from "@/api/queryClient";
import type { Task } from "@/api/types";
import { AuthProvider, useAuth } from "@/auth/AuthProvider";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import { resetUploads, startUpload } from "@/storage/upload";
import { settleTasks, trackTask } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { ADMIN } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const denied = () => HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 });

function Probe() {
  const { principal, signedOut, login, logout } = useAuth();
  return (
    <>
      <p>{principal === undefined ? "checking" : (principal?.name ?? "signed out")}</p>
      <p>{signedOut ? "explicit" : "implicit"}</p>
      <button onClick={() => void login("admin", "admin-password")}>login</button>
      <button onClick={() => void logout()}>logout</button>
    </>
  );
}

function renderAuth() {
  return render(
    <ThemeProvider>
      <QueryClientProvider client={createQueryClient()}>
        <AuthProvider>
          <Probe />
        </AuthProvider>
        <Toaster />
      </QueryClientProvider>
    </ThemeProvider>,
  );
}

it("ignores a slow session check that answers after signing in", async () => {
  let answer = () => {};
  let checks = 0;
  server.use(
    http.get(apiUrl("/auth/whoami"), () => {
      checks += 1;
      if (checks > 1) {
        return HttpResponse.json(ADMIN);
      }
      // 처음(화면을 열 때)의 확인은 로그인한 뒤에야 401로 답한다(만료된 세션).
      return new Promise<Response>((resolve) => {
        answer = () => resolve(denied());
      });
    }),
    http.post(apiUrl("/auth/login"), () => HttpResponse.json(ADMIN)),
  );
  renderAuth();
  expect(screen.getByText("checking")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "login" }));
  expect(await screen.findByText("admin")).toBeInTheDocument();

  answer();
  await act(() => new Promise((resolve) => setTimeout(resolve, 50)));
  expect(screen.getByText("admin")).toBeInTheDocument();
});

it("forgets toasts and tracked tasks when signing out", async () => {
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/auth/logout"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const task = {
    id: "019a0f3c-8a10-7000-8000-0000000000a1",
    spec: { op: { kind: "vm-start" }, target: "x", node: "n" },
    status: { state: "pending", error: null },
  } as Task;
  trackTask(task, "Start web");
  toast("something else");
  expect(await screen.findByText("Start web")).toBeInTheDocument();

  await userEvent.click(screen.getByRole("button", { name: "logout" }));
  expect(await screen.findByText("signed out")).toBeInTheDocument();
  await vi.waitFor(() => {
    expect(screen.queryByText("Start web")).not.toBeInTheDocument();
    expect(screen.queryByText("something else")).not.toBeInTheDocument();
  });
  // 로그아웃 전에 지켜보던 Task의 결과는 알리지 않는다.
  const success = vi.spyOn(toast, "success");
  settleTasks(
    { revision: 2, items: [{ ...task, status: { state: "succeeded", error: null } }] },
    i18n.t,
  );
  expect(success).not.toHaveBeenCalled();
  success.mockRestore();
});

it("forgets toasts and tracked tasks when the session ends", async () => {
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.get(apiUrl("/vms"), denied),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const task = {
    id: "019a0f3c-8a10-7000-8000-0000000000a2",
    spec: { op: { kind: "vm-start" }, target: "x", node: "n" },
    status: { state: "pending", error: null },
  } as Task;
  trackTask(task, "Start db");
  toast("other");
  expect(await screen.findByText("Start db")).toBeInTheDocument();

  await act(() => call(api.GET("/vms")).catch(() => {}));
  expect(await screen.findByText("signed out")).toBeInTheDocument();
  // 세션이 끝났으면 돌아올 주소를 남긴다.
  expect(screen.getByText("implicit")).toBeInTheDocument();
  await vi.waitFor(() => {
    expect(screen.queryByText("Start db")).not.toBeInTheDocument();
    expect(screen.queryByText("other")).not.toBeInTheDocument();
  });
  const success = vi.spyOn(toast, "success");
  settleTasks(
    { revision: 2, items: [{ ...task, status: { state: "succeeded", error: null } }] },
    i18n.t,
  );
  expect(success).not.toHaveBeenCalled();
  success.mockRestore();
});

it("stays signed out explicitly when requests get 401 during sign out", async () => {
  let answer = () => {};
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.get(apiUrl("/vms"), denied),
    // 세션이 이미 끝나 로그아웃도 401을 받는다.
    http.post(
      apiUrl("/auth/logout"),
      () =>
        new Promise<Response>((resolve) => {
          answer = () => resolve(denied());
        }),
    ),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "logout" }));
  // 로그아웃을 보낸 뒤 다른 요청이 401을 받는다.
  await act(() => call(api.GET("/vms")).catch(() => {}));
  expect(await screen.findByText("signed out")).toBeInTheDocument();
  expect(screen.getByText("explicit")).toBeInTheDocument();
  answer();
  await act(() => new Promise((resolve) => setTimeout(resolve, 50)));
  expect(screen.getByText("explicit")).toBeInTheDocument();
});

it("stops uploads when signing out", async () => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  onTestFinished(() => {
    vi.unstubAllGlobals();
  });
  server.use(
    http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/auth/logout"), () => new HttpResponse(null, { status: 204 })),
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const params = { name: "img", pool: "local", format: "qcow2", sourceFormat: "raw" } as const;
  act(() => startUpload(new File(["x"], "img.raw"), params, i18n.t));
  expect(await screen.findByText(/^Uploading img/)).toBeInTheDocument();
  // 확인을 통과해 파일을 보내는 중이다.
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());

  await userEvent.click(screen.getByRole("button", { name: "logout" }));
  expect(await screen.findByText("signed out")).toBeInTheDocument();
  expect(FakeXhr.last!.aborted).toBe(true);
  await vi.waitFor(() => expect(screen.queryByText(/img/)).not.toBeInTheDocument());
});

it("does not sign out for an upload's 401 from before signing in again", async () => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  onTestFinished(() => {
    vi.unstubAllGlobals();
    resetUploads();
  });
  let checks = 0;
  server.use(
    http.get(apiUrl("/auth/whoami"), () => {
      checks += 1;
      return HttpResponse.json(ADMIN);
    }),
    http.post(apiUrl("/auth/login"), () => HttpResponse.json(ADMIN)),
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderAuth();
  expect(await screen.findByText("admin")).toBeInTheDocument();
  const params = { name: "img", pool: "local", format: "qcow2", sourceFormat: "raw" } as const;
  act(() => startUpload(new File(["x"], "img.raw"), params, i18n.t));
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  // 다시 로그인해 세대가 바뀐 뒤에 앞 세대의 업로드가 401을 받는다.
  await userEvent.click(screen.getByRole("button", { name: "login" }));
  await vi.waitFor(() => expect(checks).toBe(2));
  act(() => FakeXhr.last!.respond(401, { code: "unauthorized", message: "no session" }));
  expect(
    await screen.findByText("Upload of img failed. Your session has ended. Sign in again."),
  ).toBeInTheDocument();
  expect(screen.getByText("admin")).toBeInTheDocument();
});
