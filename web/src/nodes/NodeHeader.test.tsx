import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { NodeDetailPage } from "@/nodes/NodeDetailPage";
import { expectNoAxeViolations } from "@/test/axe";
import { history, NODE, NODE_FIELDS, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const DOWN = {
  ...NODE,
  id: "n2",
  name: "node-b",
  revision: 7,
  labels: { zone: "rack-a" },
  status: { ...NODE.status, online: false },
};

const at = { path: "/nodes/:name", url: "/nodes/node-b" };

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
    http.get(apiUrl("/nodes/:key/metrics/history"), () => HttpResponse.json(history(NODE_FIELDS))),
  ),
);

const onB = vm("web", "stopped", { spec: { ...vm("web", "stopped").spec, node: "n2" } });

it("shows the node header with state, summary cards and main actions", async () => {
  mockLists({ node: [DOWN], vm: [onB] });
  const { container } = renderPage(<NodeDetailPage />, at);
  expect(await screen.findByRole("heading", { level: 1, name: "node-b" })).toBeInTheDocument();
  expect(screen.getByText("Offline")).toHaveAttribute("data-slot", "status-chip");
  const cards = screen.getByText("API address").closest("dl")!;
  expect(within(cards).getByText("10.0.0.1")).toBeInTheDocument();
  expect(within(cards).getByText("16 GiB")).toBeInTheDocument();
  await waitFor(() => expect(cards).toHaveTextContent("VMs1"));
  expect(screen.getByRole("button", { name: "Enter maintenance" })).toBeInTheDocument();
  // 꺼진 노드는 비우지 못한다(이유와 함께 막힘).
  expect(screen.getByRole("button", { name: "Drain node…" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(screen.getByText("The node is offline.")).toBeInTheDocument();
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("shows maintenance before the server answers and rolls back on failure", async () => {
  mockLists({ node: [DOWN], vm: [] });
  let answer: () => void = () => {};
  server.use(
    http.put(apiUrl("/nodes/n2"), async () => {
      await new Promise<void>((resolve) => (answer = resolve));
      return HttpResponse.json({ code: "revision_mismatch", message: "changed" }, { status: 409 });
    }),
  );
  renderPage(<NodeDetailPage />, at);
  await userEvent.click(await screen.findByRole("button", { name: "Enter maintenance" }));
  // 낙관적: 응답 전에 칩과 단추가 바뀐다.
  expect(await screen.findByText("Maintenance")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "End maintenance" })).toBeInTheDocument();
  answer();
  expect(
    await screen.findByText("node-b: Someone else changed it first. Reload and try again."),
  ).toBeInTheDocument();
  expect(screen.queryByText("Maintenance")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Enter maintenance" })).toBeInTheDocument();
});

it("edits labels from the header menu and undoes the change", async () => {
  mockLists({ node: [DOWN], vm: [] });
  const sent: { ifMatch: string | null; labels: unknown }[] = [];
  server.use(
    http.put(apiUrl("/nodes/n2"), async ({ request }) => {
      const body = (await request.json()) as { labels: Record<string, string> };
      sent.push({ ifMatch: request.headers.get("If-Match"), labels: body.labels });
      return HttpResponse.json({ ...DOWN, revision: 7 + sent.length, labels: body.labels });
    }),
  );
  renderPage(<NodeDetailPage />, at);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  await userEvent.click(await screen.findByRole("menuitem", { name: "Edit labels" }));
  const dialog = await screen.findByRole("dialog", { name: "Labels of node-b" });
  const input = within(dialog).getByLabelText("Labels");
  await userEvent.clear(input);
  await userEvent.type(input, "zone=rack-b");
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Labels of node-b saved.")).toBeInTheDocument();
  // sonner 단추는 pointer capture를 쓰므로(jsdom에 없음) 클릭만 보낸다.
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(await screen.findByText("Undone.")).toBeInTheDocument();
  expect(sent).toEqual([
    { ifMatch: "7", labels: { zone: "rack-b" } },
    { ifMatch: "8", labels: { zone: "rack-a" } },
  ]);
});

it("removes the node from the header after typing its name, listing its VMs", async () => {
  mockLists({ node: [NODE, DOWN], vm: [onB] });
  let removed = false;
  server.use(
    http.delete(apiUrl("/nodes/n2"), () => {
      removed = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const { router } = renderPage(<NodeDetailPage />, at);
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  // 머리에 단추로 있는 유지보수·비우기는 메뉴에 없다.
  expect(screen.queryByRole("menuitem", { name: "Enter maintenance" })).not.toBeInTheDocument();
  await userEvent.click(await screen.findByRole("menuitem", { name: "Remove from cluster" }));
  const dialog = await screen.findByRole("alertdialog");
  expect(within(dialog).getByRole("list", { name: "What this affects" })).toHaveTextContent(
    "VMs assigned to it: 1",
  );
  // 기본 포커스는 취소다.
  expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
  const confirm = within(dialog).getByRole("button", { name: "Remove from cluster" });
  expect(confirm).toBeDisabled();
  await userEvent.type(within(dialog).getByLabelText("Type node-b to confirm"), "node-b");
  await userEvent.click(confirm);
  await waitFor(() => expect(router.state.location.pathname).toBe("/nodes"));
  expect(removed).toBe(true);
});

it("offers only the read-only dialogs to viewers", async () => {
  mockLists({ node: [{ ...DOWN, status: { ...DOWN.status, online: true } }], vm: [] });
  renderPage(<NodeDetailPage />, {
    ...at,
    as: { user: "00000000-0000-0000-0000-000000000009", name: "v", role: "viewer" },
  });
  await userEvent.click(await screen.findByRole("button", { name: "Actions for node-b" }));
  expect(await screen.findByRole("menuitem", { name: /interfaces/i })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Edit labels" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Enter maintenance" })).not.toBeInTheDocument();
});
