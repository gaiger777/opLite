import { QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { createQueryClient } from "@/api/queryClient";
import type { Task } from "@/api/types";
import { AuthProvider, useAuth } from "@/auth/AuthProvider";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import { settleTasks, trackTask } from "@/tasks/tasks";
import { ADMIN } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const denied = () => HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 });

function Probe() {
  const { principal, login, logout } = useAuth();
  return (
    <>
      <p>{principal === undefined ? "checking" : (principal?.name ?? "signed out")}</p>
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
  settleTasks([{ ...task, status: { state: "succeeded", error: null } }], i18n.t);
  expect(success).not.toHaveBeenCalled();
  success.mockRestore();
});
