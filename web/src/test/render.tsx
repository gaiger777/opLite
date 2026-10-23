import { QueryClientProvider } from "@tanstack/react-query";
import { render } from "@testing-library/react";
import { http, HttpResponse } from "msw";
import { createMemoryRouter, RouterProvider } from "react-router";

import { createQueryClient } from "@/api/queryClient";
import type { Principal, Resources, WatchKind } from "@/api/types";
import { AuthProvider } from "@/auth/AuthProvider";
import { Toaster } from "@/components/ui/sonner";
import { ThemeProvider } from "@/lib/theme";
import { apiUrl, server } from "@/test/server";

const PATHS: Record<WatchKind, string> = {
  vm: "/vms",
  volume: "/volumes",
  "storage-pool": "/storage-pools",
  node: "/nodes",
  task: "/tasks",
  network: "/networks",
};

/** 목록 API의 응답을 정한다. 주지 않은 종류는 빈 목록이다. */
export function mockLists(lists: { [K in WatchKind]?: Partial<Resources[K]>[] }) {
  server.use(
    ...(Object.keys(PATHS) as WatchKind[]).map((kind) =>
      http.get(apiUrl(PATHS[kind]), () =>
        HttpResponse.json({ revision: 1, items: lists[kind] ?? [] }),
      ),
    ),
  );
}

export const ADMIN: Principal = {
  user: "00000000-0000-0000-0000-000000000001",
  name: "admin",
  role: "admin",
};

/** 화면 하나를 라우터와 함께 그린다. `as`로 로그인한 사용자를 정한다. */
export function renderPage(
  element: React.ReactElement,
  { path = "/", url = path, as = ADMIN }: { path?: string; url?: string; as?: Principal } = {},
) {
  server.use(http.get(apiUrl("/auth/whoami"), () => HttpResponse.json(as)));
  const router = createMemoryRouter(
    [
      { path, element },
      { path: "*", element: <p>moved</p> },
    ],
    { initialEntries: [url] },
  );
  const queryClient = createQueryClient();
  const result = render(
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>
          <RouterProvider router={router} />
        </AuthProvider>
        <Toaster />
      </QueryClientProvider>
    </ThemeProvider>,
  );
  return { ...result, router, queryClient };
}
