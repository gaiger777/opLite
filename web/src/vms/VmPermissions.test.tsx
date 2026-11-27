// 계획 62 숨기기: 전역 역할이 아니라 VM마다(폴더·객체 grant) 단추·메뉴·일괄 막대를 보인다.
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { folder, folderId, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { BuilderPage } from "@/vmBuilder/BuilderPage";
import { VmListPage } from "@/vms/VmListPage";

const ALICE = {
  user: "00000000-0000-0000-0000-0000000000a1",
  name: "alice",
  role: "none" as const,
};
const A = folderId("teamA");
const TOP = vm("top", "stopped");
const WEB = { ...vm("web", "stopped"), spec: { ...vm("web", "stopped").spec, folder: A } };

/** 만들기 화면(계획 77-5 — 빈 VM 용도) */
function openCreate() {
  // 폴더 사용량(할당량 막대)은 읽지 못해도 된다.
  server.use(
    http.get(apiUrl("/folders/:id/usage"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  return renderPage(<BuilderPage />, {
    path: "/vms/-new",
    url: "/vms/-new?purpose=blank",
    as: ALICE,
  });
}

/** 고급 설정 서랍을 연다. */
async function advanced(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Advanced settings" }));
  return screen.findByRole("dialog", { name: "Advanced settings" });
}

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
    http.post(apiUrl("/vm-checks"), () =>
      HttpResponse.json({ ok: true, node: null, ips: [], problems: [] }),
    ),
  );
  const user = userEvent.setup();
  openCreate();
  await user.type(await screen.findByLabelText("Name"), "app");
  // 루트에는 만들 수 없으니 자기 폴더가 골라져 있다. 노드 칸은 없고(서버가 고름) 머신은 자동이다.
  expect(screen.getByLabelText("Folder")).toHaveDisplayValue("/teamA");
  expect(screen.queryByLabelText("Node")).not.toBeInTheDocument();
  expect(screen.getByText("The server chooses where the VM runs.")).toBeInTheDocument();
  const drawer = await advanced(user);
  expect(within(drawer).getByLabelText("Virtual motherboard (advanced)")).toHaveDisplayValue(
    "Automatic (cluster default)",
  );
  await user.keyboard("{Escape}");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  const spec = (bodies[0] as { spec: Record<string, unknown> }).spec;
  expect(spec.folder).toBe(A);
  expect(spec).not.toHaveProperty("machine");
  expect(nodesRead).toBe(false);
});

it("waits for the user's permissions before picking the new VM's folder", async () => {
  // 폴더 목록이 먼저 오고 권한이 늦게 와도(새로 고침) 루트가 아니라 자기 폴더가 골라진다(리뷰 1).
  let release!: () => void;
  const late = new Promise<void>((resolve) => (release = resolve));
  server.use(
    http.get(apiUrl("/permissions/mine"), async () => {
      await late;
      return HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: { [A]: ["system.read"] },
        contents: { [A]: ["system.read", "vm.create"] },
        objects: [],
      });
    }),
  );
  mockLists({ folder: [folder("teamA")] });
  openCreate();
  expect(await screen.findByText("Loading…")).toBeInTheDocument();
  // 폴더 목록은 이미 왔다. 권한이 오기 전에는 폼을 열지 않는다(첫 값을 굳히므로).
  await new Promise((resolve) => setTimeout(resolve, 200));
  expect(screen.queryByLabelText("Name")).not.toBeInTheDocument();
  release();
  expect(await screen.findByLabelText("Folder")).toHaveDisplayValue("/teamA");
});

it("keeps the VM name as the first column when folders exist", async () => {
  mine(["system.read"]);
  mockLists({ folder: [folder("teamA")], vm: [WEB] });
  renderPage(<VmListPage />, { path: "/vms", as: ALICE });
  await screen.findByRole("link", { name: "web" });
  const headers = screen.getAllByRole("columnheader").map((h) => h.textContent);
  expect(headers.indexOf("Name")).toBeLessThan(headers.indexOf("Folder"));
  expect(headers.indexOf("State")).toBeLessThan(headers.indexOf("Folder"));
});

// 리뷰 2 웹 #5: 자동 머신(노드를 모름)에서 고른 x86 수준은 virt로 바꾸면 host로 돌아간다(저장하면 400이 될 것).
it("resets an x86 level when the automatic machine becomes virt", async () => {
  mine(["system.read", "vm.create", "vm.config"]);
  mockLists({ folder: [folder("teamA")] });
  server.use(
    http.get(apiUrl("/nodes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  const user = userEvent.setup();
  openCreate();
  await screen.findByLabelText("Name");
  const drawer = await advanced(user);
  const machine = within(drawer).getByLabelText("Virtual motherboard (advanced)");
  expect(machine).toHaveValue("");
  await screen.findAllByRole("option", { name: /x86-64-v3/ });
  await user.selectOptions(screen.getByLabelText("CPU model"), "x86-64-v3");
  await user.selectOptions(machine, "virt");
  await waitFor(() => expect(screen.getByLabelText("CPU model")).toHaveValue("host"));
});
