import { act, screen, within } from "@testing-library/react";
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
  cluster_addresses: [`${name}:8007`],
  voter: true,
  role: "voter",
  online: true,
  witness: false,
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
          member("n1", "node-a", {
            fencing: "watchdog",
            cluster_addresses: ["node-a:8007", "10.1.0.11:8007"],
          }),
          member("n2", "node-b", { fencing: null }),
          member("n3", null, { voter: false, role: "learner", online: false, address: null }),
          member("n4", "witness-w", { witness: true }),
          member("n5", "node-o", { voter: false, role: "observer" }),
        ],
      }),
    ),
  ),
);

it("quotes the join URL in the command", () => {
  expect(joinCommand("opl-join://h:8006?token=a&ca=sha256:00")).toBe(
    "orbivisorctl cluster join 'opl-join://h:8006?token=a&ca=sha256:00'",
  );
  // 작은따옴표는 닫고 이스케이프한 뒤 다시 연다.
  expect(joinCommand("opl-join://h:8006?token=a'b")).toBe(
    "orbivisorctl cluster join 'opl-join://h:8006?token=a'\\''b'",
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
  // 관찰 노드(Raft 밖에서 투표 멤버를 따름)는 learner와 따로 보인다.
  const observer = within(table).getByText("node-o").closest("tr")!;
  expect(observer).toHaveTextContent("Observer");
  expect(observer).not.toHaveTextContent("Learner");
  // 위트니스는 역할 옆에 배지가 붙는다.
  expect(within(table).getByText("witness-w").closest("tr")).toHaveTextContent("VoterWitness");
  expect(within(table).queryAllByText("Witness")).toHaveLength(1);
  // 펜싱이 꺼졌거나(null) 모르면(옛 서버) 대시다.
  const column = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Fencing");
  const fencing = (name: string) =>
    within(within(table).getByText(name).closest("tr")!).getAllByRole("cell")[column];
  expect(fencing("node-a")).toHaveTextContent("Watchdog");
  expect(fencing("node-b")).toHaveTextContent("—");
  expect(fencing("witness-w")).toHaveTextContent("—");
  // 클러스터 주소는 우선 링크이고 둘째 링크부터 그 아래 보인다.
  const links = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Cluster address");
  const linksOf = (name: string) =>
    within(within(table).getByText(name).closest("tr")!).getAllByRole("cell")[links];
  expect(linksOf("node-a")).toHaveTextContent("node-a:800710.1.0.11:8007");
  expect(linksOf("node-b")).toHaveTextContent(/^node-b:8007$/);
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
  const command = "orbivisorctl cluster join 'opl-join://node-b:8006?token=oplj_x&ca=sha256:00ff'";
  expect(within(dialog).getByLabelText("Command")).toHaveValue(command);
  expect(within(dialog).getByText(/2030/)).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Copy the command" }));
  expect(await navigator.clipboard.readText()).toBe(command);
  // 명령을 놓치지 않게 Esc나 바깥 클릭으로는 닫히지 않는다.
  expect(within(dialog).queryByRole("button", { name: "Close" })).not.toBeInTheDocument();
  await user.keyboard("{Escape}");
  expect(dialog).toBeInTheDocument();
  await user.click(document.querySelector<HTMLElement>("[data-slot=dialog-overlay]")!);
  expect(dialog).toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  await vi.waitFor(() => expect(dialog).not.toBeInTheDocument());
  // 명령 대화상자를 닫으면 연 버튼으로 돌아간다.
  await vi.waitFor(() =>
    expect(screen.getByRole("button", { name: "Issue a join token" })).toHaveFocus(),
  );
});

it("returns focus to the issue button after cancelling", async () => {
  const user = userEvent.setup();
  renderPage(<ClusterPage />);
  await user.click(await screen.findByRole("button", { name: "Issue a join token" }));
  const form = await screen.findByRole("dialog", { name: "Issue a join token" });
  await user.click(within(form).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(form).not.toBeInTheDocument());
  await vi.waitFor(() =>
    expect(screen.getByRole("button", { name: "Issue a join token" })).toHaveFocus(),
  );
});

it("shows no leader while there is none and shortens unnamed ids", async () => {
  const id = "019a0f3c-8a10-7000-8000-00000000abcd";
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "sha256:00ff",
        leader: null,
        node: "n1",
        members: [member("n1", "node-a"), member(id, null)],
      }),
    ),
  );
  renderPage(<ClusterPage />);
  const table = await screen.findByRole("table", { name: "Members" });
  expect(within(table).getByText("node-a").closest("tr")).toHaveTextContent("Voter");
  expect(within(table).getByText("0000abcd")).toBeInTheDocument();
  expect(within(table).queryByText("Leader")).not.toBeInTheDocument();
});

it("keeps the form open when issuing fails", async () => {
  const user = userEvent.setup();
  server.use(
    http.post(apiUrl("/cluster/join-tokens"), () =>
      HttpResponse.json({ code: "forbidden", message: "forbidden" }, { status: 403 }),
    ),
  );
  renderPage(<ClusterPage />);
  await user.click(await screen.findByRole("button", { name: "Issue a join token" }));
  const form = await screen.findByRole("dialog", { name: "Issue a join token" });
  await user.click(within(form).getByRole("button", { name: "Issue a join token" }));
  expect(await within(form).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(screen.queryByRole("dialog", { name: "Join a new node" })).not.toBeInTheDocument();
});

it("keeps the members and says when it cannot read the cluster again", async () => {
  const { queryClient } = renderPage(<ClusterPage />);
  expect(await screen.findByRole("table", { name: "Members" })).toBeInTheDocument();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  await act(() => queryClient.refetchQueries({ queryKey: ["cluster"] }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Could not refresh the cluster. You do not have permission to do this.",
  );
  expect(screen.getByRole("table", { name: "Members" })).toBeInTheDocument();
});
