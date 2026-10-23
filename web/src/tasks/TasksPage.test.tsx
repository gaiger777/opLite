import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { TasksPage } from "@/tasks/TasksPage";
import { mockLists, renderPage } from "@/test/render";

const VM = "019a0f3c-0000-7000-8000-000000000001";

it("lists tasks newest first and filters failures", async () => {
  mockLists({
    vm: [{ id: VM, name: "web" }],
    node: [{ id: "n1", name: "node-a" }],
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000001",
        spec: { op: { kind: "vm-start" }, target: VM, node: "n1" },
        status: { state: "failed", error: "no firmware" },
      },
      {
        id: "019a0f3c-9a10-7000-8000-000000000002",
        spec: { op: { kind: "vm-stop" }, target: VM, node: "n1" },
        status: { state: "succeeded", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  const stop = await screen.findByRole("link", { name: "Stop web" });
  expect(stop).toHaveAttribute("href", "/vms/web");
  const rows = await screen.findAllByRole("row");
  expect(within(rows[1]!).getByRole("link")).toBe(stop);
  expect(rows[2]).toHaveTextContent("Start web");
  expect(rows[2]).toHaveTextContent("node-a");
  expect(rows[2]).toHaveTextContent("no firmware");

  await userEvent.click(screen.getByLabelText("Failed only"));
  expect(screen.getAllByRole("row")).toHaveLength(2);
  expect(screen.queryByText("Stop web")).not.toBeInTheDocument();
});
