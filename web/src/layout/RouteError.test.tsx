import { render, screen } from "@testing-library/react";
import { createMemoryRouter, Outlet, RouterProvider } from "react-router";
import { toast } from "sonner";

import { Toaster } from "@/components/ui/sonner";
import { isChunkError, onPreloadError, RouteError } from "@/layout/RouteError";
import { ThemeProvider } from "@/lib/theme";

afterEach(() => toast.dismiss());

it("recognises a missing chunk in every browser's words", () => {
  for (const message of [
    "Failed to fetch dynamically imported module: https://h/assets/a-1.js", // Chrome
    "error loading dynamically imported module: https://h/assets/a-1.js", // Firefox
    "Importing a module script failed.", // Safari
    "Unable to preload CSS for /assets/a-1.css", // Vite
  ]) {
    expect(isChunkError(new TypeError(message))).toBe(true);
  }
  expect(isChunkError(new Error("x is undefined"))).toBe(false);
  expect(isChunkError(null)).toBe(false);
});

it("shows other route errors briefly with a reload and a way home", async () => {
  const router = createMemoryRouter(
    [
      {
        element: <Outlet />,
        errorElement: <RouteError />,
        children: [
          {
            path: "/",
            Component: () => {
              throw new Error("boom");
            },
          },
        ],
      },
    ],
    { initialEntries: ["/"] },
  );
  const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
  render(<RouterProvider router={router} />);
  expect(
    await screen.findByRole("heading", { level: 1, name: "Something went wrong on this page" }),
  ).toBeInTheDocument();
  expect(screen.getByText("boom")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Go to the dashboard" })).toHaveAttribute("href", "/");
  quiet.mockRestore();
});

it("offers one reload toast when Vite cannot preload a chunk", async () => {
  render(
    <ThemeProvider>
      <Toaster />
    </ThemeProvider>,
  );
  onPreloadError();
  onPreloadError();
  expect(await screen.findAllByText("A new version is available")).toHaveLength(1);
  expect(screen.getByRole("button", { name: "Reload" })).toBeInTheDocument();
});
