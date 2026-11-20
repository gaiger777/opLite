import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { FolderDetailPage, FolderViewPage } from "@/folders/FolderPages";
import { folder, folderId, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => toast.dismiss());

const ALICE = {
  user: "00000000-0000-0000-0000-0000000000a1",
  name: "alice",
  role: "none" as const,
};
const A = folderId("teamA");
const PROD = folderId("prod");

/** alice: /teamA에 operator(+ 폴더 관리). 서버는 그가 볼 수 있는 폴더·VM만 준다. */
function alice() {
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: {
          [A]: ["system.read", "inventory.folder", "inventory.move"],
          [PROD]: ["system.read", "inventory.folder", "inventory.move"],
        },
        contents: {
          [A]: ["system.read", "vm.create", "vm.power", "inventory.move"],
          [PROD]: ["system.read", "vm.create", "vm.power", "inventory.move"],
        },
        objects: [],
      }),
    ),
  );
  const inA = (name: string, where: string) => ({
    ...vm(name, "stopped"),
    spec: { ...vm(name, "stopped").spec, folder: folderId(where) },
  });
  mockLists({
    folder: [folder("teamA"), folder("prod", "vm", "teamA")],
    vm: [inA("web", "teamA"), inA("db", "prod")],
  });
}

it("shows a non-admin only the folders and VMs they can see, with keyboard tree navigation", async () => {
  alice();
  const user = userEvent.setup();
  renderPage(<FolderViewPage kind="vm" />, { path: "/vm-folders", as: ALICE });
  const tree = await screen.findByRole("tree", { name: "VM folders" });
  const items = () => within(tree).getAllByRole("treeitem");
  expect(items().map((i) => i.textContent)).toEqual(["All VMs", "teamA"]);
  // 루트 아래 바로 든 VM은 없다(alice의 VM은 모두 /teamA 안).
  expect(await screen.findByText("Nothing in this folder.")).toBeInTheDocument();

  // 화살표: 아래로 teamA, →로 펼치기, ↓로 prod, Enter로 고르기
  items()[0]!.focus();
  await user.keyboard("{ArrowDown}");
  expect(items()[1]).toHaveFocus();
  await user.keyboard("{ArrowRight}");
  expect(items().map((i) => i.textContent)).toEqual(["All VMs", "teamA", "prod"]);
  expect(items()[1]).toHaveAttribute("aria-expanded", "true");
  await user.keyboard("{ArrowDown}{Enter}");
  expect(items()[2]).toHaveAttribute("aria-selected", "true");
  expect(await screen.findByRole("heading", { name: "/teamA/prod" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "db" })).toBeInTheDocument();
  expect(screen.queryByRole("link", { name: "web" })).not.toBeInTheDocument();
  // ←는 부모로
  await user.keyboard("{ArrowLeft}");
  expect(items()[1]).toHaveFocus();

  // "하위 포함"이면 아래 폴더의 VM도
  await user.click(items()[1]!);
  await user.click(screen.getByRole("button", { name: "Include subfolders" }));
  expect(await screen.findByRole("link", { name: "db" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "web" })).toBeInTheDocument();
  // 그 폴더에 VM을 만들 수 있으니 여기에 만들기가 그 폴더로 간다.
  expect(screen.getByRole("link", { name: "Create a VM here" })).toHaveAttribute(
    "href",
    `/vms/-new?folder=${A}`,
  );
});

it("moves the chosen VMs with the bulk bar after a confirmation", async () => {
  alice();
  const moved: unknown[] = [];
  server.use(
    http.post(apiUrl("/folders/:id/members"), async ({ request, params }) => {
      moved.push({ to: params.id, body: await request.json() });
      return HttpResponse.json({ items: [{ kind: "vm", key: "web", ok: true }] });
    }),
  );
  const user = userEvent.setup();
  renderPage(<FolderViewPage kind="vm" />, {
    path: "/vm-folders",
    url: `/vm-folders?folder=${A}`,
    as: ALICE,
  });
  await user.click(await screen.findByRole("checkbox", { name: "Select web" }));
  await user.click(
    within(screen.getByRole("region", { name: "Bulk actions" })).getByRole("button", {
      name: "Move to folder (1 possible, 0 not possible)",
    }),
  );
  const dialog = screen.getByRole("dialog", { name: "Move 1 item to a folder" });
  // 이미 있는 폴더(/teamA)를 고르면 옮길 것이 없다.
  await user.selectOptions(within(dialog).getByLabelText("Destination folder"), "/teamA");
  expect(within(dialog).getByText("Already in this folder")).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Move" })).toBeDisabled();
  await user.selectOptions(within(dialog).getByLabelText("Destination folder"), "/teamA/prod");
  await user.click(within(dialog).getByRole("button", { name: "Move" }));
  expect(
    await screen.findByText("Move to /teamA/prod: 1 of 1 succeeded, 0 failed."),
  ).toBeInTheDocument();
  expect(moved).toEqual([{ to: PROD, body: { items: [{ kind: "vm", key: "web" }] } }]);
});

it("drops dragged rows on a tree folder and says why a folder does not take them", async () => {
  alice();
  const moved: unknown[] = [];
  server.use(
    http.post(apiUrl("/folders/:id/members"), async ({ request, params }) => {
      moved.push({ to: params.id, body: await request.json() });
      return HttpResponse.json({ items: [{ kind: "vm", key: "web", ok: true }] });
    }),
  );
  const user = userEvent.setup();
  renderPage(<FolderViewPage kind="vm" />, {
    path: "/vm-folders",
    url: `/vm-folders?folder=${A}`,
    as: ALICE,
  });
  const row = (await screen.findByRole("link", { name: "web" })).closest("tr")!;
  expect(row).toHaveAttribute("draggable", "true");
  const tree = screen.getByRole("tree", { name: "VM folders" });
  await user.click(within(tree).getByText("teamA").closest("[role=treeitem]")!);
  const data = new Map<string, string>();
  const dataTransfer = {
    effectAllowed: "",
    dropEffect: "",
    get types() {
      return [...data.keys()];
    },
    setData: (k: string, v: string) => data.set(k, v),
    getData: (k: string) => data.get(k) ?? "",
  };
  fireEvent.dragStart(row, { dataTransfer });
  // 루트에는 옮길 권한이 없다(루트 inventory.move 없음).
  const root = within(tree).getByText("All VMs").closest("[role=treeitem]")!;
  fireEvent.dragOver(root, { dataTransfer });
  expect(root).toHaveAttribute("data-drop", "no");
  expect(within(root as HTMLElement).getByRole("status")).toHaveTextContent(
    "No permission to move here",
  );
  // 지금 폴더에는 이미 있다.
  const teamA = within(tree).getByText("teamA").closest("[role=treeitem]")!;
  fireEvent.dragOver(teamA, { dataTransfer });
  expect(teamA).toHaveAttribute("data-drop", "no");
  const prod = within(tree).getByText("prod").closest("[role=treeitem]")!;
  fireEvent.dragOver(prod, { dataTransfer });
  expect(prod).toHaveAttribute("data-drop", "ok");
  fireEvent.drop(prod, { dataTransfer });
  await waitFor(() =>
    expect(moved).toEqual([{ to: PROD, body: { items: [{ kind: "vm", key: "web" }] } }]),
  );
});

it("creates, renames and quotas a folder from its detail page", async () => {
  mockLists({ folder: [folder("teamA", "vm", undefined, { quota: { vcpus: 8 } })], vm: [] });
  const requests: unknown[] = [];
  server.use(
    http.get(apiUrl("/folders/:id/usage"), () =>
      HttpResponse.json({
        folder: A,
        path: "/teamA",
        usage: { vcpus: 2, memory_mib: 1024, disk_bytes: 0, vms: 1 },
        limits: [
          {
            folder: A,
            path: "/teamA",
            quota: { vcpus: 8 },
            usage: { vcpus: 2, memory_mib: 1024, disk_bytes: 0, vms: 1 },
          },
        ],
        binding: { vcpus: A },
      }),
    ),
    http.patch(apiUrl("/folders/:id"), async ({ request }) => {
      requests.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(folder("teamA"));
    }),
  );
  const user = userEvent.setup();
  renderPage(<FolderDetailPage kind="vm" />, {
    path: "/vm-folders/:id",
    url: `/vm-folders/${A}`,
    as: ADMIN,
  });
  expect(await screen.findByRole("heading", { name: "teamA" })).toBeInTheDocument();
  expect(await screen.findByText("2 of 8")).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Actions for teamA" }));
  await user.click(screen.getByRole("menuitem", { name: "Set quota" }));
  const dialog = screen.getByRole("dialog", { name: "Quota of teamA" });
  expect(within(dialog).getByLabelText("vCPUs")).toHaveValue("8");
  await user.type(within(dialog).getByLabelText("Memory (GiB)"), "16");
  await user.clear(within(dialog).getByLabelText("vCPUs"));
  await user.type(within(dialog).getByLabelText("VMs"), "x");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("Enter a whole number of 0 or more.")).toBeInTheDocument();
  // 개수는 반올림하지 않는다(0.4 VM이 0이 되어 모두 막지 않게, 리뷰 1).
  await user.clear(within(dialog).getByLabelText("VMs"));
  await user.type(within(dialog).getByLabelText("VMs"), "0.4");
  await user.type(within(dialog).getByLabelText("Memory (GiB)"), "x");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("Enter a whole number of 0 or more.")).toBeInTheDocument();
  expect(within(dialog).getByText("Enter a number of 0 or more.")).toBeInTheDocument();
  expect(requests).toEqual([]);
  await user.clear(within(dialog).getByLabelText("Memory (GiB)"));
  await user.type(within(dialog).getByLabelText("Memory (GiB)"), "16");
  await user.clear(within(dialog).getByLabelText("VMs"));
  await user.type(within(dialog).getByLabelText("VMs"), "20");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(requests).toEqual([{ ifMatch: "1", body: { quota: { memory_mib: 16384, vms: 20 } } }]),
  );
});

it("sets the router and floating IP quota of a network folder", async () => {
  // 계획 66: 네트워크 폴더의 할당량은 라우터·유동 IP 수다(서버 `FolderKind::quota_dimensions`).
  const T = folderId("tenant");
  mockLists({ folder: [folder("tenant", "network", undefined, { quota: { floating_ips: 4 } })] });
  const requests: unknown[] = [];
  server.use(
    // 서버 `FolderUsage`: VM 칸은 늘, 라우터·유동 IP는 0이면 빠진다.
    http.get(apiUrl("/folders/:id/usage"), () =>
      HttpResponse.json({
        folder: T,
        path: "/tenant",
        usage: { vcpus: 0, memory_mib: 0, disk_bytes: 0, vms: 0, routers: 1, floating_ips: 3 },
        limits: [
          {
            folder: T,
            path: "/tenant",
            quota: { floating_ips: 4 },
            usage: { vcpus: 0, memory_mib: 0, disk_bytes: 0, vms: 0, routers: 1, floating_ips: 3 },
          },
        ],
        binding: { floating_ips: T },
      }),
    ),
    http.patch(apiUrl("/folders/:id"), async ({ request }) => {
      requests.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(folder("tenant", "network"));
    }),
  );
  const user = userEvent.setup();
  renderPage(<FolderDetailPage kind="network" />, {
    path: "/network-folders/:id",
    url: `/network-folders/${T}`,
    as: ADMIN,
  });
  expect(await screen.findByText("3 of 4")).toBeInTheDocument();
  expect(screen.getByRole("meter", { name: "Floating IPs" })).toBeInTheDocument();
  expect(screen.queryByRole("meter", { name: "vCPUs" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Actions for tenant" }));
  await user.click(screen.getByRole("menuitem", { name: "Set quota" }));
  const dialog = screen.getByRole("dialog", { name: "Quota of tenant" });
  expect(within(dialog).getByLabelText("Floating IPs")).toHaveValue("4");
  expect(within(dialog).queryByLabelText("vCPUs")).not.toBeInTheDocument();
  await user.type(within(dialog).getByLabelText("Routers"), "2");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(requests).toEqual([{ ifMatch: "1", body: { quota: { routers: 2, floating_ips: 4 } } }]),
  );
});
