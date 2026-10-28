import { QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";

import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider, useTaskIndex } from "@/api/taskIndex";
import type { Task } from "@/api/types";
import { task } from "@/test/fixtures";

function of(id: string, target: string, state: Task["status"]["state"]): Task {
  return { ...task(id, "vm-start", target), status: { state, error: null } };
}

it("indexes unfinished tasks by target", async () => {
  const queryClient = createQueryClient();
  queryClient.setQueryData(["list", "task"], {
    revision: 1,
    items: [
      of("1", "web", "pending"),
      of("2", "web", "running"),
      of("3", "web", "succeeded"),
      of("4", "db", "failed"),
      of("5", "db", "running"),
    ],
  });
  const { result } = renderHook(() => useTaskIndex(), {
    wrapper: ({ children }) => (
      <QueryClientProvider client={queryClient}>
        <TaskIndexProvider>{children}</TaskIndexProvider>
      </QueryClientProvider>
    ),
  });
  await waitFor(() => expect(result.current.size).toBe(2));
  expect(result.current.get("web")?.map((t) => t.id)).toEqual(["1", "2"]);
  expect(result.current.get("db")?.map((t) => t.id)).toEqual(["5"]);
});
