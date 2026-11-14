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
  expect(screen.getByRole("link", { name: "Audit log" })).toHaveAttribute("href", "/audit");
  expect(screen.getByRole("link", { name: "Realms" })).toHaveAttribute("href", "/realms");
});

it("hides account pages from other roles", async () => {
  renderPage(<AppLayout />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("link", { name: "Volumes" })).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 감사 로그도 admin만 읽는다.
  expect(screen.queryByRole("link", { name: "Audit log" })).not.toBeInTheDocument();
  // 인증 영역도 계정 종류처럼 admin만 다룬다.
  expect(screen.queryByRole("link", { name: "Realms" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업, 가져오기는 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
    "href",
    "/security-groups",
  );
  expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/backups");
  expect(screen.getByRole("link", { name: "Backup jobs" })).toHaveAttribute("href", "/backup-jobs");
  expect(screen.getByRole("link", { name: "Imports" })).toHaveAttribute("href", "/imports");
  // HA 그룹과 배치 규칙도 누구나 읽는다(바꾸기는 admin만).
  expect(screen.getByRole("link", { name: "HA groups" })).toHaveAttribute("href", "/ha-groups");
  // HA 검사도 누구나 읽는다(바꾸기는 admin만).
  expect(screen.getByRole("link", { name: "HA checks" })).toHaveAttribute("href", "/ha-checks");
  expect(screen.getByRole("link", { name: "Placement rules" })).toHaveAttribute(
    "href",
    "/placement-rules",
  );
  // 설정도 누구나 읽는다(바꾸기는 admin만).
  expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute("href", "/settings");
  // 원격 클러스터도 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Remote clusters" })).toHaveAttribute("href", "/remotes");
});
