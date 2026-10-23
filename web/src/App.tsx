import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import { createBrowserRouter, RouterProvider } from "react-router";

import { LiveProvider } from "@/api/LiveProvider";
import { createQueryClient } from "@/api/queryClient";
import { AuthProvider } from "@/auth/AuthProvider";
import { RequireAuth } from "@/auth/RequireAuth";
import { Toaster } from "@/components/ui/sonner";
import { AppLayout } from "@/layout/AppLayout";
import { ThemeProvider } from "@/lib/theme";
import { DashboardPage } from "@/pages/DashboardPage";
import { LoginPage } from "@/pages/LoginPage";
import { NotFoundPage } from "@/pages/NotFoundPage";
import { TasksPage } from "@/tasks/TasksPage";
import { TaskToasts } from "@/tasks/TaskToasts";
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
              <TaskToasts />
              <AppLayout />
            </LiveProvider>
          ),
          children: [
            { index: true, element: <DashboardPage /> },
            { path: "vms", element: <VmListPage /> },
            { path: "vms/new", element: <VmCreatePage /> },
            { path: "vms/:name", element: <VmDetailPage /> },
            { path: "vms/:name/edit", element: <VmEditPage /> },
            { path: "tasks", element: <TasksPage /> },
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
