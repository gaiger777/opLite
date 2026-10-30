import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { toast } from "sonner";

import { createQueryClient } from "@/api/queryClient";
import type { Task } from "@/api/types";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import {
  describeTask,
  onTrack,
  resetTasks,
  settleTasks,
  TASK_TIMEOUT_MS,
  trackTask,
  waitForTask,
} from "@/tasks/tasks";
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
  settleTasks({ revision: 2, items: [task("1", "running")] }, i18n.t);
  settleTasks({ revision: 3, items: [task("1", "failed", "no firmware")] }, i18n.t);
  expect(await screen.findByText("Start web: failed. no firmware")).toBeInTheDocument();
  // 한 번 알린 결과는 다시 알리지 않는다.
  const success = vi.spyOn(toast, "success");
  settleTasks({ revision: 4, items: [task("1", "succeeded")] }, i18n.t);
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

it("settles a tracked task that was deleted before its result arrived", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  trackTask({ ...task("8", "pending"), revision: 5 }, "Start web");
  expect(await screen.findByText("Start web")).toBeInTheDocument();
  // 만들기 전에 읽은 목록에 없는 것은 아직 들어오지 않은 것이다.
  settleTasks({ revision: 4, items: [] }, i18n.t);
  expect(screen.queryByText("Start web: result is no longer available")).not.toBeInTheDocument();
  // 만든 뒤에 읽은 목록에 없으면 정리 작업이 지운 것이다.
  settleTasks({ revision: 9, items: [] }, i18n.t);
  expect(await screen.findByText("Start web: result is no longer available")).toBeInTheDocument();
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
  const unknown = { ...task("4", "pending"), spec: { op: { kind: "vm-teleport" }, target: "x" } };
  expect(describeTask(unknown as unknown as Task, i18n.t, "web")).toBe("vm-teleport web");
  expect(describeTask(task("4", "pending"), i18n.t, "web")).toBe("Start web");
  const clone = { ...task("4", "pending"), spec: { op: { kind: "vm-clone" }, target: "x" } };
  expect(describeTask(clone as unknown as Task, i18n.t, "web")).toBe("Clone web");
  const migrate = { ...task("4", "pending"), spec: { op: { kind: "vm-migrate" }, target: "x" } };
  expect(describeTask(migrate as unknown as Task, i18n.t, "web")).toBe("Migrate web");
});

it("waits for a task and rejects with its error when it fails", async () => {
  const queryClient = createQueryClient();
  const id = task("5", "pending").id;
  const done = waitForTask(queryClient, id);
  queryClient.setQueryData(["list", "task"], { revision: 2, items: [task("5", "running")] });
  queryClient.setQueryData(["list", "task"], {
    revision: 3,
    items: [task("5", "failed", "pool is full")],
  });
  await expect(done).rejects.toThrow("pool is full");
});

it("stops waiting when aborted or after the timeout", async () => {
  const queryClient = createQueryClient();
  const controller = new AbortController();
  const aborted = waitForTask(queryClient, task("6", "pending").id, controller.signal);
  controller.abort(new Error("left the page"));
  await expect(aborted).rejects.toThrow("left the page");
  // 중단한 뒤에는 Task가 끝나도 아무 일이 없다(구독을 풀었다).
  expect(queryClient.getQueryCache().hasListeners()).toBe(false);

  vi.useFakeTimers();
  try {
    const late = waitForTask(queryClient, task("7", "pending").id);
    vi.advanceTimersByTime(TASK_TIMEOUT_MS);
    await expect(late).rejects.toThrow("did not finish in time");
    expect(queryClient.getQueryCache().hasListeners()).toBe(false);
  } finally {
    vi.useRealTimers();
  }
});
