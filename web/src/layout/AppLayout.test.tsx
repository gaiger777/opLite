import { screen } from "@testing-library/react";
import { http, HttpResponse } from "msw";

import { AppLayout } from "@/layout/AppLayout";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ name: "lab", ca_fingerprint: "x", node: "n1", members: [] }),
    ),
  ),
);

it("shows account pages only to admins", async () => {
  renderPage(<AppLayout />);
  expect(await screen.findByRole("link", { name: "Volumes" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Users" })).toHaveAttribute("href", "/users");
});

it("hides account pages from other roles", async () => {
  renderPage(<AppLayout />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("link", { name: "Volumes" })).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
});
