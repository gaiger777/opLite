import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network } from "@/api/types";
import { bridgeName, NetworksPage, validBridge } from "@/networks/NetworksPage";
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

function vxlan(name: string, vni: number, mtu?: number): Network {
  return { ...network(name, ""), spec: { vxlan: { vni }, ...(mtu && { mtu }) } };
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
  // VXLAN 네트워크의 관리 이름 모양은 서버가 예약해 둔다.
  expect(validBridge("oplv100")).toBe(false);
  expect(validBridge("oplx7")).toBe(false);
  expect(validBridge("oplv")).toBe(true);
  expect(validBridge("oplvx1")).toBe(true);
});

it("names the bridge of each network kind", () => {
  expect(bridgeName(network("lan", "vmbr0").spec)).toBe("vmbr0");
  expect(bridgeName(vxlan("ov", 100).spec)).toBe("oplv100");
});

it("lists VXLAN networks with their managed bridge, VNI and MTU", async () => {
  const overlay = {
    ...NODE,
    status: { ...NODE.status, bridges: [{ name: "oplv100", vlan_filtering: false }] },
  };
  mockLists({
    network: [network("lan", "vmbr0", 10), vxlan("ov", 100), vxlan("jumbo", 7, 8950)],
    node: [overlay],
    vm: [],
  });
  renderPage(<NetworksPage />);
  const table = await screen.findByRole("table", { name: "Networks" });
  const headers = within(table).getAllByRole("columnheader");
  const cell = (row: HTMLElement, name: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === name)];
  const lan = (await within(table).findByText("lan")).closest("tr")!;
  expect(cell(lan, "Type")).toHaveTextContent("Bridge");
  expect(cell(lan, "VLAN/VNI")).toHaveTextContent("10");
  expect(cell(lan, "MTU")).toHaveTextContent("—");
  const ov = within(table).getByText("ov").closest("tr")!;
  expect(cell(ov, "Type")).toHaveTextContent("VXLAN");
  expect(cell(ov, "Bridge")).toHaveTextContent("oplv100");
  expect(cell(ov, "VLAN/VNI")).toHaveTextContent("100");
  expect(cell(ov, "MTU")).toHaveTextContent("1450");
  // opld가 만든 oplv100이 있는 노드
  expect(cell(ov, "Nodes with the bridge")).toHaveTextContent("node-a");
  const jumbo = within(table).getByText("jumbo").closest("tr")!;
  expect(cell(jumbo, "MTU")).toHaveTextContent("8950");
  expect(cell(jumbo, "Nodes with the bridge")).toHaveTextContent("No node has this bridge");
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
  expect(lan).toHaveTextContent("Remove the VM interfaces first.");
  const dmz = within(table).getByText("dmz").closest("tr")!;
  expect(dmz).toHaveTextContent("VLAN filtering is off on node-a");
  const lab = within(table).getByText("lab").closest("tr")!;
  expect(lab).toHaveTextContent("No node has this bridge");
  expect(within(lab).getByRole("button", { name: "Delete lab" })).toBeEnabled();
});

it("warns nothing when the bridge filters VLANs", async () => {
  const aware = {
    ...NODE,
    status: { ...NODE.status, bridges: [{ name: "vmbr0", vlan_filtering: true }] },
  };
  mockLists({ network: [network("dmz", "vmbr0", 20)], node: [aware] });
  renderPage(<NetworksPage />);
  const dmz = (await screen.findByText("dmz")).closest("tr")!;
  expect(dmz).toHaveTextContent("node-a");
  expect(dmz).not.toHaveTextContent("VLAN filtering is off");
});

// 노드와 VM 목록을 읽지 못하면 브리지가 없다고도, 지워도 된다고도 말하지 않는다.
it("judges nothing without the nodes and VMs", async () => {
  mockLists({ network: [network("lab", "br9")] });
  server.use(
    http.get(apiUrl("/nodes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<NetworksPage />);
  const lab = (await screen.findByText("lab")).closest("tr")!;
  expect(within(lab).getByRole("button", { name: "Delete lab" })).toBeDisabled();
  expect(lab).not.toHaveTextContent("No node has this bridge");
  const headers = within(lab.closest("table")!).getAllByRole("columnheader");
  const cell = (name: string) =>
    within(lab).getAllByRole("cell")[headers.findIndex((h) => h.textContent === name)];
  expect(cell("Nodes with the bridge")).toHaveTextContent("—");
  expect(cell("VMs")).toHaveTextContent("—");
});

it("suggests the nodes' bridges except the managed VXLAN ones", async () => {
  mockLists({
    node: [
      {
        ...NODE,
        status: {
          ...NODE.status,
          bridges: [
            { name: "vmbr1", vlan_filtering: false },
            { name: "oplv100", vlan_filtering: false },
            { name: "vmbr0", vlan_filtering: true },
          ],
        },
      },
    ],
  });
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  const input = within(dialog).getByLabelText("Bridge");
  const list = document.getElementById(input.getAttribute("list")!)!;
  await vi.waitFor(() => expect(list.querySelectorAll("option")).toHaveLength(2));
  const options = [...list.querySelectorAll("option")].map((o) => o.value);
  expect(options).toEqual(["vmbr0", "vmbr1"]);
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
  await userEvent.type(within(dialog).getByLabelText("VLAN tag (optional)"), "0x10");
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

it("rejects VLAN tags out of range and sends no tag when empty", async () => {
  mockLists({ node: [bridged] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(network("wan", "vmbr0"), { status: 201 });
    }),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "wan");
  await userEvent.type(within(dialog).getByLabelText("Bridge"), "vmbr0");
  await userEvent.type(within(dialog).getByLabelText("VLAN tag (optional)"), "5000");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Use a number from 1 to 4094.")).toBeVisible();
  expect(body).toBeUndefined();

  // VXLAN 네트워크의 관리 이름은 브리지로 쓸 수 없다.
  await userEvent.clear(within(dialog).getByLabelText("Bridge"));
  await userEvent.type(within(dialog).getByLabelText("Bridge"), "oplv100");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(
    within(dialog).getByText("Names like oplv100 and oplx100 are reserved for VXLAN networks."),
  ).toBeVisible();
  await userEvent.clear(within(dialog).getByLabelText("Bridge"));
  await userEvent.type(within(dialog).getByLabelText("Bridge"), "vmbr0");

  await userEvent.clear(within(dialog).getByLabelText("VLAN tag (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created network wan.")).toBeInTheDocument();
  expect(body).toEqual({ name: "wan", spec: { bridge: "vmbr0", vlan: null } });
});

it("creates a VXLAN network with a VNI and an optional MTU", async () => {
  mockLists({ node: [bridged] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(vxlan("ov", 100), { status: 201 });
    }),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "ov");
  await userEvent.selectOptions(within(dialog).getByLabelText("Type"), "vxlan");
  // VXLAN에는 브리지와 VLAN이 없다.
  expect(within(dialog).queryByLabelText("Bridge")).not.toBeInTheDocument();
  expect(within(dialog).queryByLabelText("VLAN tag (optional)")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("MTU (optional)")).toHaveAccessibleDescription(
    /Defaults to 1450/,
  );
  await userEvent.type(within(dialog).getByLabelText("VNI"), "16777216");
  await userEvent.type(within(dialog).getByLabelText("MTU (optional)"), "1000");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("Use a number from 1 to 16777215.")).toBeVisible();
  expect(within(dialog).getByText("Use a number from 1280 to 9000.")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("VNI"));
  await userEvent.type(within(dialog).getByLabelText("VNI"), "100");
  await userEvent.clear(within(dialog).getByLabelText("MTU (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created network ov.")).toBeInTheDocument();
  expect(body).toEqual({ name: "ov", spec: { vxlan: { vni: 100 } } });
});

it("sends the MTU of a VXLAN network and explains a VNI in use", async () => {
  mockLists({ node: [bridged] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(
        { code: "vni_in_use", message: 'VNI 7 is used by network "ov"' },
        { status: 409 },
      );
    }),
  );
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "jumbo");
  await userEvent.selectOptions(within(dialog).getByLabelText("Type"), "vxlan");
  await userEvent.type(within(dialog).getByLabelText("VNI"), "7");
  await userEvent.type(within(dialog).getByLabelText("MTU (optional)"), "8950");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Another network already uses this VNI.",
  );
  expect(body).toEqual({ name: "jumbo", spec: { vxlan: { vni: 7 }, mtu: 8950 } });
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
