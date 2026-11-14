import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { PlacementStatus, Principal } from "@/api/types";
import { PlacementPage } from "@/placement/PlacementPage";
import { resetTasks } from "@/tasks/tasks";
import { PLACEMENT, task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const VIEWER: Principal = { ...ADMIN, name: "view", role: "viewer" };
const OPERATOR: Principal = { ...ADMIN, name: "ops", role: "operator" };

// 아직 저장한 적 없는 설정(서버가 기본값을 revision 0으로 돌려준다)
const SETTINGS = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 0,
  labels: {},
  spec: {},
  status: {},
};

/** 배치 상태. 읽은 수를 센다. */
function placement(status: PlacementStatus = PLACEMENT) {
  const reads = { n: 0 };
  server.use(
    http.get(apiUrl("/placement"), () => {
      reads.n += 1;
      return HttpResponse.json(status);
    }),
  );
  return reads;
}

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

/** 요약의 값(이름표 `label` 아래) */
function summary(label: string) {
  return screen.getByText(label, { selector: "dt" }).closest("div")!;
}

it("shows the balancer, the node load and the recommendations", async () => {
  mockLists({});
  placement();
  renderPage(<PlacementPage />, { as: VIEWER });
  const nodes = await screen.findByRole("table", { name: "Node load" });
  // 서버가 쓰지 않은 기본 설정
  expect(summary("Mode")).toHaveTextContent("Recommend");
  expect(summary("Policy")).toHaveTextContent("Balance");
  expect(summary("Threshold")).toHaveTextContent("15%");
  expect(summary("Imbalance")).toHaveTextContent("21.0% (CPU 18.2%, memory 23.8%)");
  expect(screen.queryByText("Paused", { selector: "dt" })).not.toBeInTheDocument();
  const a = cells(nodes, "node-a");
  expect(within(a("CPU")!).getByRole("meter", { name: "CPU" })).toHaveAttribute(
    "aria-valuenow",
    "88",
  );
  expect(a("CPU 1 min")).toHaveTextContent("90.5%");
  expect(a("CPU 5 min")).toHaveTextContent("88.0%");
  expect(a("Memory")).toHaveTextContent("75.0%Used 12 GiB · Allocated 10 GiB / 16 GiB");
  expect(a("VMs")).toHaveTextContent("5");
  expect(a("Summary age")).toHaveTextContent("3 s");
  // 요약이 없는 노드
  const b = cells(nodes, "node-b");
  expect(b("CPU 1 min")).toHaveTextContent("—");
  expect(b("Memory")).toHaveTextContent("Used — · Allocated 2 GiB / 16 GiB");
  expect(b("Summary age")).toHaveTextContent("No summary");
  const moves = screen.getByRole("table", { name: "Recommendations" });
  const web = cells(moves, "web1");
  expect(web("From")).toHaveTextContent("node-a");
  expect(web("To")).toHaveTextContent("node-b");
  expect(web("Reason")).toHaveTextContent("BalanceImbalance −8.5%");
  expect(web("Cost")).toHaveTextContent("4 GiB");
  expect(cells(moves, "db1")("Reason")).toHaveTextContent("RuleBreaks only-b");
  // 뷰어는 적용하지도, 설정을 바꾸지도 못한다.
  expect(screen.queryByRole("button", { name: /^Apply/ })).not.toBeInTheDocument();
  expect(screen.queryByText("Balancer settings")).not.toBeInTheDocument();
});

it("applies a recommendation and reads the status again", async () => {
  mockLists({});
  const reads = placement();
  const applied: string[] = [];
  server.use(
    http.post(apiUrl("/placement/recommendations/:id/apply"), ({ params }) => {
      applied.push(String(params.id));
      return HttpResponse.json(task("t-1", "vm-migrate", "web1"), { status: 202 });
    }),
  );
  renderPage(<PlacementPage />, { as: OPERATOR });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Apply: move web1 to node-b" }));
  expect(await screen.findByText("Move web1 to node-b")).toBeInTheDocument();
  expect(applied).toEqual(["019a0f3c-0000-7000-800c-000000000001"]);
  await waitFor(() => expect(reads.n).toBe(2));
});

it("says when the recommendation is gone", async () => {
  mockLists({});
  const reads = placement();
  server.use(
    http.post(apiUrl("/placement/recommendations/:id/apply"), () =>
      HttpResponse.json(
        { code: "recommendation_not_found", message: "the recommendation is gone" },
        { status: 404 },
      ),
    ),
  );
  renderPage(<PlacementPage />, { as: OPERATOR });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Apply: move db1 to node-b" }));
  expect(await screen.findByText(/The recommendation has changed/)).toBeInTheDocument();
  // 바뀐 추천을 보이려고 다시 읽는다.
  await waitFor(() => expect(reads.n).toBe(2));
});

it("says why a recommendation no longer fits", async () => {
  mockLists({});
  const reads = placement();
  server.use(
    http.post(apiUrl("/placement/recommendations/:id/apply"), () =>
      HttpResponse.json(
        {
          code: "recommendation_stale",
          message:
            "the recommendation is stale (busy with a task); recommendations are recomputed every run",
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<PlacementPage />, { as: OPERATOR });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Apply: move db1 to node-b" }));
  expect(
    await screen.findByText(/The recommendation no longer fits the cluster: .*busy with a task/),
  ).toBeInTheDocument();
  await waitFor(() => expect(reads.n).toBe(2));
});

it("shows started moves and why the balancer is paused in auto mode", async () => {
  mockLists({});
  placement({
    ...PLACEMENT,
    settings: { mode: "auto", policy: "pack", threshold: 20 },
    paused: "policy is pack",
    recommendations: [
      { ...PLACEMENT.recommendations[1]!, task: "019a0f3c-77aa-7000-8000-000000000009" },
    ],
  });
  renderPage(<PlacementPage />, { as: OPERATOR });
  const moves = await screen.findByRole("table", { name: "Moves started" });
  expect(summary("Mode")).toHaveTextContent("Automatic");
  expect(summary("Policy")).toHaveTextContent("Pack");
  expect(summary("Threshold")).toHaveTextContent("20%");
  expect(summary("Paused")).toHaveTextContent(
    "The policy is pack, so the balancer does not move VMs for balance.",
  );
  // 이미 시작한 옮기기는 적용 대신 Task를 보인다.
  expect(cells(moves, "db1")("Task")).toHaveTextContent("019a0f3c-77aa-7000-8000-000000000009");
  expect(within(moves).queryByRole("button")).not.toBeInTheDocument();
});

it("says when the leader cannot be asked", async () => {
  mockLists({});
  server.use(
    http.get(apiUrl("/placement"), () =>
      HttpResponse.json(
        { code: "placement_unavailable", message: "leader unreachable" },
        { status: 503 },
      ),
    ),
  );
  renderPage(<PlacementPage />, { as: VIEWER });
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Could not read placement from the cluster leader. Try again shortly.",
  );
});

it("changes only the balancer settings an admin edited", async () => {
  mockLists({});
  const reads = placement();
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(SETTINGS)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { placement: object };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...SETTINGS, revision: 3, spec: { placement: body.placement } });
    }),
  );
  renderPage(<PlacementPage />);
  const user = userEvent.setup();
  const mode = await screen.findByLabelText("Mode");
  expect(mode).toHaveValue("recommend");
  expect(screen.getByLabelText("Policy")).toHaveValue("balance");
  const threshold = screen.getByLabelText("Threshold");
  expect(threshold).toHaveValue(15);
  // 문턱 설명: 문턱의 0.8배 아래가 될 때까지 옮기고, 평균보다 문턱의 2배 넘게 높은 노드는 따로 던다.
  expect(screen.getByText(/goes on until it is below 0\.8 times this/)).toBeInTheDocument();
  expect(
    screen.getByText(/more than twice this above the average is relieved/),
  ).toBeInTheDocument();
  const save = screen.getByRole("button", { name: "Save" });
  expect(save).toBeDisabled();
  await user.selectOptions(mode, "auto");
  expect(
    screen.getByText("The balancer starts up to two live migrations at a time by itself."),
  ).toBeInTheDocument();
  await user.clear(threshold);
  await user.type(threshold, "51");
  await user.click(save);
  expect(screen.getByText("A whole number from 1 to 50.")).toBeInTheDocument();
  expect(sent).toHaveLength(0);
  await user.clear(threshold);
  await user.type(threshold, "20");
  await user.click(save);
  expect(await screen.findByText("Placement settings saved.")).toBeInTheDocument();
  expect(sent).toEqual([{ ifMatch: "0", body: { placement: { mode: "auto", threshold: 20 } } }]);
  // 저장한 값을 보이고, 상태를 다시 읽는다.
  expect(screen.getByLabelText("Mode")).toHaveValue("auto");
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  await waitFor(() => expect(reads.n).toBe(2));
});
