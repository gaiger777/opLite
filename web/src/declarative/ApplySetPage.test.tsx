import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { ApplySetPage } from "@/declarative/ApplySetPage";
import { ApplySetsPage } from "@/declarative/ApplySetsPage";
import { expectNoAxeViolations } from "@/test/axe";
import { applySet, DRIFTED, SET_ID, WEB_ID } from "@/test/declarative";
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
  expect(web).toHaveTextContent("Web, revision 81");
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
  await waitFor(() => expect(adopted).toEqual({ objects: [WEB_ID] }));
  expect(
    await screen.findByText("Apply set web-prod now declares the actual values."),
  ).toBeInTheDocument();
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
