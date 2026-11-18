import { screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import type { LiveStatus } from "@/api/LiveProvider";
import { AppLayout } from "@/layout/AppLayout";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

let status: LiveStatus = "offline";
vi.mock("@/api/LiveProvider", () => ({ useLiveStatus: () => status }));

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);

const BANNER = "The live connection was lost – reconnecting. What you see may be out of date.";

it("shows a banner above the page while the live connection is down", async () => {
  status = "offline";
  renderPage(<AppLayout />, { path: "*", url: "/vms" });
  const banner = await screen.findByText(BANNER);
  expect(banner).toHaveAttribute("data-offline-banner");
  // 머리줄의 상태(role=status)가 읽어 주므로 띠는 역할이 없다.
  expect(banner).not.toHaveAttribute("role");
});

it("has no banner while live", async () => {
  status = "live";
  renderPage(<AppLayout />, { path: "*", url: "/vms" });
  expect(await screen.findByRole("navigation", { name: "Main" })).toBeInTheDocument();
  expect(screen.queryByText(BANNER)).not.toBeInTheDocument();
});
