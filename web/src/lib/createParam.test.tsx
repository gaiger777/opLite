import { screen, waitFor } from "@testing-library/react";

import type { Principal } from "@/api/types";
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { ImportsPage } from "@/imports/ImportsPage";
import { IsosPage } from "@/isos/IsosPage";
import { NetworksPage } from "@/networks/NetworksPage";
import { PoolsPage } from "@/storage/PoolsPage";
import { pool } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";

const VIEWER: Principal = {
  user: "00000000-0000-0000-0000-000000000003",
  name: "viewer",
  role: "viewer",
};

beforeEach(() => mockLists({ "storage-pool": [pool("backup", { backups: true })] }));

it.each([
  ["/storage-pools", <PoolsPage />, "Create a storage pool"],
  ["/networks", <NetworksPage />, "Create a network"],
  ["/backup-jobs", <BackupJobsPage />, "Create backup schedule"],
  ["/isos", <IsosPage />, "Upload an ISO image"],
  ["/imports", <ImportsPage />, "Upload an OVA"],
])(
  "opens the create dialog of %s from ?create=1 and drops the query",
  async (path, page, title) => {
    const { router } = renderPage(page, { path, url: `${path}?create=1&x=1` });
    expect(await screen.findByRole("dialog", { name: title })).toBeInTheDocument();
    expect(router.state.location.search).toBe("?x=1");
  },
);

it("does not open it for a role that cannot create", async () => {
  const { router } = renderPage(<PoolsPage />, {
    path: "/storage-pools",
    url: "/storage-pools?create=1",
    as: VIEWER,
  });
  await waitFor(() => expect(router.state.location.search).toBe(""));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});
