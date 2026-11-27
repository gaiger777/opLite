import { screen, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { PlansPage } from "@/dr/PlansPage";
import { expectNoAxeViolations } from "@/test/axe";
import { DR_NODE, plan, RUN_ID } from "@/test/dr";
import { NODE } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

function setup(items = [plan("web-tier", { status: { run: RUN_ID } })], nodes = [DR_NODE]) {
  mockLists({ node: nodes });
  server.use(http.get(apiUrl("/dr-plans"), () => HttpResponse.json({ revision: 1, items })));
}

it("lists plans with their source, VMs, RPO bar, last test and running run", async () => {
  setup();
  const { container } = renderPage(<PlansPage />, { path: "/dr-plans" });
  const table = await screen.findByRole("table", { name: "DR" });
  const row = within(table).getByRole("row", { name: /web-tier/ });
  expect(within(row).getByRole("link", { name: "web-tier" })).toHaveAttribute(
    "href",
    "/dr-plans/web-tier",
  );
  expect(row).toHaveTextContent("prod");
  expect(row).toHaveTextContent("3");
  // 셋 가운데 둘이 RPO를 지킨다(복구 지점 없는 web2는 빨강)
  expect(within(row).getByRole("meter", { name: "VMs within RPO" })).toHaveAttribute(
    "aria-valuenow",
    "2",
  );
  expect(row).toHaveTextContent("2/3 within RPO");
  expect(row).toHaveTextContent("Succeeded");
  expect(row).toHaveTextContent("RTO 12m 34s");
  expect(within(row).getByRole("link", { name: "Running" })).toHaveAttribute(
    "href",
    `/dr-runs/${RUN_ID}`,
  );
  expect(screen.getByRole("link", { name: "New plan" })).toHaveAttribute("href", "/dr-plans/-new");
  await expectNoAxeViolations(container);
});

it("offers no new plan to viewers", async () => {
  setup([]);
  renderPage(<PlansPage />, { path: "/dr-plans", as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("No recovery plans")).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "New plan" })).not.toBeInTheDocument();
});

it("tells which nodes lack DR plans and offers no new plan", async () => {
  setup([], [DR_NODE, { ...NODE, id: "n2", name: "node-b" }]);
  renderPage(<PlansPage />, { path: "/dr-plans" });
  expect(
    await screen.findByText(/Some nodes do not support DR plans yet \(node-b\)/),
  ).toBeInTheDocument();
  expect(await screen.findByText("No recovery plans")).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "New plan" })).not.toBeInTheDocument();
});
