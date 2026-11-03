import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { SecurityGroupPage } from "@/security-groups/SecurityGroupPage";
import { securityGroup } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const ssh = securityGroup(
  "ssh",
  [
    { direction: "in", action: "accept", protocol: "tcp", ports: "22", remote: "10.0.0.0/8" },
    { direction: "out", action: "drop" },
  ],
  "SSH",
);

function open(as = ADMIN) {
  return renderPage(<SecurityGroupPage />, {
    path: "/security-groups/:name",
    url: "/security-groups/ssh",
    as,
  });
}

it("shows the rules in order", async () => {
  mockLists({ "security-group": [ssh] });
  open();
  expect(await screen.findByRole("heading", { name: "ssh" })).toBeInTheDocument();
  expect(screen.getByLabelText("Description")).toHaveValue("SSH");
  expect(screen.getByLabelText("Direction of rule 1")).toHaveValue("in");
  expect(screen.getByLabelText("Protocol of rule 1")).toHaveValue("tcp");
  expect(screen.getByLabelText("Ports of rule 1")).toHaveValue("22");
  expect(screen.getByLabelText("Remote CIDR of rule 1")).toHaveValue("10.0.0.0/8");
  expect(screen.getByLabelText("Direction of rule 2")).toHaveValue("out");
  expect(screen.getByLabelText("Action of rule 2")).toHaveValue("drop");
  // 프로토콜이 없으면 모두이고 포트는 쓸 수 없다.
  expect(screen.getByLabelText("Protocol of rule 2")).toHaveValue("any");
  expect(screen.getByLabelText("Ports of rule 2")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Move rule 1 up" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Move rule 2 down" })).toBeDisabled();
});

it("edits, reorders and saves the rules with the revision it read", async () => {
  mockLists({ "security-group": [ssh] });
  let ifMatch: string | null = null;
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/security-groups/${ssh.id}`), async ({ request }) => {
      ifMatch = request.headers.get("if-match");
      body = await request.json();
      return HttpResponse.json({ ...ssh, revision: 2 });
    }),
  );
  open();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Add a rule" }));
  // 새 규칙은 들어오는 tcp 허용이다.
  expect(screen.getByLabelText("Protocol of rule 3")).toHaveValue("tcp");
  await user.selectOptions(screen.getByLabelText("Protocol of rule 3"), "icmp");
  await user.click(screen.getByRole("button", { name: "Move rule 3 up" }));
  await user.click(screen.getByRole("button", { name: "Move rule 2 up" }));
  // tcp에서 udp로 바꾸면 포트는 그대로다.
  await user.selectOptions(screen.getByLabelText("Protocol of rule 2"), "udp");
  expect(screen.getByLabelText("Ports of rule 2")).toHaveValue("22");
  await user.clear(screen.getByLabelText("Ports of rule 2"));
  await user.type(screen.getByLabelText("Ports of rule 2"), "53");
  await user.click(screen.getByRole("button", { name: "Remove rule 3" }));
  await user.clear(screen.getByLabelText("Description"));
  await user.click(screen.getByRole("button", { name: "Save" }));

  expect(await screen.findByText("Saved security group ssh.")).toBeInTheDocument();
  expect(ifMatch).toBe("1");
  expect(body).toEqual({
    description: "",
    rules: [
      { direction: "in", action: "accept", protocol: "icmp" },
      { direction: "in", action: "accept", protocol: "udp", ports: "53", remote: "10.0.0.0/8" },
    ],
  });
});

it("clears the ports when the protocol cannot have them", async () => {
  mockLists({ "security-group": [ssh] });
  open();
  const user = userEvent.setup();
  await user.selectOptions(await screen.findByLabelText("Protocol of rule 1"), "icmp");
  expect(screen.getByLabelText("Ports of rule 1")).toHaveValue("");
  expect(screen.getByLabelText("Ports of rule 1")).toBeDisabled();
});

it("ties input errors to their rules without calling the API", async () => {
  mockLists({ "security-group": [ssh] });
  let called = false;
  server.use(
    http.patch(apiUrl(`/security-groups/${ssh.id}`), () => {
      called = true;
      return HttpResponse.json(ssh);
    }),
  );
  open();
  const user = userEvent.setup();
  await user.clear(await screen.findByLabelText("Ports of rule 1"));
  await user.type(screen.getByLabelText("Ports of rule 1"), "70000");
  await user.type(screen.getByLabelText("Remote CIDR of rule 2"), "10.0.0/8");
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(screen.getByLabelText("Ports of rule 1")).toHaveAccessibleDescription(
    "Use a port like 22 or a range like 8000-8080 (1–65535).",
  );
  expect(screen.getByLabelText("Remote CIDR of rule 2")).toHaveAccessibleDescription(
    "Use a CIDR like 10.0.0.0/8 or 2001:db8::/32.",
  );
  expect(called).toBe(false);
  // 행을 옮기면 행 번호로 단 오류는 지운다.
  await user.click(screen.getByRole("button", { name: "Move rule 2 up" }));
  expect(screen.getByLabelText("Ports of rule 2")).not.toHaveAttribute("aria-invalid");
});

it("offers to reload when someone else changed the group first", async () => {
  mockLists({ "security-group": [ssh] });
  server.use(
    http.patch(apiUrl(`/security-groups/${ssh.id}`), () =>
      HttpResponse.json(
        { code: "revision_mismatch", message: "expected 1, found 2" },
        { status: 412 },
      ),
    ),
  );
  const { queryClient } = open();
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Remove rule 2" }));
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Someone else changed it first.");

  const changed = { ...ssh, revision: 2, spec: { ...ssh.spec, rules: [] } };
  queryClient.setQueryData(["list", "security-group"], { revision: 2, items: [changed] });
  await user.click(screen.getByRole("button", { name: "Reload" }));
  expect(screen.getByText(/^No rules/)).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("shows the rules read-only to operators", async () => {
  mockLists({ "security-group": [ssh] });
  open({ ...ADMIN, role: "operator" });
  expect(await screen.findByLabelText("Ports of rule 1")).toBeDisabled();
  expect(screen.getByLabelText("Direction of rule 1")).toBeDisabled();
  for (const name of ["Save", "Add a rule", "Remove rule 1"]) {
    expect(screen.queryByRole("button", { name })).not.toBeInTheDocument();
  }
});

it("says when the group does not exist", async () => {
  mockLists({ "security-group": [] });
  open();
  expect(
    await screen.findByRole("heading", { name: "Security group ssh not found" }),
  ).toBeInTheDocument();
  expect(within(document.body).queryByRole("table")).not.toBeInTheDocument();
});
