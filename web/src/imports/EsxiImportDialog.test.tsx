import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ImportsPage } from "@/imports/ImportsPage";
import { esxiImport, FINGERPRINT, NODE, vmImport } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const PASSWORD = "s3cret-Pa55";

const BROWSE = {
  datastores: [
    {
      name: "datastore1",
      vms: [
        {
          vmx: "web01/web01.vmx",
          name: "Web Server 01",
          guest_os: "ubuntu-64",
          vcpus: 2,
          memory_mib: 2048,
          disks: 1,
          locked: true,
          error: null,
        },
        {
          vmx: "broken/broken.vmx",
          name: "",
          guest_os: "",
          vcpus: 0,
          memory_mib: 0,
          disks: 0,
          locked: false,
          error: "ethernet0 has no networkName",
        },
      ],
    },
    {
      name: "datastore2",
      vms: [
        {
          vmx: "db/db.vmx",
          name: "db",
          guest_os: "windows2019srv-64",
          vcpus: 4,
          memory_mib: 8192,
          disks: 2,
          locked: false,
          error: null,
        },
      ],
    },
  ],
};

/**
 * 요청을 모두 기록하는 가짜 서버. 실제 서버처럼 browse는 지문이 없으면 409 `untrusted_certificate`, 비밀번호가
 * 틀리면 400 `esxi_auth`다. `create`는 만들기 응답을 바꾼다.
 */
function mockEsxi(create?: () => Response) {
  const requests: { url: string; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
    http.post(apiUrl("/vm-imports/esxi/browse"), async ({ request }) => {
      const body = (await request.json()) as { fingerprint?: string; password: string };
      requests.push({ url: request.url, body });
      if (!body.fingerprint) {
        return HttpResponse.json(
          {
            code: "untrusted_certificate",
            message: "verify the ESXi certificate fingerprint",
            details: { fingerprint: FINGERPRINT },
          },
          { status: 409 },
        );
      }
      if (body.password !== PASSWORD) {
        return HttpResponse.json(
          { code: "esxi_auth", message: "ESXi rejected the user name or password" },
          { status: 400 },
        );
      }
      return HttpResponse.json(BROWSE);
    }),
    http.post(apiUrl("/vm-imports/esxi"), async ({ request }) => {
      requests.push({ url: request.url, body: await request.json() });
      return create?.() ?? HttpResponse.json(esxiImport("web-server-01", 9), { status: 201 });
    }),
  );
  return requests;
}

async function openDialog() {
  await userEvent.click(await screen.findByRole("button", { name: "Import from ESXi" }));
  return screen.findByRole("dialog", { name: "Import from ESXi" });
}

async function connect(dialog: HTMLElement, password = PASSWORD) {
  await userEvent.type(within(dialog).getByLabelText("Host"), "esxi1.lab");
  await userEvent.type(within(dialog).getByLabelText("User"), "root");
  await userEvent.type(within(dialog).getByLabelText("Password"), password);
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
}

/** 연결 → 지문 믿기 → VM 목록 */
async function connectAndTrust(dialog: HTMLElement) {
  await connect(dialog);
  await userEvent.click(await within(dialog).findByRole("button", { name: "Trust and continue" }));
  return within(dialog).findByRole("table", { name: "Virtual machines" });
}

it("connects, confirms the fingerprint, picks a VM and creates the import", async () => {
  mockLists({ "vm-import": [vmImport("db", 1)], node: [NODE] });
  const requests = mockEsxi();
  const { router } = renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  // 기본값이 채워져 있다.
  expect(within(dialog).getByLabelText("Port")).toHaveValue("443");
  expect(within(dialog).getByLabelText("Datacenter")).toHaveValue("ha-datacenter");
  // 브라우저가 저장된 비밀번호를 채우지 않는다.
  expect(within(dialog).getByLabelText("Password")).toHaveAttribute("autocomplete", "new-password");
  // 빈 칸은 보내지 않는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(within(dialog).getByText("Enter the ESXi or vCenter host.")).toBeVisible();
  expect(within(dialog).getByText("Enter the user name.")).toBeVisible();
  expect(requests).toEqual([]);

  // 지문 없이 연결하면 서버가 알려 준 지문을 보이고 믿을지 묻는다.
  await connect(dialog);
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  expect(requests[0]!.body).toEqual({
    host: "esxi1.lab",
    port: 443,
    user: "root",
    password: PASSWORD,
    datacenter: "ha-datacenter",
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Trust and continue" }));

  // VM 목록: 잠금 경고, 읽지 못한 VM은 고를 수 없다.
  const table = await within(dialog).findByRole("table", { name: "Virtual machines" });
  expect(requests[1]!.body).toEqual({
    host: "esxi1.lab",
    port: 443,
    user: "root",
    password: PASSWORD,
    datacenter: "ha-datacenter",
    fingerprint: FINGERPRINT,
  });
  // 라디오 이름에 데이터스토어, 설명에 잠금 경고와 읽지 못한 이유
  const web = within(table).getByRole("radio", { name: "Web Server 01 (datastore1)" });
  expect(web.closest("tr")).toHaveTextContent("datastore1Web Server 01ubuntu-6422 GiB1");
  expect(within(web.closest("tr")!).getByLabelText(/looks powered on/)).toBeInTheDocument();
  expect(web).toHaveAccessibleDescription(/looks powered on/);
  const broken = within(table).getByRole("radio", { name: "broken/broken.vmx (datastore1)" });
  expect(broken).toBeDisabled();
  expect(broken.closest("tr")).toHaveTextContent("ethernet0 has no networkName");
  expect(broken).toHaveAccessibleDescription("ethernet0 has no networkName");
  const db = within(table).getByRole("radio", { name: "db (datastore2)" });
  expect(db).toBeEnabled();
  expect(db).not.toHaveAccessibleDescription();

  // 고르지 않으면 넘어가지 않는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  expect(within(dialog).getByRole("alert")).toHaveTextContent("Choose a VM.");
  await userEvent.click(web);
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));

  // 이름은 VM 이름에서 이름 규칙으로
  const name = await within(dialog).findByLabelText("Name");
  expect(name).toHaveValue("web-server-01");
  expect(within(dialog).getByText("datastore1 / web01/web01.vmx")).toBeInTheDocument();
  await userEvent.clear(name);
  await userEvent.type(name, "esxi");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("The names ova and esxi are reserved.")).toBeVisible();
  await userEvent.clear(name);
  await userEvent.type(name, "web-server-01");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));

  await vi.waitFor(() => expect(router.state.location.pathname).toBe("/imports/web-server-01"));
  expect(requests).toHaveLength(3);
  expect(new URL(requests[2]!.url).pathname).toBe("/api/v1/vm-imports/esxi");
  expect(requests[2]!.body).toEqual({
    name: "web-server-01",
    connection: {
      host: "esxi1.lab",
      port: 443,
      user: "root",
      password: PASSWORD,
      datacenter: "ha-datacenter",
      fingerprint: FINGERPRINT,
    },
    datastore: "datastore1",
    vmx: "web01/web01.vmx",
  });
  // 비밀번호는 요청 본문에만 있다(주소·브라우저 저장소에 남기지 않는다).
  for (const r of requests) {
    expect(r.url).not.toContain(PASSWORD);
  }
  for (const storage of [localStorage, sessionStorage]) {
    expect(JSON.stringify({ ...storage })).not.toContain(PASSWORD);
  }
});

it("goes back to the VM list from the name step and keeps an edited name", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  mockEsxi();
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connectAndTrust(dialog);
  await userEvent.click(within(dialog).getByRole("radio", { name: "db (datastore2)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  const name = await within(dialog).findByLabelText("Name");
  expect(name).toHaveValue("db");
  await userEvent.type(name, "-prod");
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  expect(within(dialog).getByRole("radio", { name: "db (datastore2)" })).toBeChecked();
  // 같은 VM이면 고친 이름이 남는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  expect(await within(dialog).findByLabelText("Name")).toHaveValue("db-prod");
  // 다른 VM을 고르면 그 VM에서 다시 제안한다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  await userEvent.click(within(dialog).getByRole("radio", { name: "Web Server 01 (datastore1)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  expect(await within(dialog).findByLabelText("Name")).toHaveValue("web-server-01");
});

it("goes back to the connection step from the fingerprint without trusting it", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const requests = mockEsxi();
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connect(dialog);
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  // 연결 단계로 돌아가고 입력은 남는다. 믿지 않았으므로 다시 연결하면 지문을 다시 묻는다.
  expect(within(dialog).getByLabelText("Host")).toHaveValue("esxi1.lab");
  expect(within(dialog).queryByText(FINGERPRINT)).not.toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  expect(requests.map((r) => (r.body as { fingerprint?: string }).fingerprint)).toEqual([
    undefined,
    undefined,
  ]);
});

it("returns to the connection step on an auth error and reuses the trusted fingerprint", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const requests = mockEsxi();
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  // 처음엔 지문이 없어 untrusted → 믿으면 비밀번호가 틀려 esxi_auth → 연결 단계에 이유를 보인다.
  await connect(dialog, "wrong");
  await userEvent.click(await within(dialog).findByRole("button", { name: "Trust and continue" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "ESXi rejected the user name or password. Check them and try again.",
  );
  const password = within(dialog).getByLabelText("Password");
  // 비밀번호를 고쳐 다시 연결하면 믿은 지문을 함께 보낸다(다시 묻지 않는다).
  await userEvent.clear(password);
  await userEvent.type(password, PASSWORD);
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByRole("table", { name: "Virtual machines" })).toBeVisible();
  expect(requests.map((r) => (r.body as { fingerprint?: string }).fingerprint)).toEqual([
    undefined,
    FINGERPRINT,
    FINGERPRINT,
  ]);

  // 목록에서 뒤로 가면 연결 단계다. 호스트를 바꾸면 믿은 지문을 버린다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Back" }));
  const host = within(dialog).getByLabelText("Host");
  await userEvent.clear(host);
  await userEvent.type(host, "esxi2.lab");
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByText(FINGERPRINT)).toBeInTheDocument();
  expect(requests[3]!.body).toEqual({
    host: "esxi2.lab",
    port: 443,
    user: "root",
    password: PASSWORD,
    datacenter: "ha-datacenter",
  });
});

it("reconnects when the certificate changes before the import is created", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const requests = mockEsxi(() =>
    HttpResponse.json(
      {
        code: "fingerprint_mismatch",
        message: "certificate fingerprint mismatch",
        details: { fingerprint: "AA:BB" },
      },
      { status: 409 },
    ),
  );
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connectAndTrust(dialog);
  await userEvent.click(within(dialog).getByRole("radio", { name: "db (datastore2)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  await userEvent.click(await within(dialog).findByRole("button", { name: "Create" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The ESXi certificate changed since you trusted it. Connect again and check the new fingerprint.",
  );
  expect(within(dialog).getByLabelText("Host")).toHaveValue("esxi1.lab");
  // 믿은 지문을 버렸으므로 다시 연결하면 새 지문을 묻는다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
  expect(await within(dialog).findByRole("button", { name: "Trust and continue" })).toBeVisible();
  expect((requests.at(-1)!.body as { fingerprint?: string }).fingerprint).toBeUndefined();
});

it("shows a taken name on the name field", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  mockEsxi(() =>
    HttpResponse.json(
      { code: "name_taken", message: 'vm-import "db" already exists' },
      { status: 409 },
    ),
  );
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connectAndTrust(dialog);
  await userEvent.click(within(dialog).getByRole("radio", { name: "db (datastore2)" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Next" }));
  await userEvent.click(await within(dialog).findByRole("button", { name: "Create" }));
  const name = within(dialog).getByLabelText("Name");
  await vi.waitFor(() =>
    expect(name).toHaveAccessibleDescription("An import with this name already exists."),
  );
  expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
});

it("says why the connection failed", async () => {
  mockLists({ "vm-import": [], node: [NODE] });
  const failures = [
    {
      status: 400,
      code: "esxi_auth",
      message: "ESXi rejected the user name or password",
      text: "ESXi rejected the user name or password. Check them and try again.",
    },
    {
      status: 502,
      code: "esxi_unreachable",
      message: "cannot connect to esxi1.lab:443: connection refused",
      text: "Cannot reach the ESXi host (cannot connect to esxi1.lab:443: connection refused).",
    },
    {
      status: 409,
      code: "fingerprint_mismatch",
      message: "certificate fingerprint mismatch",
      text: "The ESXi certificate changed since you trusted it. Connect again and check the new fingerprint.",
    },
  ];
  let next = 0;
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
    http.post(apiUrl("/vm-imports/esxi/browse"), () => {
      const f = failures[next++]!;
      return HttpResponse.json(
        { code: f.code, message: f.message, details: { fingerprint: FINGERPRINT } },
        { status: f.status },
      );
    }),
  );
  renderPage(<ImportsPage />, { path: "/imports" });
  const dialog = await openDialog();
  await connect(dialog);
  for (const f of failures) {
    if (f !== failures[0]) {
      await userEvent.click(within(dialog).getByRole("button", { name: "Connect" }));
    }
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(f.text);
    // 연결 칸은 그대로 남아 고쳐서 다시 연결할 수 있다.
    expect(within(dialog).getByLabelText("Password")).toHaveValue(PASSWORD);
  }
});

it("hides the ESXi import from viewers", async () => {
  mockLists({ "vm-import": [vmImport("web01", 1)] });
  renderPage(<ImportsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("web01")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Import from ESXi" })).not.toBeInTheDocument();
});

it("shows the ESXi source and explains lost credentials", async () => {
  const lost = esxiImport("web01", 1, {
    phase: "failed",
    error:
      "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again",
  });
  mockLists({ "vm-import": [lost], node: [NODE] });
  renderPage(<ImportsPage />);
  const table = await screen.findByRole("table", { name: "Imports" });
  const row = (await within(table).findByText("web01")).closest("tr")!;
  expect(row).toHaveTextContent("Web 01esxi1.lab / datastore1 / web01/web01.vmx");
  expect(row).toHaveTextContent(
    "The ESXi password is no longer on this node (orbivisord restarted or the request was interrupted). Delete this import and create it again.",
  );
});
