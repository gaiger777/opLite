import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { SettingsPage } from "@/settings/SettingsPage";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const SETTINGS = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 3,
  labels: {},
  spec: { updates: { channel: "stable" } },
  status: {},
};

it("turns the Prometheus endpoint on and off", async () => {
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { prometheus: { enabled: boolean } };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({
        ...SETTINGS,
        revision: SETTINGS.revision + sent.length,
        spec: {
          ...SETTINGS.spec,
          ...(body.prometheus.enabled && { prometheus: { enabled: true } }),
        },
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SettingsPage />, { path: "/settings" });
  const section = await screen.findByRole("region", { name: "Prometheus" });
  expect(within(section).getByText("Off")).toBeInTheDocument();
  expect(within(section).getByText(`${window.location.origin}/api/v1/metrics`)).toBeInTheDocument();
  expect(
    within(section).getByLabelText("Example Prometheus configuration (scrape_configs)"),
  ).toHaveTextContent("metrics_path: /api/v1/metrics");
  expect(
    within(section).getByRole("button", { name: "Copy example Prometheus configuration" }),
  ).toBeInTheDocument();
  await user.click(within(section).getByRole("button", { name: "Turn on Prometheus endpoint" }));
  expect(await screen.findByText("The Prometheus endpoint is on.")).toBeInTheDocument();
  expect(within(section).getByText("On")).toBeInTheDocument();
  await user.click(within(section).getByRole("button", { name: "Turn off Prometheus endpoint" }));
  expect(await screen.findByText("The Prometheus endpoint is off.")).toBeInTheDocument();
  expect(sent).toEqual([
    { ifMatch: "3", body: { prometheus: { enabled: true } } },
    { ifMatch: "4", body: { prometheus: { enabled: false } } },
  ]);
});

it("says when old nodes do not know the Prometheus setting", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "cluster_upgrading", message: "node-b does not support metrics-history" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SettingsPage />, { path: "/settings" });
  const section = await screen.findByRole("region", { name: "Prometheus" });
  await userEvent.click(
    within(section).getByRole("button", { name: "Turn on Prometheus endpoint" }),
  );
  expect(await within(section).findByRole("alert")).toHaveTextContent(
    "The cluster is being upgraded: node-b does not support metrics-history",
  );
});

it("shows Prometheus read-only to non-admins", async () => {
  server.use(http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)));
  renderPage(<SettingsPage />, {
    path: "/settings",
    as: { user: "00000000-0000-0000-0000-000000000002", name: "eve", role: "viewer" },
  });
  const section = await screen.findByRole("region", { name: "Prometheus" });
  expect(
    within(section).queryByRole("button", { name: /Prometheus endpoint/ }),
  ).not.toBeInTheDocument();
});
