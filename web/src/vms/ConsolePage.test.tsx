import { screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { ConsolePage } from "@/vms/ConsolePage";

// 콘솔 자체(noVNC)는 VmConsole.test.tsx가 본다.
vi.mock("@/vms/lazyConsole", () => ({
  VmConsole: ({ vm }: { vm: { name: string } }) => <p>console of {vm.name}</p>,
}));

const page = { path: "/vms/:name/console", url: "/vms/web/console" };

it("opens the console of the VM", async () => {
  mockLists({ vm: [vm("web", "running")] });
  renderPage(<ConsolePage />, page);
  expect(await screen.findByText("console of web")).toBeInTheDocument();
});

it("does not open the console without the operate permission", async () => {
  mockLists({ vm: [vm("web", "running")] });
  renderPage(<ConsolePage />, { ...page, as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "You do not have permission to use the console.",
  );
  expect(screen.queryByText("console of web")).not.toBeInTheDocument();
});

it("says why the VM could not be read", async () => {
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "permission denied" }, { status: 403 }),
    ),
  );
  renderPage(<ConsolePage />, page);
  expect(await screen.findByRole("status")).toHaveTextContent("Loading…");
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
});
