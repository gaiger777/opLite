import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { toast } from "sonner";

import { createQueryClient } from "@/api/queryClient";
import type { Task } from "@/api/types";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import { describeTask, onTrack, resetTasks, settleTasks, trackTask } from "@/tasks/tasks";
import { TaskToasts } from "@/tasks/TaskToasts";

// 알림과 지켜보는 Task는 모듈 전역이므로 테스트마다 비우고, Task id도 테스트마다 다르게 쓴다.
afterEach(() => {
  toast.dismiss();
  resetTasks();
  vi.restoreAllMocks();
});

function task(id: string, state: Task["status"]["state"], error: string | null = null): Task {
  return {
    id: `019a0f3c-8a10-7000-8000-${id.padStart(12, "0")}`,
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
  trackTask(task("1", "pending"), "Start web");
  expect(await screen.findByText("Start web")).toBeInTheDocument();
  settleTasks([task("1", "running")], i18n.t);
  settleTasks([task("1", "failed", "no firmware")], i18n.t);
  expect(await screen.findByText("Start web: failed. no firmware")).toBeInTheDocument();
  // 한 번 알린 결과는 다시 알리지 않는다.
  const success = vi.spyOn(toast, "success");
  settleTasks([task("1", "succeeded")], i18n.t);
  expect(success).not.toHaveBeenCalled();
});

it("settles a task that finished before it was tracked", async () => {
  const queryClient = createQueryClient();
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [task("2", "succeeded")] });
  render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <TaskToasts />
        <Toaster />
      </QueryClientProvider>
    </ThemeProvider>,
  );
  trackTask(task("2", "pending"), "Stop web");
  expect(await screen.findByText("Stop web: done")).toBeInTheDocument();
});

it("keeps the newer listener when an older one unsubscribes", () => {
  const first = vi.fn();
  const second = vi.fn();
  const stopFirst = onTrack(first);
  const stopSecond = onTrack(second);
  stopFirst();
  trackTask(task("3", "pending"), "Start web");
  expect(second).toHaveBeenCalledOnce();
  expect(first).not.toHaveBeenCalled();
  stopSecond();
});

it("describes unknown operations by their kind", () => {
  const unknown = { ...task("4", "pending"), spec: { op: { kind: "vm-migrate" }, target: "x" } };
  expect(describeTask(unknown as unknown as Task, i18n.t, "web")).toBe("vm-migrate web");
  expect(describeTask(task("4", "pending"), i18n.t, "web")).toBe("Start web");
});
