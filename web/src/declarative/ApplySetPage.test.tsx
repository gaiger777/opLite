import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { ApplySetPage } from "@/declarative/ApplySetPage";
import { refreshApplySets } from "@/declarative/api";
import { ApplySetsPage } from "@/declarative/ApplySetsPage";
import { expectNoAxeViolations } from "@/test/axe";
import { applySet, AT, DRIFTED, LIVE_MANIFESTS, SET_ID, WEB_ID } from "@/test/declarative";
import { task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => toast.dismiss());

const VIEWER = { ...ADMIN, role: "viewer" as const };
const CLEAN = { ...applySet("infra"), id: "019a0f3c-0000-7000-8073-0000000000a2" };

function mockSets() {
  mockLists({});
  server.use(
    http.get(apiUrl("/manifests"), () =>
      HttpResponse.json({ revision: 9, documents: LIVE_MANIFESTS }),
    ),
    http.get(apiUrl("/apply-sets"), () =>
      HttpResponse.json({ revision: 1, items: [DRIFTED, CLEAN] }),
    ),
    http.get(apiUrl("/apply-sets/:key"), ({ params }) =>
      HttpResponse.json(params.key === "infra" ? CLEAN : DRIFTED),
    ),
  );
}

it("explains managing as code and lists sets in plain words, and passes axe", async () => {
  mockSets();
  const { container } = renderPage(<ApplySetsPage />, { path: "/apply-sets" });
  // 첫 화면의 그림 설명: 무엇·언제·어떻게 시작과 적용 명령(복사)
  const about = await screen.findByRole("region", { name: "What is managing as code (YAML)?" });
  expect(about.querySelector("[data-slot=diagram]")).not.toBeNull();
  expect(about).toHaveTextContent("What Write settings");
  expect(about).toHaveTextContent("When To review");
  expect(about).toHaveTextContent("orbivisorctl apply --set <name> -f <folder>");
  expect(within(about).getByRole("button", { name: "Copy the command" })).toBeInTheDocument();
  const table = await screen.findByRole("table", { name: "Managed as code (YAML)" });
  const drifted = within(table).getByRole("row", { name: /web-prod/ });
  // 무엇을 관리하나 · 마지막 적용(누가·결과) · 파일과 비교
  expect(drifted).toHaveTextContent("Network 1 · VM 1");
  expect(drifted).toHaveTextContent("by ci");
  expect(drifted).toHaveTextContent("Succeeded");
  expect(drifted).toHaveTextContent("1 differs from the files");
  expect(within(drifted).getByRole("link", { name: "web-prod" })).toHaveAttribute(
    "href",
    "/apply-sets/web-prod",
  );
  expect(within(drifted).getByRole("link", { name: "What differs" })).toHaveAttribute(
    "href",
    "/apply-sets/web-prod#compare",
  );
  expect(within(table).getByRole("row", { name: /infra/ })).toHaveTextContent("Same as the files");
  await expectNoAxeViolations(container);
});

it("reads seconds from older servers as the last apply time", async () => {
  mockLists({});
  const old = applySet("old", { applied_at: Math.floor((AT - 86_400_000) / 1000) });
  server.use(
    http.get(apiUrl("/apply-sets"), () => HttpResponse.json({ revision: 1, items: [old] })),
  );
  renderPage(<ApplySetsPage />, { path: "/apply-sets" });
  const row = await screen.findByRole("row", { name: /old/ });
  expect(row).toHaveTextContent(String(new Date(AT).getFullYear()));
  expect(row).not.toHaveTextContent("1970");
});

it("compares file and actual values, shows who changed them and adopts the chosen ones", async () => {
  const user = userEvent.setup();
  mockSets();
  let adopted: unknown;
  server.use(
    http.post(apiUrl("/apply-sets/:key/adopt"), async ({ request, params }) => {
      expect(params.key).toBe(SET_ID);
      adopted = await request.json();
      return HttpResponse.json(applySet("web-prod"));
    }),
  );
  const { container } = renderPage(<ApplySetPage />, {
    path: "/apply-sets/:name",
    url: "/apply-sets/web-prod",
  });
  const table = await screen.findByRole("table", { name: "Resources managed by web-prod" });
  const web = within(table).getByRole("row", { name: /web/ });
  expect(web).toHaveTextContent("VM");
  expect(web).toHaveTextContent("Different");
  expect(within(web).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(table).getByRole("row", { name: /lan/ })).toHaveTextContent("Same");
  // 두 길: 파일대로 되돌리기(명령 복사) · 지금 값을 파일 값으로 삼기
  expect(
    screen.getByText("orbivisorctl apply --set web-prod -f <folder of files>"),
  ).toBeInTheDocument();
  // 파일 값 ↔ 실제 값: 다른 리소스는 펼치고, 다른 칸은 강조, 비밀은 견주지 않는다
  const compare = await screen.findByRole("table", { name: "VM web: file and actual values" });
  expect(compare.closest("details")).toHaveAttribute("open");
  expect(compare.closest("details")).toHaveTextContent("Changed by admin");
  expect(compare.closest("details")).toHaveTextContent("Web UI, record #81");
  expect(compare.closest("details")).toHaveTextContent("“more memory for the sale”");
  const memory = await within(compare).findByRole("row", { name: /memory_mib/ });
  expect(memory).toHaveTextContent("4 GiB");
  expect(memory).toHaveTextContent("8 GiB");
  expect(memory).toHaveTextContent("differs");
  expect(within(compare).getByRole("row", { name: /vcpus/ })).toHaveTextContent(
    "vCPUsvcpusdiffers24",
  );
  expect(within(compare).getByRole("row", { name: /labels.tier/ })).not.toHaveTextContent(
    "differs",
  );
  expect(within(compare).getByRole("row", { name: /cloud_init/ })).toHaveTextContent(
    "(secret — not compared)",
  );
  const lan = screen.getByRole("table", { name: "Network lan: file and actual values" });
  expect(lan.closest("details")).not.toHaveAttribute("open");
  expect(screen.getByRole("link", { name: "Change history" })).toHaveAttribute(
    "href",
    "/history?apply_set=web-prod&period=all",
  );
  await expectNoAxeViolations(container);
  await user.click(within(web).getByRole("checkbox", { name: "Choose web" }));
  await user.click(
    screen.getByRole("button", { name: "Keep 1 chosen current value as the file value" }),
  );
  const confirm = await screen.findByRole("alertdialog", {
    name: "web-prod: keep the current values as the file values?",
  });
  expect(confirm).toHaveTextContent("VM web: its current values become the file values");
  await user.click(within(confirm).getByRole("button", { name: "Keep current values" }));
  await waitFor(() => expect(adopted).toEqual({ objects: [WEB_ID] }));
  expect(
    await screen.findByText("web-prod now remembers the current values as its file values."),
  ).toBeInTheDocument();
});

it("lists the tasks that applied the set, newest first, opening the task drawer", async () => {
  mockSets();
  const base = task("t-new", "apply", SET_ID);
  const mine = {
    ...base,
    id: "019a0f3c-0000-7000-8073-0000000000f2",
    spec: { ...base.spec, op: { kind: "apply", set: SET_ID, plan: "p1" } },
    // 서버가 만들 때 단 대상 이름(계획 77-1)
    labels: { "target-name": "web-prod", "target-kind": "apply-set" },
  } as typeof base;
  const older = { ...task("t-old", "apply", SET_ID), id: "019a0f3c-0000-7000-8073-0000000000f1" };
  const other = { ...task("t-other", "apply", "019a0f3c-0000-7000-8073-0000000000ff") };
  mockLists({ task: [older, other, mine] });
  renderPage(<ApplySetPage />, { path: "/apply-sets/:name", url: "/apply-sets/web-prod" });
  const applies = await screen.findByRole("table", { name: "Applies" });
  const rows = within(applies).getAllByRole("row").slice(1);
  expect(rows).toHaveLength(2);
  // "web-prod 적용" 대신 무엇인지 보이는 이름, 누르면 Task 서랍(계획 77-1)
  const link = within(rows[0]!).getByRole("link", { name: "YAML apply — set web-prod" });
  expect(link).toHaveAttribute("href", expect.stringContaining(`task=${mine.id}`));
});

const LAN_ID = "019a0f3c-0000-7000-8073-000000000002";

/** 서버 `visible_set`처럼: 밖에서 지운 `lan`은 `status.objects`에서 빠지고 드리프트 보고에만 `missing`으로 남는다. */
function lanDeletedSet() {
  const set = applySet("web-prod", {
    drift: {
      checked_at: AT,
      revision: 90,
      items: [
        { kind: "vm", id: WEB_ID, name: "web", state: "modified", paths: ["vcpus"] },
        { kind: "network", id: LAN_ID, name: "lan", state: "missing" },
        // 묶음 레이블만 단 것은 받아들일 것이 없다
        {
          kind: "vm",
          id: "019a0f3c-0000-7000-8073-000000000009",
          name: "stray",
          state: "unmanaged",
        },
      ],
    },
  });
  set.status.objects = set.status.objects.filter((o) => o.id !== LAN_ID);
  return set;
}

function mockAdopt(set: ReturnType<typeof lanDeletedSet>) {
  mockLists({});
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/manifests"), () =>
      HttpResponse.json({ revision: 9, documents: LIVE_MANIFESTS.slice(0, 1) }),
    ),
    http.get(apiUrl("/apply-sets/:key"), () => HttpResponse.json(set)),
    http.post(apiUrl("/apply-sets/:key/adopt"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(applySet("web-prod"));
    }),
  );
  return sent;
}

it("adopts all only after confirming, with an empty body so the server picks what the caller may update", async () => {
  const user = userEvent.setup();
  const sent = mockAdopt(lanDeletedSet());
  renderPage(<ApplySetPage />, { path: "/apply-sets/:name", url: "/apply-sets/web-prod" });
  await user.click(
    await screen.findByRole("button", { name: "Keep all current values as the file values" }),
  );
  const confirm = await screen.findByRole("alertdialog", {
    name: "web-prod: keep the current values as the file values?",
  });
  expect(confirm).toHaveTextContent("VM web: its current values become the file values");
  expect(confirm).toHaveTextContent("Network lan: gone from the cluster, so it leaves the set");
  expect(confirm).not.toHaveTextContent("stray");
  expect(sent).toEqual([]);
  await user.click(within(confirm).getByRole("button", { name: "Keep current values" }));
  await waitFor(() => expect(sent).toEqual([{}]));
});

it("lets an object deleted outside the set be chosen and adopted", async () => {
  const user = userEvent.setup();
  const sent = mockAdopt(lanDeletedSet());
  renderPage(<ApplySetPage />, { path: "/apply-sets/:name", url: "/apply-sets/web-prod" });
  const table = await screen.findByRole("table", { name: "Resources managed by web-prod" });
  const lan = within(table).getByRole("row", { name: /lan/ });
  expect(lan).toHaveTextContent("Missing");
  expect(within(table).queryByRole("checkbox", { name: "Choose stray" })).not.toBeInTheDocument();
  await user.click(within(lan).getByRole("checkbox", { name: "Choose lan" }));
  await user.click(
    screen.getByRole("button", { name: "Keep 1 chosen current value as the file value" }),
  );
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent("Network lan: gone from the cluster");
  expect(confirm).not.toHaveTextContent("VM web");
  await user.click(within(confirm).getByRole("button", { name: "Keep current values" }));
  await waitFor(() => expect(sent).toEqual([{ objects: [LAN_ID] }]));
});

it("drops chosen rows that stop drifting on refresh instead of widening to all", async () => {
  const user = userEvent.setup();
  const set = lanDeletedSet();
  const sent = mockAdopt(set);
  const { queryClient } = renderPage(<ApplySetPage />, {
    path: "/apply-sets/:name",
    url: "/apply-sets/web-prod",
  });
  const table = await screen.findByRole("table", { name: "Resources managed by web-prod" });
  await user.click(within(table).getByRole("checkbox", { name: "Choose web" }));
  await user.click(
    screen.getByRole("button", { name: "Keep 1 chosen current value as the file value" }),
  );
  const confirm = await screen.findByRole("alertdialog");
  // 누가 web을 다시 적용해 드리프트 고리가 web을 지웠다 — 지켜보기가 묶음을 다시 받는다
  set.status.drift!.items = set.status.drift!.items!.filter((d) => d.id !== WEB_ID);
  await refreshApplySets(queryClient);
  await waitFor(() =>
    expect(within(confirm).getByRole("button", { name: "Keep current values" })).toBeDisabled(),
  );
  expect(confirm).not.toHaveTextContent("Network lan");
  await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
  expect(
    screen.getByRole("button", { name: "Keep 0 chosen current values as the file values" }),
  ).toBeDisabled();
  expect(sent).toEqual([]);
});

it("hides adopting from viewers", async () => {
  mockSets();
  renderPage(<ApplySetPage />, {
    path: "/apply-sets/:name",
    url: "/apply-sets/web-prod",
    as: VIEWER,
  });
  await screen.findByRole("table", { name: "Resources managed by web-prod" });
  expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /Keep/ })).not.toBeInTheDocument();
});

it("exports the set's manifests as YAML to copy or download", async () => {
  const user = userEvent.setup();
  mockSets();
  server.use(
    http.get(apiUrl("/manifests"), ({ request }) => {
      expect(new URL(request.url).searchParams.get("apply_set")).toBe("web-prod");
      return HttpResponse.json({
        revision: 9,
        documents: [
          {
            apiVersion: "orbivisor.io/v1",
            kind: "Vm",
            metadata: { name: "web", folder: "/prod" },
            spec: { memory_mib: 4096, cloud_init: { password: { secretRef: "web-pw" } } },
          },
        ],
      });
    }),
  );
  renderPage(<ApplySetPage />, { path: "/apply-sets/:name", url: "/apply-sets/web-prod" });
  await user.click(await screen.findByRole("button", { name: "Export as YAML" }));
  const dialog = await screen.findByRole("dialog", { name: "web-prod as YAML" });
  const yaml = await within(dialog).findByLabelText("YAML");
  expect(yaml.textContent).toBe(
    "---\napiVersion: orbivisor.io/v1\nkind: Vm\nmetadata:\n  name: web\n  folder: /prod\nspec:\n  memory_mib: 4096\n  cloud_init:\n    password:\n      secretRef: web-pw\n",
  );
  expect(within(dialog).getByRole("button", { name: "Copy the YAML" })).toBeInTheDocument();
  expect(within(dialog).getByRole("button", { name: "Download" })).toBeInTheDocument();
});

it("explains how to start when there are no sets", async () => {
  mockLists({});
  server.use(http.get(apiUrl("/apply-sets"), () => HttpResponse.json({ revision: 1, items: [] })));
  renderPage(<ApplySetsPage />, { path: "/apply-sets" });
  const empty = (
    await screen.findByText("No settings are managed as YAML yet.")
  ).closest<HTMLElement>("[data-slot=empty-state]")!;
  expect(empty).toHaveTextContent("orbivisorctl apply --set name -f folder");
  expect(within(empty).getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  // 그림은 화면 위 설명에 늘 있다
  expect(document.querySelector("[data-slot=diagram]")).not.toBeNull();
});
