import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { createBrowserRouter, RouterProvider } from "react-router";

import { TokensPage } from "@/accounts/TokensPage";
import { UsersPage } from "@/accounts/UsersPage";
import { AuditPage } from "@/audit/AuditPage";
import { LiveProvider } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { TaskIndexProvider } from "@/api/taskIndex";
import { AuthProvider } from "@/auth/AuthProvider";
import { RequireAuth } from "@/auth/RequireAuth";
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { BackupsPage } from "@/backups/BackupsPage";
import { ClusterPage } from "@/cluster/ClusterPage";
import { Toaster } from "@/components/ui/sonner";
import { HaChecksPage } from "@/ha-checks/HaChecksPage";
import { HaGroupsPage } from "@/ha-groups/HaGroupsPage";
import { ImportPage } from "@/imports/ImportPage";
import { ImportsPage } from "@/imports/ImportsPage";
import { AppLayout } from "@/layout/AppLayout";
import { ThemeProvider } from "@/lib/theme";
import { DashboardPage } from "@/pages/DashboardPage";
import { LoginPage } from "@/pages/LoginPage";
import { NetworksPage } from "@/networks/NetworksPage";
import { NodesPage } from "@/nodes/NodesPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
import { PlacementRulesPage } from "@/placement/PlacementRulesPage";
import { RealmsPage } from "@/realms/RealmsPage";
import { RemotePage } from "@/remotes/RemotePage";
import { RemotesPage } from "@/remotes/RemotesPage";
import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { SettingsPage } from "@/settings/SettingsPage";
import { PoolsPage } from "@/storage/PoolsPage";
import { VolumesPage } from "@/storage/VolumesPage";
import { TasksPage } from "@/tasks/TasksPage";
import { TaskToasts } from "@/tasks/TaskToasts";
import { ConsolePage } from "@/vms/ConsolePage";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmCreatePage, VmEditPage } from "@/vms/VmFormPage";
import { VmListPage } from "@/vms/VmListPage";

function routes() {
  return createBrowserRouter([
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
            { path: "imports", element: <ImportsPage /> },
            { path: "imports/:name", element: <ImportPage /> },
            { path: "volumes", element: <VolumesPage /> },
            { path: "storage-pools", element: <PoolsPage /> },
            { path: "backups", element: <BackupsPage /> },
            { path: "backup-jobs", element: <BackupJobsPage /> },
            { path: "networks", element: <NetworksPage /> },
            { path: "security-groups", element: <SecurityGroupsPage /> },
            { path: "security-groups/:name", element: <SecurityGroupPage /> },
            { path: "nodes", element: <NodesPage /> },
            { path: "ha-groups", element: <HaGroupsPage /> },
            { path: "ha-checks", element: <HaChecksPage /> },
            { path: "placement-rules", element: <PlacementRulesPage /> },
            { path: "tasks", element: <TasksPage /> },
            { path: "audit", element: <AuditPage /> },
            { path: "users", element: <UsersPage /> },
            { path: "realms", element: <RealmsPage /> },
            { path: "tokens", element: <TokensPage /> },
            { path: "cluster", element: <ClusterPage /> },
            { path: "remotes", element: <RemotesPage /> },
            { path: "remotes/:name", element: <RemotePage /> },
            { path: "settings", element: <SettingsPage /> },
            { path: "*", element: <NotFoundPage /> },
          ],
        },
      ],
    },
  ]);
}

export function App() {
  const [queryClient] = useState(createQueryClient);
  const [router] = useState(routes);
  return (
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>
          <RouterProvider router={router} />
        </AuthProvider>
        <Toaster position="bottom-right" />
      </QueryClientProvider>
    </ThemeProvider>
  );
}
