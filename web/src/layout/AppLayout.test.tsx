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
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
    "href",
    "/security-groups",
  );
  expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/backups");
  expect(screen.getByRole("link", { name: "Backup jobs" })).toHaveAttribute("href", "/backup-jobs");
  expect(screen.getByRole("link", { name: "Imports" })).toHaveAttribute("href", "/imports");
  // 설정도 누구나 읽는다(바꾸기는 admin만).
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute("href", "/settings");
});
