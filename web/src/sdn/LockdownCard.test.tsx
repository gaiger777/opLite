import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import { SdnPage } from "@/sdn/SdnPage";
import { expectNoAxeViolations } from "@/test/axe";
import { readyChassis, sdnNode, sdnTask, sdnView } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
});

const N1 = sdnNode(1, readyChassis(1, "leader"));
const N2 = sdnNode(2, readyChassis(2));

/** 서버 `LockdownView`(`pending` 거짓·`error` 없음은 빠진다) */
const LOCKED = {
  enabled: true,
  applied: [N1.id],
  members: ["10.0.0.1", "10.0.0.2"],
  centrals: ["10.0.0.1"],
  nodes: [
    { node: N1.id, name: "node-a", firewall: true, allowed: 3 },
    { node: N2.id, name: "node-b", firewall: false, allowed: 0, error: "nft: permission denied" },
  ],
};

function sdn(extra: Record<string, unknown> = {}) {
  mockLists({ node: [N1, N2] });
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([N1], [N1, N2], extra))),
    // 서버 `GET /cluster`(투표 멤버는 node-a)
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "ab:cd",
        leader: N1.id,
        node: N1.id,
        members: [N1, N2].map((n, i) => ({
          id: n.id,
          name: n.name,
          address: n.spec.address,
          cluster_address: n.spec.address,
          cluster_addresses: [n.spec.address],
          voter: i === 0,
          role: i === 0 ? "voter" : "learner",
          online: true,
          witness: false,
          fencing: null,
          version: "0.67.0",
        })),
      }),
    ),
  );
}

it("shows the lockdown state per central and node", async () => {
  sdn({ lockdown: LOCKED });
  const { container } = renderPage(<SdnPage />);
  const card = (await screen.findByRole("heading", { name: "OVN access lockdown" })).closest(
    "section",
  )!;
  expect(within(card).getByText("On")).toBeVisible();
  expect(within(card).getByText("1 of 1 locked")).toBeVisible();
  expect(within(card).getByText("on 1 of 2 nodes")).toBeVisible();
  const b = within(card).getByText("node-b").closest("tr")!;
  expect(within(b).getByText("not applied")).toBeVisible();
  expect(within(b).getByText("nft: permission denied")).toBeVisible();
  expect(within(card).getByRole("button", { name: "Turn off" })).toBeVisible();
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("checks first and turns on only with force when peers are outside the allow list", async () => {
  sdn();
  const sent: { search: string; body: unknown }[] = [];
  server.use(
    http.post(apiUrl("/sdn/lockdown"), async ({ request }) => {
      const url = new URL(request.url);
      sent.push({ search: url.search, body: await request.json() });
      return url.searchParams.get("dry_run") === "true"
        ? HttpResponse.json({
            enabled: true,
            members: ["10.0.0.1", "10.0.0.2"],
            centrals: ["10.0.0.1"],
            outside: [{ central: "node-a", peer: "192.0.2.7" }],
            blocked: true,
          })
        : HttpResponse.json(sdnTask("t1", "sdn-init", N1.id), { status: 202 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SdnPage />);
  const card = (await screen.findByRole("heading", { name: "OVN access lockdown" })).closest(
    "section",
  )!;
  // 켠 적이 없으면 꺼짐
  expect(within(card).getByText("Off")).toBeVisible();
  await user.click(within(card).getByRole("button", { name: "Check first" }));
  expect(await within(card).findByText("192.0.2.7 → node-a")).toBeVisible();
  const on = within(card).getByRole("button", { name: "Turn on" });
  expect(on).toBeDisabled();
  await user.click(within(card).getByLabelText("Turn on anyway (these connections are cut off)"));
  await user.click(on);
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent).toEqual([
    { search: "?dry_run=true", body: { enabled: true } },
    { search: "", body: { enabled: true, force: true } },
  ]);
});

it("turns on the recommended SDN alert rules with the server's sdn-defaults", async () => {
  sdn();
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/alert-rules"), () =>
      HttpResponse.json({
        revision: 3,
        items: [
          {
            id: "019a0f3c-0000-7000-8010-000000000001",
            name: "sdn-quorum-lost",
            revision: 2,
            spec: {
              condition: { kind: "sdn_quorum_lost" },
              enabled: true,
              for_secs: 0,
              severity: "critical",
              targets: ["ops"],
              repeat_secs: 14400,
              send_resolved: true,
            },
            status: {},
          },
        ],
      }),
    ),
    http.get(apiUrl("/notification-targets"), () =>
      HttpResponse.json({
        revision: 3,
        items: [
          {
            id: "019a0f3c-0000-7000-8011-000000000001",
            name: "ops",
            revision: 1,
            enabled: true,
            target: {
              kind: "webhook",
              url: "https://h/…",
              format: "json",
              header_names: [],
              has_token: false,
            },
          },
        ],
      }),
    ),
    // 서버가 이미 있는 이름은 건너뛰고 만든 것과 있던 것을 알려 준다(여기서는 그새 하나가 더 생겼다).
    http.post(apiUrl("/alert-rules/sdn-defaults"), async ({ request }) => {
      sent.push(await request.json());
      const rule = (name: string) => ({
        id: `id-${name}`,
        name,
        revision: 4,
        labels: {},
        spec: {
          condition: { kind: name.replaceAll("-", "_") },
          enabled: true,
          for_secs: 0,
          severity: "warning",
          targets: ["ops"],
          repeat_secs: 14400,
          send_resolved: true,
        },
        status: {},
      });
      return HttpResponse.json({
        created: [
          "sdn-central-down",
          "sdn-chassis-down",
          "sdn-gateway-unavailable",
          "sdn-gateway-failover",
          "sdn-sync-failed",
          "sdn-physnet-not-ready",
        ].map(rule),
        existing: ["sdn-quorum-lost", "sdn-lb-member-down"],
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SdnPage />);
  expect(await screen.findByText(/1 SDN alert rule\./)).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Turn on SDN alerts" }));
  const dialog = await screen.findByRole("dialog", { name: "Turn on SDN alerts" });
  await user.click(within(dialog).getByRole("button", { name: "Create 7 rules" }));
  expect(await within(dialog).findByText("Choose 1 to 16 notification targets.")).toBeVisible();
  await user.click(within(dialog).getByLabelText("ops"));
  await user.click(within(dialog).getByRole("button", { name: "Create 7 rules" }));
  await waitFor(() => expect(sent).toHaveLength(1));
  expect(sent[0]).toEqual({ targets: ["ops"] });
  expect(await screen.findByText("Created 6 SDN alert rules; 2 already existed.")).toBeVisible();
});

it("offers to finish a turn-on that stopped halfway, with Raft peers and warnings from the check (review 1 M6)", async () => {
  // Task가 실패해 중앙 하나만 잠갔다(켜짐·바꾸는 중·오류, 도는 Task 없음).
  sdn({
    lockdown: {
      ...LOCKED,
      applied: [],
      pending: true,
      error: "central node-a did not come back",
    },
    tasks: [],
  });
  const sent: { search: string; body: unknown }[] = [];
  server.use(
    http.post(apiUrl("/sdn/lockdown"), async ({ request }) => {
      const url = new URL(request.url);
      sent.push({ search: url.search, body: await request.json() });
      return url.searchParams.get("dry_run") === "true"
        ? HttpResponse.json({
            enabled: true,
            members: ["10.0.0.1", "10.0.0.2"],
            centrals: ["10.0.0.1"],
            outside: [{ central: "node-a", peer: "192.0.2.8", raft: true }],
            blocked: true,
            // 서버 `lockdown_warnings`의 문장 그대로
            warnings: [
              "the cluster has two OVN centrals: while each one restarts its southbound database the other alone is not a majority, so the SB is unavailable for a few seconds per central (the data path keeps working); add a third central to avoid it",
            ],
          })
        : HttpResponse.json(sdnTask("t1", "sdn-init", N1.id), { status: 202 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SdnPage />);
  const card = (await screen.findByRole("heading", { name: "OVN access lockdown" })).closest(
    "section",
  )!;
  expect(await within(card).findByText("Stopped halfway")).toBeVisible();
  expect(within(card).getByRole("button", { name: "Turn off" })).toBeVisible();
  await user.click(within(card).getByRole("button", { name: "Check first" }));
  const peer = await within(card).findByText("192.0.2.8 → node-a");
  expect(within(peer).getByText("Raft")).toBeVisible();
  expect(
    within(card).getByText(/^The cluster has two OVN centrals: while each one restarts/),
  ).toBeVisible();
  const finish = within(card).getByRole("button", { name: "Finish turning on" });
  expect(finish).toBeDisabled();
  await user.click(within(card).getByLabelText("Turn on anyway (these connections are cut off)"));
  await user.click(finish);
  await waitFor(() => expect(sent).toHaveLength(2));
  expect(sent[1]).toEqual({ search: "", body: { enabled: true, force: true } });
});

it("offers to finish a turn-off that stopped halfway and translates lockdown errors (review 2 m3, m2)", async () => {
  sdn({
    lockdown: {
      ...LOCKED,
      enabled: false,
      pending: true,
      error:
        "the OVN address allow-list is not removed on node-b: nft: permission denied yet; the SDN agent there keeps trying",
      nodes: [
        LOCKED.nodes[0],
        {
          ...LOCKED.nodes[1],
          error:
            "the address allow-list cut the OVN Raft connections (no fresh message from 10.0.0.1); removed it again",
        },
      ],
    },
    tasks: [],
  });
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/sdn/lockdown"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(sdnTask("t1", "sdn-init", N1.id), { status: 202 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<SdnPage />);
  const card = (await screen.findByRole("heading", { name: "OVN access lockdown" })).closest(
    "section",
  )!;
  expect(await within(card).findByText("Stopped halfway")).toBeVisible();
  expect(
    within(card).getByText(/^The OVN address allow-list is not removed on node-b/),
  ).toBeVisible();
  expect(
    within(card).getByText(/^The address allow-list cut the OVN Raft connections and was removed/),
  ).toBeVisible();
  await user.click(within(card).getByRole("button", { name: "Finish turning off" }));
  await waitFor(() => expect(sent).toEqual([{ enabled: false }]));
});
