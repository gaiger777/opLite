// SDN NIC 주소(계획 65)의 VM 폼·상세·복제·배포 화면
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Vm } from "@/api/types";
import { resetTasks } from "@/tasks/tasks";
import { TemplatesPage } from "@/templates/TemplatesPage";
import {
  floatingIpView,
  NODE,
  pool,
  securityGroup,
  sdnNetwork,
  snapshot,
  task,
  vm,
  volume,
} from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";
import { VmEditPage } from "@/vms/VmFormPage";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const TENANT = sdnNetwork("tenant", [{ cidr: "10.10.0.0/24" }]);
const LAN = { id: "net-lan", name: "lan", revision: 1, spec: { bridge: "br0" }, status: {} };

function withNics(v: Vm, nics: Vm["spec"]["nics"], extra: Partial<Vm["spec"]> = {}): Vm {
  return { ...v, spec: { ...v.spec, nics, ...extra } };
}

it("gives an SDN interface addresses and keeps its security groups", async () => {
  // 만들기는 그림 화면(계획 77-5)이라 같은 NIC 칸을 멈춘 VM 고치기에서 본다.
  const web = withNics(vm("web", "stopped"), [
    { network: "user", model: "virtio", mac: "52:54:00:00:00:01" },
  ]);
  mockLists({
    vm: [web],
    node: [NODE],
    network: [LAN, TENANT],
    "security-group": [securityGroup("ssh")],
  });
  const bodies: { spec: Vm["spec"] }[] = [];
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      bodies.push((await request.json()) as (typeof bodies)[number]);
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const user = userEvent.setup();
  const nic = await screen.findByRole("group", { name: "Network interface 1" });
  await within(nic).findByRole("option", { name: "tenant" });
  await user.selectOptions(within(nic).getByLabelText("Network"), "lan");
  await user.click(await within(nic).findByRole("checkbox", { name: "ssh" }));
  // SDN 네트워크로 옮기면 주소 칸이 생기고 그룹은 그대로다(계획 66 — OVN ACL).
  await user.selectOptions(within(nic).getByLabelText("Network"), "tenant");
  expect(within(nic).getByRole("checkbox", { name: "ssh" })).toBeChecked();
  expect(within(nic).getByText(/Port security lets this interface send only/)).toBeVisible();
  const ips = within(nic).getByLabelText("IP addresses");
  expect(ips).toHaveAttribute("placeholder", "Automatic");
  await user.type(ips, "10.10.0.5, 10.10.0.6");
  // cloud-init의 그 NIC 주소는 IPAM이 정한다.
  await user.click(screen.getByLabelText("Set up the guest on first boot (cloud-init)"));
  expect(
    screen.getByText("NIC 1 is on an SDN network: its addresses come from the subnet (IPAM)."),
  ).toBeVisible();
  expect(screen.queryByLabelText("nic0 IPv4")).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Save" }));
  expect(within(nic).getByText("Give at most one IPv4 and one IPv6 address.")).toBeVisible();
  expect(bodies).toEqual([]);
  await user.clear(ips);
  await user.type(ips, "10.10.0.5");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]!.spec.nics).toEqual([
    {
      network: "tenant",
      model: "virtio",
      mac: "52:54:00:00:00:01",
      security_groups: ["ssh"],
      ips: ["10.10.0.5"],
    },
  ]);
  expect(bodies[0]!.spec.cloud_init?.networks).toBeUndefined();
});

it("keeps the addresses of a running VM's SDN interface", async () => {
  const web = withNics(vm("web", "running"), [
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2"] },
  ]);
  mockLists({ vm: [web], node: [NODE], network: [TENANT] });
  let body: { spec: Vm["spec"] } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const ips = await screen.findByLabelText("IP addresses");
  expect(ips).toHaveValue("10.10.0.2");
  expect(ips).toBeDisabled();
  expect(ips).toHaveAccessibleDescription("Stop the VM to change the addresses of this interface.");
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.nics).toEqual([
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2"] },
  ]);
});

it("shows each interface's addresses on the VM page", async () => {
  const web = withNics(vm("web", "stopped"), [
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2", "fd00::2"] },
    { network: "lan", model: "virtio", mac: "52:54:00:00:00:02" },
  ]);
  mockLists({ vm: [web], node: [NODE] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await userEvent.click(await screen.findByRole("tab", { name: "Hardware" }));
  const table = screen.getByRole("table", { name: "Network interfaces" });
  expect(within(table).getByText("tenant").closest("tr")).toHaveTextContent("10.10.0.2, fd00::2");
  expect(within(table).getByText("lan").closest("tr")).toHaveTextContent("52:54:00:00:00:02—");
});

it("lets a running VM give an address to an interface that has none", async () => {
  // 리뷰 2 N-4·웹 I-1: 주소가 없던 NIC(포트 꺼짐)는 켜진 채로 주소를 받는다(서버 `check_running_ips`).
  const web = withNics(vm("web", "running"), [
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:01" },
  ]);
  mockLists({ vm: [web], node: [NODE], network: [TENANT] });
  let body: { spec: Vm["spec"] } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const ips = await screen.findByLabelText("IP addresses");
  expect(ips).toBeEnabled();
  expect(ips).toHaveAccessibleDescription(
    "This interface has no address, so its port is off. You can give it one now; the guest picks it up at its next DHCP renewal.",
  );
  const user = userEvent.setup();
  await user.type(ips, "10.10.0.9");
  await user.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.nics![0]!.ips).toEqual(["10.10.0.9"]);
});

it("marks an SDN interface without an address as off on the VM page", async () => {
  const web = withNics(vm("web", "running"), [
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:01" },
    { network: "l2", model: "virtio", mac: "52:54:00:00:00:02" },
  ]);
  mockLists({ vm: [web], node: [NODE], network: [TENANT, sdnNetwork("l2")] });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await userEvent.click(await screen.findByRole("tab", { name: "Hardware" }));
  const table = screen.getByRole("table", { name: "Network interfaces" });
  expect(await within(table).findByText("Off: no address")).toBeInTheDocument();
  expect(within(table).getByText("tenant").closest("tr")).toHaveTextContent("Off: no address");
  // 서브넷이 없는 SDN 네트워크(L2만)는 MAC만으로 켜진다.
  expect(within(table).getByText("l2").closest("tr")).not.toHaveTextContent("Off");
});

it("clones a VM with an address for its SDN interface", async () => {
  const db = withNics(
    vm("db", "stopped"),
    [
      { network: "lan", model: "virtio", mac: "52:54:00:00:00:01" },
      { network: "tenant", model: "virtio", mac: "52:54:00:00:00:02", ips: ["10.10.0.2"] },
    ],
    { disks: [{ volume: "db-root", bus: "virtio-blk", boot_order: 1 }] },
  );
  mockLists({
    vm: [db],
    node: [NODE],
    network: [LAN, TENANT],
    volume: [volume("db-root", "local")],
    "storage-pool": [pool("local")],
  });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${db.id}/clone`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-clone", db.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: /^Actions for / }));
  await user.click(screen.getByRole("menuitem", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: /Clone db/ });
  await user.type(within(dialog).getByLabelText("New VM name"), "db2");
  // 단순 네트워크 NIC에는 주소 칸이 없다.
  const ips = await within(dialog).findByLabelText("Interface 2 (tenant) IP addresses");
  expect(within(dialog).queryByLabelText(/Interface 1/)).not.toBeInTheDocument();
  await user.type(ips, "10.10.0.9");
  await user.click(within(dialog).getByRole("button", { name: "Clone" }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toMatchObject({ name: "db2", ips: [[], ["10.10.0.9"]] });
});

it("deploys a template with an address only for one VM", async () => {
  const tmpl = withNics(
    { ...vm("tmpl", "stopped"), spec: { ...vm("tmpl", "stopped").spec, template: true } },
    [{ network: "tenant", model: "virtio", mac: "52:54:00:00:00:09" }],
    { disks: [{ volume: "tmpl-root", bus: "virtio-blk", boot_order: 1 }] },
  );
  mockLists({
    vm: [tmpl],
    node: [NODE],
    network: [LAN, TENANT],
    volume: [volume("tmpl-root", "local")],
    "storage-pool": [pool("local")],
  });
  const bodies: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${tmpl.id}/clone`), async ({ request }) => {
      bodies.push(await request.json());
      return HttpResponse.json(task("t1", "vm-clone", tmpl.id), { status: 202 });
    }),
  );
  renderPage(<TemplatesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Deploy from tmpl" }));
  const dialog = await screen.findByRole("dialog", { name: "Deploy from tmpl" });
  const ips = await within(dialog).findByLabelText("Interface 1 IP addresses");
  await user.type(ips, "10.10.0.9");
  await user.clear(within(dialog).getByLabelText("Count"));
  await user.type(within(dialog).getByLabelText("Count"), "2");
  await user.click(within(dialog).getByRole("button", { name: /^Deploy/ }));
  expect(
    within(dialog).getByText(
      "One address cannot go to several VMs. Deploy one VM or leave it empty.",
    ),
  ).toBeVisible();
  await user.clear(within(dialog).getByLabelText("Count"));
  await user.type(within(dialog).getByLabelText("Count"), "1");
  // 다른 네트워크로 바꾸면 주소 칸이 사라진다.
  await user.selectOptions(within(dialog).getByLabelText("nic0 network"), "lan");
  expect(within(dialog).queryByLabelText("Interface 1 IP addresses")).not.toBeInTheDocument();
  await user.selectOptions(within(dialog).getByLabelText("nic0 network"), "");
  await user.type(within(dialog).getByLabelText("Interface 1 IP addresses"), "10.10.0.9");
  await user.click(within(dialog).getByRole("button", { name: /^Deploy/ }));
  await waitFor(() => expect(bodies).toHaveLength(1));
  expect(bodies[0]).toMatchObject({ ips: [["10.10.0.9"]] });
});

it("keeps security groups on an SDN interface and saved addresses when empty", async () => {
  // 계획 66: SDN NIC의 그룹은 OVN ACL로 걸리므로 그대로 보낸다. 리뷰 1 #9: 저장된 NIC의 빈 주소 칸은 "지금 주소를 둠"이다.
  const web = withNics(vm("web", "stopped"), [
    {
      network: "tenant",
      model: "virtio",
      mac: "52:54:00:00:00:01",
      ips: ["10.10.0.2"],
      security_groups: ["ssh"],
    },
  ]);
  mockLists({
    vm: [web],
    node: [NODE],
    network: [TENANT],
    "security-group": [securityGroup("ssh")],
  });
  let body: { spec: Vm["spec"] } | undefined;
  server.use(
    http.put(apiUrl(`/vms/${web.id}`), async ({ request }) => {
      body = (await request.json()) as typeof body;
      return HttpResponse.json(web);
    }),
  );
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/web/edit" });
  const ips = await screen.findByLabelText("IP addresses");
  expect(ips).toBeEnabled();
  expect(ips).toHaveAccessibleDescription(/Leave empty to keep the current addresses/);
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(body!.spec.nics).toEqual([
    {
      network: "tenant",
      model: "virtio",
      mac: "52:54:00:00:00:01",
      security_groups: ["ssh"],
      ips: ["10.10.0.2"],
    },
  ]);
});

it("has no address field for a template's SDN interface", async () => {
  // 리뷰 1 #9: 서버가 템플릿의 주소를 늘 지운다.
  const tmpl = withNics(
    vm("tmpl", "stopped"),
    [{ network: "tenant", model: "virtio", mac: "52:54:00:00:00:09" }],
    { template: true },
  );
  mockLists({ vm: [tmpl], node: [NODE], network: [TENANT] });
  renderPage(<VmEditPage />, { path: "/vms/:name/edit", url: "/vms/tmpl/edit" });
  const nic = await screen.findByRole("group", { name: "Network interface 1" });
  expect(await within(nic).findByText(/Port security lets this interface send only/)).toBeVisible();
  expect(within(nic).queryByLabelText("IP addresses")).not.toBeInTheDocument();
});

it("clears typed clone addresses when the source changes", async () => {
  // 리뷰 1 #11: 주소는 NIC 순번별이다. 스냅샷의 NIC가 다르면 다른 NIC에 가면 안 된다.
  const db = withNics(
    vm("db", "stopped"),
    [{ network: "tenant", model: "virtio", mac: "52:54:00:00:00:02" }],
    { disks: [{ volume: "db-root", bus: "virtio-blk", boot_order: 1 }] },
  );
  const old = snapshot("before", db, 1);
  mockLists({
    vm: [db],
    snapshot: [old],
    node: [NODE],
    network: [LAN, TENANT],
    volume: [volume("db-root", "local")],
    "storage-pool": [pool("local")],
  });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/db" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: /^Actions for / }));
  await user.click(screen.getByRole("menuitem", { name: "Clone" }));
  const dialog = await screen.findByRole("dialog", { name: /Clone db/ });
  const ips = await within(dialog).findByLabelText("Interface 1 (tenant) IP addresses");
  await user.type(ips, "10.10.0.9");
  await within(dialog).findByRole("option", { name: "Snapshot before" });
  await user.selectOptions(within(dialog).getByLabelText("Source"), old.id);
  expect(within(dialog).getByLabelText("Interface 1 (tenant) IP addresses")).toHaveValue("");
});

it("shows the floating IP of an SDN interface on the VM page", async () => {
  // 계획 66: 유동 IP는 SDN NIC의 MAC으로 붙는다(서버 `FloatingIpView.spec.target`).
  const web = withNics(vm("web", "running"), [
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:01", ips: ["10.10.0.2"] },
    { network: "tenant", model: "virtio", mac: "52:54:00:00:00:03", ips: ["10.10.0.3"] },
  ]);
  mockLists({ vm: [web], node: [NODE], network: [TENANT] });
  server.use(
    http.get(apiUrl("/floating-ips"), () =>
      HttpResponse.json({
        revision: 3,
        items: [
          floatingIpView(
            "203.0.113.120",
            "ext",
            { kind: "distributed", node: "node-a" },
            { target: { vm: web.id, mac: "52:54:00:00:00:01" }, vm_name: "web" },
          ),
        ],
      }),
    ),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await userEvent.click(await screen.findByRole("tab", { name: "Hardware" }));
  const table = screen.getByRole("table", { name: "Network interfaces" });
  expect(await within(table).findByRole("columnheader", { name: "Public IP" })).toBeVisible();
  const link = await within(table).findByRole("link", { name: "Public IP 203.0.113.120" });
  expect(link).toHaveAttribute("href", "/floating-ips");
  expect(within(table).getByText("10.10.0.3").closest("tr")).toHaveTextContent(/—$/);
});

it("lets the VM owner detach a floating IP and shows NIC QoS on the VM page (plan 67)", async () => {
  // VM 주인은 `vm.config`만 있고 유동 IP의 `network.config`는 없다(서버는 `{target: null}`이면 VM 쪽 권한으로 뗀다).
  const web = withNics(vm("web", "running"), [
    {
      network: "tenant",
      model: "virtio",
      mac: "52:54:00:00:00:01",
      ips: ["10.10.0.2"],
      qos: { out_mbps: 100, in_mbps: 200, dscp: 46 },
    },
  ]);
  const fip = floatingIpView(
    "203.0.113.120",
    "ext",
    { kind: "distributed", node: "node-a" },
    { target: { vm: web.id, mac: "52:54:00:00:00:01" }, vm_name: "web" },
  );
  mockLists({ vm: [web], node: [NODE], network: [TENANT] });
  const sent: { ifMatch: string | null; body: unknown }[] = [];
  server.use(
    http.get(apiUrl("/permissions/mine"), () =>
      HttpResponse.json({
        all: false,
        root: [],
        root_contents: ["system.read", "vm.config", "vm.power"],
        folders: {},
        contents: {},
        objects: [],
      }),
    ),
    http.get(apiUrl("/floating-ips"), () => HttpResponse.json({ revision: 3, items: [fip] })),
    http.patch(apiUrl(`/floating-ips/${fip.id}`), async ({ request }) => {
      sent.push({ ifMatch: request.headers.get("If-Match"), body: await request.json() });
      return HttpResponse.json({ ...fip, spec: { network: "ext", address: "203.0.113.120" } });
    }),
  );
  const user = userEvent.setup();
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  await user.click(await screen.findByRole("tab", { name: "Hardware" }));
  const table = screen.getByRole("table", { name: "Network interfaces" });
  expect(within(table).getByText("↑ 100 Mbit/s · ↓ 200 Mbit/s · DSCP 46")).toBeVisible();
  await user.click(await within(table).findByRole("button", { name: "Detach 203.0.113.120" }));
  await user.click(await screen.findByRole("button", { name: "Detach" }));
  await waitFor(() => expect(sent).toEqual([{ ifMatch: "8", body: { target: null } }]));
});
