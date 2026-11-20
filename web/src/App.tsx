import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { createBrowserRouter, type RouteObject, RouterProvider } from "react-router";

import { LiveProvider } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider } from "@/api/taskIndex";
import { AuthProvider } from "@/auth/AuthProvider";
import { RequireAuth } from "@/auth/RequireAuth";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { AppLayout } from "@/layout/AppLayout";
import { AREAS, LEGACY_REDIRECTS } from "@/layout/nav";
import { AreaRedirect, Redirect } from "@/layout/Redirect";
import { RouteError } from "@/layout/RouteError";
import { DensityProvider } from "@/lib/density";
import { ThemeProvider } from "@/lib/theme";
import { DashboardPage } from "@/pages/DashboardPage";
import { LoginPage } from "@/pages/LoginPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
import { TasksPage } from "@/tasks/TasksPage";
import { TaskToasts } from "@/tasks/TaskToasts";
import { VmActionHost } from "@/vms/VmActionHost";
import { VmListPage } from "@/vms/VmListPage";

/**
 * 드물게 여는 화면은 열 때 내려받는다(설계 §9 묶음 크기). 모듈의 이름 붙은 내보내기를 경로의 `Component`로 준다.
 */
function page<M, K extends keyof M>(load: () => Promise<M>, name: K) {
  return async () => ({ Component: (await load())[name] as React.ComponentType });
}

/** 앱의 경로 표. 넘기기 시험(`routes.test.tsx`)이 같은 표로 메모리 라우터를 만든다. */
export const appRoutes: RouteObject[] = [
  { path: "/login", element: <LoginPage /> },
  {
    element: <RequireAuth />,
    // 바로 연 주소가 늦게 받는 화면이면 받을 때까지 아무것도 그리지 않는다.
    HydrateFallback: () => null,
    // 셸 자체가 깨지면 셸 없이 같은 오류 화면을 보인다.
    errorElement: <RouteError />,
    children: [
      {
        element: (
          <LiveProvider>
            <TaskIndexProvider>
              <TaskToasts />
              <VmActionHost>
                <AppLayout />
              </VmActionHost>
            </TaskIndexProvider>
          </LiveProvider>
        ),
        children: [
          {
            // 화면의 오류(받지 못한 조각 포함)는 셸(사이드바·머리) 안에 보인다.
            errorElement: <RouteError />,
            children: [
              { index: true, element: <DashboardPage /> },
              { path: "vms", element: <VmListPage /> },
              // 계획 62: 목록 | 폴더 보기 전환의 폴더 쪽과 폴더 상세
              ...(
                [
                  ["vm-folders", "VmFoldersPage", "VmFolderDetailPage"],
                  ["network-folders", "NetworkFoldersPage", "NetworkFolderDetailPage"],
                  ["storage-folders", "StorageFoldersPage", "StorageFolderDetailPage"],
                ] as const
              ).flatMap(([path, list, detail]) => [
                { path, lazy: page(() => import("@/folders/FolderPages"), list) },
                { path: `${path}/:id`, lazy: page(() => import("@/folders/FolderPages"), detail) },
              ]),
              // VM 이름은 `-`로 시작할 수 없으므로 "new"라는 VM과 겹치지 않는다.
              { path: "vms/-new", lazy: page(() => import("@/vms/VmFormPage"), "VmCreatePage") },
              { path: "vms/:name", lazy: page(() => import("@/vms/VmDetailPage"), "VmDetailPage") },
              {
                path: "vms/:name/edit",
                lazy: page(() => import("@/vms/VmFormPage"), "VmEditPage"),
              },
              {
                path: "vms/:name/console",
                lazy: page(() => import("@/vms/ConsolePage"), "ConsolePage"),
              },
              {
                path: "templates",
                lazy: page(() => import("@/templates/TemplatesPage"), "TemplatesPage"),
              },
              { path: "images", lazy: page(() => import("@/images/ImagesPage"), "ImagesPage") },
              { path: "imports", lazy: page(() => import("@/imports/ImportsPage"), "ImportsPage") },
              {
                path: "imports/:name",
                lazy: page(() => import("@/imports/ImportPage"), "ImportPage"),
              },
              { path: "volumes", lazy: page(() => import("@/storage/VolumesPage"), "VolumesPage") },
              { path: "isos", lazy: page(() => import("@/isos/IsosPage"), "IsosPage") },
              {
                path: "storage-pools",
                lazy: page(() => import("@/storage/PoolsPage"), "PoolsPage"),
              },
              { path: "ceph", lazy: page(() => import("@/ceph/CephPage"), "CephPage") },
              { path: "backups", lazy: page(() => import("@/backups/BackupsPage"), "BackupsPage") },
              {
                path: "backup-jobs",
                lazy: page(() => import("@/backups/BackupJobsPage"), "BackupJobsPage"),
              },
              {
                path: "networks",
                lazy: page(() => import("@/networks/NetworksPage"), "NetworksPage"),
              },
              { path: "sdn", lazy: page(() => import("@/sdn/SdnPage"), "SdnPage") },
              {
                path: "security-groups",
                lazy: page(
                  () => import("@/security-groups/SecurityGroupsPage"),
                  "SecurityGroupsPage",
                ),
              },
              {
                path: "security-groups/:name",
                lazy: page(
                  () => import("@/security-groups/SecurityGroupPage"),
                  "SecurityGroupPage",
                ),
              },
              { path: "nodes", lazy: page(() => import("@/nodes/NodesPage"), "NodesPage") },
              {
                path: "nodes/:name",
                lazy: page(() => import("@/nodes/NodeDetailPage"), "NodeDetailPage"),
              },
              {
                path: "ha-groups",
                lazy: page(() => import("@/ha-groups/HaGroupsPage"), "HaGroupsPage"),
              },
              {
                path: "ha-checks",
                lazy: page(() => import("@/ha-checks/HaChecksPage"), "HaChecksPage"),
              },
              {
                path: "placement-rules",
                lazy: page(() => import("@/placement/PlacementRulesPage"), "PlacementRulesPage"),
              },
              {
                path: "placement",
                lazy: page(() => import("@/placement/PlacementPage"), "PlacementPage"),
              },
              { path: "tasks", element: <TasksPage /> },
              { path: "alerts", lazy: page(() => import("@/alerts/AlertsPage"), "AlertsPage") },
              { path: "audit", lazy: page(() => import("@/audit/AuditPage"), "AuditPage") },
              { path: "cluster", lazy: page(() => import("@/cluster/ClusterPage"), "ClusterPage") },
              { path: "remotes", lazy: page(() => import("@/remotes/RemotesPage"), "RemotesPage") },
              {
                path: "remotes/:name",
                lazy: page(() => import("@/remotes/RemotePage"), "RemotePage"),
              },
              {
                path: "settings",
                lazy: page(() => import("@/settings/SettingsPage"), "SettingsPage"),
              },
              {
                path: "settings/users",
                lazy: page(() => import("@/accounts/UsersPage"), "UsersPage"),
              },
              {
                path: "settings/realms",
                lazy: page(() => import("@/realms/RealmsPage"), "RealmsPage"),
              },
              {
                path: "settings/permissions",
                lazy: page(() => import("@/permissions/SettingsPermissionsPage"), "GrantsPage"),
              },
              {
                path: "settings/roles",
                lazy: page(() => import("@/permissions/SettingsPermissionsPage"), "RolesPage"),
              },
              {
                path: "settings/groups",
                lazy: page(() => import("@/permissions/SettingsPermissionsPage"), "GroupsPage"),
              },
              {
                path: "settings/security",
                lazy: page(() => import("@/accounts/TwoFactorPages"), "SecurityPage"),
              },
              {
                path: "settings/account",
                lazy: page(() => import("@/accounts/TwoFactorPages"), "AccountPage"),
              },
              {
                path: "settings/tokens",
                lazy: page(() => import("@/accounts/TokensPage"), "TokensPage"),
              },
              {
                path: "settings/remotes",
                lazy: page(() => import("@/remotes/RemotesManagePage"), "RemotesManagePage"),
              },
              {
                path: "settings/notifications",
                lazy: page(
                  () => import("@/alerts/NotificationTargetsPage"),
                  "NotificationTargetsPage",
                ),
              },
              {
                path: "settings/subscription",
                lazy: page(() => import("@/settings/SubscriptionPage"), "SubscriptionPage"),
              },
              // 영역 주소는 기억한 탭으로 간다(`/`·`/vms`·`/settings`는 첫 탭 화면 자체다).
              ...AREAS.filter((area) => !area.tabs.some((tab) => tab.to === area.to)).map(
                (area) => ({
                  path: area.to.slice(1),
                  element: <AreaRedirect area={area.id} />,
                }),
              ),
              ...LEGACY_REDIRECTS.map(({ from, to }) => ({
                path: from.slice(1),
                element: <Redirect to={to} />,
              })),
              { path: "*", element: <NotFoundPage /> },
            ],
          },
        ],
      },
    ],
  },
];

const routes = () => createBrowserRouter(appRoutes);

export function App() {
  const [queryClient] = useState(createQueryClient);
  const [router] = useState(routes);
  return (
    <ThemeProvider>
      <DensityProvider>
        <TooltipProvider delayDuration={300}>
          <QueryClientProvider client={queryClient}>
            <AuthProvider>
              <RouterProvider router={router} />
            </AuthProvider>
            <Toaster position="bottom-right" />
          </QueryClientProvider>
        </TooltipProvider>
      </DensityProvider>
    </ThemeProvider>
  );
}
