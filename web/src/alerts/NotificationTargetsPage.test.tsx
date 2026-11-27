import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { NotificationTargetView } from "@/alerts/alerts";
import { NotificationTargetsPage } from "@/alerts/NotificationTargetsPage";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const MAIL: NotificationTargetView = {
  id: "t1",
  name: "ops-mail",
  revision: 4,
  enabled: true,
  target: {
    kind: "smtp",
    host: "mail.example.com",
    port: 587,
    tls: "starttls",
    user: "alerts",
    has_password: true,
    from: "orbivisor@example.com",
    to: ["ops@example.com"],
  },
};

const SLACK: NotificationTargetView = {
  id: "t2",
  name: "chat",
  revision: 2,
  enabled: false,
  target: {
    kind: "webhook",
    url: "https://hooks.slack.com/…",
    format: "slack",
    header_names: ["X-Team"],
    has_token: false,
  },
};

const at = { path: "/settings/notifications" };

function list(items: NotificationTargetView[] = [MAIL, SLACK]) {
  return http.get(apiUrl("/notification-targets"), () => HttpResponse.json({ revision: 1, items }));
}

it("lists targets without their secrets", async () => {
  server.use(list());
  renderPage(<NotificationTargetsPage />, at);
  const table = await screen.findByRole("table", { name: "Notification targets" });
  const mail = await within(table).findByRole("row", { name: /ops-mail/ });
  expect(mail).toHaveTextContent("Email (SMTP)mail.example.com:587 → ops@example.comOn");
  const chat = within(table).getByRole("row", { name: /chat/ });
  expect(chat).toHaveTextContent("Slackhttps://hooks.slack.com/…Off");
});

it("sends a test notification and says how it went", async () => {
  let fail = false;
  server.use(
    list(),
    http.post(apiUrl("/notification-targets/:key/test"), ({ params }) => {
      expect(params.key).toBe("t1");
      return HttpResponse.json(
        fail ? { ok: false, error: "mail.example.com:587: connection refused" } : { ok: true },
      );
    }),
  );
  const user = userEvent.setup();
  renderPage(<NotificationTargetsPage />, at);
  const test = await screen.findByRole("button", { name: "Send a test notification to ops-mail" });
  await user.click(test);
  expect(await screen.findByText("Test notification sent to ops-mail.")).toBeInTheDocument();
  fail = true;
  await user.click(test);
  expect(
    await screen.findByText(
      "Test notification to ops-mail failed: mail.example.com:587: connection refused",
    ),
  ).toBeInTheDocument();
});

it("keeps saved secrets when editing", async () => {
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    list(),
    http.put(apiUrl("/notification-targets/:key"), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json(SLACK);
    }),
  );
  const user = userEvent.setup();
  renderPage(<NotificationTargetsPage />, at);
  await user.click(await screen.findByRole("button", { name: "Edit chat" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit notification target chat" });
  // 줄인 URL과 저장된 헤더 이름으로 시작한다. 값은 비어 있고 저장된 값을 지킨다.
  expect(within(dialog).getByLabelText("URL")).toHaveValue("https://hooks.slack.com/…");
  expect(within(dialog).getByLabelText("Header 1 name")).toHaveValue("X-Team");
  expect(within(dialog).getByLabelText("Header 1 value")).toHaveValue("");
  await user.click(within(dialog).getByRole("checkbox", { name: "Send notifications" }));
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Notification target chat saved.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      ifMatch: "2",
      body: {
        enabled: true,
        target: {
          kind: "webhook",
          url: "https://hooks.slack.com/…",
          format: "slack",
          headers: { "X-Team": "" },
        },
      },
    },
  ]);
});

it("asks for the password again when the mail server changes", async () => {
  const sent: unknown[] = [];
  server.use(
    list(),
    http.put(apiUrl("/notification-targets/:key"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(MAIL);
    }),
  );
  const user = userEvent.setup();
  renderPage(<NotificationTargetsPage />, at);
  await user.click(await screen.findByRole("button", { name: "Edit ops-mail" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit notification target ops-mail" });
  expect(within(dialog).getByLabelText("Password")).toHaveAccessibleDescription(
    "A value is saved. Leave empty to keep it.",
  );
  const host = within(dialog).getByLabelText("SMTP server");
  await user.clear(host);
  await user.type(host, "smtp.other.example");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText(
      "The server, port, user or TLS changed. Enter the password again or remove it.",
    ),
  ).toBeInTheDocument();
  expect(sent).toEqual([]);
  await user.type(within(dialog).getByLabelText("Password"), "n3w");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Notification target ops-mail saved.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      enabled: true,
      target: {
        kind: "smtp",
        host: "smtp.other.example",
        port: 587,
        tls: "starttls",
        user: "alerts",
        password: "n3w",
        from: "orbivisor@example.com",
        to: ["ops@example.com"],
      },
    },
  ]);
});

it("creates an ntfy target with a token", async () => {
  const sent: unknown[] = [];
  server.use(
    list([]),
    http.post(apiUrl("/notification-targets"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(SLACK, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<NotificationTargetsPage />, at);
  // 빈 목록은 빈 상태(설명·만들기·자세히)로 보인다.
  const empty = (await screen.findByText(/No notification targets/)).closest<HTMLElement>(
    "[data-slot=empty-state]",
  )!;
  expect(empty).toHaveTextContent("Create one place to receive them.");
  expect(within(empty).getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  await user.click(within(empty).getByRole("button", { name: "Create notification target" }));
  const dialog = await screen.findByRole("dialog", { name: "Create notification target" });
  await user.type(within(dialog).getByLabelText("Name"), "phone");
  await user.type(within(dialog).getByLabelText("URL"), "https://ntfy.sh/orbivisor");
  await user.selectOptions(within(dialog).getByLabelText("Format"), "ntfy");
  await user.type(within(dialog).getByLabelText("Token"), "tk_1");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Notification target phone created.")).toBeInTheDocument();
  expect(sent).toEqual([
    {
      name: "phone",
      enabled: true,
      target: { kind: "webhook", url: "https://ntfy.sh/orbivisor", format: "ntfy", token: "tk_1" },
    },
  ]);
});

it("shows why a target in use cannot be deleted", async () => {
  server.use(
    list(),
    http.delete(apiUrl("/notification-targets/:key"), () =>
      HttpResponse.json({ code: "in_use", message: "rules use it: cpu-hot" }, { status: 409 }),
    ),
  );
  const user = userEvent.setup();
  renderPage(<NotificationTargetsPage />, at);
  await user.click(await screen.findByRole("button", { name: "Delete ops-mail" }));
  const confirm = await screen.findByRole("alertdialog");
  await user.click(within(confirm).getByRole("button", { name: "Delete" }));
  expect(
    await within(confirm).findByText("It is still in use: rules use it: cpu-hot"),
  ).toBeInTheDocument();
});

it("is for administrators only", async () => {
  renderPage(<NotificationTargetsPage />, {
    ...at,
    as: { user: "00000000-0000-0000-0000-000000000003", name: "op", role: "operator" },
  });
  expect(
    await screen.findByText("Only administrators can manage notification targets."),
  ).toBeInTheDocument();
});
