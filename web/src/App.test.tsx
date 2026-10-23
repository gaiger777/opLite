import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse, ws } from "msw";

import { App } from "@/App";
import { safeNext } from "@/auth/RequireAuth";
import { apiUrl, server } from "@/test/server";

const admin = { user: "00000000-0000-0000-0000-000000000001", name: "admin", role: "admin" };

function signedOut() {
  let signedIn = false;
  server.use(
    http.get(apiUrl("/auth/whoami"), () =>
      signedIn
        ? HttpResponse.json(admin)
        : HttpResponse.json({ code: "unauthorized", message: "login" }, { status: 401 }),
    ),
    http.post(apiUrl("/auth/login"), async ({ request }) => {
      const body = (await request.json()) as { password: string };
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
    ...["/vms", "/volumes", "/storage-pools", "/nodes", "/tasks", "/networks"].map((path) =>
      http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] })),
    ),
    ws.link(`ws://${window.location.host}/api/v1/watch`).addEventListener("connection", () => {}),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: admin.user, members: [] }),
    ),
  );
}

it("signs in, returns to the requested page and signs out", async () => {
  signedOut();
  window.history.pushState({}, "", "/somewhere");
  const user = userEvent.setup();
  render(<App />);

  await screen.findByRole("button", { name: "Sign in" });
  expect(window.location.pathname).toBe("/login");
  await user.type(screen.getByLabelText("Username"), "admin");
  await user.type(screen.getByLabelText("Password"), "nope");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Wrong username or password.");

  await user.clear(screen.getByLabelText("Password"));
  await user.type(screen.getByLabelText("Password"), "admin-password");
  await user.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("heading", { name: "Page not found" })).toBeInTheDocument();
  expect(window.location.pathname).toBe("/somewhere");
  expect(await screen.findByText("lab")).toBeInTheDocument();
  expect(await screen.findByRole("status")).toHaveTextContent("Live");

  await user.click(screen.getByRole("button", { name: "Account" }));
  await user.click(await screen.findByRole("menuitem", { name: "Sign out" }));
  expect(await screen.findByRole("button", { name: "Sign in" })).toBeInTheDocument();
});

it("only returns to pages on this site", () => {
  expect(safeNext("/vms/web-01?tab=console")).toBe("/vms/web-01?tab=console");
  expect(safeNext("//evil.example")).toBe("/");
  expect(safeNext("https://evil.example")).toBe("/");
  expect(safeNext(null)).toBe("/");
});
