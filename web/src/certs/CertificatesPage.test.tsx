import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { CertificatesPage } from "@/certs/CertificatesPage";
import { daysLeft, expiryTone, http01Missing, splitNames } from "@/certs/api";
import { resetTasks } from "@/tasks/tasks";
import { expectNoAxeViolations } from "@/test/axe";
import { account, certificate, dnsProvider } from "@/test/declarative";
import { NODE, task } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const OPERATOR = { ...ADMIN, role: "operator" as const };
const LISTENING = {
  ...NODE,
  id: "n2",
  name: "node-b",
  status: { ...NODE.status, features: ["http01"] },
};
const LE = account("le");
const CF = dnsProvider("cf");
const API = certificate("api", 45);
const SOON = certificate("old", 3, { failures: 2, error: "rate limited" });

function mockCerts({ accounts = [LE], dns = [CF], certs = [API, SOON] } = {}) {
  mockLists({ node: [NODE, LISTENING] });
  server.use(
    http.get(apiUrl("/acme-accounts"), () => HttpResponse.json({ revision: 1, items: accounts })),
    http.get(apiUrl("/acme-dns"), () => HttpResponse.json({ revision: 1, items: dns })),
    http.get(apiUrl("/certificates"), () => HttpResponse.json({ revision: 1, items: certs })),
  );
}

it("computes expiry, HTTP-01 coverage and name lists", () => {
  expect(daysLeft(86_400 * 10, 0)).toBe(10);
  expect(expiryTone(3)).toBe("danger");
  expect(expiryTone(15)).toBe("warning");
  expect(expiryTone(60)).toBe("success");
  const witness = { ...NODE, id: "w", name: "w", spec: { ...NODE.spec, witness: true } };
  expect(http01Missing([NODE, LISTENING, witness], false, [])).toEqual(["node-a"]);
  expect(http01Missing([NODE, LISTENING], true, ["n2"])).toEqual([]);
  expect(splitNames("a.example.com, {node}.example.com\n*.x.org")).toEqual([
    "a.example.com",
    "{node}.example.com",
    "*.x.org",
  ]);
});

it("lists certificates with expiry and failure chips, accounts and DNS providers, and passes axe", async () => {
  mockCerts();
  const { container } = renderPage(<CertificatesPage />, { path: "/settings/certificates" });
  const table = await screen.findByRole("table", { name: "Certificates" });
  const api = within(table).getByRole("row", { name: /^api/ });
  expect(api).toHaveTextContent("45 days left");
  expect(api).toHaveTextContent("Shared");
  expect(api).toHaveTextContent("DNS-01 (cf)");
  const old = within(table).getByRole("row", { name: /^old/ });
  expect(old).toHaveTextContent("3 days left");
  expect(old).toHaveTextContent("2 failures");
  expect(old).toHaveTextContent("rate limited");
  const accounts = screen.getByRole("table", { name: "ACME accounts" });
  expect(within(accounts).getByRole("row", { name: /le/ })).toHaveTextContent("Registered");
  expect(screen.getByRole("table", { name: "DNS providers" })).toHaveTextContent("Cloudflare");
  // 노드 HTTPS 인증서를 서명하는 클러스터 CA는 클러스터 화면에 있다.
  expect(
    screen.getByRole("link", { name: "Cluster & availability › Cluster (members, CA)" }),
  ).toHaveAttribute("href", "/cluster");
  // DNS 비밀은 보기에 없다
  expect(container).not.toHaveTextContent("api_token");
  await expectNoAxeViolations(container);
});

it("renews a certificate now", async () => {
  const user = userEvent.setup();
  mockCerts();
  let renewed = "";
  server.use(
    http.post(apiUrl("/certificates/:key/actions/renew"), ({ params }) => {
      renewed = String(params.key);
      return HttpResponse.json({ tasks: [task("t1", "acme-order", API.id)] }, { status: 202 });
    }),
  );
  renderPage(<CertificatesPage />, { path: "/settings/certificates" });
  await user.click(await screen.findByRole("button", { name: "Renew api now" }));
  await waitFor(() => expect(renewed).toBe(API.id));
});

it("shows no write buttons to users without cluster.admin", async () => {
  mockCerts();
  renderPage(<CertificatesPage />, { path: "/settings/certificates", as: OPERATOR });
  await screen.findByRole("table", { name: "Certificates" });
  expect(screen.queryByRole("button", { name: /^Renew/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add certificate" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add account" })).not.toBeInTheDocument();
});

it("registers an ACME account only after agreeing to the terms, with EAB", async () => {
  const user = userEvent.setup();
  mockCerts({ accounts: [], certs: [] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/acme-accounts"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(account("zero"), { status: 201 });
    }),
  );
  renderPage(<CertificatesPage />, { path: "/settings/certificates" });
  expect(
    await screen.findByText(
      "To get a certificate, first add an ACME account (your account at the certificate authority) below.",
    ),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Add certificate" })).toBeDisabled();
  await user.click(screen.getAllByRole("button", { name: "Add account" })[0]!);
  const dialog = await screen.findByRole("dialog", { name: "Add an ACME account" });
  await user.type(within(dialog).getByLabelText("Name"), "zero");
  await user.selectOptions(within(dialog).getByLabelText("Directory"), "zerossl");
  await user.type(within(dialog).getByLabelText("EAB key id"), "kid-1");
  const hmac = within(dialog).getByLabelText("EAB HMAC key");
  expect(hmac).toHaveAttribute("type", "password");
  await user.type(hmac, "c2VjcmV0");
  await user.click(within(dialog).getByRole("button", { name: "Register" }));
  expect(
    within(dialog).getByText("You must agree to the CA's terms of service."),
  ).toBeInTheDocument();
  expect(body).toBeUndefined();
  await user.click(within(dialog).getByRole("checkbox", { name: /terms of service/ }));
  await user.click(within(dialog).getByRole("button", { name: "Register" }));
  await waitFor(() =>
    expect(body).toEqual({
      name: "zero",
      directory: "zerossl",
      agree_tos: true,
      eab: { kid: "kid-1", hmac_key: "c2VjcmV0" },
    }),
  );
});

it("adds a DNS provider with its secret in a password field", async () => {
  const user = userEvent.setup();
  mockCerts({ dns: [] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/acme-dns"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(dnsProvider("ns1"), { status: 201 });
    }),
  );
  renderPage(<CertificatesPage />, { path: "/settings/certificates" });
  await user.click(await screen.findByRole("button", { name: "Add DNS provider" }));
  const dialog = await screen.findByRole("dialog", { name: "Add a DNS provider" });
  await user.type(within(dialog).getByLabelText("Name"), "ns1");
  await user.selectOptions(within(dialog).getByLabelText("Provider"), "rfc2136");
  await user.type(within(dialog).getByLabelText("Server (ip:53)"), "192.0.2.53:53");
  await user.type(within(dialog).getByLabelText("TSIG key name"), "acme.");
  const secret = within(dialog).getByLabelText("TSIG secret (base64)");
  expect(secret).toHaveAttribute("type", "password");
  expect(within(dialog).getAllByText(/not shown again after saving/)).toHaveLength(1);
  await user.type(secret, "a2V5");
  await user.type(within(dialog).getByLabelText("Zones"), "example.com");
  await user.click(within(dialog).getByRole("button", { name: "Add" }));
  await waitFor(() =>
    expect(body).toEqual({
      name: "ns1",
      provider: { kind: "rfc2136", server: "192.0.2.53:53", key_name: "acme.", secret: "a2V5" },
      zones: ["example.com"],
    }),
  );
});

it("warns about nodes without the port 80 listener for HTTP-01 and creates the certificate", async () => {
  const user = userEvent.setup();
  mockCerts();
  let body: unknown;
  server.use(
    http.post(apiUrl("/certificates"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(certificate("vip", 90), { status: 201 });
    }),
  );
  renderPage(<CertificatesPage />, { path: "/settings/certificates" });
  await user.click(await screen.findByRole("button", { name: "Add certificate" }));
  const dialog = await screen.findByRole("dialog", { name: "Add a certificate" });
  await user.type(within(dialog).getByLabelText("Name"), "vip");
  await user.type(within(dialog).getByLabelText("Names"), "*.example.com");
  await user.selectOptions(within(dialog).getByLabelText("Ownership check"), "http01");
  expect(within(dialog).getByRole("alert")).toHaveTextContent("Missing on: node-a.");
  await user.click(within(dialog).getByRole("button", { name: "Add" }));
  expect(within(dialog).getByText("Wildcard names need DNS-01.")).toBeInTheDocument();
  await user.selectOptions(within(dialog).getByLabelText("Ownership check"), "dns01");
  expect(within(dialog).queryByRole("alert")).not.toBeInTheDocument();
  await user.selectOptions(within(dialog).getByLabelText("Profile"), "shortlived");
  await user.click(within(dialog).getByRole("button", { name: "Add" }));
  await waitFor(() =>
    expect(body).toEqual({
      name: "vip",
      account: "le",
      names: ["*.example.com"],
      scope: { kind: "shared" },
      challenge: { kind: "dns01", dns: "cf" },
      key: "ecdsa-p256",
      precheck: true,
      profile: "shortlived",
    }),
  );
});

it("explains empty lists and starts from the ACME account", async () => {
  const user = userEvent.setup();
  mockCerts({ accounts: [], dns: [], certs: [] });
  renderPage(<CertificatesPage />, { path: "/settings/certificates" });
  const accounts = (await screen.findByText("No ACME accounts.")).closest<HTMLElement>(
    "[data-slot=empty-state]",
  )!;
  expect(accounts).toHaveTextContent("All it needs is an email and accepting the terms.");
  expect(within(accounts).getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  const dns = screen
    .getByText("No DNS providers.")
    .closest<HTMLElement>("[data-slot=empty-state]")!;
  expect(dns).toHaveTextContent("wildcard names");
  await user.click(within(accounts).getByRole("button", { name: "Add account" }));
  expect(await screen.findByRole("dialog", { name: "Add an ACME account" })).toBeInTheDocument();
});
