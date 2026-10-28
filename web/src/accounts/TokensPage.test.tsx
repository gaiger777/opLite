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
  let reads = 0;
  server.use(
    http.get(apiUrl("/users"), () => {
      reads += 1;
      return HttpResponse.json({ revision: 1, items: [] });
    }),
  );
  renderPage(<TokensPage />, { as: { ...ADMIN, role: "viewer" } });
  const table = await screen.findByRole("table", { name: "API tokens" });
  expect(await within(table).findByText("cli")).toBeInTheDocument();
  expect(within(table).queryByRole("columnheader", { name: "Owner" })).not.toBeInTheDocument();
  expect(reads).toBe(0);
});

it("shows a new token's secret once, with a copy button", async () => {
  const user = userEvent.setup();
  const change = tokens([]);
  const created = { ...MINE, id: "019a0f3c-8a10-7000-8000-0000000000c3", name: "backup" };
  let body: unknown;
  server.use(
    http.post(apiUrl("/tokens"), async ({ request }) => {
      body = await request.json();
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
  expect(body).toEqual({ name: "backup" });
  expect(within(dialog).getByLabelText("Token")).toHaveValue("opl_abc_secret");
  await user.click(within(dialog).getByRole("button", { name: "Copy the token" }));
  expect(await navigator.clipboard.readText()).toBe("opl_abc_secret");
  expect(await screen.findByText("Copied.")).toBeInTheDocument();
  // 비밀값을 놓치지 않게 Esc나 바깥 클릭으로는 닫히지 않는다.
  expect(within(dialog).queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  await user.keyboard("{Escape}");
  expect(dialog).toBeInTheDocument();
  await user.click(document.querySelector<HTMLElement>("[data-slot=dialog-overlay]")!);
  expect(dialog).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  await vi.waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(screen.queryByDisplayValue("opl_abc_secret")).not.toBeInTheDocument();
  expect(await screen.findByRole("cell", { name: "backup" })).toBeInTheDocument();
  // 비밀값 대화상자를 닫으면 연 버튼으로 돌아간다.
  await vi.waitFor(() => expect(screen.getByRole("button", { name: "Create" })).toHaveFocus());
});

it("returns focus to the create button after cancelling", async () => {
  const user = userEvent.setup();
  tokens([]);
  renderPage(<TokensPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const form = await screen.findByRole("dialog", { name: "Create an API token" });
  await user.click(within(form).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(form).not.toBeInTheDocument());
  await vi.waitFor(() => expect(screen.getByRole("button", { name: "Create" })).toHaveFocus());
});

it("deletes a token after confirming", async () => {
  const user = userEvent.setup();
  const change = tokens([MINE]);
  server.use(
    http.delete(apiUrl(`/tokens/${MINE.id}`), () => {
      change([]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<TokensPage />);
  await user.click(await screen.findByRole("button", { name: "Delete cli" }));
  await user.click(
    within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Delete" }),
  );
  expect(await screen.findByText("Deleted API token cli.")).toBeInTheDocument();
  expect(await screen.findByText("No API tokens.")).toBeInTheDocument();
});
