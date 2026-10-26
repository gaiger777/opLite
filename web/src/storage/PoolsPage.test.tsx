import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { PoolsPage } from "@/storage/PoolsPage";
import { NODE, pool, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

it("lists pools with their nodes, space and volumes", async () => {
  mockLists({
    "storage-pool": [pool("local"), pool("busy", { nodes: ["n1"] })],
    node: [NODE],
    volume: [volume("disk", "busy")],
  });
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const local = (await within(table).findByText("local")).closest("tr")!;
  expect(local).toHaveTextContent("/srv/local");
  expect(local).toHaveTextContent("All nodes");
  expect(local).toHaveTextContent("64 GiB / 128 GiB");
  const busy = within(table).getByText("busy").closest("tr")!;
  expect(busy).toHaveTextContent("node-a");
  expect(within(busy).getByRole("button", { name: "Delete busy" })).toBeDisabled();
});

it("creates a pool on the chosen nodes", async () => {
  mockLists({ node: [NODE, { ...NODE, id: "n2", name: "node-b" }] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("fast"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "srv/fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Enter an absolute path (starting with /).")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Directory"));
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/srv/fast");
  await userEvent.click(await within(dialog).findByRole("checkbox", { name: "node-b" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool fast.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "fast",
    spec: { backend: { kind: "dir", path: "/srv/fast" }, nodes: ["n2"] },
  });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("deletes an unused pool after confirming", async () => {
  const local = pool("local");
  mockLists({ "storage-pool": [local] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/storage-pools/${local.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete local" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted storage pool local.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("hides changes from operators", async () => {
  mockLists({ "storage-pool": [pool("local")] });
  renderPage(<PoolsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("local")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete local" })).not.toBeInTheDocument();
});
