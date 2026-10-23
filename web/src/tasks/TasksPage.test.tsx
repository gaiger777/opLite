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

  expect(screen.getByRole("table", { name: "Tasks" })).toBeInTheDocument();
  expect(rows[2]!.querySelector("time")).toHaveAttribute("datetime", "2025-10-23T04:03:37.104Z");

  await userEvent.click(screen.getByLabelText("Failed only"));
  expect(screen.getAllByRole("row")).toHaveLength(2);
  expect(screen.queryByText("Stop web")).not.toBeInTheDocument();
});

it("names deleted targets by the end of their id and says when no task failed", async () => {
  mockLists({
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000003",
        spec: {
          op: { kind: "volume-delete" },
          target: "019a0f3c-0000-7000-8000-00000000beef",
          node: "n1",
        },
        status: { state: "succeeded", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  expect(await screen.findByText("Delete volume 0000beef")).toBeInTheDocument();
  await userEvent.click(screen.getByLabelText("Failed only"));
  expect(screen.getByText("No failed tasks.")).toBeInTheDocument();
});
