import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { RealmsPage } from "@/realms/RealmsPage";
import { ADMIN, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const CORP = {
  id: "00000000-0000-0000-0000-0000000000c1",
  name: "corp",
  revision: 7,
  display: "Corp directory",
  backend: {
    kind: "ldap",
    url: "ldaps://ldap.corp.example",
    base_dn: "dc=corp,dc=example",
    bind_dn: "cn=svc,dc=corp,dc=example",
    has_bind_password: true,
    user_filter: "(&(objectClass=person)(uid={username}))",
    group_attr: "memberOf",
    timeout_secs: 10,
  },
  roles: {
    groups: [{ group: "cn=ops,ou=groups,dc=corp,dc=example", role: "operator" }],
    default: "viewer",
  },
};

const LAB = {
  ...CORP,
  id: "00000000-0000-0000-0000-0000000000c2",
  name: "lab",
  revision: 3,
  display: undefined,
  backend: { ...CORP.backend, url: "ldap://10.0.0.5:389", has_bind_password: false },
  roles: { groups: [] },
};

/** 영역 목록. 돌려받은 함수로 다음 읽기의 목록을 바꾼다. */
function realms(list: unknown[]) {
  let items = list;
  server.use(http.get(apiUrl("/realms"), () => HttpResponse.json({ revision: 1, items })));
  return (next: unknown[]) => {
    items = next;
  };
}

it("lists realms by name", async () => {
  realms([LAB, CORP]);
  renderPage(<RealmsPage />);
  const table = await screen.findByRole("table", { name: "Realms" });
  await within(table).findByText("corp");
  const rows = within(table).getAllByRole("row");
  expect(
    rows.map((r) =>
      within(r)
        .queryAllByRole("cell")
        .map((c) => c.textContent),
    ),
  ).toEqual([
    [],
    ["corp", "Corp directory", "LDAP", "ldaps://ldap.corp.example", expect.any(String)],
    ["lab", "—", "LDAP", "ldap://10.0.0.5:389", expect.any(String)],
  ]);
});

it("tells other roles that only admins manage realms", async () => {
  let reads = 0;
  server.use(
    http.get(apiUrl("/realms"), () => {
      reads += 1;
      return HttpResponse.json({ revision: 1, items: [CORP] });
    }),
  );
  renderPage(<RealmsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("Only administrators can manage realms.")).toBeInTheDocument();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(reads).toBe(0);
});

it("creates an LDAP realm with a group mapping", async () => {
  const change = realms([CORP]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/realms"), async ({ request }) => {
      body = await request.json();
      change([CORP, LAB]);
      return HttpResponse.json(LAB, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await screen.findByText("corp");
  await user.click(screen.getByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create an LDAP realm" });
  // 서버의 기본값으로 시작한다.
  expect(within(dialog).getByLabelText("User filter")).toHaveValue(
    "(&(objectClass=person)(uid={username}))",
  );
  expect(within(dialog).getByLabelText("Group attribute")).toHaveValue("memberOf");
  expect(within(dialog).getByLabelText("Timeout (seconds)")).toHaveValue("10");

  // `local`은 예약된 이름이고, 주소와 base DN이 있어야 하며, 시간 제한은 30초까지다.
  await user.type(within(dialog).getByLabelText("Name"), "local");
  await user.clear(within(dialog).getByLabelText("Timeout (seconds)"));
  await user.type(within(dialog).getByLabelText("Timeout (seconds)"), "31");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("local is reserved for local users.")).toBeVisible();
  expect(within(dialog).getByText("Use a whole number of seconds from 1 to 30.")).toBeVisible();
  expect(
    within(dialog).getByText("Use ldap://host[:port] (with StartTLS) or ldaps://host[:port]."),
  ).toBeVisible();
  expect(body).toBeUndefined();

  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "lab");
  await user.type(within(dialog).getByLabelText("Display name"), "Lab LDAP");
  await user.type(within(dialog).getByLabelText("URL"), "ldap://10.0.0.5:389");
  await user.type(within(dialog).getByLabelText("Base DN"), "dc=lab,dc=example");
  await user.type(within(dialog).getByLabelText("Bind DN"), "cn=svc,dc=lab,dc=example");
  await user.type(within(dialog).getByLabelText("Bind password"), "svc-secret");
  await user.type(within(dialog).getByLabelText("CA certificate (PEM)"), "-----BEGIN CERT");
  await user.clear(within(dialog).getByLabelText("Timeout (seconds)"));
  await user.type(within(dialog).getByLabelText("Timeout (seconds)"), "5");
  // 그룹 셋을 더하고 가운데 것을 뺀다. 빈 그룹 행의 오류는 입력의 설명이다.
  for (let i = 0; i < 3; i++) {
    await user.click(within(dialog).getByRole("button", { name: "Add group" }));
  }
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Group 1")).toHaveAccessibleDescription(
    "Enter a group DN or remove the row.",
  );
  expect(within(dialog).getByLabelText("Group 1")).toBeInvalid();
  expect(body).toBeUndefined();
  await user.type(within(dialog).getByLabelText("Group 1"), "cn=admins,dc=lab,dc=example");
  await user.selectOptions(within(dialog).getByLabelText("Role of group 1"), "admin");
  await user.type(within(dialog).getByLabelText("Group 2"), "cn=gone,dc=lab,dc=example");
  await user.type(within(dialog).getByLabelText("Group 3"), "cn=ops,dc=lab,dc=example");
  await user.selectOptions(within(dialog).getByLabelText("Role of group 3"), "operator");
  await user.click(within(dialog).getByRole("button", { name: "Remove group 2" }));
  await user.selectOptions(within(dialog).getByLabelText("Default role"), "viewer");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));

  expect(await screen.findByText("Created realm lab.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "lab",
    display: "Lab LDAP",
    backend: {
      kind: "ldap",
      url: "ldap://10.0.0.5:389",
      base_dn: "dc=lab,dc=example",
      bind_dn: "cn=svc,dc=lab,dc=example",
      bind_password: "svc-secret",
      user_filter: "(&(objectClass=person)(uid={username}))",
      group_attr: "memberOf",
      ca_pem: "-----BEGIN CERT",
      timeout_secs: 5,
    },
    roles: {
      groups: [
        { group: "cn=admins,dc=lab,dc=example", role: "admin" },
        { group: "cn=ops,dc=lab,dc=example", role: "operator" },
      ],
      default: "viewer",
    },
  });
  expect(await screen.findByText("lab")).toBeInTheDocument();
});

it("keeps, replaces or clears the bind password when editing", async () => {
  realms([CORP]);
  const sent: { ifMatch: string | null; body: { backend: Record<string, unknown> } }[] = [];
  server.use(
    http.put(apiUrl("/realms/corp"), async ({ request }) => {
      sent.push({
        ifMatch: request.headers.get("If-Match"),
        body: (await request.json()) as { backend: Record<string, unknown> },
      });
      return HttpResponse.json(CORP);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);

  async function edit() {
    await user.click(await screen.findByRole("button", { name: "Edit corp" }));
    return screen.findByRole("dialog", { name: "Edit realm corp" });
  }
  async function save(dialog: HTMLElement) {
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }

  // 비워 두면 보내지 않아 저장된 비밀번호가 남는다.
  let dialog = await edit();
  expect(within(dialog).getByLabelText("Name")).toHaveValue("corp");
  expect(within(dialog).getByLabelText("Name")).toHaveAttribute("readonly");
  expect(within(dialog).getByLabelText("Bind password")).toHaveValue("");
  expect(
    within(dialog).getByText("A password is saved. Leave this empty to keep it."),
  ).toBeVisible();
  expect(within(dialog).getByLabelText("Group 1")).toHaveValue(
    "cn=ops,ou=groups,dc=corp,dc=example",
  );
  expect(within(dialog).getByLabelText("Default role")).toHaveValue("viewer");
  await user.clear(within(dialog).getByLabelText("Display name"));
  await save(dialog);
  expect(await screen.findByText("Saved realm corp.")).toBeInTheDocument();
  expect(sent[0]!.ifMatch).toBe("7");
  expect(sent[0]!.body).toEqual({
    backend: {
      kind: "ldap",
      url: "ldaps://ldap.corp.example",
      base_dn: "dc=corp,dc=example",
      bind_dn: "cn=svc,dc=corp,dc=example",
      user_filter: "(&(objectClass=person)(uid={username}))",
      group_attr: "memberOf",
      timeout_secs: 10,
    },
    roles: {
      groups: [{ group: "cn=ops,ou=groups,dc=corp,dc=example", role: "operator" }],
      default: "viewer",
    },
  });

  // 새 비밀번호를 넣으면 바꾼다.
  dialog = await edit();
  await user.type(within(dialog).getByLabelText("Bind password"), "new-secret");
  await save(dialog);
  expect(sent[1]!.body.backend.bind_password).toBe("new-secret");

  // 지우기를 고르면 빈 글을 보낸다(입력은 막힌다). 바인드 DN도 비워야 한다(익명 검색).
  dialog = await edit();
  await user.click(within(dialog).getByLabelText("Clear the saved password"));
  expect(within(dialog).getByLabelText("Bind password")).toBeDisabled();
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(
    within(dialog).getByText("To clear the saved password, also clear the bind DN."),
  ).toBeVisible();
  expect(sent).toHaveLength(2);
  await user.clear(within(dialog).getByLabelText("Bind DN"));
  await save(dialog);
  expect(sent[2]!.body.backend.bind_password).toBe("");
  expect(sent[2]!.body.backend).not.toHaveProperty("bind_dn");
  expect(sent).toHaveLength(3);
});

it("asks for the bind password again when the server changes", async () => {
  realms([CORP, LAB]);
  const sent: { backend: Record<string, unknown> }[] = [];
  server.use(
    http.put(apiUrl("/realms/corp"), async ({ request }) => {
      sent.push((await request.json()) as { backend: Record<string, unknown> });
      return HttpResponse.json(CORP);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  const again = "Enter the bind password again: the URL, bind DN or CA certificate changed.";

  // URL·바인드 DN·CA 가운데 하나라도 바꾸면 비밀번호를 다시 넣어야 한다(서버의 400과 같은 규칙).
  for (const [label, value] of [
    ["URL", "ldaps://other.example"],
    ["Bind DN", "cn=other,dc=corp,dc=example"],
    ["CA certificate (PEM)", "-----BEGIN CERT"],
  ] as const) {
    await user.click(await screen.findByRole("button", { name: "Edit corp" }));
    const dialog = await screen.findByRole("dialog", { name: "Edit realm corp" });
    await user.clear(within(dialog).getByLabelText(label));
    await user.type(within(dialog).getByLabelText(label), value);
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(within(dialog).getByText(again)).toBeVisible();
    expect(sent).toHaveLength(0);
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }

  // 바인드 DN만 비우면 비밀번호를 다시 넣을 것이 아니라 저장된 비밀번호 지우기를 골라야 한다.
  await user.click(await screen.findByRole("button", { name: "Edit corp" }));
  const cleared = await screen.findByRole("dialog", { name: "Edit realm corp" });
  await user.clear(within(cleared).getByLabelText("Bind DN"));
  await user.click(within(cleared).getByRole("button", { name: "Save" }));
  expect(
    within(cleared).getByText(
      'The bind DN is empty: choose "Clear the saved password" to search anonymously.',
    ),
  ).toBeVisible();
  expect(within(cleared).queryByText(again)).not.toBeInTheDocument();
  expect(sent).toHaveLength(0);
  await user.click(within(cleared).getByLabelText("Clear the saved password"));
  await user.click(within(cleared).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(cleared).not.toBeInTheDocument());
  expect(sent[0]!.backend.bind_password).toBe("");
  expect(sent[0]!.backend).not.toHaveProperty("bind_dn");

  // 새 비밀번호와 함께면 보낸다.
  await user.click(await screen.findByRole("button", { name: "Edit corp" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit realm corp" });
  await user.clear(within(dialog).getByLabelText("URL"));
  await user.type(within(dialog).getByLabelText("URL"), "ldaps://other.example");
  await user.type(within(dialog).getByLabelText("Bind password"), "svc-secret");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(sent[1]!.backend).toMatchObject({
    url: "ldaps://other.example",
    bind_password: "svc-secret",
  });

  // 바인드 DN이 있는데 저장된 비밀번호가 없으면 넣어야 한다.
  await user.click(await screen.findByRole("button", { name: "Edit lab" }));
  const lab = await screen.findByRole("dialog", { name: "Edit realm lab" });
  await user.click(within(lab).getByRole("button", { name: "Save" }));
  expect(
    within(lab).getByText(
      "Enter a bind password for the bind DN, or clear the bind DN to search anonymously.",
    ),
  ).toBeVisible();
  expect(within(lab).getByLabelText("Bind password")).toBeInvalid();
});

it("deletes a realm after confirming", async () => {
  const change = realms([CORP, LAB]);
  let deleted = false;
  server.use(
    http.delete(apiUrl("/realms/lab"), () => {
      deleted = true;
      change([CORP]);
      return new HttpResponse(null, { status: 204 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Delete lab" }));
  const confirm = await screen.findByRole("alertdialog");
  expect(confirm).toHaveTextContent("Delete realm lab?");
  await user.click(within(confirm).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted realm lab.")).toBeInTheDocument();
  expect(deleted).toBe(true);
  await waitFor(() => expect(screen.queryByRole("cell", { name: "lab" })).not.toBeInTheDocument());
});

it("tests a realm and shows each step", async () => {
  realms([CORP]);
  const bodies: unknown[] = [];
  const results = [
    {
      steps: [
        { name: "connect", ok: true, message: "connected" },
        { name: "service_bind", ok: true, message: "bound as cn=svc" },
        { name: "search", ok: true, message: "found 1 entry" },
        { name: "user_bind", ok: true, message: "password accepted" },
        { name: "groups", ok: true, message: "2 groups" },
      ],
      dn: "uid=alice,ou=people,dc=corp,dc=example",
      groups: ["cn=ops,ou=groups,dc=corp,dc=example", "cn=all,ou=groups,dc=corp,dc=example"],
      role: "operator",
    },
    {
      steps: [
        { name: "connect", ok: true, message: "connected" },
        { name: "service_bind", ok: false, message: "invalid credentials" },
      ],
      dn: null,
      groups: [],
      role: null,
    },
  ];
  server.use(
    http.post(apiUrl("/realms/corp/actions/test"), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(results.shift());
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Test corp" }));
  const dialog = await screen.findByRole("dialog", { name: "Test realm corp" });
  await user.type(within(dialog).getByLabelText("Username"), "alice");
  await user.type(within(dialog).getByLabelText("Password"), "alice-secret");
  await user.click(within(dialog).getByRole("button", { name: "Test" }));

  const steps = await within(dialog).findByRole("table", { name: "Steps" });
  expect(
    within(steps)
      .getAllByRole("row")
      .slice(1)
      .map((r) =>
        within(r)
          .getAllByRole("cell")
          .map((c) => c.textContent),
      ),
  ).toEqual([
    ["Connect", "OK", "connected"],
    ["Service account bind", "OK", "bound as cn=svc"],
    ["Find the user", "OK", "found 1 entry"],
    ["User bind", "OK", "password accepted"],
    ["Groups", "OK", "2 groups"],
  ]);
  expect(within(dialog).getByText("uid=alice,ou=people,dc=corp,dc=example")).toBeVisible();
  expect(within(dialog).getByText("cn=all,ou=groups,dc=corp,dc=example")).toBeVisible();
  expect(within(dialog).getByText("Operator")).toBeVisible();
  expect(bodies).toEqual([{ username: "alice", password: "alice-secret" }]);

  // 실패한 단계에서 멈추고, 역할이 없으면 로그인이 거절된다고 알린다.
  await user.click(within(dialog).getByRole("button", { name: "Test" }));
  await waitFor(() =>
    expect(within(dialog).getByRole("table", { name: "Steps" })).toHaveTextContent(
      "invalid credentials",
    ),
  );
  expect(within(dialog).getByRole("table", { name: "Steps" })).toHaveTextContent("Failed");
  expect(within(dialog).getByText("No role: sign-in would be refused.")).toBeVisible();
  // 빈 DN과 그룹은 같은 "—"(고정폭 글꼴이 아님)
  const blanks = within(dialog).getAllByText("—");
  expect(blanks).toHaveLength(2);
  for (const blank of blanks) {
    expect(blank).not.toHaveClass("font-mono");
  }
  // 영역 시험은 사용자를 만들지 않는다. 대화상자는 닫을 때까지 남는다.
  await user.click(within(dialog).getByRole("button", { name: "Done" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
});

const OKTA = {
  id: "00000000-0000-0000-0000-0000000000c3",
  name: "okta",
  revision: 9,
  display: "Okta",
  backend: {
    kind: "oidc",
    issuer: "https://corp.okta.example/oauth2/default",
    client_id: "oplite",
    has_client_secret: true,
    scopes: ["openid", "profile", "email", "groups"],
    username_claim: "preferred_username",
    groups_claim: "groups",
    timeout_secs: 10,
  },
  roles: { groups: [{ group: "opl-admins", role: "admin" }] },
};

it("lists an OIDC realm with its issuer", async () => {
  realms([OKTA, CORP]);
  renderPage(<RealmsPage />);
  const table = await screen.findByRole("table", { name: "Realms" });
  const row = (await within(table).findByText("okta")).closest("tr")!;
  expect(
    within(row)
      .getAllByRole("cell")
      .slice(0, 4)
      .map((c) => c.textContent),
  ).toEqual(["okta", "Okta", "OIDC", "https://corp.okta.example/oauth2/default"]);
});

it("creates an OIDC realm and shows the redirect URI to register", async () => {
  const change = realms([CORP]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/realms"), async ({ request }) => {
      body = await request.json();
      change([CORP, OKTA]);
      return HttpResponse.json(OKTA, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await screen.findByText("corp");
  await user.click(screen.getByRole("button", { name: "Create" }));
  let dialog = await screen.findByRole("dialog", { name: "Create an LDAP realm" });
  await user.selectOptions(within(dialog).getByLabelText("Kind"), "oidc");
  dialog = await screen.findByRole("dialog", { name: "Create an OIDC realm" });
  expect(within(dialog).queryByLabelText("Base DN")).not.toBeInTheDocument();
  // 서버의 기본값으로 시작한다.
  expect(within(dialog).getByLabelText("Scopes")).toHaveValue("openid profile email");
  expect(within(dialog).getByLabelText("Username claim")).toHaveValue("preferred_username");
  expect(within(dialog).getByLabelText("Groups claim")).toHaveValue("groups");
  expect(within(dialog).getByLabelText("Timeout (seconds)")).toHaveValue("10");
  // 이 노드의 콜백 주소를 IdP에 등록하도록 보이고 복사할 수 있다.
  const uri = `${window.location.origin}/api/v1/auth/oidc/callback`;
  expect(within(dialog).getByLabelText("Redirect URI")).toHaveValue(uri);
  expect(within(dialog).getByLabelText("Redirect URI")).toHaveAttribute("readonly");
  await user.click(within(dialog).getByRole("button", { name: "Copy the redirect URI" }));
  expect(await navigator.clipboard.readText()).toBe(uri);

  // 서버 검증과 같은 검사: https issuer(쿼리 없음), client ID, openid scope, 클레임 이름.
  await user.type(within(dialog).getByLabelText("Name"), "okta");
  await user.type(within(dialog).getByLabelText("Issuer"), "http://corp.okta.example");
  await user.clear(within(dialog).getByLabelText("Scopes"));
  await user.type(within(dialog).getByLabelText("Scopes"), "profile email");
  await user.clear(within(dialog).getByLabelText("Groups claim"));
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(
    within(dialog).getByText("Use https://host[:port][/path] without a query or fragment."),
  ).toBeVisible();
  expect(within(dialog).getByLabelText("Client ID")).toHaveAccessibleDescription("Enter a value.");
  expect(within(dialog).getByText("Include openid.")).toBeVisible();
  expect(within(dialog).getByLabelText("Groups claim")).toBeInvalid();
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(within(dialog).getByLabelText("Issuer"), "https://corp.okta.example/?x=1");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Issuer")).toBeInvalid();
  expect(body).toBeUndefined();

  await user.type(within(dialog).getByLabelText("Display name"), "Okta");
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(
    within(dialog).getByLabelText("Issuer"),
    "https://corp.okta.example/oauth2/default",
  );
  await user.type(within(dialog).getByLabelText("Client ID"), "oplite");
  await user.type(within(dialog).getByLabelText("Client secret"), "client-secret");
  await user.clear(within(dialog).getByLabelText("Scopes"));
  await user.type(within(dialog).getByLabelText("Scopes"), " openid  profile email groups ");
  await user.type(within(dialog).getByLabelText("Groups claim"), "groups");
  await user.click(within(dialog).getByRole("button", { name: "Add group" }));
  await user.type(within(dialog).getByLabelText("Group 1"), "opl-admins");
  await user.selectOptions(within(dialog).getByLabelText("Role of group 1"), "admin");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));

  expect(await screen.findByText("Created realm okta.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "okta",
    display: "Okta",
    backend: {
      kind: "oidc",
      issuer: "https://corp.okta.example/oauth2/default",
      client_id: "oplite",
      client_secret: "client-secret",
      scopes: ["openid", "profile", "email", "groups"],
      username_claim: "preferred_username",
      groups_claim: "groups",
      timeout_secs: 10,
    },
    roles: { groups: [{ group: "opl-admins", role: "admin" }] },
  });
});

it("keeps, replaces or clears the client secret when editing", async () => {
  realms([OKTA]);
  const sent: { ifMatch: string | null; body: { backend: Record<string, unknown> } }[] = [];
  server.use(
    http.put(apiUrl("/realms/okta"), async ({ request }) => {
      sent.push({
        ifMatch: request.headers.get("If-Match"),
        body: (await request.json()) as { backend: Record<string, unknown> },
      });
      return HttpResponse.json(OKTA);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  async function edit() {
    await user.click(await screen.findByRole("button", { name: "Edit okta" }));
    return screen.findByRole("dialog", { name: "Edit realm okta" });
  }
  async function save(dialog: HTMLElement) {
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }
  const again = "Enter the client secret again: the issuer, client ID or CA certificate changed.";

  // 비워 두면 보내지 않아 저장된 비밀이 남는다. 종류는 고칠 수 없다.
  let dialog = await edit();
  expect(within(dialog).queryByLabelText("Kind")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("Issuer")).toHaveValue(
    "https://corp.okta.example/oauth2/default",
  );
  expect(within(dialog).getByLabelText("Scopes")).toHaveValue("openid profile email groups");
  expect(within(dialog).getByLabelText("Client secret")).toHaveValue("");
  expect(within(dialog).getByText("A secret is saved. Leave this empty to keep it.")).toBeVisible();
  await save(dialog);
  expect(sent[0]!.ifMatch).toBe("9");
  expect(sent[0]!.body.backend).toEqual({
    kind: "oidc",
    issuer: "https://corp.okta.example/oauth2/default",
    client_id: "oplite",
    scopes: ["openid", "profile", "email", "groups"],
    username_claim: "preferred_username",
    groups_claim: "groups",
    timeout_secs: 10,
  });

  // issuer·client ID·CA 가운데 하나라도 바꾸면 비밀을 다시 넣어야 한다(서버의 400과 같은 규칙).
  for (const [label, value] of [
    ["Issuer", "https://other.example"],
    ["Client ID", "other"],
    ["CA certificate (PEM)", "-----BEGIN CERT"],
  ] as const) {
    dialog = await edit();
    await user.clear(within(dialog).getByLabelText(label));
    await user.type(within(dialog).getByLabelText(label), value);
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    expect(within(dialog).getByText(again)).toBeVisible();
    expect(sent).toHaveLength(1);
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
  }

  // 새 비밀과 함께면 보낸다.
  dialog = await edit();
  await user.clear(within(dialog).getByLabelText("Client ID"));
  await user.type(within(dialog).getByLabelText("Client ID"), "other");
  await user.type(within(dialog).getByLabelText("Client secret"), "new-secret");
  await save(dialog);
  expect(sent[1]!.body.backend).toMatchObject({ client_id: "other", client_secret: "new-secret" });

  // 지우기를 고르면 빈 글을 보낸다(공개 클라이언트). 입력은 막히고 바뀐 issuer도 괜찮다.
  dialog = await edit();
  await user.click(within(dialog).getByLabelText("Clear the saved secret (public client)"));
  expect(within(dialog).getByLabelText("Client secret")).toBeDisabled();
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(within(dialog).getByLabelText("Issuer"), "https://other.example");
  await save(dialog);
  expect(sent[2]!.body.backend).toMatchObject({
    issuer: "https://other.example",
    client_secret: "",
  });
});

it("edits a public client without asking for a secret", async () => {
  const pub = { ...OKTA, backend: { ...OKTA.backend, has_client_secret: false } };
  realms([pub]);
  let body: { backend: Record<string, unknown> } | undefined;
  server.use(
    http.put(apiUrl("/realms/okta"), async ({ request }) => {
      body = (await request.json()) as { backend: Record<string, unknown> };
      return HttpResponse.json(pub);
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Edit okta" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit realm okta" });
  expect(within(dialog).getByText("Leave empty for a public client (PKCE only).")).toBeVisible();
  expect(
    within(dialog).queryByLabelText("Clear the saved secret (public client)"),
  ).not.toBeInTheDocument();
  await user.clear(within(dialog).getByLabelText("Issuer"));
  await user.type(within(dialog).getByLabelText("Issuer"), "https://other.example/");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(dialog).not.toBeInTheDocument());
  expect(body!.backend.issuer).toBe("https://other.example/");
  expect(body!.backend).not.toHaveProperty("client_secret");
});

it("tests an OIDC realm without asking for a user", async () => {
  realms([OKTA]);
  const bodies: string[] = [];
  server.use(
    http.post(apiUrl("/realms/okta/actions/test"), async ({ request }) => {
      bodies.push(await request.text());
      return HttpResponse.json({
        steps: [
          { name: "discovery", ok: true, message: "issuer matches" },
          { name: "jwks", ok: false, message: "no RS256 or ES256 signing keys" },
        ],
        dn: null,
        groups: [],
        role: null,
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<RealmsPage />);
  await user.click(await screen.findByRole("button", { name: "Test okta" }));
  const dialog = await screen.findByRole("dialog", { name: "Test realm okta" });
  expect(dialog).toHaveTextContent(
    "Checks the identity provider's discovery document and signing keys. No one signs in.",
  );
  expect(within(dialog).queryByLabelText("Username")).not.toBeInTheDocument();
  expect(within(dialog).queryByLabelText("Password")).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Test" }));
  const steps = await within(dialog).findByRole("table", { name: "Steps" });
  expect(
    within(steps)
      .getAllByRole("row")
      .slice(1)
      .map((r) =>
        within(r)
          .getAllByRole("cell")
          .map((c) => c.textContent),
      ),
  ).toEqual([
    ["Discovery", "OK", "issuer matches"],
    ["Signing keys", "Failed", "no RS256 or ES256 signing keys"],
  ]);
  // 사용자가 없으므로 DN·그룹·역할은 보이지 않는다.
  expect(within(dialog).queryByText("No role: sign-in would be refused.")).not.toBeInTheDocument();
  // 본문 없이 부른다.
  expect(bodies).toEqual([""]);
});
