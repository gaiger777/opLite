import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { HaGroupsPage } from "@/ha-groups/HaGroupsPage";
import { expectNoAxeViolations } from "@/test/axe";
import { entry, TIMELINE, WEB_ID } from "@/test/declarative";
import { haGroup, NODE, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

const WEB = { ...vm("web", "stopped"), id: WEB_ID };

function mockConfig() {
  const asked: string[] = [];
  server.use(
    http.get(apiUrl("/history/:kind/:key"), ({ params }) => {
      asked.push(`${String(params.kind)}/${String(params.key)}`);
      return HttpResponse.json({ items: TIMELINE });
    }),
    http.get(apiUrl("/history/:kind/:key/:revision"), ({ params }) =>
      HttpResponse.json({
        compare: "prev",
        diff: [{ path: "memory_mib", op: "replace", old: 4096, new: 8192 }],
        record: { ...entry(Number(params.revision)), spec: { memory_mib: 8192 } },
      }),
    ),
    http.get(apiUrl("/manifests/:kind/:key"), ({ params }) => {
      asked.push(`manifest ${String(params.kind)}/${String(params.key)}`);
      return HttpResponse.json({
        apiVersion: "orbivisor.io/v1",
        kind: "Vm",
        metadata: { name: "web" },
        spec: { memory_mib: 8192 },
        state: "stopped",
      });
    }),
  );
  return asked;
}

it("opens the resource's config history drawer from the VM menu", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [WEB], node: [NODE] });
  const asked = mockConfig();
  const { container } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await user.click(await screen.findByRole("button", { name: "Actions for web" }));
  await user.click(screen.getByRole("menuitem", { name: "Change history…" }));
  const drawer = await screen.findByRole("dialog", { name: "Change history of web" });
  const list = await within(drawer).findByRole("list", { name: "Change history of web" });
  expect(within(list).getAllByRole("button")).toHaveLength(3);
  expect(asked).toEqual([`vm/${WEB_ID}`]);
  await expectNoAxeViolations(container);
  // 같은 서랍 안에서 줄 하나를 열고 목록으로 돌아온다
  await user.click(within(list).getAllByRole("button")[0]!);
  expect(
    await within(drawer).findByRole("table", { name: "Changed fields of web" }),
  ).toHaveTextContent("8 GiB");
  await user.click(within(drawer).getByRole("button", { name: "All changes" }));
  expect(within(drawer).getByRole("list", { name: "Change history of web" })).toBeInTheDocument();
});

it("exports the VM as YAML from its menu", async () => {
  const user = userEvent.setup();
  mockLists({ vm: [WEB], node: [NODE] });
  const asked = mockConfig();
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await user.click(await screen.findByRole("button", { name: "Actions for web" }));
  await user.click(screen.getByRole("menuitem", { name: "Export as YAML" }));
  const dialog = await screen.findByRole("dialog", { name: "web as YAML" });
  expect((await within(dialog).findByLabelText("YAML")).textContent).toBe(
    "---\napiVersion: orbivisor.io/v1\nkind: Vm\nmetadata:\n  name: web\nspec:\n  memory_mib: 8192\nstate: stopped\n",
  );
  expect(asked).toEqual([`manifest vm/${WEB_ID}`]);
});

it("marks Terraform-managed VMs in the header", async () => {
  mockLists({
    vm: [{ ...WEB, labels: { "orbivisor.io/managed-by": "terraform" } }],
    node: [NODE],
  });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  expect(await screen.findByText("Managed by Terraform")).toBeInTheDocument();
});

it("adds the same two items to a list row menu", async () => {
  const user = userEvent.setup();
  const group = haGroup("rack-a", 1);
  mockLists({ "ha-group": [group], node: [NODE] });
  const asked = mockConfig();
  renderPage(<HaGroupsPage />, { path: "/ha-groups" });
  await user.click(await screen.findByRole("button", { name: "More for rack-a" }));
  expect(screen.getAllByRole("menuitem").map((m) => m.textContent)).toEqual([
    "Change history…",
    "Export as YAML",
  ]);
  await user.click(screen.getByRole("menuitem", { name: "Change history…" }));
  await screen.findByRole("dialog", { name: "Change history of rack-a" });
  await waitFor(() => expect(asked).toEqual([`ha-group/${group.id}`]));
});
