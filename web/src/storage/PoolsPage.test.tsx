import { act, screen, within } from "@testing-library/react";
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
  // 지울 수 없는 이유를 행에 글로 보인다.
  expect(busy).toHaveTextContent("Delete its volumes first.");
  expect(within(local).getByRole("button", { name: "Delete local" })).toBeEnabled();
  expect(local).not.toHaveTextContent("Delete its volumes first.");
});

// 볼륨 목록을 읽기 전·읽지 못했을 때는 쓰지 않는 풀인지 모른다.
it("does not offer a delete without the volumes", async () => {
  mockLists({ "storage-pool": [pool("local")] });
  server.use(
    http.get(apiUrl("/volumes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<PoolsPage />);
  const local = (await screen.findByText("local")).closest("tr")!;
  expect(within(local).getByRole("button", { name: "Delete local" })).toBeDisabled();
  expect(within(local).getAllByRole("cell").at(-2)).toHaveTextContent("—");
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
  await userEvent.type(within(dialog).getByLabelText("Name"), "Fast");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "srv/fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText(/lowercase letters/)).toBeVisible();
  expect(within(dialog).getByText("Enter an absolute path (starting with /).")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
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

// 고른 노드가 모두 빠진 채로 보내면 빈 목록이 되어 모든 노드의 풀이 된다.
it("does not create a pool when the chosen nodes are gone", async () => {
  mockLists({ node: [NODE, { ...NODE, id: "n2", name: "node-b" }] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("fast"), { status: 201 });
    }),
  );
  const { queryClient } = renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/srv/fast");
  await userEvent.click(await within(dialog).findByRole("checkbox", { name: "node-b" }));
  act(() => {
    queryClient.setQueryData(["list", "node"], { revision: 2, items: [NODE] });
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByRole("group", { name: "Nodes" })).toHaveAccessibleDescription(
    "The chosen nodes are no longer in the cluster. Choose again.",
  );
  expect(body).toBeUndefined();
});

it("says when the nodes cannot be read", async () => {
  mockLists({});
  server.use(
    http.get(apiUrl("/nodes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  expect(await within(dialog).findByRole("alert")).toBeInTheDocument();
  expect(within(dialog).queryByRole("checkbox")).not.toBeInTheDocument();
});

it("keeps the confirmation open when the pool is still in use", async () => {
  const local = pool("local");
  mockLists({ "storage-pool": [local] });
  server.use(
    http.delete(apiUrl(`/storage-pools/${local.id}`), () =>
      HttpResponse.json({ code: "pool_in_use", message: "in use" }, { status: 409 }),
    ),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete local" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The pool still has volumes. Delete them first.",
  );
  expect(dialog).toBeVisible();
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
