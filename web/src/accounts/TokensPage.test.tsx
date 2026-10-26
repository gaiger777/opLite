import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { TokensPage } from "@/accounts/TokensPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const KIM = "00000000-0000-0000-0000-00000000000b";
const MINE = {
  id: "019a0f3c-8a10-7000-8000-0000000000c1",
  name: "cli",
  user: ADMIN.user,
  revision: 1,
};
const THEIRS = { id: "019a0f3c-8a10-7000-8000-0000000000c2", name: "ci", user: KIM, revision: 2 };

function tokens(list: unknown[]) {
  let items = list;
  server.use(http.get(apiUrl("/tokens"), () => HttpResponse.json({ revision: 1, items })));
  return (next: unknown[]) => {
    items = next;
  };
}

it("shows owners to admins", async () => {
  tokens([MINE, THEIRS]);
  server.use(
    http.get(apiUrl("/users"), () =>
      HttpResponse.json({
        revision: 1,
        items: [{ id: KIM, name: "kim", role: "operator", revision: 1 }],
      }),
    ),
  );
  renderPage(<TokensPage />);
  const table = await screen.findByRole("table", { name: "API tokens" });
  expect((await within(table).findByText("cli")).closest("tr")).toHaveTextContent("admin");
  expect(await within(table).findByText("kim")).toBeInTheDocument();
  expect(
    within(table)
      .getAllByRole("columnheader")
      .map((h) => h.textContent),
  ).toContain("Owner");
});

it("does not read users for other roles", async () => {
  tokens([MINE]);
  // /users 응답을 정하지 않았으므로 읽으면 테스트가 실패한다.
  renderPage(<TokensPage />, { as: { ...ADMIN, role: "viewer" } });
  const table = await screen.findByRole("table", { name: "API tokens" });
  expect(await within(table).findByText("cli")).toBeInTheDocument();
  expect(within(table).queryByRole("columnheader", { name: "Owner" })).not.toBeInTheDocument();
});

it("shows a new token's secret once, with a copy button", async () => {
  const user = userEvent.setup();
  const change = tokens([]);
  const created = { ...MINE, id: "019a0f3c-8a10-7000-8000-0000000000c3", name: "backup" };
  server.use(
    http.post(apiUrl("/tokens"), async ({ request }) => {
      expect(await request.json()).toEqual({ name: "backup" });
      change([created]);
      return HttpResponse.json({ token: created, secret: "opl_abc_secret" }, { status: 201 });
    }),
  );
  renderPage(<TokensPage />, { as: { ...ADMIN, role: "viewer" } });
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const form = await screen.findByRole("dialog", { name: "Create an API token" });
  await user.type(within(form).getByLabelText("Name"), "backup");
  await user.click(within(form).getByRole("button", { name: "Create" }));

  const dialog = await screen.findByRole("dialog", { name: "Token backup was created" });
  expect(within(dialog).getByLabelText("Token")).toHaveValue("opl_abc_secret");
  await user.click(within(dialog).getByRole("button", { name: "Copy the token" }));
  expect(await navigator.clipboard.readText()).toBe("opl_abc_secret");
  expect(await screen.findByText("Copied.")).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  expect(screen.queryByText("opl_abc_secret")).not.toBeInTheDocument();
  expect(await screen.findByRole("cell", { name: "backup" })).toBeInTheDocument();
});

it("deletes a token after confirming", async () => {
  const change = tokens([MINE]);
  server.use(
    http.delete(apiUrl(`/tokens/${MINE.id}`), () => {
      change([]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<TokensPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete cli" }));
  await userEvent.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }),
  );
  expect(await screen.findByText("Deleted API token cli.")).toBeInTheDocument();
  expect(await screen.findByText("No API tokens.")).toBeInTheDocument();
});
