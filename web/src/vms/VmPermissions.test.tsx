// 계획 62 숨기기: 전역 역할이 아니라 VM마다(폴더·객체 grant) 단추·메뉴·일괄 막대를 보인다.
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { folder, folderId, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmCreatePage } from "@/vms/VmFormPage";
import { VmListPage } from "@/vms/VmListPage";

const ALICE = {
  user: "00000000-0000-0000-0000-0000000000a1",
  name: "alice",
  role: "none" as const,
};
const A = folderId("teamA");
const TOP = vm("top", "stopped");
const WEB = { ...vm("web", "stopped"), spec: { ...vm("web", "stopped").spec, folder: A } };

function mine(contents: string[]) {
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: { [A]: ["system.read"] },
        contents: { [A]: contents },
        // top은 객체 grant로 읽기만
        objects: [{ kind: "vm", id: TOP.id, privileges: ["system.read"] }],
      }),
    ),
  );
}

it("counts the bulk bar per VM and hides what the user cannot do", async () => {
  mine(["system.read", "vm.power"]);
  mockLists({ folder: [folder("teamA")], vm: [WEB, TOP] });
  const user = userEvent.setup();
  renderPage(<VmListPage />, { path: "/vms", as: ALICE });
  await screen.findByRole("link", { name: "top" });
  // 어디서도 VM을 만들 수 없으니 만들기 단추가 없다.
  await waitFor(() =>
    expect(screen.queryByRole("link", { name: "Create" })).not.toBeInTheDocument(),
  );
  // top에는 할 수 있는 동작이 없다. web은 전원만.
  await waitFor(() =>
    expect(screen.queryByRole("button", { name: "Actions for top" })).not.toBeInTheDocument(),
  );
  await user.click(screen.getByRole("button", { name: "Actions for web" }));
  expect(screen.getByRole("menuitem", { name: "Reboot" })).toBeInTheDocument();
  expect(screen.queryByRole("menuitem", { name: "Delete" })).not.toBeInTheDocument();
  await user.keyboard("{Escape}");

  await user.click(screen.getByRole("checkbox", { name: "Select all" }));
  const bar = screen.getByRole("region", { name: "Bulk actions" });
  expect(
    within(bar).getByRole("button", { name: "Start (1 possible, 1 not possible)" }),
  ).toBeEnabled();
  // 지우기·스냅샷 권한은 어디에도 없어 단추가 없다. 노드를 못 읽으니 옮기기도 없다.
  expect(within(bar).queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  expect(within(bar).queryByRole("button", { name: /^Migrate/ })).not.toBeInTheDocument();
  // 폴더 열이 있다.
  expect(screen.getByRole("columnheader", { name: "Folder" })).toBeInTheDocument();
});

it("creates a VM in the user's folder without reading nodes or picking a machine", async () => {
  mine(["system.read", "vm.create", "vm.power"]);
  mockLists({ folder: [folder("teamA")] });
  let nodesRead = false;
  const bodies: unknown[] = [];
  server.use(
    http.get(apiUrl("/nodes"), () => {
      nodesRead = true;
      return HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 });
    }),
    http.post(apiUrl("/vms"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(vm("app", "stopped"), { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<VmCreatePage />, { path: "/vms/-new", as: ALICE });
  await user.type(await screen.findByLabelText("Name"), "app");
  // 루트에는 만들 수 없으니 자기 폴더가 골라져 있다. 노드 칸은 없고 머신은 자동이다.
  expect(screen.getByLabelText("Folder")).toHaveDisplayValue("/teamA");
  expect(screen.queryByLabelText("Node")).not.toBeInTheDocument();
  expect(screen.getByLabelText("Machine type")).toHaveDisplayValue("Automatic (cluster default)");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  const spec = (bodies[0] as { spec: Record<string, unknown> }).spec;
  expect(spec.folder).toBe(A);
  expect(spec).not.toHaveProperty("machine");
  expect(nodesRead).toBe(false);
});
