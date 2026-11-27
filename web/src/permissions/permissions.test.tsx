import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { Permissions } from "@/auth/permissions";
import { grantable } from "@/permissions/grants";
import { PermissionsPanel, routingGrants, useObjectNames } from "@/permissions/PermissionsPanel";
import { GroupsPage, RolesPage } from "@/permissions/SettingsPermissionsPage";
import { floatingIpView, folder, folderId, routerView } from "@/test/fixtures";
import { expectNoAxeViolations } from "@/test/axe";
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
  await user.click(
    within(dialog).getByRole("checkbox", { name: /Apply inside the folder and its subfolders/ }),
  );
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
  const { container } = renderPage(<RolesPage />, { as: ADMIN });
  // 역할을 고르면 그 권한이 묶음별 체크 트리로 보인다(계획 77-6). 처음엔 첫 역할이다.
  await user.click(await screen.findByRole("button", { name: /^vm-user/ }));
  const detail = screen.getByRole("region", { name: "vm-user" });
  expect(within(detail).getByRole("list", { name: "vm" })).toHaveTextContent("vm.power(granted)");
  expect(within(detail).getByRole("list", { name: "vm" })).toHaveTextContent(
    "vm.snapshot(not granted)",
  );
  expect(within(detail).getByRole("button", { name: "Edit vm-user" })).toBeInTheDocument();
  await expectNoAxeViolations(container);
  // 내장 역할은 고칠 수 없다.
  await user.click(screen.getByRole("button", { name: /^Operator/ }));
  expect(screen.queryByRole("button", { name: /^Edit / })).not.toBeInTheDocument();
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

it("keeps privileges this web build does not know when saving a role", async () => {
  // 새 서버가 더한 권한은 고를 칸이 없다. 그대로 보내야 PUT이 지우지 않는다(리뷰 2).
  const puts: unknown[] = [];
  const role = { ...ROLES.items[2], privileges: ["vm.power", "vm.console", "net.future"] };
  server.use(
    http.get(apiUrl("/access-roles"), () =>
      HttpResponse.json({ ...ROLES, items: [...ROLES.items.slice(0, 2), role] }),
    ),
    http.put(apiUrl("/access-roles/:key"), async ({ request }) => {
      puts.push(await request.json());
      return HttpResponse.json({});
    }),
  );
  const user = userEvent.setup();
  renderPage(<RolesPage />, { as: ADMIN });
  await user.click(await screen.findByRole("button", { name: /^vm-user/ }));
  await user.click(screen.getByRole("button", { name: "Edit vm-user" }));
  const dialog = screen.getByRole("dialog", { name: "Edit role vm-user" });
  expect(within(dialog).getByText(/Kept: net\.future/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("checkbox", { name: "vm.console" }));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(puts).toEqual([
      { privileges: ["vm.power", "net.future"], description: "power and console" },
    ]),
  );
});

it("shows a mapping to a deleted realm and checks the directory group length", async () => {
  // 지운 영역은 선택 칸에 보이지 않아 저장이 죽은 연결을 다시 보냈다(리뷰 2).
  const puts: unknown[] = [];
  server.use(
    http.get(apiUrl("/users"), () => HttpResponse.json({ revision: 1, items: [] })),
    http.get(apiUrl("/realms"), () =>
      HttpResponse.json({ revision: 1, items: [{ name: "corp", backend: { kind: "ldap" } }] }),
    ),
    http.get(apiUrl("/groups"), () =>
      HttpResponse.json({
        revision: 1,
        items: [
          {
            id: GROUP,
            name: "ops",
            revision: 2,
            spec: { members: [], external: [{ realm: "old", group: "cn=ops" }] },
            status: {},
          },
        ],
      }),
    ),
    http.put(apiUrl("/groups/:key"), async ({ request }) => {
      puts.push(await request.json());
      return HttpResponse.json({});
    }),
  );
  const user = userEvent.setup();
  renderPage(<GroupsPage />, { as: ADMIN });
  await user.click(await screen.findByRole("button", { name: "Edit ops" }));
  const dialog = screen.getByRole("dialog", { name: "Edit group ops" });
  const realm = within(dialog).getByLabelText("Realm");
  await waitFor(() => expect(realm).toHaveDisplayValue("old (deleted)"));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText(/names a realm that was deleted/)).toBeInTheDocument();
  await user.selectOptions(realm, "corp");
  const name = within(dialog).getByLabelText("Directory group");
  await user.clear(name);
  await user.click(name);
  await user.paste("x".repeat(1025));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText(/at most 1024 characters/)).toBeInTheDocument();
  expect(puts).toEqual([]);
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
      HttpResponse.json({
        object: `folder/${A}`,
        users: [],
        grants: [
          MINE,
          {
            ...MINE,
            id: "00000000-0000-0000-0000-0000000000e3",
            spec: {
              ...MINE.spec,
              principal: { type: "group", id: GROUP },
              role: { type: "builtin", role: "admin" },
            },
          },
        ],
      }),
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

  // 줄 수 없는 역할(admin)의 grant는 서버가 빼기도 막으니 빼기 단추가 없다(리뷰 2).
  expect(
    screen.queryByRole("button", { name: `Remove the permission of ${GROUP.slice(-8)}` }),
  ).not.toBeInTheDocument();
  // 나에게 건 grant를 빼면 대상(폴더) 이름을 넣어야 한다. 주체 id 끝이 아니다(리뷰 2).
  const name = BOB.user.slice(-8);
  await user.click(screen.getByRole("button", { name: `Remove the permission of ${name}` }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent(/This grant gives you access/);
  const remove = within(confirm).getByRole("button", { name: "Remove" });
  expect(remove).toBeDisabled();
  await user.type(within(confirm).getByRole("textbox"), name);
  expect(remove).toBeDisabled();
  await user.clear(within(confirm).getByRole("textbox"));
  await user.type(within(confirm).getByRole("textbox"), "teamA");
  await user.click(remove);
  await waitFor(() => expect(deleted).toEqual([MINE.id]));
});

it("names router and floating IP grants by their names", async () => {
  // 리뷰 1 웹 M5: 라우터·유동 IP grant가 `router/1a2b3c4d`처럼 id 끝으로 보이지 않는다.
  const r1 = routerView("r1");
  const fip = floatingIpView("203.0.113.120", "ext");
  server.use(
    http.get(apiUrl("/routers"), () => HttpResponse.json({ revision: 1, items: [r1] })),
    http.get(apiUrl("/floating-ips"), () => HttpResponse.json({ revision: 1, items: [fip] })),
  );
  mockLists({});
  function Names() {
    const name = useObjectNames({ routers: true, fips: true });
    return (
      <p>
        {name("router", r1.id) ?? "?"} {name("floating-ip", fip.id) ?? "?"}
      </p>
    );
  }
  renderPage(<Names />);
  expect(await screen.findByText("r1 203-0-113-120")).toBeInTheDocument();
});

it("reads routers and floating IPs only for panels that name them", async () => {
  // 리뷰 2 웹 m6: VM·풀 권한 패널마다 라우터 목록(서버의 World 읽기)을 30초마다 읽지 않는다.
  const asked: string[] = [];
  server.use(
    http.get(apiUrl("/routers"), () => {
      asked.push("routers");
      return HttpResponse.json({ revision: 1, items: [] });
    }),
    http.get(apiUrl("/floating-ips"), () => {
      asked.push("floating-ips");
      return HttpResponse.json({ revision: 1, items: [] });
    }),
  );
  mockLists({});
  function Names() {
    const name = useObjectNames();
    return <p>{name("vm", "x") ?? "none"}</p>;
  }
  renderPage(<Names />);
  expect(await screen.findByText("none")).toBeInTheDocument();
  await new Promise((r) => setTimeout(r, 50));
  expect(asked).toEqual([]);
  const grant = (kind: string) =>
    ({ spec: { target: { type: "object", kind, id: "x" } } }) as never;
  expect(routingGrants([grant("vm")])).toEqual({ routers: false, fips: false });
  expect(routingGrants([grant("router"), grant("floating-ip")])).toEqual({
    routers: true,
    fips: true,
  });
  expect(routingGrants(undefined)).toEqual({ routers: false, fips: false });
});

it("explains an empty group list and offers to create a group", async () => {
  server.use(
    http.get(apiUrl("/users"), () => HttpResponse.json({ revision: 1, items: [] })),
    http.get(apiUrl("/realms"), () => HttpResponse.json({ revision: 1, items: [] })),
    http.get(apiUrl("/groups"), () => HttpResponse.json({ revision: 1, items: [] })),
  );
  renderPage(<GroupsPage />, { as: ADMIN });
  const empty = (await screen.findByText("No groups.")).closest<HTMLElement>(
    "[data-slot=empty-state]",
  )!;
  expect(empty).toHaveTextContent("give permissions once to the group");
  expect(within(empty).getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  await userEvent.click(within(empty).getByRole("button", { name: "Create a group" }));
  expect(await screen.findByRole("dialog", { name: "Create a group" })).toBeInTheDocument();
});
