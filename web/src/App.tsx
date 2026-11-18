import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { createBrowserRouter, type RouteObject, RouterProvider } from "react-router";

import { TokensPage } from "@/accounts/TokensPage";
import { UsersPage } from "@/accounts/UsersPage";
import { AlertsPage } from "@/alerts/AlertsPage";
import { NotificationTargetsPage } from "@/alerts/NotificationTargetsPage";
import { AuditPage } from "@/audit/AuditPage";
import { LiveProvider } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider } from "@/api/taskIndex";
import { AuthProvider } from "@/auth/AuthProvider";
import { RequireAuth } from "@/auth/RequireAuth";
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { BackupsPage } from "@/backups/BackupsPage";
import { CephPage } from "@/ceph/CephPage";
import { ClusterPage } from "@/cluster/ClusterPage";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { HaChecksPage } from "@/ha-checks/HaChecksPage";
import { HaGroupsPage } from "@/ha-groups/HaGroupsPage";
import { ImagesPage } from "@/images/ImagesPage";
import { ImportPage } from "@/imports/ImportPage";
import { ImportsPage } from "@/imports/ImportsPage";
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
import { PlacementPage } from "@/placement/PlacementPage";
import { PlacementRulesPage } from "@/placement/PlacementRulesPage";
import { RealmsPage } from "@/realms/RealmsPage";
import { RemotePage } from "@/remotes/RemotePage";
import { RemotesManagePage } from "@/remotes/RemotesManagePage";
import { RemotesPage } from "@/remotes/RemotesPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { SettingsPage } from "@/settings/SettingsPage";
import { SubscriptionPage } from "@/settings/SubscriptionPage";
import { PoolsPage } from "@/storage/PoolsPage";
import { VolumesPage } from "@/storage/VolumesPage";
import { TasksPage } from "@/tasks/TasksPage";
import { TemplatesPage } from "@/templates/TemplatesPage";
import { TaskToasts } from "@/tasks/TaskToasts";
import { ConsolePage } from "@/vms/ConsolePage";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmCreatePage, VmEditPage } from "@/vms/VmFormPage";
import { VmListPage } from "@/vms/VmListPage";

/** 앱의 경로 표. 넘기기 시험(`routes.test.tsx`)이 같은 표로 메모리 라우터를 만든다. */
export const appRoutes: RouteObject[] = [
  { path: "/login", element: <LoginPage /> },
  {
    element: <RequireAuth />,
    children: [
      {
        element: (
          <LiveProvider>
            <TaskIndexProvider>
              <TaskToasts />
              <AppLayout />
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
          { path: "images", element: <ImagesPage /> },
          { path: "imports", element: <ImportsPage /> },
          { path: "imports/:name", element: <ImportPage /> },
          { path: "volumes", element: <VolumesPage /> },
          { path: "isos", element: <IsosPage /> },
          { path: "storage-pools", element: <PoolsPage /> },
          { path: "ceph", element: <CephPage /> },
          { path: "backups", element: <BackupsPage /> },
          { path: "backup-jobs", element: <BackupJobsPage /> },
          { path: "networks", element: <NetworksPage /> },
          { path: "security-groups", element: <SecurityGroupsPage /> },
          { path: "security-groups/:name", element: <SecurityGroupPage /> },
          { path: "nodes", element: <NodesPage /> },
          { path: "nodes/:name", element: <NodeDetailPage /> },
          { path: "ha-groups", element: <HaGroupsPage /> },
          { path: "ha-checks", element: <HaChecksPage /> },
          { path: "placement-rules", element: <PlacementRulesPage /> },
          { path: "placement", element: <PlacementPage /> },
          { path: "tasks", element: <TasksPage /> },
          { path: "alerts", element: <AlertsPage /> },
          { path: "audit", element: <AuditPage /> },
          { path: "cluster", element: <ClusterPage /> },
          { path: "remotes", element: <RemotesPage /> },
          { path: "remotes/:name", element: <RemotePage /> },
          { path: "settings", element: <SettingsPage /> },
          { path: "settings/users", element: <UsersPage /> },
          { path: "settings/realms", element: <RealmsPage /> },
          { path: "settings/tokens", element: <TokensPage /> },
          { path: "settings/remotes", element: <RemotesManagePage /> },
          { path: "settings/notifications", element: <NotificationTargetsPage /> },
          { path: "settings/subscription", element: <SubscriptionPage /> },
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
