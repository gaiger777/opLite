import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ApiError, setUnauthorizedHandler } from "@/api/client";
import { RemotePage } from "@/remotes/RemotePage";
import { proxy } from "@/remotes/remotes";
import { remote, task, vm } from "@/test/fixtures";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const web = vm("web", "running");
const db = vm("db", "stopped");

function mockRemote(
  vms: Record<string, unknown> = { revision: 1, items: [web, db] },
  status = 200,
) {
  server.use(
    http.get(apiUrl("/remotes/dr"), () => HttpResponse.json(remote("dr"))),
    http.get(apiUrl("/remotes/dr/api/vms"), () => HttpResponse.json(vms, { status })),
  );
}

function render(role: "admin" | "viewer" = "admin") {
  return renderPage(<RemotePage />, {
    path: "/remotes/:name",
    url: "/remotes/dr",
    as: { ...ADMIN, role },
  });
}

it("lists the VMs of the remote through the proxy", async () => {
  mockRemote();
  render();
  const table = await screen.findByRole("table", { name: "Virtual machines on dr" });
  expect(await within(table).findByText("web")).toBeInTheDocument();
  const row = within(table).getByText("web").closest("tr")!;
  expect(row).toHaveTextContent("Running");
  expect(row).toHaveTextContent("2");
  expect(row).toHaveTextContent("2 GiB");
  expect(screen.getByText("dr · https://dr.example:8006")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Back to remote clusters" })).toHaveAttribute(
    "href",
    "/remotes",
  );
});

it("starts, shuts down and force stops remote VMs", async () => {
  mockRemote();
  const calls: string[] = [];
  server.use(
    http.post(apiUrl("/remotes/dr/api/vms/:id/actions/:action"), ({ request, params }) => {
      const url = new URL(request.url);
      calls.push(`${String(params.id)} ${String(params.action)}${url.search}`);
      return HttpResponse.json(task("t1", "vm-start", "x"), { status: 202 });
    }),
  );
  render();
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  // 멈춘 VM은 정지할 수 없다.
  expect(screen.getByRole("menuitem", { name: "Stop" })).toHaveAttribute("aria-disabled", "true");
  await userEvent.click(screen.getByRole("menuitem", { name: "Start" }));
  expect(await screen.findByText("Start requested for db on dr.")).toBeInTheDocument();

  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  await userEvent.click(screen.getByRole("menuitem", { name: "Stop" }));
  await screen.findByText("Stop requested for web on dr.");

  await userEvent.click(screen.getByRole("button", { name: "Actions for web" }));
  await userEvent.click(screen.getByRole("menuitem", { name: "Force stop" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Force stop web?" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Force stop" }));
  await screen.findByText("Force stop requested for web on dr.");
  expect(calls).toEqual([`${db.id} start`, `${web.id} stop`, `${web.id} stop?mode=force`]);
});

it("shows the error of a refused action", async () => {
  mockRemote();
  server.use(
    http.post(apiUrl("/remotes/dr/api/vms/:id/actions/start"), () =>
      HttpResponse.json({ code: "forbidden", message: "viewer" }, { status: 403 }),
    ),
  );
  render();
  await userEvent.click(await screen.findByRole("button", { name: "Actions for db" }));
  await userEvent.click(screen.getByRole("menuitem", { name: "Start" }));
  expect(await screen.findByText("db: You do not have permission to do this.")).toBeInTheDocument();
});

it("shows the VMs to viewers without actions", async () => {
  mockRemote();
  render("viewer");
  expect(await screen.findByText("web")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Actions for web" })).not.toBeInTheDocument();
});

it("shows a token the remote refused as an error, not a sign-out", async () => {
  mockRemote(
    { code: "remote_unauthorized", message: "the remote cluster refused the stored token" },
    502,
  );
  render();
  // 서버 오류라 두 번 더 읽어 본 뒤(1초·2초 뒤) 보인다.
  expect(
    await screen.findByText(
      "The remote cluster rejected the API token. Check that the token still exists there.",
      {},
      { timeout: 6000 },
    ),
  ).toBeInTheDocument();
  expect(screen.queryByText("moved")).not.toBeInTheDocument();
}, 10_000);

it("reports a 401 of this cluster like other requests", async () => {
  const unauthorized = vi.fn();
  setUnauthorizedHandler(unauthorized, () => 3);
  onTestFinished(() =>
    setUnauthorizedHandler(
      () => {},
      () => 0,
    ),
  );
  server.use(
    http.get(apiUrl("/remotes/dr/api/vms"), () =>
      HttpResponse.json({ code: "unauthorized", message: "no session" }, { status: 401 }),
    ),
  );
  await expect(proxy("dr", "GET", "/vms")).rejects.toBeInstanceOf(ApiError);
  expect(unauthorized).toHaveBeenCalledWith(3);
});
