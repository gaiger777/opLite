import { QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { http, HttpResponse, ws } from "msw";

import { useList } from "@/api/live";
import { LiveProvider, useLiveStatus } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { apiUrl, server } from "@/test/server";

const watch = ws.link(`ws://${window.location.host}/api/v1/watch`);

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
  expect(urls[0]).toContain("kinds=vm,volume,storage-pool,node,task,network&since=7");
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
  vi.useRealTimers();
});
