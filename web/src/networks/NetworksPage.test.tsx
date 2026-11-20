import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { Network } from "@/api/types";
import { bridgeName, NetworksPage, validBridge } from "@/networks/NetworksPage";
import {
  folder,
  folderId,
  NODE,
  providerNetwork,
  readyChassis,
  sdnNetwork,
  sdnNode,
  sdnView,
  vm,
} from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { expectNoAxeViolations } from "@/test/axe";
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
    'This network is still in use: network "lab" is used by VM "x"',
  );
});

it("hides changes from operators", async () => {
  mockLists({ network: [network("lan", "vmbr0")] });
  renderPage(<NetworksPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("lan")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
});

it("lets a folder network delegator create only VXLAN and SDN networks", async () => {
  // 브리지 네트워크는 루트 cluster.admin만 만든다(계획 62 리뷰 1). 폴더의 network.config는 VXLAN과 SDN(계획 65 §11).
  // 폴더 위임자는 루트 cluster.read가 없어 서버가 `GET /sdn`에 403이다 — 화면은 묻지 않고 서버에 맡긴다(리뷰 1 #5).
  const A = folderId("teamA");
  let sdnReads = 0;
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: [],
        folders: {},
        contents: { [A]: ["system.read", "network.config"] },
        objects: [],
      }),
    ),
    http.get(apiUrl("/sdn"), () => {
      sdnReads += 1;
      return HttpResponse.json({ code: "forbidden", message: "forbidden" }, { status: 403 });
    }),
  );
  mockLists({ folder: [folder("teamA", "network")] });
  const user = userEvent.setup();
  renderPage(<NetworksPage />, { as: { ...ADMIN, role: "none" } });
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = screen.getByRole("dialog", { name: "Create a network" });
  const kind = within(dialog).getByLabelText("Type");
  expect(
    within(kind)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["VXLAN", "SDN (OVN)"]);
  await user.selectOptions(kind, "sdn");
  expect(
    within(dialog).getByText(
      "The server checks that SDN (OVN) is set up when you create the network.",
    ),
  ).toBeInTheDocument();
  expect(within(dialog).queryByText(/Cannot read the SDN state/)).not.toBeInTheDocument();
  expect(sdnReads).toBe(0);
});

// --- SDN 네트워크(계획 65) ---

const TENANT = sdnNetwork("tenant", [{ cidr: "10.10.0.0/24", dns_servers: ["10.0.0.53"] }]);

/** 서버 `GET /networks/{key}/ips`의 모양: NB를 읽었으면 `live`(up·노드), 못 읽었으면 `live: null`과 `live_error` */
function ips(live: boolean) {
  return {
    network: "tenant",
    subnets: [{ cidr: "10.10.0.0/24", size: 253, used: 2, free: 251 }],
    allocations: [
      {
        vm: vm("db", "stopped").id,
        vm_name: "db",
        nic: 1,
        mac: "52:54:00:aa:bb:02",
        ips: ["10.10.0.3"],
        live: live ? { up: false } : null,
      },
      {
        vm: vm("web", "running").id,
        vm_name: "web",
        nic: 0,
        mac: "52:54:00:aa:bb:01",
        ips: ["10.10.0.2"],
        live: live ? { up: true, node: "node-a" } : null,
      },
    ],
    ...(!live && { live_error: "cannot reach the OVN NB: connection refused" }),
  };
}

it("lists SDN networks with their subnets, usage and ready chassis", async () => {
  const a = sdnNode(1, readyChassis(1, "leader"));
  const b = sdnNode(2, { chassis: "down" });
  mockLists({ network: [TENANT], node: [a, b] });
  const queries: string[] = [];
  server.use(
    http.get(apiUrl("/networks/:key/ips"), ({ request }) => {
      queries.push(new URL(request.url).search);
      return HttpResponse.json(ips(true));
    }),
  );
  renderPage(<NetworksPage />);
  const table = await screen.findByRole("table", { name: "Networks" });
  const headers = within(table).getAllByRole("columnheader");
  const row = (await within(table).findByText("tenant")).closest("tr")!;
  const cell = (name: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === name)];
  expect(cell("Type")).toHaveTextContent("SDN (OVN)");
  expect(cell("Bridge")).toHaveTextContent("br-int");
  expect(cell("VLAN/VNI")).toHaveTextContent("—");
  expect(cell("MTU")).toHaveTextContent("1442");
  expect(cell("Nodes with the bridge")).toHaveTextContent("node-a");
  expect(cell("Nodes with the bridge")).not.toHaveTextContent("node-b");
  expect(cell("Subnets")).toHaveTextContent("10.10.0.0/24");
  expect(await within(cell("Subnets")!).findByText("2/253")).toBeInTheDocument();
  expect(within(cell("Subnets")!).getByRole("meter", { name: "Subnet usage" })).toHaveAttribute(
    "aria-valuenow",
    "2",
  );
  // 목록은 OVN 포트 상태를 읽지 않는다(행마다 NB·SB를 읽지 않게, 리뷰 1 #10).
  expect(queries).toEqual(["?live=false"]);
});

it("shows IPv6 subnet sizes readably and counts the interfaces of hidden VMs", async () => {
  const v6 = sdnNetwork("six", [{ cidr: "fd00::/64", dhcp: false }]);
  mockLists({ network: [v6], node: [sdnNode(1, readyChassis(1))] });
  server.use(
    http.get(apiUrl("/networks/:key/ips"), () =>
      HttpResponse.json({
        network: "six",
        subnets: [
          // 서버의 u64 2^64-2: JSON 수로는 정확하지 않다.
          { cidr: "fd00::/64", size: 2 ** 64, used: 2, free: 2 ** 64 },
        ],
        allocations: [],
        hidden: 2,
      }),
    ),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  const table = await screen.findByRole("table", { name: "Networks" });
  expect(await within(table).findByText("2/≈1.8e19")).toBeInTheDocument();
  // 크기가 아주 크면 막대는 늘 0%라 그리지 않는다.
  expect(within(table).queryByRole("meter")).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Open six" }));
  const sheet = await screen.findByRole("dialog", { name: "Network six" });
  expect(await within(sheet).findByText("2 used of ≈1.8e19, ≈1.8e19 free")).toBeInTheDocument();
  // 다른 폴더 VM의 NIC는 서버가 빼고 수만 준다(리뷰 1 #3).
  expect(
    within(sheet).getByText(
      "2 more interfaces belong to VMs you cannot see; they count in the usage above.",
    ),
  ).toBeInTheDocument();
  expect(within(sheet).queryByText("No interfaces on this network.")).not.toBeInTheDocument();
});

it("blocks the SDN type until SDN is set up", async () => {
  mockLists({ node: [bridged] });
  renderPage(<NetworksPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  const option = await within(dialog).findByRole("option", { name: "SDN (OVN) — not set up" });
  expect(option).toBeDisabled();
  expect(
    within(dialog).getByText("SDN networks need SDN (OVN) set up first, under Networks › SDN."),
  ).toBeInTheDocument();
});

it("creates an SDN network with subnets", async () => {
  const a = sdnNode(1, readyChassis(1, "leader"));
  mockLists({ node: [a] });
  let body: unknown;
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([a], [a]))),
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(TENANT, { status: 201 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await user.type(within(dialog).getByLabelText("Name"), "tenant");
  await within(dialog).findByRole("option", { name: "SDN (OVN)" });
  await user.selectOptions(within(dialog).getByLabelText("Type"), "sdn");
  expect(within(dialog).queryByLabelText("VNI")).not.toBeInTheDocument();
  expect(within(dialog).getByLabelText("MTU (optional)")).toHaveAccessibleDescription(
    /Defaults to 1442/,
  );
  await user.click(within(dialog).getByRole("button", { name: "Add a subnet" }));
  await user.click(within(dialog).getByRole("button", { name: "Add a subnet" }));
  const first = within(dialog).getByRole("group", { name: "Subnet 1" });
  const second = within(dialog).getByRole("group", { name: "Subnet 2" });
  await user.type(within(first).getByLabelText("CIDR"), "10.10.0.0/24");
  await user.type(within(first).getByLabelText("DNS servers"), "10.0.0.53, 1.1.1.1");
  await user.type(within(first).getByLabelText("Allocation ranges"), "10.10.0.100-10.10.0.199");
  await user.click(within(first).getByLabelText("DHCP"));
  await user.type(within(second).getByLabelText("CIDR"), "10.20.0.0/24");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  // 한 네트워크에 IPv4 서브넷은 하나다.
  expect(
    within(second).getByText("A network has at most one IPv4 and one IPv6 subnet."),
  ).toBeVisible();
  await user.clear(within(second).getByLabelText("CIDR"));
  await user.type(within(second).getByLabelText("CIDR"), "fd00:10::/64");
  // IPv6 서브넷에는 DHCP가 없다.
  expect(within(second).queryByLabelText("DHCP")).not.toBeInTheDocument();
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created network tenant.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "tenant",
    spec: {
      sdn: {},
      subnets: [
        {
          cidr: "10.10.0.0/24",
          dhcp: false,
          dns_servers: ["10.0.0.53", "1.1.1.1"],
          pools: [{ start: "10.10.0.100", end: "10.10.0.199" }],
        },
        { cidr: "fd00:10::/64" },
      ],
    },
  });
});

it("shows the addresses of an SDN network and the OVN port state", async () => {
  mockLists({ network: [TENANT], node: [sdnNode(1, readyChassis(1))] });
  let live = true;
  server.use(http.get(apiUrl("/networks/:key/ips"), () => HttpResponse.json(ips(live))));
  const user = userEvent.setup();
  const { queryClient } = renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Open tenant" }));
  const sheet = await screen.findByRole("dialog", { name: "Network tenant" });
  const table = await within(sheet).findByRole("table", { name: "IP addresses" });
  const web = within(table).getByRole("link", { name: "web" }).closest("tr")!;
  expect(web).toHaveTextContent("#1");
  expect(web).toHaveTextContent("52:54:00:aa:bb:01");
  expect(web).toHaveTextContent("10.10.0.2");
  expect(web).toHaveTextContent("Up");
  expect(web).toHaveTextContent("node-a");
  expect(within(table).getByText("db").closest("tr")).toHaveTextContent("Down");
  expect(within(sheet).getByText("2 used of 253, 251 free")).toBeInTheDocument();
  await expectNoAxeViolations(sheet, { disable: ["region"] });
  // OVN을 읽지 못하면 상태를 모른다고 하고 까닭을 보인다.
  live = false;
  await queryClient.invalidateQueries({ queryKey: ["network-ips"] });
  expect(
    await within(sheet).findByText(
      "Could not read the port state from OVN: cannot reach the OVN NB: connection refused",
    ),
  ).toBeInTheDocument();
  expect(within(table).getByText("web").closest("tr")).not.toHaveTextContent("Up");
});

it("saves the whole subnet list with If-Match and names the VMs using a removed subnet", async () => {
  mockLists({ network: [TENANT], node: [] });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  let inUse = true;
  server.use(
    http.get(apiUrl("/networks/:key/ips"), () => HttpResponse.json(ips(true))),
    http.patch(apiUrl("/networks/:key"), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      if (inUse) {
        return HttpResponse.json(
          {
            code: "subnet_in_use",
            message:
              'subnet 10.10.0.0/24 of network "tenant" has addresses of VMs db, web; remove those NICs or change their addresses first',
          },
          { status: 409 },
        );
      }
      return HttpResponse.json({
        ...TENANT,
        revision: 5,
        spec: { sdn: {}, subnets: [{ cidr: "fd00::/64" }] },
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Open tenant" }));
  const sheet = await screen.findByRole("dialog", { name: "Network tenant" });
  await user.click(within(sheet).getByRole("button", { name: "Edit subnets" }));
  // 저장된 서브넷의 CIDR·게이트웨이는 바꿀 수 없다.
  const saved = within(sheet).getByRole("group", { name: "Subnet 1" });
  expect(within(saved).getByLabelText("CIDR")).toBeDisabled();
  expect(within(saved).getByLabelText("Gateway")).toBeDisabled();
  await user.click(within(sheet).getByRole("button", { name: "Remove subnet 1" }));
  await user.click(within(sheet).getByRole("button", { name: "Add a subnet" }));
  await user.type(
    within(within(sheet).getByRole("group", { name: "Subnet 1" })).getByLabelText("CIDR"),
    "fd00::/64",
  );
  await user.click(within(sheet).getByRole("button", { name: "Save" }));
  expect(await within(sheet).findByRole("alert")).toHaveTextContent(
    'The subnet is still in use: subnet 10.10.0.0/24 of network "tenant" has addresses of VMs db, web',
  );
  expect(sent).toEqual([{ ifMatch: "4", body: { subnets: [{ cidr: "fd00::/64" }] } }]);
  inUse = false;
  await user.click(within(sheet).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Saved the subnets of tenant.")).toBeInTheDocument();
  expect(sent).toHaveLength(2);
});

it("lets only network writers edit the subnets", async () => {
  mockLists({ network: [TENANT] });
  server.use(http.get(apiUrl("/networks/:key/ips"), () => HttpResponse.json(ips(true))));
  renderPage(<NetworksPage />, { as: { ...ADMIN, role: "viewer" } });
  await userEvent.click(await screen.findByRole("button", { name: "Open tenant" }));
  const sheet = await screen.findByRole("dialog", { name: "Network tenant" });
  expect(await within(sheet).findByRole("table", { name: "Subnets" })).toHaveTextContent(
    "10.0.0.53",
  );
  expect(within(sheet).queryByRole("button", { name: "Edit subnets" })).not.toBeInTheDocument();
});

it("marks interfaces without an address as a port that is off", async () => {
  // 리뷰 2 웹 I-1: 서브넷이 있는 네트워크의 주소 없는 NIC는 포트가 꺼져 있다(VM이 꺼진 "Down"과 다르다).
  mockLists({ network: [TENANT], node: [] });
  const data = ips(true);
  data.allocations[0]!.ips = [];
  server.use(http.get(apiUrl("/networks/:key/ips"), () => HttpResponse.json(data)));
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Open tenant" }));
  const sheet = await screen.findByRole("dialog", { name: "Network tenant" });
  const table = await within(sheet).findByRole("table", { name: "IP addresses" });
  const db = within(table).getByText("db").closest("tr")!;
  expect(db).toHaveTextContent("Off: no address");
  expect(db).not.toHaveTextContent("Down");
  expect(within(table).getByText("web").closest("tr")).toHaveTextContent("Up");
});

it("tells how many interfaces get an address from an added subnet and shows ip_exhausted", async () => {
  // 리뷰 2 웹 C-1: 서브넷을 더하면 서버가 주소 없는 인터페이스를 채운다. 빈 주소가 모자라면 409 ip_exhausted다.
  const l2 = sdnNetwork("l2");
  mockLists({ network: [l2], node: [] });
  const bare = {
    network: "l2",
    subnets: [],
    allocations: ["a", "b"].map((name, i) => ({
      vm: vm(name, "running").id,
      vm_name: name,
      nic: 0,
      mac: `52:54:00:aa:bb:0${i}`,
      ips: [],
      live: null,
    })),
  };
  server.use(
    http.get(apiUrl("/networks/:key/ips"), () => HttpResponse.json(bare)),
    http.patch(apiUrl("/networks/:key"), () =>
      HttpResponse.json(
        {
          code: "ip_exhausted",
          message:
            'network "l2": 2 interface(s) have no address and would get one from the new subnets, but 10.20.0.0/29 (1 free) cannot give that many',
        },
        { status: 409 },
      ),
    ),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Open l2" }));
  const sheet = await screen.findByRole("dialog", { name: "Network l2" });
  await user.click(within(sheet).getByRole("button", { name: "Edit subnets" }));
  expect(within(sheet).getByText(/interfaces that have no address get one/)).toBeInTheDocument();
  expect(within(sheet).queryByRole("status")).not.toBeInTheDocument();
  await user.click(within(sheet).getByRole("button", { name: "Add a subnet" }));
  await user.type(
    within(within(sheet).getByRole("group", { name: "Subnet 1" })).getByLabelText("CIDR"),
    "10.20.0.0/29",
  );
  expect(await within(sheet).findByRole("status")).toHaveTextContent(
    "2 interfaces on this network have no address; saving gives each one from the new subnet.",
  );
  await user.click(within(sheet).getByRole("button", { name: "Save" }));
  expect(await within(sheet).findByRole("alert")).toHaveTextContent(
    'The subnet has no free addresses left: network "l2": 2 interface(s) have no address',
  );
});

it("reloads the revision after a conflict and keeps the typed rows", async () => {
  // 리뷰 2 웹 M-1: 412 revision_mismatch 뒤 옛 리비전으로 다시 보내면 늘 412였다.
  mockLists({ network: [TENANT], node: [] });
  const sent: (string | null)[] = [];
  server.use(
    http.get(apiUrl("/networks/:key/ips"), () => HttpResponse.json(ips(true))),
    http.get(apiUrl("/networks/:key"), () =>
      HttpResponse.json({
        ...TENANT,
        revision: 7,
        spec: { ...TENANT.spec, subnets: [{ cidr: "10.10.0.0/24" }, { cidr: "fd00::/64" }] },
      }),
    ),
    http.patch(apiUrl("/networks/:key"), ({ request }) => {
      sent.push(request.headers.get("If-Match"));
      if (sent.length === 1) {
        return HttpResponse.json(
          { code: "revision_mismatch", message: "revision mismatch: expected 4, actual 7" },
          { status: 412 },
        );
      }
      return HttpResponse.json({ ...TENANT, revision: 8 });
    }),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Open tenant" }));
  const sheet = await screen.findByRole("dialog", { name: "Network tenant" });
  await user.click(within(sheet).getByRole("button", { name: "Edit subnets" }));
  await user.click(within(sheet).getByRole("button", { name: "Add a subnet" }));
  const added = within(sheet).getByRole("group", { name: "Subnet 2" });
  await user.type(within(added).getByLabelText("CIDR"), "fd10::/64");
  await user.click(within(sheet).getByRole("button", { name: "Save" }));
  expect(await within(sheet).findByRole("alert")).toHaveTextContent(
    "Someone changed the subnets meanwhile (now: 10.10.0.0/24, fd00::/64). Your rows are kept",
  );
  expect(
    within(within(sheet).getByRole("group", { name: "Subnet 2" })).getByLabelText("CIDR"),
  ).toHaveValue("fd10::/64");
  await user.click(within(sheet).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(sent).toEqual(["4", "7"]));
  expect((await screen.findAllByText("Saved the subnets of tenant.")).length).toBeGreaterThan(0);
});

it("lists a provider network with its physnet, MTU and the nodes that have the physnet", async () => {
  const a = sdnNode(1, {
    ...readyChassis(1, "leader"),
    physnets: [{ name: "ext", bridge: "vmbr1", ready: true }],
  });
  const b = sdnNode(2, {
    ...readyChassis(2),
    physnets: [{ name: "ext", bridge: "vmbr1", ready: false, error: "no bridge vmbr1" }],
  });
  const ext = providerNetwork("ext", "ext", [], 100);
  const lone = providerNetwork("lone", "dmz");
  mockLists({ node: [a, b], network: [ext, lone] });
  server.use(
    http.get(apiUrl("/networks/:key/ips"), () =>
      HttpResponse.json({ network: "ext", subnets: [], allocations: [] }),
    ),
  );
  renderPage(<NetworksPage />);
  const table = await screen.findByRole("table", { name: "Networks" });
  const row = (await within(table).findByRole("button", { name: "Open ext" })).closest("tr")!;
  expect(row).toHaveTextContent("External (provider)");
  expect(row).toHaveTextContent("ext:100");
  expect(row).toHaveTextContent("1500");
  expect(await within(row).findByText("node-a")).toBeVisible();
  const other = within(table).getByRole("button", { name: "Open lone" }).closest("tr")!;
  expect(other).toHaveTextContent("No node has physnet dmz ready");
});

it("creates a provider network with a physnet, VLAN and floating IP ranges", async () => {
  const a = sdnNode(1, {
    ...readyChassis(1, "leader"),
    physnets: [{ name: "ext", bridge: "vmbr1", ready: true }],
  });
  mockLists({ node: [a] });
  let body: unknown;
  server.use(
    http.get(apiUrl("/sdn"), () => HttpResponse.json(sdnView([a], [a]))),
    http.post(apiUrl("/networks"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(providerNetwork("ext", "ext", [], 100), {
        status: 201,
        headers: { "orbivisor-warning": "no node has physnet ext ready yet" },
      });
    }),
  );
  const user = userEvent.setup();
  renderPage(<NetworksPage />);
  await user.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a network" });
  await user.type(within(dialog).getByLabelText("Name"), "ext");
  await within(dialog).findByRole("option", { name: "External (provider)" });
  await user.selectOptions(within(dialog).getByLabelText("Type"), "provider");
  expect(within(dialog).getByLabelText("MTU (optional)")).toHaveAccessibleDescription(
    /Defaults to 1500/,
  );
  await user.type(within(dialog).getByLabelText("Physical network"), "Ext");
  await user.type(within(dialog).getByLabelText("VLAN tag (optional)"), "100");
  await user.click(within(dialog).getByRole("button", { name: "Add a subnet" }));
  const subnet = within(dialog).getByRole("group", { name: "Subnet 1" });
  // 공급자 서브넷의 DHCP는 기본 끔(물리 망의 DHCP 서버)
  expect(within(subnet).getByLabelText("DHCP")).not.toBeChecked();
  await user.type(within(subnet).getByLabelText("CIDR"), "203.0.113.0/24");
  await user.type(within(subnet).getByLabelText("Gateway"), "203.0.113.1");
  await user.type(
    within(subnet).getByLabelText("Floating IP ranges"),
    "203.0.113.100-203.0.113.199",
  );
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(
    within(dialog).getByText(
      "Use a lowercase letter, then up to 30 lowercase letters, digits or -.",
    ),
  ).toBeVisible();
  await user.clear(within(dialog).getByLabelText("Physical network"));
  await user.type(within(dialog).getByLabelText("Physical network"), "ext");
  await user.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created network ext.")).toBeInTheDocument();
  expect(await screen.findByText("no node has physnet ext ready yet")).toBeInTheDocument();
  expect(body).toEqual({
    name: "ext",
    spec: {
      sdn: { provider: { physnet: "ext", vlan: 100 } },
      subnets: [
        {
          cidr: "203.0.113.0/24",
          gateway: "203.0.113.1",
          floating_pools: [{ start: "203.0.113.100", end: "203.0.113.199" }],
        },
      ],
    },
  });
});
