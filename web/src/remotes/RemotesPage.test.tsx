import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { RemotesPage } from "@/remotes/RemotesPage";
import { backupJob, remote } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => mockLists({}));

function mockRemotes(...items: ReturnType<typeof remote>[]) {
  server.use(http.get(apiUrl("/remotes"), () => HttpResponse.json({ revision: 1, items })));
}

it("shows a card for each remote with its status", async () => {
  mockRemotes(
    remote("dr"),
    remote("lab", {
      reachable: false,
      error: "connection refused",
      cluster_name: null,
      version: null,
      nodes: null,
      vms: null,
      cpus: null,
      memory_gib: null,
    }),
  );
  renderPage(<RemotesPage />);
  const dr = await screen.findByRole("group", { name: /^dr/ });
  expect(within(dr).getByRole("link", { name: "dr" })).toHaveAttribute("href", "/remotes/dr");
  expect(dr).toHaveTextContent("Reachable");
  expect(dr).toHaveTextContent("https://dr.example:8006");
  expect(dr).toHaveTextContent("Version0.37.0");
  expect(dr).toHaveTextContent("Nodes1 / 1 online");
  expect(dr).toHaveTextContent("VMs1 / 2 running");
  expect(dr).toHaveTextContent("CPUs8");
  expect(dr).toHaveTextContent("Memory32 GiB");
  const lab = screen.getByRole("group", { name: /^lab/ });
  expect(lab).toHaveTextContent("Unreachable");
  expect(lab).toHaveTextContent("connection refused");
  expect(lab).toHaveTextContent("Version—");
  expect(within(lab).getByRole("button", { name: "Refresh lab" })).toBeInTheDocument();
});

it("shows the list to viewers without changes", async () => {
  mockRemotes(remote("dr"));
  renderPage(<RemotesPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByRole("link", { name: "dr" })).toBeInTheDocument();
  for (const name of ["Add", "Refresh dr", "Edit dr", "Delete dr"]) {
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  }
  expect(screen.queryByRole("link", { name: "Manage remote clusters" })).not.toBeInTheDocument();
});

it("shows why a refresh failed", async () => {
  mockRemotes(remote("dr"));
  server.use(
    http.post(apiUrl("/remotes/dr/actions/refresh"), () =>
      HttpResponse.json({ code: "not_leader", message: "x" }, { status: 503 }),
    ),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Refresh dr" }));
  expect(
    await screen.findByText(
      "Could not refresh dr: The cluster leader is changing. Try again shortly.",
    ),
  ).toBeInTheDocument();
});
it("refreshes a remote", async () => {
  let refreshed = false;
  server.use(
    http.get(apiUrl("/remotes"), () =>
      HttpResponse.json({
        revision: 1,
        items: [refreshed ? remote("dr", { version: "0.38.0" }) : remote("dr")],
      }),
    ),
    http.post(apiUrl("/remotes/dr/actions/refresh"), () => {
      refreshed = true;
      return HttpResponse.json(remote("dr", { version: "0.38.0" }));
    }),
  );
  renderPage(<RemotesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Refresh dr" }));
  expect(await screen.findByText("0.38.0")).toBeInTheDocument();
  // 고치기·지우기는 같은 항목의 원격 클러스터 관리 탭(화면 안 탭)에 있다. 화면 머리에 따로 링크를 두지 않는다.
  expect(screen.queryByRole("button", { name: "Edit dr" })).not.toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Manage remote clusters" })).not.toBeInTheDocument();
});

it("lists the backup jobs that copy to remote clusters", async () => {
  mockRemotes(remote("dr"));
  mockLists({
    "backup-job": [
      backupJob("nightly", 1, { spec: { replicate: { remote: "dr", pool: "dr-bk" } } }),
      backupJob("local-only", 2),
    ],
  });
  renderPage(<RemotesPage />);
  const jobs = await screen.findByRole("table", {
    name: "Backup jobs that copy to remote clusters",
  });
  expect(await within(jobs).findByRole("link", { name: "nightly" })).toHaveAttribute(
    "href",
    "/backup-jobs",
  );
  expect(within(jobs).getByRole("cell", { name: "dr / dr-bk" })).toBeInTheDocument();
  expect(within(jobs).queryByText("local-only")).not.toBeInTheDocument();
});
