import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { Permissions } from "@/auth/permissions";
import { grantable } from "@/permissions/grants";
import { PermissionsPanel } from "@/permissions/PermissionsPanel";
import { RolesPage } from "@/permissions/SettingsPermissionsPage";
import { folder, folderId } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const BOB = { user: "00000000-0000-0000-0000-0000000000b0", name: "bob", role: "none" as const };
const A = folderId("teamA");
const GROUP = "00000000-0000-0000-0000-0000000000c1";
const ROLES = {
  revision: 1,
  items: [
    { name: "admin", builtin: true, privileges: ["cluster.admin"], description: "", revision: 0 },
    { name: "operator", builtin: true, privileges: ["vm.power"], description: "", revision: 0 },
    {
      id: "00000000-0000-0000-0000-0000000000d1",
      name: "vm-user",
      builtin: false,
      privileges: ["vm.power", "vm.console"],
      description: "power and console",
      revision: 5,
    },
  ],
};

it("lets a folder delegator who cannot list users add a grant by id", async () => {
  mockLists({ folder: [folder("teamA")] });
  const grants: unknown[] = [];
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: { [A]: ["system.read", "permission.modify", "vm.power", "vm.console"] },
        contents: { [A]: ["system.read", "vm.power", "permission.modify"] },
        objects: [],
      }),
    ),
    http.get(apiUrl("/permissions/effective"), () =>
      HttpResponse.json({
        user: BOB.user,
        name: "bob",
        role: "none",
        object: `folder/${A}`,
        privileges: ["system.read", "permission.modify"],
        grants: [],
      }),
    ),
    http.get(apiUrl("/permissions/who"), () =>
      HttpResponse.json({
        object: `folder/${A}`,
        users: [{ id: ADMIN.user, name: "admin", role: "admin" }],
        grants: [
          {
            id: "00000000-0000-0000-0000-0000000000e1",
            name: "g1",
            revision: 1,
            spec: {
              principal: { type: "group", id: GROUP },
              role: { type: "builtin", role: "operator" },
              target: { type: "folder", id: A },
            },
            status: {},
          },
        ],
      }),
    ),
    http.get(apiUrl("/access-roles"), () => HttpResponse.json(ROLES)),
    http.post(apiUrl("/grants"), async ({ request }) => {
      grants.push(await request.json());
      return HttpResponse.json({}, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<PermissionsPanel target={{ type: "folder", id: A }} label="/teamA" />, { as: BOB });
  expect(await screen.findByText("permission.modify")).toBeInTheDocument();
  const who = await screen.findByRole("table", { name: "Who has access" });
  expect(within(who).getByRole("cell", { name: "Global role" })).toBeInTheDocument();
  // 그룹 이름을 읽을 수 없어 id 끝으로 보인다. 그 grant는 빼기 단추가 있다(이 폴더의 permission.modify).
  expect(
    within(who).getByRole("button", { name: `Remove the permission of ${GROUP.slice(-8)}` }),
  ).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Add permission" }));
  const dialog = screen.getByRole("dialog", { name: "Add a permission on /teamA" });
  await user.selectOptions(within(dialog).getByLabelText("Give to"), "group");
  const id = within(dialog).getByLabelText("Group");
  await user.type(id, "not-an-id");
  await user.click(within(dialog).getByRole("button", { name: "Add permission" }));
  expect(within(dialog).getByText("Enter a user or group id.")).toBeInTheDocument();
  await user.clear(id);
  await user.type(id, GROUP);
  // 줄 수 있는 역할만 보인다(리뷰 1): 아래로 내려가면 폴더 안에 vm.console이 없어 vm-user는 없고, admin은 늘 없다.
  const role = within(dialog).getByLabelText("Role");
  const offered = () =>
    within(role)
      .getAllByRole("option")
      .map((o) => o.textContent);
  expect(offered()).toEqual(["Operator"]);
  await user.click(within(dialog).getByRole("checkbox", { name: /Also apply to subfolders/ }));
  expect(offered()).toEqual(["Operator", "vm-user"]);
  await user.selectOptions(role, "vm-user");
  await user.click(within(dialog).getByRole("button", { name: "Add permission" }));
  await waitFor(() =>
    expect(grants).toEqual([
      {
        principal: { type: "group", id: GROUP },
        role: { type: "custom", id: "00000000-0000-0000-0000-0000000000d1" },
        target: { type: "folder", id: A },
        propagate: false,
      },
    ]),
  );
});

it("creates a custom role from the privilege checklist", async () => {
  const posts: unknown[] = [];
  server.use(
    http.get(apiUrl("/access-roles"), () => HttpResponse.json(ROLES)),
    http.post(apiUrl("/access-roles"), async ({ request }) => {
      posts.push(await request.json());
      return HttpResponse.json({}, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RolesPage />, { as: ADMIN });
  expect(await screen.findByRole("cell", { name: "vm-user" })).toBeInTheDocument();
  // 내장 역할은 고칠 수 없다.
  expect(screen.queryByRole("button", { name: "Edit operator" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Create" }));
  const dialog = screen.getByRole("dialog", { name: "Create a role" });
  await user.type(within(dialog).getByLabelText("Name"), "operator");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText(/not admin, operator, viewer or none/)).toBeInTheDocument();
  expect(within(dialog).getByText("Choose at least one privilege.")).toBeInTheDocument();
  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "snapper");
  await user.click(within(dialog).getByRole("checkbox", { name: "vm.snapshot" }));
  await user.click(within(dialog).getByRole("checkbox", { name: "system.read" }));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  await waitFor(() =>
    expect(posts).toEqual([
      { name: "snapper", privileges: ["system.read", "vm.snapshot"], description: "" },
    ]),
  );
});

describe("grantable", () => {
  const perms = new Permissions({
    all: false,
    root: ["system.read"],
    root_contents: ["system.read"],
    folders: { [A]: ["system.read", "permission.modify", "vm.power"] },
    contents: { [A]: ["system.read", "permission.modify"] },
    objects: [{ kind: "vm", id: "v1", privileges: ["vm.console", "permission.modify"] }],
  });
  it("needs the role's privileges and permission.modify where the grant reaches", () => {
    const folderA = { type: "folder", id: A } as const;
    expect(grantable(perms, ["vm.power"], folderA, false)).toBe(true);
    // 아래로 내려가면 폴더 안에 vm.power가 없다.
    expect(grantable(perms, ["vm.power"], folderA, true)).toBe(false);
    expect(grantable(perms, ["system.read"], folderA, true)).toBe(true);
    const v1 = { type: "object", kind: "vm", id: "v1", folder: A } as const;
    expect(grantable(perms, ["vm.console", "system.read"], v1, true)).toBe(true);
    expect(grantable(perms, ["vm.power"], v1, true)).toBe(false);
    // 루트 grant는 cluster.admin이 있어야 한다.
    expect(grantable(perms, ["system.read"], { type: "root" }, false)).toBe(false);
    expect(
      grantable(Permissions.fromRole("admin"), ["cluster.admin"], { type: "root" }, true),
    ).toBe(true);
  });
});

it("shows the server's reason when a grant is refused and confirms removing your own access", async () => {
  mockLists({ folder: [folder("teamA")] });
  const MINE = {
    id: "00000000-0000-0000-0000-0000000000e2",
    name: "g2",
    revision: 1,
    spec: {
      principal: { type: "user", id: BOB.user },
      role: { type: "builtin", role: "operator" },
      target: { type: "folder", id: A },
    },
    status: {},
  };
  const deleted: string[] = [];
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: { [A]: ["system.read", "permission.modify", "vm.power"] },
        contents: { [A]: ["system.read", "permission.modify", "vm.power"] },
        objects: [],
      }),
    ),
    http.get(apiUrl("/permissions/effective"), () =>
      HttpResponse.json({
        user: BOB.user,
        name: "bob",
        role: "none",
        object: `folder/${A}`,
        privileges: ["system.read", "permission.modify", "vm.power"],
        grants: [MINE],
      }),
    ),
    http.get(apiUrl("/permissions/who"), () =>
      // 관리자가 아니면 전역 역할 사용자는 비어 온다(리뷰 1).
      HttpResponse.json({ object: `folder/${A}`, users: [], grants: [MINE] }),
    ),
    http.get(apiUrl("/access-roles"), () => HttpResponse.json(ROLES)),
    http.post(apiUrl("/grants"), () =>
      HttpResponse.json(
        { code: "forbidden", message: "cannot grant privileges you do not hold: vm.migrate" },
        { status: 403 },
      ),
    ),
    http.delete(apiUrl("/grants/:id"), ({ params }) => {
      deleted.push(String(params.id));
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<PermissionsPanel target={{ type: "folder", id: A }} label="/teamA" />, { as: BOB });
  await screen.findByRole("table", { name: "Who has access" });
  expect(screen.getByText(/Only administrators see who they are/)).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Add permission" }));
  const dialog = screen.getByRole("dialog", { name: "Add a permission on /teamA" });
  await user.selectOptions(within(dialog).getByLabelText("Give to"), "group");
  await user.type(within(dialog).getByLabelText("Group"), GROUP);
  await user.click(within(dialog).getByRole("button", { name: "Add permission" }));
  expect(
    await within(dialog).findByText(
      "You do not hold every permission this needs: cannot grant privileges you do not hold: vm.migrate",
    ),
  ).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Cancel" }));

  // 나에게 건 grant를 빼면 이름을 넣어야 한다.
  const name = BOB.user.slice(-8);
  await user.click(screen.getByRole("button", { name: `Remove the permission of ${name}` }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent(/This grant gives you access/);
  const remove = within(confirm).getByRole("button", { name: "Remove" });
  expect(remove).toBeDisabled();
  await user.type(within(confirm).getByRole("textbox"), name);
  await user.click(remove);
  await waitFor(() => expect(deleted).toEqual([MINE.id]));
});
