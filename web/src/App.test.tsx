import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse, ws } from "msw";

import { App } from "@/App";
import { safeNext } from "@/auth/RequireAuth";
import { apiUrl, server } from "@/test/server";

const admin = { user: "00000000-0000-0000-0000-000000000001", name: "admin", role: "admin" };

function signedOut() {
  let signedIn = false;
  const logins: unknown[] = [];
  server.use(
    http.get(apiUrl("/auth/whoami"), () =>
      signedIn
        ? HttpResponse.json(admin)
        : HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }),
    ),
    http.post(apiUrl("/auth/login"), async ({ request }) => {
      const body = (await request.json()) as { password: string };
      logins.push(body);
      if (body.password !== "admin-password") {
        return HttpResponse.json(
          { code: "invalid_credentials", message: "wrong" },
          { status: 401 },
        );
      }
      signedIn = true;
      return HttpResponse.json({ id: admin.user, name: "admin", role: "admin" });
    }),
    http.post(apiUrl("/auth/logout"), () => {
      signedIn = false;
      return new HttpResponse(null, { status: 204 });
    }),
    ...[
      "/vms",
      "/volumes",
      "/storage-pools",
      "/nodes",
      "/tasks",
      "/networks",
      "/snapshots",
      "/security-groups",
      "/backups",
      "/backup-jobs",
      "/vm-imports",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
    http.get(apiUrl("/version"), () => HttpResponse.json({ version: "0.9.0~ci7" })),
    // 영역은 로그인 없이 읽는다. 기본은 로컬뿐이다.
    http.get(apiUrl("/auth/realms"), () =>
      HttpResponse.json([{ name: "local", display: null, kind: "local" }]),
    ),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: admin.user, members: [] }),
    ),
  );
  return logins;
}

/** 로컬과 LDAP 영역 `corp`가 있는 서버 */
function withRealms() {
  server.use(
    http.get(apiUrl("/auth/realms"), () =>
      HttpResponse.json([
        { name: "local", display: null, kind: "local" },
        { name: "corp", display: "Corp directory", kind: "ldap" },
        { name: "lab", display: null, kind: "ldap" },
      ]),
    ),
  );
}

it("signs in, returns to the requested page and signs out", async () => {
  const logins = signedOut();
  window.history.pushState({}, "", "/somewhere");
  const user = userEvent.setup();
  render(<App />);

  await screen.findByRole("button", { name: "Sign in" });
  expect(window.location.pathname).toBe("/login");
  expect(screen.getByRole("heading", { level: 1, name: "opLite" })).toBeInTheDocument();
  // 로그인하기 전에도 서버 버전이 보인다.
  expect(await screen.findByText("opLite 0.9.0~ci7")).toBeInTheDocument();
  // 영역이 로컬뿐이면 고르지 않는다.
  expect(screen.queryByLabelText("Realm")).not.toBeInTheDocument();
  await user.type(screen.getByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "nope");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Wrong username or password.");
  expect(screen.getByLabelText("Password")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByLabelText("Password")).toHaveAccessibleDescription(
    "Wrong username or password.",
  );

  await user.clear(screen.getByLabelText("Password"));
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
  expect(window.location.pathname).toBe("/somewhere");
  // 로컬 로그인에는 영역을 보내지 않는다.
  expect(logins.at(-1)).toEqual({ username: "admin", password: "admin-password" });
  expect(await screen.findByText("lab")).toBeInTheDocument();
  expect(await screen.findByRole("status")).toHaveTextContent("Live");

  await user.click(screen.getByRole("button", { name: "Account (admin)" }));
  await user.click(await screen.findByRole("menuitem", { name: "Sign out" }));
  expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
  // 직접 로그아웃하면 다음 사용자를 앞 사용자의 화면으로 보내지 않는다.
  expect(window.location.pathname).toBe("/login");
  expect(window.location.search).toBe("");
});

it("signs in to the chosen realm", async () => {
  const logins = signedOut();
  withRealms();
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  const realm = await screen.findByLabelText("Realm");
  expect(realm).toHaveValue("local");
  expect(
    within(realm)
      .getAllByRole("option")
      .map((o) => [o.getAttribute("value"), o.textContent]),
  ).toEqual([
    ["local", "Local users"],
    ["corp", "Corp directory"],
    ["lab", "lab"],
  ]);
  await user.selectOptions(realm, "corp");
  await user.type(screen.getByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByText("lab")).toBeInTheDocument();
  expect(logins).toEqual([{ username: "alice", password: "admin-password", realm: "corp" }]);
});

it("explains realm sign-in errors", async () => {
  signedOut();
  withRealms();
  const answers = [
    HttpResponse.json(
      { code: "no_role", message: "no role is mapped for this user" },
      { status: 403 },
    ),
    HttpResponse.json(
      { code: "realm_unavailable", message: "the directory of realm corp is unavailable" },
      { status: 503 },
    ),
  ];
  server.use(http.post(apiUrl("/auth/login"), () => answers.shift()));
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  await user.selectOptions(await screen.findByLabelText("Realm"), "corp");
  await user.type(screen.getByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), "secret-123");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Your directory account has no role in opLite. Ask an administrator to map one of your groups.",
  );
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Cannot reach the directory of this realm. Try again later or ask an administrator.",
  );
});

it("says when sign-in attempts are throttled", async () => {
  signedOut();
  server.use(
    http.post(apiUrl("/auth/login"), () =>
      HttpResponse.json(
        { code: "too_many_attempts", message: "try again in 30 seconds" },
        { status: 429, headers: { "Retry-After": "30" } },
      ),
    ),
  );
  window.history.pushState({}, "", "/login");
  const user = userEvent.setup();
  render(<App />);
  await user.type(await screen.findByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Too many attempts. Try again later.");
});

it("does not show the sign-in form while checking the session", async () => {
  signedOut();
  let answer = () => {};
  server.use(
    http.get(
      apiUrl("/auth/whoami"),
      () =>
        new Promise<Response>((resolve) => {
          answer = () =>
            resolve(HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }));
        }),
    ),
  );
  window.history.pushState({}, "", "/login");
  render(<App />);
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(screen.queryByRole("button", { name: "Sign in" })).not.toBeInTheDocument();
  answer();
  expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
});

it("only returns to pages on this site", () => {
  expect(safeNext("/vms/web-01?tab=console")).toBe("/vms/web-01?tab=console");
  expect(safeNext("//evil.example")).toBe("/");
  expect(safeNext("https://evil.example")).toBe("/");
  expect(safeNext(null)).toBe("/");
  // 브라우저는 `\`를 `/`로 읽고 탭·줄바꿈을 지워 `//evil.example`로 만든다.
  expect(safeNext("/\\evil.example")).toBe("/");
  expect(safeNext("/\t/evil.example")).toBe("/");
  expect(safeNext("/\n/evil.example")).toBe("/");
  expect(safeNext("vms")).toBe("/");
  expect(safeNext("/vms#console")).toBe("/vms#console");
});
