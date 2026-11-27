import { QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { createQueryClient } from "@/api/queryClient";
import type { Task } from "@/api/types";
import { Toaster } from "@/components/ui/sonner";
import i18n from "@/i18n";
import { ThemeProvider } from "@/lib/theme";
import { isCenterOpen, setCenterOpen, setTaskOpener } from "@/tasks/center";
import {
  describeDetail,
  describeProgress,
  describeTask,
  onTrack,
  resetTasks,
  settleTasks,
  TASK_TIMEOUT_MS,
  trackTask,
  waitForTask,
} from "@/tasks/tasks";
import { TaskToasts } from "@/tasks/TaskToasts";
import { vm } from "@/test/fixtures";
import { apiUrl, server } from "@/test/server";

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

it("warns when a task succeeded with a warning", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  const warning = vi.spyOn(toast, "warning");
  trackTask(task("9", "pending"), "Delete web-1");
  settleTasks(
    {
      revision: 2,
      items: [task("9", "succeeded", "backup deleted, but remote copies may be left: down")],
    },
    i18n.t,
  );
  expect(
    await screen.findByText(
      "Delete web-1: done with a warning. backup deleted, but remote copies may be left: down",
    ),
  ).toBeInTheDocument();
  expect(warning).toHaveBeenCalledOnce();
});

it("leaves running and success toasts to the open center and offers View on failures", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  const loading = vi.spyOn(toast, "loading");
  const success = vi.spyOn(toast, "success");
  setCenterOpen(true);
  trackTask(task("20", "pending"), "Start web");
  trackTask(task("21", "pending"), "Start db");
  expect(loading).not.toHaveBeenCalled();
  settleTasks(
    { revision: 2, items: [task("20", "succeeded"), task("21", "failed", "no firmware")] },
    i18n.t,
  );
  expect(success).not.toHaveBeenCalled();
  // 실패는 센터가 열려 있어도 알리고, "보기"는 그 Task의 상세 서랍을 연다(계획 77 — 서랍이 없으면 센터).
  expect(await screen.findByText("Start db: failed. no firmware")).toBeInTheDocument();
  setCenterOpen(false);
  fireEvent.click(screen.getByRole("button", { name: "View" }));
  expect(isCenterOpen()).toBe(true);
});

it("opens the task drawer from View when the shell has one", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  const opened = vi.fn();
  const off = setTaskOpener(opened);
  try {
    trackTask(task("22", "pending"), "Start web");
    settleTasks({ revision: 2, items: [task("22", "failed", "boom")] }, i18n.t);
    fireEvent.click(await screen.findByRole("button", { name: "View" }));
    expect(opened).toHaveBeenCalledWith(task("22", "failed").id);
    expect(isCenterOpen()).toBe(false);
  } finally {
    off();
  }
});

it("offers to turn a stopped VM on again", async () => {
  const web = vm("web", "stopped");
  const start = vi.fn();
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      start(params.key, params.action);
      return HttpResponse.json(
        { ...task("31", "pending"), spec: { op: { kind: "vm-start" }, target: web.id, node: "n" } },
        { status: 202 },
      );
    }),
  );
  const queryClient = createQueryClient();
  queryClient.setQueryData(["list", "vm"], { revision: 1, items: [web] });
  const stop = {
    ...task("30", "pending"),
    spec: { op: { kind: "vm-stop" }, target: web.id, node: "n" },
  } as Task;
  queryClient.setQueryData(["list", "task"], {
    revision: 2,
    items: [{ ...stop, status: { state: "succeeded", error: null } }],
  });
  render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <TaskToasts />
        <Toaster />
      </QueryClientProvider>
    </ThemeProvider>,
  );
  trackTask(stop, "Stop web");
  expect(await screen.findByText("Stop web: done")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Turn on again" }));
  await waitFor(() => expect(start).toHaveBeenCalledWith(web.id, "start"));
});

it("does not turn a VM on again when it is already running", async () => {
  const web = vm("web", "stopped");
  const start = vi.fn();
  server.use(
    http.post(apiUrl("/vms/:key/actions/:action"), ({ params }) => {
      start(params.key, params.action);
      return HttpResponse.json(task("41", "pending"), { status: 202 });
    }),
  );
  const queryClient = createQueryClient();
  queryClient.setQueryData(["list", "vm"], { revision: 1, items: [web] });
  const stop = {
    ...task("40", "pending"),
    spec: { op: { kind: "vm-force-stop" }, target: web.id, node: "n" },
  } as Task;
  queryClient.setQueryData(["list", "task"], {
    revision: 2,
    items: [{ ...stop, status: { state: "succeeded", error: null } }],
  });
  render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <TaskToasts />
        <Toaster />
      </QueryClientProvider>
    </ThemeProvider>,
  );
  trackTask(stop, "Force stop web");
  expect(await screen.findByText("Force stop web: done")).toBeInTheDocument();
  // 그 사이 누가 켰다.
  queryClient.setQueryData(["list", "vm"], {
    revision: 2,
    items: [{ ...vm("web", "running"), revision: 2 }],
  });
  fireEvent.click(screen.getByRole("button", { name: "Turn on again" }));
  expect(await screen.findByText("The VM is running.")).toBeInTheDocument();
  expect(start).not.toHaveBeenCalled();
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
  const untemplate = {
    ...task("4", "pending"),
    spec: { op: { kind: "vm-untemplate" }, target: "x" },
  };
  expect(describeTask(untemplate as unknown as Task, i18n.t, "tmpl")).toBe(
    "Convert tmpl back to a VM",
  );
  const flatten = {
    ...task("4", "pending"),
    spec: { op: { kind: "volume-flatten" }, target: "x" },
  };
  expect(describeTask(flatten as unknown as Task, i18n.t, "web-1-disk0")).toBe(
    "Flatten volume web-1-disk0",
  );
  const reset = {
    ...task("4", "pending"),
    spec: { op: { kind: "vm-firmware-reset", uefi_vars: true }, target: "x" },
  };
  expect(describeTask(reset as unknown as Task, i18n.t, "win")).toBe("Reset the firmware of win");
});

it("names the failed node of an HA recovery", () => {
  const recover = {
    ...task("4", "succeeded"),
    spec: { op: { kind: "vm-ha-recover", from: "id-a" }, target: "x", node: "id-b" },
  } as Task;
  // 노드 이름을 모르면 id로
  expect(describeTask(recover, i18n.t, "web")).toBe("HA recovery of web (from node id-a)");
  const name = (id: string) => (id === "id-a" ? "node-a" : undefined);
  expect(describeTask(recover, i18n.t, "web", name)).toBe("HA recovery of web (from node node-a)");
});

it("names the power-off of a fenced node", () => {
  const off = {
    ...task("5", "succeeded"),
    spec: {
      op: { kind: "node-power-off", method: "ipmi", reason: "lease expired" },
      target: "id-b",
      node: "id-a",
    },
  } as Task;
  expect(describeTask(off, i18n.t, "node-b")).toBe("Power off node-b (fencing)");
});

it("names the leader's HA check reactions and bond warnings with their details", () => {
  const moved = {
    ...task("6", "succeeded"),
    spec: {
      op: {
        kind: "ha-check",
        check: "stor",
        action: "restart_elsewhere",
        vms: ["db", "web"],
        detail: "HA check stor failed on node-b",
      },
      target: "id-b",
      node: "id-a",
    },
  } as Task;
  expect(describeTask(moved, i18n.t, "node-b")).toBe("HA check stor on node-b: Restart elsewhere");
  expect(describeDetail(moved, i18n.t)).toBe("HA check stor failed on node-b VMs: db, web.");
  // 실패한 기록은 설명이 오류 칸에 있으므로 되풀이하지 않는다.
  const failed = {
    ...moved,
    spec: { ...moved.spec, op: { ...moved.spec.op, vms: [], action: "fence" } },
    status: { state: "failed", error: "HA check stor failed on node-b" },
  } as Task;
  expect(describeTask(failed, i18n.t, "node-b")).toBe(
    "HA check stor on node-b: Power off the node",
  );
  expect(describeDetail(failed, i18n.t)).toBeNull();
  const bond = {
    ...task("7", "succeeded"),
    spec: {
      op: { kind: "bond-degraded", bond: "bond0", ports: ["eno2: down", "eno3: lacp_mismatch"] },
      target: "id-b",
      node: "id-b",
    },
  } as Task;
  expect(describeTask(bond, i18n.t, "node-b")).toBe("Bond bond0 on node-b lost ports");
  expect(describeDetail(bond, i18n.t)).toBe("eno2: down, eno3: lacp_mismatch");
  expect(describeDetail(task("8", "succeeded"), i18n.t)).toBeNull();
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

it("describes the progress of downloads", () => {
  const t = i18n.t;
  const at = (stage: string, done: number, total: number) =>
    ({
      id: "t1",
      spec: { op: { kind: "volume-download", url: "https://x/a.iso", source_format: "raw" } },
      status: {
        state: "running",
        error: null,
        progress: { stage, done_bytes: done, total_bytes: total },
      },
    }) as Task;
  expect(describeProgress(at("downloading", 1024 ** 3, 4 * 1024 ** 3), t)).toBe(
    "Downloading 25% (1.0/4.0 GiB)",
  );
  // 전체 길이를 모르면(Content-Length 없음) 받은 양만
  expect(describeProgress(at("downloading", 2 * 1024 ** 3, 0), t)).toBe("Downloading (2.0 GiB)");
  expect(describeProgress(at("verifying", 1, 1), t)).toBe("Checking the checksum");
  expect(describeProgress(at("importing", 1, 1), t)).toBe("Importing into the volume");
  expect(describeTask(at("importing", 1, 1), t, "alpine")).toBe("Download volume alpine");
  // 계획 58: 체크섬 파일 단계와 받은 디스크로 템플릿 만들기
  expect(describeProgress(at("checksum", 0, 0), t)).toBe("Fetching the checksum file");
  expect(describeProgress(at("template", 1, 1), t)).toBe("Creating the template VM");
});

it("names the node a central kick removes and the central that does it", () => {
  // 리뷰 2 N-1·N-2: 지운 노드는 이름이 없어 Raft 주소로 댄다.
  const kick = {
    ...task("5", "pending"),
    spec: {
      op: { kind: "sdn-central-kick", node: "n9", address: "10.0.0.9" },
      target: "sdn-0001",
      node: "n1",
    },
  } as unknown as Task;
  const names: Record<string, string> = { n1: "node-a", n9: "node-z" };
  expect(describeTask(kick, i18n.t, "x", (id) => names[id])).toBe(
    "Kick node-z out of the OVN central databases (on node-a)",
  );
  expect(describeTask(kick, i18n.t, "x", (id) => (id === "n1" ? "node-a" : undefined))).toBe(
    "Kick 10.0.0.9 out of the OVN central databases (on node-a)",
  );
});
