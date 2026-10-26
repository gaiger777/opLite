import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { passwordErrors } from "@/accounts/password";
import { UsersPage } from "@/accounts/UsersPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const ME = { id: ADMIN.user, name: "admin", role: "admin", revision: 1 };
const KIM = {
  id: "00000000-0000-0000-0000-0000000000k1",
  name: "kim",
  role: "viewer",
  revision: 2,
};

function users(list: unknown[]) {
  let items = list;
  server.use(http.get(apiUrl("/users"), () => HttpResponse.json({ revision: 1, items })));
  return (next: unknown[]) => {
    items = next;
  };
}

it("checks new passwords like the server", () => {
  expect(passwordErrors("short", "short")).toEqual({ password: "accounts.errors.password" });
  expect(passwordErrors("long enough", "long enougH")).toEqual({
    confirm: "accounts.errors.mismatch",
  });
  // 서버는 바이트가 아니라 글자를 센다.
  expect(passwordErrors("비밀번호여덟글자", "비밀번호여덟글자")).toEqual({});
});

it("lists users and keeps admins from deleting themselves", async () => {
  users([ME, KIM]);
  renderPage(<UsersPage />);
  const table = await screen.findByRole("table", { name: "Users" });
  const me = (await within(table).findByText("admin")).closest("tr")!;
  expect(me).toHaveTextContent("(you)");
  await userEvent.click(within(me).getByRole("button", { name: "Actions for admin" }));
  expect(await screen.findByRole("menuitem", { name: "Delete" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByRole("menuitem", { name: "Reset password" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(within(table).getByText("kim").closest("tr")).toHaveTextContent("Viewer");
});

it("creates a user and reads the list again", async () => {
  const change = users([ME]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/users"), async ({ request }) => {
      body = await request.json();
      change([ME, KIM]);
      return HttpResponse.json(KIM, { status: 201 });
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a user" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "kim");
  await userEvent.selectOptions(within(dialog).getByLabelText("Role"), "operator");
  await userEvent.type(within(dialog).getByLabelText("New password"), "secret-123");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "secret-12");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("The passwords do not match.")).toBeVisible();

  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "3");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created user kim.")).toBeInTheDocument();
  expect(body).toEqual({ name: "kim", role: "operator", password: "secret-123" });
  expect(await screen.findByText("kim")).toBeInTheDocument();
});

it("changes a role and shows why the server refused", async () => {
  users([ME, KIM]);
  const sent: unknown[] = [];
  server.use(
    http.put(apiUrl(`/users/${ME.id}`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(
        { code: "last_admin", message: "the last admin cannot give up the admin role" },
        { status: 409 },
      );
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for admin" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change role" }));
  const dialog = await screen.findByRole("dialog", { name: "Change the role of admin" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Role"), "viewer");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The last administrator cannot give up the administrator role.",
  );
  expect(sent).toEqual([{ role: "viewer" }]);
});

it("returns focus to the row menu after cancelling a dialog", async () => {
  users([ME, KIM]);
  renderPage(<UsersPage />);
  const trigger = await screen.findByRole("button", { name: "Actions for kim" });
  await userEvent.click(trigger);
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change role" }));
  const dialog = await screen.findByRole("dialog", { name: "Change the role of kim" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(trigger).toHaveFocus());

  await userEvent.click(trigger);
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Cancel" }),
  );
  await waitFor(() => expect(trigger).toHaveFocus());
});

it("resets another user's password", async () => {
  users([ME, KIM]);
  let body: unknown;
  server.use(
    http.put(apiUrl(`/users/${KIM.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(KIM);
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for kim" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Reset password" }));
  const dialog = await screen.findByRole("dialog", { name: "Reset the password of kim" });
  await userEvent.type(within(dialog).getByLabelText("New password"), "new-secret");
  await userEvent.type(within(dialog).getByLabelText("Confirm the new password"), "new-secret");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Reset the password of kim.")).toBeInTheDocument();
  expect(body).toEqual({ password: "new-secret" });
});

it("deletes a user after confirming", async () => {
  const change = users([ME, KIM]);
  server.use(
    http.delete(apiUrl(`/users/${KIM.id}`), () => {
      change([ME]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for kim" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Delete" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }),
  );
  expect(await screen.findByText("Deleted user kim.")).toBeInTheDocument();
  await vi.waitFor(() =>
    expect(screen.queryByRole("cell", { name: "kim" })).not.toBeInTheDocument(),
  );
});
