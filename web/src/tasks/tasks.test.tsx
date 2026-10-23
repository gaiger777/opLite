import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";

import { createQueryClient } from "@/api/queryClient";
import type { Task } from "@/api/types";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import { settleTasks, trackTask } from "@/tasks/tasks";
import { TaskToasts } from "@/tasks/TaskToasts";

function task(state: Task["status"]["state"], error: string | null = null): Task {
  return {
    id: "019a0f3c-8a10-7000-8000-000000000001",
    name: "t",
    revision: 1,
    spec: { op: { kind: "vm-start" }, target: "x", node: "n" },
    status: { state, error },
  } as Task;
}

it("turns a tracked task's toast into its result", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  trackTask(task("pending"), "Start web");
  expect(await screen.findByText("Start web")).toBeInTheDocument();
  settleTasks([task("running")], i18n.t);
  settleTasks([task("failed", "no firmware")], i18n.t);
  expect(await screen.findByText("Start web: failed. no firmware")).toBeInTheDocument();
  // 한 번 알린 결과는 다시 알리지 않는다.
  settleTasks([task("succeeded")], i18n.t);
  expect(screen.queryByText("Start web: done")).not.toBeInTheDocument();
});

it("settles a task that finished before it was tracked", async () => {
  const queryClient = createQueryClient();
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [task("succeeded")] });
  render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <TaskToasts />
        <Toaster />
      </QueryClientProvider>
    </ThemeProvider>,
  );
  trackTask(task("pending"), "Stop web");
  expect(await screen.findByText("Stop web: done")).toBeInTheDocument();
});
