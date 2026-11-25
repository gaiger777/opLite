import { screen, within } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { expectNoAxeViolations } from "@/test/axe";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { migration, vmwareSource } from "@/test/vmware";
import { MigrationsPage } from "@/vmware/MigrationsPage";

function setup(
  items = [
    migration("wave1", 1, { status: { phase: "running", counts: { syncing: 3, done: 1 } } }),
  ],
) {
  mockLists({});
  server.use(
    http.get(apiUrl("/vmware-migrations"), () => HttpResponse.json({ revision: 1, items })),
    http.get(apiUrl("/vmware-sources"), () =>
      HttpResponse.json({ revision: 1, items: [vmwareSource("vc1")] }),
    ),
  );
}

it("lists migrations with their source, mode, VM counts and phase", async () => {
  setup();
  const { container } = renderPage(<MigrationsPage />, { path: "/vmware-migrations" });
  const table = await screen.findByRole("table", { name: "VMware migrations" });
  const row = within(table).getByRole("row", { name: /wave1/ });
  expect(within(row).getByRole("link", { name: "wave1" })).toHaveAttribute(
    "href",
    "/vmware-migrations/wave1",
  );
  expect(row).toHaveTextContent("vc1");
  expect(row).toHaveTextContent("Warm");
  expect(row).toHaveTextContent("4");
  expect(row).toHaveTextContent("3 Syncing · 1 Done");
  expect(row).toHaveTextContent("Running");
  expect(screen.getByRole("link", { name: "New migration" })).toHaveAttribute(
    "href",
    "/vmware-migrations/-new",
  );
  await expectNoAxeViolations(container);
});

it("offers no new migration without vm.create", async () => {
  setup([]);
  renderPage(<MigrationsPage />, { path: "/vmware-migrations", as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("No VMware migrations")).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "New migration" })).not.toBeInTheDocument();
});
