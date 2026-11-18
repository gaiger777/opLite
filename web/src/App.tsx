import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { createBrowserRouter, type RouteObject, RouterProvider } from "react-router";

import { LiveProvider } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider } from "@/api/taskIndex";
import { AuthProvider } from "@/auth/AuthProvider";
import { RequireAuth } from "@/auth/RequireAuth";
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { BackupsPage } from "@/backups/BackupsPage";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { IsosPage } from "@/isos/IsosPage";
import { AppLayout } from "@/layout/AppLayout";
import { AREAS, LEGACY_REDIRECTS } from "@/layout/nav";
import { AreaRedirect, Redirect } from "@/layout/Redirect";
import { DensityProvider } from "@/lib/density";
import { ThemeProvider } from "@/lib/theme";
import { DashboardPage } from "@/pages/DashboardPage";
import { LoginPage } from "@/pages/LoginPage";
import { NetworksPage } from "@/networks/NetworksPage";
import { NodeDetailPage } from "@/nodes/NodeDetailPage";
import { NodesPage } from "@/nodes/NodesPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { PoolsPage } from "@/storage/PoolsPage";
import { VolumesPage } from "@/storage/VolumesPage";
import { TasksPage } from "@/tasks/TasksPage";
import { TemplatesPage } from "@/templates/TemplatesPage";
import { TaskToasts } from "@/tasks/TaskToasts";
import { ConsolePage } from "@/vms/ConsolePage";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmActionHost } from "@/vms/VmActionHost";
import { VmCreatePage, VmEditPage } from "@/vms/VmFormPage";
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
          { index: true, element: <DashboardPage /> },
          { path: "vms", element: <VmListPage /> },
          // VM 이름은 `-`로 시작할 수 없으므로 "new"라는 VM과 겹치지 않는다.
          { path: "vms/-new", element: <VmCreatePage /> },
          { path: "vms/:name", element: <VmDetailPage /> },
          { path: "vms/:name/edit", element: <VmEditPage /> },
          { path: "vms/:name/console", element: <ConsolePage /> },
          { path: "templates", element: <TemplatesPage /> },
          { path: "images", lazy: page(() => import("@/images/ImagesPage"), "ImagesPage") },
          { path: "imports", lazy: page(() => import("@/imports/ImportsPage"), "ImportsPage") },
          { path: "imports/:name", lazy: page(() => import("@/imports/ImportPage"), "ImportPage") },
          { path: "volumes", element: <VolumesPage /> },
          { path: "isos", element: <IsosPage /> },
          { path: "storage-pools", element: <PoolsPage /> },
          { path: "ceph", lazy: page(() => import("@/ceph/CephPage"), "CephPage") },
          { path: "backups", element: <BackupsPage /> },
          { path: "backup-jobs", element: <BackupJobsPage /> },
          { path: "networks", element: <NetworksPage /> },
          { path: "security-groups", element: <SecurityGroupsPage /> },
          { path: "security-groups/:name", element: <SecurityGroupPage /> },
          { path: "nodes", element: <NodesPage /> },
          { path: "nodes/:name", element: <NodeDetailPage /> },
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
          { path: "remotes/:name", lazy: page(() => import("@/remotes/RemotePage"), "RemotePage") },
          { path: "settings", lazy: page(() => import("@/settings/SettingsPage"), "SettingsPage") },
          { path: "settings/users", lazy: page(() => import("@/accounts/UsersPage"), "UsersPage") },
          {
            path: "settings/realms",
            lazy: page(() => import("@/realms/RealmsPage"), "RealmsPage"),
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
            lazy: page(() => import("@/alerts/NotificationTargetsPage"), "NotificationTargetsPage"),
          },
          {
            path: "settings/subscription",
            lazy: page(() => import("@/settings/SubscriptionPage"), "SubscriptionPage"),
          },
          // 영역 주소는 기억한 탭으로 간다(`/`·`/vms`·`/settings`는 첫 탭 화면 자체다).
          ...AREAS.filter((area) => !area.tabs.some((tab) => tab.to === area.to)).map((area) => ({
            path: area.to.slice(1),
            element: <AreaRedirect area={area.id} />,
          })),
          ...LEGACY_REDIRECTS.map(({ from, to }) => ({
            path: from.slice(1),
            element: <Redirect to={to} />,
          })),
          { path: "*", element: <NotFoundPage /> },
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
