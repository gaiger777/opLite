import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { ApplySetPage } from "@/declarative/ApplySetPage";
import { refreshApplySets } from "@/declarative/api";
import { ApplySetsPage } from "@/declarative/ApplySetsPage";
import { expectNoAxeViolations } from "@/test/axe";
import { applySet, AT, DRIFTED, SET_ID, WEB_ID } from "@/test/declarative";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => toast.dismiss());

const VIEWER = { ...ADMIN, role: "viewer" as const };
const CLEAN = { ...applySet("infra"), id: "019a0f3c-0000-7000-8073-0000000000a2" };

function mockSets() {
  mockLists({});
  server.use(
    http.get(apiUrl("/apply-sets"), () =>
      HttpResponse.json({ revision: 1, items: [DRIFTED, CLEAN] }),
    ),
    http.get(apiUrl("/apply-sets/:key"), ({ params }) =>
      HttpResponse.json(params.key === "infra" ? CLEAN : DRIFTED),
    ),
  );
}

it("lists apply sets with object counts, the last apply and drift chips, and passes axe", async () => {
  mockSets();
  const { container } = renderPage(<ApplySetsPage />, { path: "/apply-sets" });
  const table = await screen.findByRole("table", { name: "Apply sets" });
  const drifted = within(table).getByRole("row", { name: /web-prod/ });
  expect(drifted).toHaveTextContent("1 drifted");
  expect(drifted).toHaveTextContent("by ci");
  expect(within(drifted).getByRole("link", { name: "web-prod" })).toHaveAttribute(
    "href",
    "/apply-sets/web-prod",
  );
  expect(within(table).getByRole("row", { name: /infra/ })).toHaveTextContent("In sync");
  await expectNoAxeViolations(container);
});

it("shows each object's state, who changed it, and adopts the chosen ones", async () => {
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
  const table = await screen.findByRole("table", { name: "Objects of web-prod" });
  const web = within(table).getByRole("row", { name: /web/ });
  expect(web).toHaveTextContent("Different");
  expect(web).toHaveTextContent("Changed by admin");
  expect(web).toHaveTextContent("Web UI, revision 81");
  expect(web).toHaveTextContent("“more memory for the sale”");
  await user.click(within(web).getByText("2 fields differ"));
  expect(within(web).getByText("vcpus")).toBeInTheDocument();
  expect(within(table).getByRole("row", { name: /lan/ })).toHaveTextContent("Same");
  expect(screen.getByRole("link", { name: "Config history" })).toHaveAttribute(
    "href",
    "/history?apply_set=web-prod&period=all",
  );
  await expectNoAxeViolations(container);
  await user.click(within(web).getByRole("checkbox", { name: "Choose web" }));
  await user.click(screen.getByRole("button", { name: "Accept 1 actual value as declared" }));
  const confirm = await screen.findByRole("alertdialog", {
    name: "Accept the actual values in web-prod?",
  });
  expect(confirm).toHaveTextContent("vm web: its actual values become the declaration");
  await user.click(within(confirm).getByRole("button", { name: "Accept" }));
  await waitFor(() => expect(adopted).toEqual({ objects: [WEB_ID] }));
  expect(
    await screen.findByText("Apply set web-prod now declares the actual values."),
  ).toBeInTheDocument();
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
    await screen.findByRole("button", { name: "Accept all actual values as declared" }),
  );
  const confirm = await screen.findByRole("alertdialog", {
    name: "Accept the actual values in web-prod?",
  });
  expect(confirm).toHaveTextContent("vm web: its actual values become the declaration");
  expect(confirm).toHaveTextContent(
    "network lan: gone from the cluster, so it leaves the apply set",
  );
  expect(confirm).not.toHaveTextContent("stray");
  expect(sent).toEqual([]);
  await user.click(within(confirm).getByRole("button", { name: "Accept" }));
  await waitFor(() => expect(sent).toEqual([{}]));
});

it("lets an object deleted outside the set be chosen and adopted", async () => {
  const user = userEvent.setup();
  const sent = mockAdopt(lanDeletedSet());
  renderPage(<ApplySetPage />, { path: "/apply-sets/:name", url: "/apply-sets/web-prod" });
  const table = await screen.findByRole("table", { name: "Objects of web-prod" });
  const lan = within(table).getByRole("row", { name: /lan/ });
  expect(lan).toHaveTextContent("Missing");
  expect(within(table).queryByRole("checkbox", { name: "Choose stray" })).not.toBeInTheDocument();
  await user.click(within(lan).getByRole("checkbox", { name: "Choose lan" }));
  await user.click(screen.getByRole("button", { name: "Accept 1 actual value as declared" }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent("network lan: gone from the cluster");
  expect(confirm).not.toHaveTextContent("vm web");
  await user.click(within(confirm).getByRole("button", { name: "Accept" }));
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
  const table = await screen.findByRole("table", { name: "Objects of web-prod" });
  await user.click(within(table).getByRole("checkbox", { name: "Choose web" }));
  await user.click(screen.getByRole("button", { name: "Accept 1 actual value as declared" }));
  const confirm = await screen.findByRole("alertdialog");
  // 누가 web을 다시 적용해 드리프트 고리가 web을 지웠다 — 지켜보기가 묶음을 다시 받는다
  set.status.drift!.items = set.status.drift!.items!.filter((d) => d.id !== WEB_ID);
  await refreshApplySets(queryClient);
  await waitFor(() =>
    expect(within(confirm).getByRole("button", { name: "Accept" })).toBeDisabled(),
  );
  expect(confirm).not.toHaveTextContent("network lan");
  await user.click(within(confirm).getByRole("button", { name: "Cancel" }));
  expect(screen.getByRole("button", { name: "Accept 0 actual values as declared" })).toBeDisabled();
  expect(sent).toEqual([]);
});

it("hides adopting from viewers", async () => {
  mockSets();
  renderPage(<ApplySetPage />, {
    path: "/apply-sets/:name",
    url: "/apply-sets/web-prod",
    as: VIEWER,
  });
  await screen.findByRole("table", { name: "Objects of web-prod" });
  expect(screen.queryByRole("checkbox")).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /Accept/ })).not.toBeInTheDocument();
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

it("explains apply sets with a picture and the first steps when there are none", async () => {
  mockLists({});
  server.use(http.get(apiUrl("/apply-sets"), () => HttpResponse.json({ revision: 1, items: [] })));
  renderPage(<ApplySetsPage />, { path: "/apply-sets" });
  const empty = (await screen.findByText("No apply sets.")).closest<HTMLElement>(
    "[data-slot=empty-state]",
  )!;
  expect(empty).toHaveTextContent("orbivisorctl apply --set name -f folder");
  expect(empty.querySelector("[data-slot=diagram]")).not.toBeNull();
  expect(within(empty).getByRole("button", { name: "Learn more" })).toBeInTheDocument();
});
