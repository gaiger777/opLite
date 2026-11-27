import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { expectNoAxeViolations } from "@/test/axe";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VC_FP, vmwareSource } from "@/test/vmware";
import { SourcesPage } from "@/vmware/SourcesPage";

afterEach(() => toast.dismiss());

const SHA1 = "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD";
const VC1 = vmwareSource("vc1");
const ESX = vmwareSource("esx7", 2, {
  status: { kind: "esxi", version: "7.0.3", error: "connection refused", checked_at: 1 },
});

function mockSources(items = [VC1, ESX]) {
  mockLists({});
  server.use(http.get(apiUrl("/vmware-sources"), () => HttpResponse.json({ revision: 1, items })));
}

it("lists the saved connections without passwords and passes axe", async () => {
  mockSources();
  const { container } = renderPage(<SourcesPage />, { path: "/vmware-sources" });
  const table = await screen.findByRole("table", { name: "Connected vCenter & ESXi" });
  const vc = within(table).getByRole("row", { name: /vc1/ });
  expect(vc).toHaveTextContent("vc1.example.com");
  expect(vc).toHaveTextContent("vCenter 8.0.2");
  expect(vc).toHaveTextContent("Connected");
  expect(vc).toHaveTextContent(`${VC_FP.split(":").slice(0, 4).join(":")}…`);
  const esx = within(table).getByRole("row", { name: /esx7/ });
  expect(esx).toHaveTextContent("Failed");
  expect(esx).toHaveTextContent("connection refused");
  await expectNoAxeViolations(container);
});

it("asks to trust the certificate and sends the fingerprint with the password", async () => {
  const user = userEvent.setup();
  mockSources([]);
  const bodies: Record<string, unknown>[] = [];
  server.use(
    http.post(apiUrl("/vmware-sources"), async ({ request }) => {
      const body = (await request.json()) as Record<string, unknown>;
      bodies.push(body);
      if (!body.fingerprint) {
        return HttpResponse.json(
          {
            code: "untrusted_certificate",
            message: "untrusted",
            details: { fingerprint: VC_FP, thumbprint_sha1: SHA1 },
          },
          { status: 409 },
        );
      }
      return HttpResponse.json(vmwareSource("vc2", 3), { status: 201 });
    }),
  );
  const { queryClient } = renderPage(<SourcesPage />, { path: "/vmware-sources" });
  await user.click(await screen.findByRole("button", { name: "Connect vCenter or ESXi" }));
  const dialog = screen.getByRole("dialog", { name: "Connect a vCenter or ESXi" });
  // 빈 칸은 보내지 않는다
  await user.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(within(dialog).getByText("Enter the host.")).toBeInTheDocument();
  expect(bodies).toHaveLength(0);
  await user.type(within(dialog).getByLabelText("Name"), "vc2");
  await user.type(within(dialog).getByLabelText("Host"), "vc2.example.com");
  await user.type(within(dialog).getByLabelText("User"), "migr@vsphere.local");
  await user.type(within(dialog).getByLabelText("Password"), "s3cret-pass");
  await user.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByText(VC_FP)).toBeInTheDocument();
  expect(within(dialog).getByText(SHA1)).toBeInTheDocument();
  expect(bodies[0]).not.toHaveProperty("fingerprint");
  await user.click(within(dialog).getByRole("button", { name: "Trust and connect" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(bodies[1]).toEqual({
    name: "vc2",
    host: "vc2.example.com",
    port: 443,
    user: "migr@vsphere.local",
    password: "s3cret-pass",
    fingerprint: VC_FP,
  });
  // 비밀번호는 요청 본문에만 — 브라우저 저장소·질의 캐시에 남지 않는다
  for (const store of [localStorage, sessionStorage]) {
    const all = Object.keys(store).map((k) => store.getItem(k) ?? "");
    expect(all.join("\n")).not.toContain("s3cret-pass");
  }
  expect(
    JSON.stringify(
      queryClient
        .getQueryCache()
        .getAll()
        .map((q) => q.state.data),
    ),
  ).not.toContain("s3cret-pass");
});

it("shows the upgrade notice when a node does not know VMware migration", async () => {
  const user = userEvent.setup();
  mockSources([]);
  server.use(
    http.post(apiUrl("/vmware-sources"), () =>
      HttpResponse.json(
        { code: "cluster_upgrading", message: "node b does not support vmware-migrate" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SourcesPage />, { path: "/vmware-sources" });
  await user.click(await screen.findByRole("button", { name: "Connect vCenter or ESXi" }));
  const dialog = screen.getByRole("dialog");
  await user.type(within(dialog).getByLabelText("Name"), "vc2");
  await user.type(within(dialog).getByLabelText("Host"), "vc2.example.com");
  await user.type(within(dialog).getByLabelText("User"), "u");
  await user.type(within(dialog).getByLabelText("Password"), "p");
  await user.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The cluster is being upgraded: node b does not support vmware-migrate",
  );
});

it("changes only the edited fields and checks a source again", async () => {
  const user = userEvent.setup();
  mockSources();
  let patch: unknown;
  let checked = false;
  server.use(
    // 서버(`patch_source`)처럼: 호스트·포트·사용자를 바꾸며 비밀번호를 주지 않으면 400 `password_required`
    http.patch(apiUrl("/vmware-sources/:key"), async ({ request }) => {
      const body = (await request.json()) as Record<string, unknown>;
      if (("host" in body || "port" in body || "user" in body) && !body.password) {
        return HttpResponse.json(
          {
            code: "password_required",
            message: "give the password again when changing the host, port or user",
          },
          { status: 400 },
        );
      }
      patch = body;
      return HttpResponse.json(VC1);
    }),
    http.post(apiUrl("/vmware-sources/:key/actions/check"), () => {
      checked = true;
      return HttpResponse.json(VC1);
    }),
  );
  renderPage(<SourcesPage />, { path: "/vmware-sources" });
  await user.click(await screen.findByRole("button", { name: "Change vc1" }));
  const dialog = screen.getByRole("dialog", { name: "Change vc1" });
  expect(within(dialog).getByLabelText("Password")).toHaveValue("");
  expect(dialog).toHaveTextContent(
    "Leave empty to keep the saved password (only if host, port and user stay the same).",
  );
  await user.clear(within(dialog).getByLabelText("User"));
  await user.type(within(dialog).getByLabelText("User"), "admin@vsphere.local");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    await within(dialog).findByText(
      "Enter the password again when you change the host, port or user.",
    ),
  ).toBeInTheDocument();
  expect(patch).toBeUndefined();
  await user.type(within(dialog).getByLabelText("Password"), "secret");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(patch).toEqual({ user: "admin@vsphere.local", password: "secret" }));
  await user.click(screen.getByRole("button", { name: "Check vc1 again" }));
  await waitFor(() => expect(checked).toBe(true));
  expect(await screen.findByText("vc1: vCenter 8.0.2")).toBeInTheDocument();
});

it("keeps the saved password when only other fields change", async () => {
  const user = userEvent.setup();
  mockSources();
  let patch: unknown;
  server.use(
    http.patch(apiUrl("/vmware-sources/:key"), async ({ request }) => {
      patch = await request.json();
      return HttpResponse.json(VC1);
    }),
  );
  renderPage(<SourcesPage />, { path: "/vmware-sources" });
  await user.click(await screen.findByRole("button", { name: "Change vc1" }));
  const dialog = screen.getByRole("dialog", { name: "Change vc1" });
  await user.type(within(dialog).getByLabelText("Datacenter (optional)"), "dc1");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(patch).toEqual({ datacenter_hint: "dc1" }));
});

it("explains a source that migrations still use", async () => {
  const user = userEvent.setup();
  mockSources();
  server.use(
    http.delete(apiUrl("/vmware-sources/:key"), () =>
      HttpResponse.json({ code: "source_in_use", message: "used by wave1" }, { status: 409 }),
    ),
  );
  renderPage(<SourcesPage />, { path: "/vmware-sources" });
  await user.click(await screen.findByRole("button", { name: "Delete vc1" }));
  const dialog = screen.getByRole("alertdialog");
  await user.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Migrations still use this source. Delete them first.",
  );
});

it("shows no buttons to users without cluster.admin", async () => {
  mockSources();
  renderPage(<SourcesPage />, { path: "/vmware-sources", as: { ...ADMIN, role: "viewer" } });
  await screen.findByRole("table", { name: "Connected vCenter & ESXi" });
  expect(screen.queryByRole("button", { name: "Connect vCenter or ESXi" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Change vc1" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete vc1" })).not.toBeInTheDocument();
});

it("explains an empty connection list with a connect button and more help", async () => {
  mockSources([]);
  renderPage(<SourcesPage />, { path: "/vmware-sources" });
  expect(await screen.findByText("No vCenter or ESXi connected")).toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "Connected vCenter & ESXi" })).toBeVisible();
  expect(screen.getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Connect vCenter or ESXi" }));
  expect(screen.getByRole("dialog", { name: "Connect a vCenter or ESXi" })).toBeInTheDocument();
});
