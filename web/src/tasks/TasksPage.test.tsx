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

it("names backup tasks by the backup and the VM being restored", async () => {
  const backup = "019a0f3c-0000-7000-8006-000000000001";
  mockLists({
    vm: [{ id: VM, name: "web" }],
    backup: [{ id: backup, name: "web-1" }],
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000001",
        spec: { op: { kind: "vm-backup", backup }, target: VM, node: "n1" },
        status: { state: "succeeded", error: null },
      },
      {
        id: "019a0f3c-8a10-7000-8000-000000000002",
        spec: { op: { kind: "backup-delete" }, target: backup, node: "n1" },
        status: { state: "running", error: null },
      },
      {
        // 복원의 대상은 아직 없는 새 VM이다. 요청한 이름으로 보인다.
        id: "019a0f3c-8a10-7000-8000-000000000003",
        spec: {
          op: { kind: "backup-restore", backup, name: "web2", pool: "local" },
          target: "019a0f3c-0000-7000-8000-00000000cafe",
          node: "n1",
        },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  expect(await screen.findByRole("link", { name: "Back up web" })).toHaveAttribute(
    "href",
    "/vms/web",
  );
  expect(await screen.findByText("Delete backup web-1")).toBeInTheDocument();
  expect(screen.getByText("Restore a backup as web2")).toBeInTheDocument();
});

it("names import tasks by the new VM and the import", async () => {
  const imp = "019a0f3c-0000-7000-8008-000000000001";
  mockLists({
    "vm-import": [{ id: imp, name: "web01" }],
    task: [
      {
        // 가져오기의 대상도 아직 없는 새 VM이다.
        id: "019a0f3c-8a10-7000-8000-000000000001",
        spec: {
          op: {
            kind: "vm-import",
            import: imp,
            run: { name: "web", pool: "local", keep_macs: true },
          },
          target: "019a0f3c-0000-7000-8000-00000000cafe",
          node: "n1",
        },
        status: { state: "running", error: null },
      },
      {
        id: "019a0f3c-8a10-7000-8000-000000000002",
        spec: { op: { kind: "vm-import-delete" }, target: imp, node: "n1" },
        status: { state: "running", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  expect(await screen.findByText("Import VM web")).toBeInTheDocument();
  expect(await screen.findByRole("link", { name: "Delete import web01" })).toHaveAttribute(
    "href",
    "/imports/web01",
  );
});

it("shows HA recoveries with the failed node's name", async () => {
  mockLists({
    vm: [{ id: VM, name: "web" }],
    node: [
      { id: "n1", name: "node-a" },
      { id: "n2", name: "node-b" },
    ],
    task: [
      {
        id: "019a0f3c-8a10-7000-8000-000000000004",
        spec: { op: { kind: "vm-ha-recover", from: "n1" }, target: VM, node: "n2" },
        status: { state: "succeeded", error: null },
      },
    ],
  });
  renderPage(<TasksPage />);
  const row = (
    await screen.findByRole("link", { name: "HA recovery of web (from node node-a)" })
  ).closest("tr")!;
  expect(row).toHaveTextContent("node-b");
});

function many(n: number, state: "failed" | "succeeded" = "succeeded") {
  return Array.from({ length: n }, (_, i) => ({
    id: `019a0f3c-8a10-7000-8000-${String(i).padStart(12, "0")}`,
    spec: { op: { kind: "vm-start" as const }, target: VM, node: "n1" },
    status: { state, error: state === "failed" ? "boom" : null },
  }));
}

it("shows tasks fifty at a time", async () => {
  mockLists({ vm: [{ id: VM, name: "web" }], task: many(51) });
  renderPage(<TasksPage />);
  expect(await screen.findByText("1–50 of 51")).toBeInTheDocument();
  // 머리 줄 + 50줄
  expect(screen.getAllByRole("row")).toHaveLength(51);
  await userEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("51–51 of 51")).toBeInTheDocument();
  expect(screen.getAllByRole("row")).toHaveLength(2);
});

it("goes back to the first page when the filter changes", async () => {
  mockLists({ vm: [{ id: VM, name: "web" }], task: many(120, "failed") });
  renderPage(<TasksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Next" }));
  expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
  await userEvent.click(screen.getByLabelText("Failed only"));
  expect(screen.getByText("1–50 of 120")).toBeInTheDocument();
});
