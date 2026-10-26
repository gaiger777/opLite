import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ClusterPage, joinCommand } from "@/cluster/ClusterPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const member = (id: string, name: string | null, extra = {}) => ({
  id,
  name,
  address: `${name}:8006`,
  cluster_address: `${name}:8007`,
  voter: true,
  online: true,
  ...extra,
});

beforeEach(() =>
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "sha256:00ff",
        leader: "n1",
        node: "n2",
        members: [
          member("n1", "node-a"),
          member("n2", "node-b"),
          member("n3", null, { voter: false, online: false, address: null }),
        ],
      }),
    ),
  ),
);

it("quotes the join URL in the command", () => {
  expect(joinCommand("opl-join://h:8006?token=a&ca=sha256:00")).toBe(
    "oplctl cluster join 'opl-join://h:8006?token=a&ca=sha256:00'",
  );
});

it("shows the CA and the members", async () => {
  renderPage(<ClusterPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByRole("heading", { name: "Cluster lab" })).toBeInTheDocument();
  expect(screen.getByText("sha256:00ff")).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Download the CA" })).toHaveAttribute(
    "href",
    "/api/v1/cluster/ca",
  );
  const table = screen.getByRole("table", { name: "Members" });
  expect(within(table).getByText("node-a").closest("tr")).toHaveTextContent("Leader");
  expect(within(table).getByText("node-b").closest("tr")).toHaveTextContent("(this node)");
  const joining = within(table).getByText("n3").closest("tr")!;
  expect(joining).toHaveTextContent("Learner");
  expect(joining).toHaveTextContent("Offline");
  expect(screen.queryByRole("button", { name: "Issue a join token" })).not.toBeInTheDocument();
});

it("issues a join token and shows the command once", async () => {
  const user = userEvent.setup();
  let body: unknown;
  server.use(
    http.post(apiUrl("/cluster/join-tokens"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        { url: "opl-join://node-b:8006?token=oplj_x&ca=sha256:00ff", expires_at: 1_900_000_000 },
        { status: 201 },
      );
    }),
  );
  renderPage(<ClusterPage />);
  await user.click(await screen.findByRole("button", { name: "Issue a join token" }));
  const form = await screen.findByRole("dialog", { name: "Issue a join token" });
  await user.selectOptions(within(form).getByLabelText("Valid for"), "1 day");
  await user.click(within(form).getByRole("button", { name: "Issue a join token" }));

  const dialog = await screen.findByRole("dialog", { name: "Join a new node" });
  expect(body).toEqual({ ttl_secs: 86400 });
  const command = "oplctl cluster join 'opl-join://node-b:8006?token=oplj_x&ca=sha256:00ff'";
  expect(within(dialog).getByLabelText("Command")).toHaveValue(command);
  expect(within(dialog).getByText(/2030/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Copy the command" }));
  expect(await navigator.clipboard.readText()).toBe(command);
});
