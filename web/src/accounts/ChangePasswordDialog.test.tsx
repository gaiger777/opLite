import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { AppLayout } from "@/layout/AppLayout";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);

async function openDialog() {
  await userEvent.click(await screen.findByRole("button", { name: "Account (admin)" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change my password" }));
  return screen.findByRole("dialog", { name: "Change my password" });
}

it("says when the current password is wrong", async () => {
  let puts = 0;
  server.use(
    http.put(apiUrl(`/users/${ADMIN.user}`), () => {
      puts += 1;
      return HttpResponse.json(
        { code: "invalid_credentials", message: "the current password is wrong" },
        { status: 403 },
      );
    }),
  );
  renderPage(<AppLayout />);
  const dialog = await openDialog();
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("Enter your current password.")).toBeVisible();
  expect(puts).toBe(0);

  await userEvent.type(within(dialog).getByLabelText("Current password"), "old-secret");
  await userEvent.type(within(dialog).getByLabelText("New password"), "new-secret");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "new-secret");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByText("The current password is wrong.")).toBeVisible();
  const current = within(dialog).getByLabelText("Current password");
  expect(current).toHaveAttribute("aria-invalid", "true");
  await waitFor(() => expect(current).toHaveFocus());
  expect(puts).toBe(1);
});

it("changes the password and signs out", async () => {
  let body: unknown;
  let signedOut = false;
  server.use(
    http.put(apiUrl(`/users/${ADMIN.user}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ id: ADMIN.user, name: "admin", role: "admin", revision: 2 });
    }),
    http.post(apiUrl("/auth/logout"), () => {
      signedOut = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { router } = renderPage(<AppLayout />);
  const dialog = await openDialog();
  await userEvent.type(within(dialog).getByLabelText("Current password"), "old-secret");
  await userEvent.type(within(dialog).getByLabelText("New password"), "new-secret");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "new-secret");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    await screen.findByText("Your password was changed. Sign in with the new password."),
  ).toBeInTheDocument();
  expect(body).toEqual({ current_password: "old-secret", password: "new-secret" });
  expect(signedOut).toBe(true);
  expect(router.state.location.pathname).toBe("/login");
  // 스스로 로그아웃했으므로 돌아올 주소(`?next=`)를 붙이지 않는다.
  expect(router.state.location.search).toBe("");
});
