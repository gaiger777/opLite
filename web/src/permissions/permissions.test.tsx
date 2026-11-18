import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

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
        folders: { [A]: ["system.read", "permission.modify"] },
        contents: { [A]: ["system.read", "vm.power"] },
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
  await user.selectOptions(within(dialog).getByLabelText("Role"), "vm-user");
  await user.click(within(dialog).getByRole("checkbox", { name: /Also apply to subfolders/ }));
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
