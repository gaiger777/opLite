import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { QueryClient } from "@tanstack/react-query";
import { http, HttpResponse } from "msw";

import { SETTINGS_KEY } from "@/api/live";

import { SettingsPage } from "@/settings/SettingsPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

// 아직 저장한 적 없는 설정: 서버가 기본값을 revision 0으로 돌려준다.
const DEFAULT = {
  id: "00000000-0000-0000-0000-000000000000",
  name: "cluster",
  revision: 0,
  labels: {},
  spec: { updates: { channel: "stable" } },
  status: {},
};

it("changes the update channel with the revision it read", async () => {
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { updates: { channel: string } };
      sent.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({ ...DEFAULT, revision: 12, spec: { updates: body.updates } });
    }),
  );
  renderPage(<SettingsPage />);
  const channel = await screen.findByLabelText("Update channel");
  expect(channel).toHaveValue("stable");
  const save = screen.getByRole("button", { name: "Save" });
  // 바뀐 것이 없으면 보낼 것도 없다.
  expect(save).toBeDisabled();
  await userEvent.selectOptions(channel, "testing");
  await userEvent.click(save);
  expect(await screen.findByText("Settings saved.")).toBeInTheDocument();
  expect(sent).toEqual([{ ifMatch: "0", body: { updates: { channel: "testing" } } }]);
  // 응답을 바로 보여 준다.
  expect(screen.getByLabelText("Update channel")).toHaveValue("testing");
  expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
});

it("keeps newer settings that arrived while saving", async () => {
  const page: { queryClient?: QueryClient } = {};
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () => {
      // 응답 전에 watch가 더 새 설정(다른 admin이 stable로 되돌림)을 넣었다.
      page.queryClient!.setQueryData(SETTINGS_KEY, { ...DEFAULT, revision: 20 });
      return HttpResponse.json({
        ...DEFAULT,
        revision: 12,
        spec: { updates: { channel: "testing" } },
      });
    }),
  );
  page.queryClient = renderPage(<SettingsPage />).queryClient;
  const channel = await screen.findByLabelText("Update channel");
  await userEvent.selectOptions(channel, "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(channel).toHaveValue("stable"));
  expect(page.queryClient.getQueryData(SETTINGS_KEY)).toMatchObject({ revision: 20 });
});

it("says why the settings could not be saved", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json({ code: "revision_mismatch", message: "stale" }, { status: 412 }),
    ),
  );
  renderPage(<SettingsPage />);
  await userEvent.selectOptions(await screen.findByLabelText("Update channel"), "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Someone else changed it first. Reload and try again.",
  );
});

it("shows the settings read-only to everyone but admins", async () => {
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({ ...DEFAULT, revision: 3, spec: { updates: { channel: "testing" } } }),
    ),
  );
  renderPage(<SettingsPage />, { as: { ...ADMIN, role: "viewer" } });
  const channel = await screen.findByLabelText("Update channel");
  expect(channel).toHaveValue("testing");
  expect(channel).toBeDisabled();
  expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
  expect(screen.getByText("Only administrators can change the settings.")).toBeInTheDocument();
});

const ACTIVE = {
  state: "active",
  id: "sub-1",
  customer: "Example Corp",
  level: "premium",
  nodes: 4,
  used_nodes: 3,
  expires_at: 4102444800,
};

it.each([
  [
    { ...ACTIVE, development: true },
    ["Active", "Development key", "Example Corp", "Premium", "3 / 4"],
  ],
  [{ ...ACTIVE, state: "expired", expires_at: 1767225600 }, ["Expired", "Example Corp"]],
  [{ ...ACTIVE, state: "over_limit", used_nodes: 5 }, ["Over the node limit", "5 / 4"]],
  [{ state: "none", used_nodes: 2 }, ["No subscription", "2"]],
  [{ state: "invalid", used_nodes: 2 }, ["Invalid key"]],
])("shows the subscription state %#", async (info, texts) => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/subscription"), () => HttpResponse.json(info)),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  for (const text of texts) {
    expect(await within(section).findByText(text)).toBeInTheDocument();
  }
  if (info.state === "none" || info.state === "invalid") {
    expect(within(section).queryByText("Customer")).not.toBeInTheDocument();
    expect(within(section).queryByText("Expires")).not.toBeInTheDocument();
  } else {
    expect(within(section).getByText("Expires")).toBeInTheDocument();
  }
  expect(within(section).queryByText("Development key") !== null).toBe("development" in info);
});

it("applies and removes a subscription key", async () => {
  const sent: { method: string; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 3 })),
    http.put(apiUrl("/subscription"), async ({ request }) => {
      sent.push({ method: "PUT", body: await request.json() });
      return HttpResponse.json(ACTIVE);
    }),
    http.delete(apiUrl("/subscription"), () => {
      sent.push({ method: "DELETE", body: null });
      return HttpResponse.json({ state: "none", used_nodes: 3 });
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  expect(await within(section).findByText("No subscription")).toBeInTheDocument();
  // 지울 키가 없다.
  expect(within(section).queryByRole("button", { name: "Remove key" })).not.toBeInTheDocument();
  const apply = within(section).getByRole("button", { name: "Apply key" });
  expect(apply).toBeDisabled();
  await userEvent.type(within(section).getByLabelText("Subscription key"), "  opl1.abc.def\n");
  await userEvent.click(apply);
  expect(await screen.findByText("Subscription key applied.")).toBeInTheDocument();
  expect(sent).toEqual([{ method: "PUT", body: { key: "opl1.abc.def" } }]);
  expect(within(section).getByText("Active")).toBeInTheDocument();
  expect(within(section).getByLabelText("Subscription key")).toHaveValue("");

  await userEvent.click(within(section).getByRole("button", { name: "Remove key" }));
  const dialog = await screen.findByRole("alertdialog");
  expect(dialog).toHaveTextContent("Remove the subscription key?");
  // 확인하기 전에는 지우지 않는다.
  expect(sent).toHaveLength(1);
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  expect(await screen.findByText("Subscription key removed.")).toBeInTheDocument();
  expect(sent[1]).toEqual({ method: "DELETE", body: null });
  expect(await within(section).findByText("No subscription")).toBeInTheDocument();
  await vi.waitFor(() =>
    expect(within(section).getByRole("heading", { name: "Subscription" })).toHaveFocus(),
  );
});

it("says why a subscription key was rejected", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.put(apiUrl("/subscription"), () =>
      HttpResponse.json(
        {
          code: "invalid_subscription_key",
          message: "the subscription key signature does not match this build",
        },
        { status: 400 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  await userEvent.type(within(section).getByLabelText("Subscription key"), "opl1.x.y");
  await userEvent.click(within(section).getByRole("button", { name: "Apply key" }));
  expect(await within(section).findByRole("alert")).toHaveTextContent(
    "The subscription key is not valid: the subscription key signature does not match this build",
  );
});

it("lets admins pick enterprise only with an active subscription", async () => {
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ ...ACTIVE, state: "expired" })),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { updates: { channel: string } };
      sent.push(body);
      return HttpResponse.json({ ...DEFAULT, revision: 2, spec: { updates: body.updates } });
    }),
  );
  const { unmount } = renderPage(<SettingsPage />);
  await screen.findByText("Expired");
  expect(screen.getByRole("option", { name: "Enterprise" })).toBeDisabled();
  expect(screen.getByLabelText("Update channel")).toHaveAccessibleDescription(
    /Enterprise needs an active subscription\./,
  );
  unmount();

  server.use(http.get(apiUrl("/subscription"), () => HttpResponse.json(ACTIVE)));
  renderPage(<SettingsPage />);
  await screen.findByText("Active");
  expect(screen.getByRole("option", { name: "Enterprise" })).toBeEnabled();
  expect(screen.getByLabelText("Update channel")).not.toHaveAccessibleDescription(
    /Enterprise needs/,
  );
  await userEvent.selectOptions(screen.getByLabelText("Update channel"), "enterprise");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(sent).toEqual([{ updates: { channel: "enterprise" } }]));
});

it("explains when the enterprise channel needs a subscription", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "subscription_required", message: "an active subscription is required" },
        { status: 400 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  await screen.findByText("Active");
  await userEvent.selectOptions(screen.getByLabelText("Update channel"), "enterprise");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "An active subscription is required for the enterprise channel.",
  );
});

const REPORT = {
  schema: 1,
  id: "00000000-0000-0000-0000-000000000000",
  sent_at: 1767225600,
  versions: { "0.9.0": 2 },
  nodes: { total: 2, witness: 0 },
  arch: { x86_64: 2 },
  hypervisor: { kvm: 2 },
  cpus: 16,
  memory_gib: 64,
  vms: { total: 3, running: 2, ha: 1 },
  pools: { dir: 1 },
  networks: { bridge: 1 },
  security_groups: 0,
  backup_jobs: 0,
  imports: {},
  subscription: "premium",
};

it("shows the report before turning telemetry on", async () => {
  const patches: unknown[] = [];
  let previews = 0;
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () => {
      previews += 1;
      return HttpResponse.json(REPORT);
    }),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { telemetry: { enabled: boolean } };
      patches.push({ ifMatch: request.headers.get("If-Match"), body });
      return HttpResponse.json({
        ...DEFAULT,
        revision: 5,
        spec: { ...DEFAULT.spec, telemetry: body.telemetry },
        status: { telemetry: { id: "0a1b2c3d-0000-4000-8000-000000000001" } },
      });
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  expect(within(section).getByText("Off")).toBeInTheDocument();
  expect(
    within(section).getByText("Not set (the build's default endpoint is used if it has one)"),
  ).toBeInTheDocument();
  expect(within(section).getByText("Never")).toBeInTheDocument();

  // 처음에는 취소한다: 켜지 않는다.
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  let dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  expect(dialog).toHaveTextContent('"memory_gib": 64');
  await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(patches).toEqual([]);

  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Turn on" }));
  expect(await screen.findByText("Telemetry turned on.")).toBeInTheDocument();
  expect(previews).toBe(2);
  expect(patches).toEqual([{ ifMatch: "0", body: { telemetry: { enabled: true } } }]);
  expect(within(section).getByText("On")).toBeInTheDocument();
  expect(within(section).getByText("0a1b2c3d-0000-4000-8000-000000000001")).toBeInTheDocument();
  expect(within(section).getByRole("button", { name: "Turn off" })).toBeInTheDocument();
  await vi.waitFor(() =>
    expect(within(section).getByRole("heading", { name: "Telemetry" })).toHaveFocus(),
  );
});

it("turns telemetry off, sets the endpoint and makes a new report id", async () => {
  const sent: unknown[] = [];
  let settings = {
    ...DEFAULT,
    revision: 4,
    spec: { ...DEFAULT.spec, telemetry: { enabled: true, endpoint: "https://t.example/r" } },
    status: {
      telemetry: {
        id: "0a1b2c3d-0000-4000-8000-000000000001",
        last_sent_at: 1767225600,
        last_error: "HTTP 503",
      },
    },
  };
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(settings)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as {
        telemetry: { enabled?: boolean; endpoint?: string };
      };
      sent.push(body);
      settings = {
        ...settings,
        revision: settings.revision + 1,
        spec: { ...settings.spec, telemetry: { ...settings.spec.telemetry, ...body.telemetry } },
      };
      return HttpResponse.json(settings);
    }),
    http.post(apiUrl("/telemetry/reset-id"), () => {
      sent.push("reset-id");
      settings = {
        ...settings,
        revision: settings.revision + 1,
        status: {
          telemetry: { ...settings.status.telemetry, id: "0a1b2c3d-0000-4000-8000-000000000002" },
        },
      };
      return HttpResponse.json(settings);
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  expect(within(section).getByText("On")).toBeInTheDocument();
  expect(within(section).getByText("https://t.example/r")).toBeInTheDocument();
  expect(within(section).getByText("HTTP 503")).toBeInTheDocument();
  expect(within(section).queryByText("Never")).not.toBeInTheDocument();

  await userEvent.click(within(section).getByRole("button", { name: "Turn off" }));
  expect(await screen.findByText("Telemetry turned off.")).toBeInTheDocument();
  expect(within(section).getByText("Off")).toBeInTheDocument();
  // 누른 버튼이 사라졌으므로 포커스는 그 부분의 제목으로 간다.
  expect(within(section).getByRole("heading", { name: "Telemetry" })).toHaveFocus();

  const endpoint = within(section).getByLabelText("Endpoint");
  expect(endpoint).toHaveValue("https://t.example/r");
  await userEvent.clear(endpoint);
  await userEvent.click(within(section).getByRole("button", { name: "Save endpoint" }));
  await vi.waitFor(() =>
    expect(
      within(section).getByText("Not set (the build's default endpoint is used if it has one)"),
    ).toBeInTheDocument(),
  );

  await userEvent.click(within(section).getByRole("button", { name: "New report ID" }));
  expect(
    await within(section).findByText("0a1b2c3d-0000-4000-8000-000000000002"),
  ).toBeInTheDocument();
  expect(sent).toEqual([
    { telemetry: { enabled: false } },
    { telemetry: { endpoint: "" } },
    "reset-id",
  ]);
});

it("says when telemetry needs an endpoint", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () => HttpResponse.json(REPORT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "telemetry_endpoint_required", message: "set telemetry.endpoint" },
        { status: 400 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  const dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Turn on" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Set a telemetry endpoint first. This build has no default endpoint.",
  );
});

it("shows subscription and telemetry read-only to everyone but admins", async () => {
  server.use(
    http.get(apiUrl("/settings"), () =>
      HttpResponse.json({
        ...DEFAULT,
        spec: { ...DEFAULT.spec, telemetry: { enabled: true } },
        status: { telemetry: { id: "0a1b2c3d-0000-4000-8000-000000000001" } },
      }),
    ),
  );
  renderPage(<SettingsPage />, { as: { ...ADMIN, role: "operator" } });
  const subscription = await screen.findByRole("region", { name: "Subscription" });
  expect(await within(subscription).findByText("Active")).toBeInTheDocument();
  expect(within(subscription).queryByLabelText("Subscription key")).not.toBeInTheDocument();
  expect(within(subscription).queryByRole("button")).not.toBeInTheDocument();
  const telemetry = screen.getByRole("region", { name: "Telemetry" });
  expect(within(telemetry).getByText("On")).toBeInTheDocument();
  expect(within(telemetry).queryByLabelText("Endpoint")).not.toBeInTheDocument();
  expect(within(telemetry).queryByRole("button")).not.toBeInTheDocument();
});

it("sends the endpoint typed but not saved when turning telemetry on", async () => {
  const patches: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () => HttpResponse.json(REPORT)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { telemetry: object };
      patches.push(body);
      return HttpResponse.json({
        ...DEFAULT,
        revision: 5,
        spec: { ...DEFAULT.spec, telemetry: body.telemetry },
      });
    }),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  await userEvent.type(within(section).getByLabelText("Endpoint"), " https://t.example/r ");
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  const dialog = await screen.findByRole("dialog", { name: "Turn on telemetry" });
  await userEvent.click(within(dialog).getByRole("button", { name: "Turn on" }));
  await vi.waitFor(() =>
    expect(patches).toEqual([{ telemetry: { enabled: true, endpoint: "https://t.example/r" } }]),
  );
  expect(await within(section).findByText("https://t.example/r")).toBeInTheDocument();
});

it("reads the settings again after the subscription key changes", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/settings"), () => {
      reads += 1;
      return HttpResponse.json(DEFAULT);
    }),
    http.get(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 1 })),
    http.put(apiUrl("/subscription"), () => HttpResponse.json(ACTIVE)),
    http.delete(apiUrl("/subscription"), () => HttpResponse.json({ state: "none", used_nodes: 1 })),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  await within(section).findByText("No subscription");
  expect(reads).toBe(1);
  await userEvent.type(within(section).getByLabelText("Subscription key"), "opl1.a.b");
  await userEvent.click(within(section).getByRole("button", { name: "Apply key" }));
  await vi.waitFor(() => expect(reads).toBe(2));

  await userEvent.click(within(section).getByRole("button", { name: "Remove key" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  await vi.waitFor(() => expect(reads).toBe(3));
});

it("says when another settings change is in progress", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "busy", message: "the settings are being changed; try again" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  await userEvent.selectOptions(await screen.findByLabelText("Update channel"), "testing");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Another settings change is in progress. Try again.",
  );
});

it("shows why the subscription key could not be removed", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.delete(apiUrl("/subscription"), () =>
      HttpResponse.json(
        {
          code: "cluster_upgrading",
          message:
            "some nodes run an older Orbivisor (node2); finish the upgrade on every node first",
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Subscription" });
  await userEvent.click(await within(section).findByRole("button", { name: "Remove key" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Remove key" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The cluster is being upgraded: some nodes run an older Orbivisor (node2)",
  );
  // 지우지 못했으니 그대로다.
  expect(within(section).getByText("Active")).toBeInTheDocument();
});

it("says why the telemetry report could not be shown", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.get(apiUrl("/telemetry/preview"), () =>
      HttpResponse.json({ code: "forbidden", message: "admin only" }, { status: 403 }),
    ),
  );
  renderPage(<SettingsPage />);
  const section = await screen.findByRole("region", { name: "Telemetry" });
  await userEvent.click(within(section).getByRole("button", { name: "Turn on…" }));
  expect(await within(section).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("changes the shutdown policy and shows the default when none is saved", async () => {
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), async ({ request }) => {
      const body = (await request.json()) as { shutdown_policy: string };
      sent.push(body);
      return HttpResponse.json({
        ...DEFAULT,
        revision: 4,
        spec: { ...DEFAULT.spec, shutdown_policy: body.shutdown_policy },
      });
    }),
  );
  renderPage(<SettingsPage />);
  const policy = await screen.findByLabelText("Policy");
  // 기본(conditional)은 저장하지 않으므로 없으면 기본이다.
  expect(policy).toHaveValue("conditional");
  expect(screen.getByText("Freeze on reboot, fail over on power off.")).toBeInTheDocument();
  const save = screen.getByRole("button", { name: "Save policy" });
  expect(save).toBeDisabled();
  await userEvent.selectOptions(policy, "migrate");
  await userEvent.click(save);
  await vi.waitFor(() => expect(sent).toEqual([{ shutdown_policy: "migrate" }]));
  // 응답을 바로 보여 준다.
  await vi.waitFor(() =>
    expect(screen.getByRole("button", { name: "Save policy" })).toBeDisabled(),
  );
  expect(screen.getByLabelText("Policy")).toHaveValue("migrate");
});

it("says the cluster is upgrading when old nodes do not know shutdown policies", async () => {
  server.use(
    http.get(apiUrl("/settings"), () => HttpResponse.json(DEFAULT)),
    http.patch(apiUrl("/settings"), () =>
      HttpResponse.json(
        { code: "cluster_upgrading", message: "node n2 does not support ha-placement yet" },
        { status: 409 },
      ),
    ),
  );
  renderPage(<SettingsPage />);
  await userEvent.selectOptions(await screen.findByLabelText("Policy"), "freeze");
  await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "The cluster is being upgraded: node n2 does not support ha-placement yet",
  );
});
