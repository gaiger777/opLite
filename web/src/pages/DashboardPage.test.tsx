import { screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { DashboardPage } from "@/pages/DashboardPage";
import { NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

it("summarizes nodes, VMs and failures", async () => {
  const down = { ...NODE, id: "n2", name: "node-b", status: { ...NODE.status, online: false } };
  const broken = vm("db", "failed", {
    status: { phase: "failed", node: null, pid: null, last_error: "no firmware" },
  });
  mockLists({ node: [NODE, down], vm: [vm("web", "running"), vm("app", "running"), broken] });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
  );
  renderPage(<DashboardPage />);
  expect(await screen.findByText("1 / 2")).toBeInTheDocument();
  expect(screen.getByText("2 running · 0 stopped · 1 failed")).toBeInTheDocument();
  expect(await screen.findByText("node-a", { selector: "div" })).toBeInTheDocument();
  const row = screen.getByText("node-a", { selector: "td" }).closest("tr")!;
  expect(row).toHaveTextContent("4 / 8");
  expect(row).toHaveTextContent("4 GiB / 16 GiB");
  expect(screen.getByText("Offline")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "db" })).toBeInTheDocument();
  expect(screen.getByText("no firmware")).toBeInTheDocument();
});
