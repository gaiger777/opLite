// 계획 62 로그인 둘째 단계: 코드·복구 코드, 강제 등록, 다른 요청의 401 `second_factor_required`.
import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { createMemoryRouter, RouterProvider } from "react-router";

import { createQueryClient } from "@/api/queryClient";
import { AuthProvider } from "@/auth/AuthProvider";
import { RequireAuth } from "@/auth/RequireAuth";
import { LoginPage } from "@/pages/LoginPage";
import { apiUrl, server } from "@/test/server";

const ALICE = { user: "00000000-0000-0000-0000-0000000000a1", name: "alice", role: "operator" };
const PROMPT = { methods: ["totp", "recovery"], enroll: false, expires_at: 4102444800 };

function app(url = "/login") {
  const router = createMemoryRouter(
    [
      { path: "/login", element: <LoginPage /> },
      { element: <RequireAuth />, children: [{ path: "/", element: <p>home</p> }] },
    ],
    { initialEntries: [url] },
  );
  render(
    <QueryClientProvider client={createQueryClient()}>
      <AuthProvider>
        <RouterProvider router={router} />
      </AuthProvider>
    </QueryClientProvider>,
  );
  return router;
}

/** 서버: 비밀번호가 맞으면 대기 세션, 둘째 단계가 맞으면 정식 세션 */
function serverWith(prompt: object = PROMPT) {
  let state: "none" | "pending" | "full" = "none";
  const seconds: unknown[] = [];
  server.use(
    http.get(apiUrl("/version"), () => HttpResponse.json({ version: "1" })),
    http.get(apiUrl("/auth/realms"), () => HttpResponse.json([])),
    http.get(apiUrl("/auth/whoami"), () =>
      state === "full"
        ? HttpResponse.json(ALICE)
        : state === "pending"
          ? HttpResponse.json(
              { code: "second_factor_required", message: "second factor" },
              { status: 401 },
            )
          : HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }),
    ),
    http.post(apiUrl("/auth/login"), () => {
      state = "pending";
      return HttpResponse.json({
        id: ALICE.user,
        name: "alice",
        role: "operator",
        revision: 1,
        second_factor: prompt,
      });
    }),
    http.post(apiUrl("/auth/login/second-factor"), async ({ request }) => {
      const body = (await request.json()) as { totp?: string; recovery?: string };
      seconds.push(body);
      if (body.totp === "123456" || body.recovery === "aaaa-bbbb-cccc-dddd") {
        state = "full";
        return HttpResponse.json({ id: ALICE.user, name: "alice", role: "operator", revision: 1 });
      }
      return HttpResponse.json(
        { code: "invalid_second_factor", message: "invalid" },
        { status: 401 },
      );
    }),
    http.post(apiUrl("/auth/logout"), () => {
      state = "none";
      return new HttpResponse(null, { status: 204 });
    }),
  );
  return { seconds, setState: (s: typeof state) => (state = s) };
}

async function password(user: ReturnType<typeof userEvent.setup>) {
  await user.type(await screen.findByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), "pw");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
}

it("asks for the authenticator code after the password and signs in", async () => {
  const { seconds } = serverWith();
  const user = userEvent.setup();
  const router = app("/login?next=%2F");
  await password(user);
  const code = await screen.findByLabelText("Authentication code");
  await user.type(code, "000000");
  await user.click(screen.getByRole("button", { name: "Verify" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("That code is not valid.");
  await user.clear(code);
  await user.type(code, "123 456");
  await user.click(screen.getByRole("button", { name: "Verify" }));
  expect(await screen.findByText("home")).toBeInTheDocument();
  expect(router.state.location.pathname).toBe("/");
  expect(seconds).toEqual([{ totp: "000000" }, { totp: "123456" }]);
});

it("takes a recovery code instead", async () => {
  const { seconds } = serverWith();
  const user = userEvent.setup();
  app();
  await password(user);
  await user.click(await screen.findByRole("button", { name: "Use a recovery code" }));
  await user.type(screen.getByLabelText("Recovery code"), "aaaa-bbbb-cccc-dddd");
  await user.click(screen.getByRole("button", { name: "Verify" }));
  expect(await screen.findByText("home")).toBeInTheDocument();
  expect(seconds).toEqual([{ recovery: "aaaa-bbbb-cccc-dddd" }]);
});

it("goes back to the second step when a request says second_factor_required", async () => {
  const { setState } = serverWith();
  setState("pending");
  // 새로 고친 브라우저: 쿠키는 대기 세션이다.
  app("/");
  expect(await screen.findByLabelText("Authentication code")).toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole("button", { name: "Back to sign-in" }));
  expect(await screen.findByLabelText("Username")).toBeInTheDocument();
});

it("shows the forced enrollment with a QR code and recovery codes", async () => {
  serverWith({ methods: [], enroll: true, expires_at: 4102444800 });
  let enrolled = false;
  server.use(
    http.get(apiUrl("/auth/2fa"), () =>
      HttpResponse.json({
        totp: false,
        totp_pending: false,
        recovery_codes: 0,
        passkeys: [],
        required: true,
        enrolling: true,
        available: true,
      }),
    ),
    http.post(apiUrl("/auth/2fa/totp"), () =>
      HttpResponse.json({
        secret: "JBSWY3DPEHPK3PXP",
        uri: "otpauth://totp/Orbivisor:alice?secret=JBSWY3DPEHPK3PXP&issuer=Orbivisor",
      }),
    ),
    http.post(apiUrl("/auth/2fa/totp/confirm"), () => {
      enrolled = true;
      return HttpResponse.json({ codes: ["aaaa-bbbb-cccc-dddd", "eeee-ffff-0000-1111"] });
    }),
    http.get(apiUrl("/auth/whoami"), () =>
      enrolled
        ? HttpResponse.json(ALICE)
        : HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }),
    ),
  );
  const user = userEvent.setup();
  app();
  await password(user);
  expect(await screen.findByText("Set up two-step verification")).toBeInTheDocument();
  // IP 주소 접속(rp_id 없음)이라 패스키는 없다.
  expect(screen.queryByRole("button", { name: "Add a passkey" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Set up an authenticator app" }));
  expect(
    await screen.findByRole("img", { name: "QR code for the authenticator app" }),
  ).toBeInTheDocument();
  expect(screen.getByTestId("totp-secret")).toHaveTextContent("JBSWY3DPEHPK3PXP");
  await user.type(screen.getByLabelText("Code from the app"), "123456");
  await user.click(screen.getByRole("button", { name: "Confirm" }));
  const codes = await screen.findByRole("list", { name: "Recovery codes" });
  expect(codes).toHaveTextContent("aaaa-bbbb-cccc-dddd");
  await user.click(screen.getByRole("button", { name: "I saved them" }));
  await waitFor(() => expect(screen.getByText("home")).toBeInTheDocument());
});
