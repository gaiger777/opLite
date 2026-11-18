import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { NODE, pool, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { NodeDetailPage } from "@/nodes/NodeDetailPage";
import { PoolsPage } from "@/storage/PoolsPage";
import { VmDetailPage } from "@/vms/VmDetailPage";

afterEach(() => localStorage.clear());

const web = vm("web", "running");

it("pins a VM from its header and remembers the visit", async () => {
  mockLists({ vm: [web], node: [NODE] });
  const user = userEvent.setup();
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const star = await screen.findByRole("button", { name: "Favorite web" });
  expect(star).toHaveAttribute("aria-pressed", "false");
  await waitFor(() =>
    expect(JSON.parse(localStorage.getItem(`opl.${ADMIN.user}.recent`) ?? "[]")).toEqual([
      { kind: "vm", id: web.id, name: "web" },
    ]),
  );
  await user.click(star);
  expect(star).toHaveAttribute("aria-pressed", "true");
  expect(JSON.parse(localStorage.getItem(`opl.${ADMIN.user}.pins`) ?? "[]")).toEqual([
    { kind: "vm", id: web.id, name: "web" },
  ]);
  await user.click(star);
  expect(star).toHaveAttribute("aria-pressed", "false");
  expect(localStorage.getItem(`opl.${ADMIN.user}.pins`)).toBe("[]");
});

it("pins a node from its header", async () => {
  mockLists({ node: [NODE] });
  const user = userEvent.setup();
  renderPage(<NodeDetailPage />, { path: "/nodes/:name", url: "/nodes/node-a" });
  await user.click(await screen.findByRole("button", { name: "Favorite node-a" }));
  expect(JSON.parse(localStorage.getItem(`opl.${ADMIN.user}.pins`) ?? "[]")).toEqual([
    { kind: "node", id: "n1", name: "node-a" },
  ]);
});

it("pins a storage pool from its row without taking the f shortcut", async () => {
  mockLists({ "storage-pool": [pool("local")], node: [NODE] });
  const user = userEvent.setup();
  renderPage(<PoolsPage />);
  const star = await screen.findByRole("button", { name: "Favorite local" });
  expect(star).not.toHaveAttribute("data-pin-toggle");
  await user.click(star);
  expect(JSON.parse(localStorage.getItem(`opl.${ADMIN.user}.pins`) ?? "[]")).toEqual([
    { kind: "storage-pool", id: pool("local").id, name: "local" },
  ]);
});
