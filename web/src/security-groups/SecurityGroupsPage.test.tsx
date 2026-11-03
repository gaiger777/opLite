import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { SecurityGroupsPage } from "@/security-groups/SecurityGroupsPage";
import { securityGroup, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const ssh = securityGroup(
  "ssh",
  [
    { direction: "in", action: "accept", protocol: "tcp", ports: "22" },
    { direction: "in", action: "accept", protocol: "icmp" },
  ],
  "SSH from anywhere",
);
const web = vm("web", "running", {
  spec: {
    ...vm("web", "running").spec,
    nics: [{ network: "lan", model: "virtio", security_groups: ["ssh"] }],
  },
});

it("lists groups with their rule counts and the VMs that use them", async () => {
  mockLists({ "security-group": [ssh, securityGroup("empty")], vm: [web] });
  renderPage(<SecurityGroupsPage />);
  const table = await screen.findByRole("table", { name: "Security groups" });
  const headers = within(table).getAllByRole("columnheader");
  const cell = (row: HTMLElement, name: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === name)];
  const row = (await within(table).findByRole("link", { name: "ssh" })).closest("tr")!;
  expect(within(row).getByRole("link", { name: "ssh" })).toHaveAttribute(
    "href",
    "/security-groups/ssh",
  );
  expect(cell(row, "Rules")).toHaveTextContent("2");
  expect(cell(row, "Description")).toHaveTextContent("SSH from anywhere");
  expect(cell(row, "VMs")).toHaveTextContent("1");
  expect(row).toHaveTextContent("Remove it from the VM interfaces first.");
  // VM NIC가 쓰는 그룹은 지울 수 없다.
  expect(within(row).getByRole("button", { name: "Delete ssh" })).toBeDisabled();
  const empty = within(table).getByRole("link", { name: "empty" }).closest("tr")!;
  expect(cell(empty, "Rules")).toHaveTextContent("0");
  expect(cell(empty, "Description")).toHaveTextContent("—");
  expect(within(empty).getByRole("button", { name: "Delete empty" })).toBeEnabled();
});

it("judges nothing without the VMs", async () => {
  mockLists({ "security-group": [ssh] });
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<SecurityGroupsPage />);
  const row = (await screen.findByRole("link", { name: "ssh" })).closest("tr")!;
  expect(within(row).getByRole("button", { name: "Delete ssh" })).toBeDisabled();
});

it("creates a group and opens it to add rules", async () => {
  mockLists({});
  let body: unknown;
  server.use(
    http.post(apiUrl("/security-groups"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(securityGroup("web"), { status: 201 });
    }),
  );
  const { router } = renderPage(<SecurityGroupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a security group" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "Web");
  await userEvent.type(within(dialog).getByLabelText("Description (optional)"), "x".repeat(201));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText(/lowercase letters/)).toBeVisible();
  expect(within(dialog).getByText("Use at most 200 characters.")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "web");
  await userEvent.clear(within(dialog).getByLabelText("Description (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created security group web.")).toBeInTheDocument();
  // 설명이 비면 보내지 않는다.
  expect(body).toEqual({ name: "web", spec: { rules: [] } });
  expect(router.state.location.pathname).toBe("/security-groups/web");
});

it("deletes an unused group and shows why a delete failed", async () => {
  const lab = securityGroup("lab");
  mockLists({ "security-group": [lab] });
  server.use(
    http.delete(apiUrl(`/security-groups/${lab.id}`), () =>
      HttpResponse.json(
        { code: "security_group_in_use", message: 'security group "lab" is used by VM "x"' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SecurityGroupsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete lab" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A VM interface uses this security group. Remove it from the VM first.",
  );
});

it("hides changes from operators", async () => {
  mockLists({ "security-group": [ssh] });
  renderPage(<SecurityGroupsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("link", { name: "ssh" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete ssh" })).not.toBeInTheDocument();
});
