import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network } from "@/api/types";
import { NetworksPage, validBridge } from "@/networks/NetworksPage";
import { NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

function network(name: string, bridge: string, vlan: number | null = null): Network {
  return {
    id: `019a0f3c-0000-7000-8003-${name.padStart(12, "0").slice(-12)}`,
    name,
    revision: 1,
    labels: {},
    spec: { bridge, vlan },
    status: {},
  };
}

const bridged = {
  ...NODE,
  status: { ...NODE.status, bridges: [{ name: "vmbr0", vlan_filtering: false }] },
};

it("follows the server's bridge name rule", () => {
  expect(validBridge("vmbr0")).toBe(true);
  expect(validBridge("br.100")).toBe(true);
  expect(validBridge("")).toBe(false);
  expect(validBridge("a".repeat(16))).toBe(false);
  expect(validBridge("br 0")).toBe(false);
  expect(validBridge("br:0")).toBe(false);
  expect(validBridge("..")).toBe(false);
});

it("shows which nodes have each bridge and which VMs use it", async () => {
  const web = vm("web", "running", {
    spec: { ...vm("web", "running").spec, nics: [{ network: "lan", model: "virtio" }] },
  });
  mockLists({
    network: [network("lan", "vmbr0"), network("dmz", "vmbr0", 20), network("lab", "br9")],
    node: [bridged],
    vm: [web],
  });
  renderPage(<NetworksPage />);
  const table = await screen.findByRole("table", { name: "Networks" });
  const lan = (await within(table).findByText("lan")).closest("tr")!;
  expect(lan).toHaveTextContent("node-a");
  expect(within(lan).getByRole("button", { name: "Delete lan" })).toBeDisabled();
  const dmz = within(table).getByText("dmz").closest("tr")!;
  expect(dmz).toHaveTextContent("VLAN filtering is off on node-a");
  const lab = within(table).getByText("lab").closest("tr")!;
  expect(lab).toHaveTextContent("No node has this bridge");
  expect(within(lab).getByRole("button", { name: "Delete lab" })).toBeEnabled();
});

it("creates a network after checking its fields", async () => {
  mockLists({ node: [bridged] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(network("lan", "vmbr0", 10), { status: 201 });
    }),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "user");
  await userEvent.type(within(dialog).getByLabelText("VLAN tag (optional)"), "5000");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("The name user is reserved for NAT.")).toBeVisible();
  expect(within(dialog).getByText("Use 1–15 characters without /, : or spaces.")).toBeVisible();
  expect(within(dialog).getByText("Use a number from 1 to 4094.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "lan");
  await userEvent.type(within(dialog).getByLabelText("Bridge"), "vmbr0");
  await userEvent.clear(within(dialog).getByLabelText("VLAN tag (optional)"));
  await userEvent.type(within(dialog).getByLabelText("VLAN tag (optional)"), "10");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created network lan.")).toBeInTheDocument();
  expect(body).toEqual({ name: "lan", spec: { bridge: "vmbr0", vlan: 10 } });
});

it("deletes an unused network and shows why a delete failed", async () => {
  const lab = network("lab", "br9");
  mockLists({ network: [lab] });
  server.use(
    http.delete(apiUrl(`/networks/${lab.id}`), () =>
      HttpResponse.json(
        { code: "network_in_use", message: 'network "lab" is used by VM "x"' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete lab" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A VM uses this network. Remove its interface first.",
  );
});

it("hides changes from operators", async () => {
  mockLists({ network: [network("lan", "vmbr0")] });
  renderPage(<NetworksPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("lan")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
});
