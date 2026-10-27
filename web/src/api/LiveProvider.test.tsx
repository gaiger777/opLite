import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { http, HttpResponse, ws } from "msw";

import { listKey, useList } from "@/api/live";
import { LiveProvider, useLiveStatus } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { apiUrl, server } from "@/test/server";

const watch = ws.link(`ws://${window.location.host}/api/v1/watch`);

// 실패한 테스트가 가짜 타이머를 남기지 않게 한다.
afterEach(() => vi.useRealTimers());

function vm(id: string, revision: number, phase: string) {
  return { id, name: id, revision, spec: {}, status: { phase } };
}

function Probe() {
  const status = useLiveStatus();
  const vms = useList("vm");
  return (
    <p>
      {status}: {vms.data?.map((v) => `${v.name}=${v.status.phase}`).join(",")}
    </p>
  );
}

function lists(vms: unknown[], revision: number) {
  const empty = () => HttpResponse.json({ revision, items: [] });
  return [
    http.get(apiUrl("/vms"), () => HttpResponse.json({ revision, items: vms })),
    http.get(apiUrl("/volumes"), empty),
    http.get(apiUrl("/storage-pools"), empty),
    http.get(apiUrl("/nodes"), empty),
    http.get(apiUrl("/tasks"), empty),
    http.get(apiUrl("/networks"), empty),
    http.get(apiUrl("/snapshots"), empty),
  ];
}

it("watches from the oldest list revision and applies changes", async () => {
  const urls: string[] = [];
  server.use(
    ...lists([vm("web", 3, "stopped")], 7),
    watch.addEventListener("connection", ({ client }) => {
      urls.push(client.url.toString());
      client.send(JSON.stringify({ type: "put", kind: "vm", resource: vm("web", 9, "running") }));
    }),
  );
  render(
    <QueryClientProvider client={createQueryClient()}>
      <LiveProvider>
        <Probe />
      </LiveProvider>
    </QueryClientProvider>,
  );
  expect(await screen.findByText("live: web=running")).toBeInTheDocument();
  expect(urls[0]).toContain("kinds=vm,volume,storage-pool,node,task,network,snapshot&since=7");
});

it("reads the lists again when the watch cannot continue", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let revision = 7;
  let connections = 0;
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ revision, items: [vm("web", revision, `r${revision}`)] }),
    ),
    ...lists([], 7).slice(1),
    watch.addEventListener("connection", ({ client }) => {
      connections += 1;
      if (connections === 1) {
        revision = 20;
        client.send(JSON.stringify({ type: "error", code: "lagged" }));
      }
    }),
  );
  render(
    <QueryClientProvider client={createQueryClient()}>
      <LiveProvider>
        <Probe />
      </LiveProvider>
    </QueryClientProvider>,
  );
  expect(await screen.findByText("offline: web=r7")).toBeInTheDocument();
  await vi.advanceTimersByTimeAsync(1000);
  expect(await screen.findByText("live: web=r20")).toBeInTheDocument();
  expect(connections).toBe(2);
});

function renderLive() {
  render(
    <QueryClientProvider client={createQueryClient()}>
      <LiveProvider>
        <Probe />
      </LiveProvider>
    </QueryClientProvider>,
  );
}

it("ignores unknown frames and reconnects after an unreadable one", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let connections = 0;
  server.use(
    ...lists([vm("web", 3, "stopped")], 7),
    watch.addEventListener("connection", ({ client }) => {
      connections += 1;
      if (connections === 1) {
        client.send(JSON.stringify({ type: "bookmark", revision: 8 }));
        client.send(JSON.stringify({ type: "put", kind: "vm", resource: vm("web", 9, "running") }));
        client.send("{not json");
      }
    }),
  );
  renderLive();
  // 모르는 프레임 뒤의 변경도 반영하고, 읽을 수 없는 프레임에서 끊는다.
  expect(await screen.findByText("offline: web=running")).toBeInTheDocument();
  await vi.advanceTimersByTimeAsync(1000);
  expect(await screen.findByText("live: web=stopped")).toBeInTheDocument();
  expect(connections).toBe(2);
});

it("keeps backing off while the watch fails right away", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  let connections = 0;
  let last: { close(): void } | undefined;
  server.use(
    ...lists([], 7),
    watch.addEventListener("connection", ({ client }) => {
      connections += 1;
      last = client;
      if (connections <= 2) {
        client.send(JSON.stringify({ type: "error", code: "lagged" }));
      }
    }),
  );
  renderLive();
  expect(await screen.findByText(/^offline/)).toBeInTheDocument();
  await vi.advanceTimersByTimeAsync(1000);
  await vi.waitFor(() => expect(connections).toBe(2));
  // 곧바로 다시 끊겼으므로 이번에는 2초를 기다린다.
  await vi.advanceTimersByTimeAsync(1000);
  expect(connections).toBe(2);
  await vi.advanceTimersByTimeAsync(1000);
  await vi.waitFor(() => expect(connections).toBe(3));
  expect(await screen.findByText(/^live/)).toBeInTheDocument();

  // 30초 넘게 이어져 있다 끊기면 다시 1초부터 기다린다.
  await vi.advanceTimersByTimeAsync(31_000);
  last!.close();
  expect(await screen.findByText(/^offline/)).toBeInTheDocument();
  await vi.advanceTimersByTimeAsync(1000);
  await vi.waitFor(() => expect(connections).toBe(4));
});

it("keeps lists cached when no screen shows them", async () => {
  vi.useFakeTimers();
  const queryClient = createQueryClient();
  const empty = async () => ({ revision: 1, items: [] });
  await queryClient.fetchQuery({ queryKey: listKey("vm"), queryFn: empty });
  await queryClient.fetchQuery({ queryKey: ["cluster"], queryFn: empty });
  await vi.advanceTimersByTimeAsync(10 * 60_000);
  // 목록은 watch가 계속 바꾸므로 남고, 다른 조회는 지워진다.
  expect(queryClient.getQueryData(listKey("vm"))).toBeDefined();
  expect(queryClient.getQueryData(["cluster"])).toBeUndefined();
});
