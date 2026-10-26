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
  // UTF-16 길이(8)나 바이트(12)가 아니라 글자(4)로 거부한다.
  expect(passwordErrors("😀😀😀😀", "😀😀😀😀")).toEqual({ password: "accounts.errors.password" });
  expect(passwordErrors("비밀번호", "비밀번호")).toEqual({ password: "accounts.errors.password" });
  expect(passwordErrors("😀".repeat(8), "😀".repeat(8))).toEqual({});
});

it("lists users and keeps admins from changing themselves", async () => {
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
  expect(screen.getByRole("menuitem", { name: "Change role" })).toHaveAttribute(
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

it("tells other roles that only admins manage users", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/users"), () => {
      reads += 1;
      return HttpResponse.json({ revision: 1, items: [ME, KIM] });
    }),
  );
  renderPage(<UsersPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("Only administrators can manage users.")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(reads).toBe(0);
});

it("changes a role and reads the list again", async () => {
  const change = users([ME, KIM]);
  let body: unknown;
  server.use(
    http.put(apiUrl(`/users/${KIM.id}`), async ({ request }) => {
      body = await request.json();
      change([ME, { ...KIM, role: "operator" }]);
      return HttpResponse.json({ ...KIM, role: "operator" });
    }),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for kim" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change role" }));
  const dialog = await screen.findByRole("dialog", { name: "Change the role of kim" });
  // 같은 역할로는 저장하지 않는다.
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  await userEvent.selectOptions(within(dialog).getByLabelText("Role"), "operator");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("kim is now Operator.")).toBeInTheDocument();
  expect(body).toEqual({ role: "operator" });
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  const table = screen.getByRole("table", { name: "Users" });
  await waitFor(() =>
    expect(within(table).getByText("kim").closest("tr")).toHaveTextContent("Operator"),
  );
});

it("shows why the server refused a role change", async () => {
  users([ME, KIM]);
  server.use(
    http.put(apiUrl(`/users/${KIM.id}`), () =>
      HttpResponse.json({ code: "not_found", message: "user not found" }, { status: 404 }),
    ),
  );
  renderPage(<UsersPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for kim" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Change role" }));
  const dialog = await screen.findByRole("dialog", { name: "Change the role of kim" });
  await userEvent.selectOptions(within(dialog).getByLabelText("Role"), "admin");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent("It no longer exists.");
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
