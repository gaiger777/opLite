import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { List } from "@/api/live";
import type { Node, Task } from "@/api/types";
import i18n from "@/i18n";
import { writeUserPref } from "@/lib/prefs";
import { expectNoAxeViolations } from "@/test/axe";
import { CEPH, LOADS, LOCAL, network, NODE_A, NODE_B, TEMPLATE } from "@/test/builder";
import { haGroup, isoVolume, securityGroup, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { BuilderPage } from "@/vmBuilder/BuilderPage";
import { readDraft } from "@/vmBuilder/draft";
import builderEn from "@/vmBuilder/i18n/en.json";
import builderKo from "@/vmBuilder/i18n/ko.json";
import { CHECK_DELAY_MS } from "@/vmBuilder/useServerCheck";
import { VmCreatePage } from "@/vms/VmFormPage";

/** 빌더의 세상: 영역 a·b의 두 노드, 공유 풀 ceph·로컬 풀, 네트워크 lan·SDN tenant, 보안 그룹, ISO, 템플릿 */
function world({
  features = false,
  nodes = [NODE_A, NODE_B],
}: { features?: boolean; nodes?: Node[] } = {}) {
  mockLists({
    node: nodes.map((n) =>
      features ? { ...n, status: { ...n.status, features: ["vm-check"] } } : n,
    ),
    "storage-pool": [CEPH, LOCAL],
    volume: [
      volume("spare", "ceph"),
      volume("tmpl-disk0", "ceph", { attached_to: TEMPLATE.id }),
      isoVolume("alpine", "ceph"),
    ],
    network: [network("lan"), network("tenant", true)],
    "security-group": [securityGroup("web"), securityGroup("ssh")],
    "ha-group": [haGroup("pair", 1)],
    vm: [TEMPLATE],
  });
  server.use(
    http.get(apiUrl("/placement"), () => HttpResponse.json({ leader: "x", nodes: LOADS })),
  );
}

function open() {
  return renderPage(<BuilderPage />, { path: "/vms/-new/builder" });
}

const part = (name: RegExp) => screen.getByRole("button", { name });
const slot = (name: string | RegExp) => screen.getByRole("group", { name });
/** 블록의 살피기 단추(이름이 칸 이름으로 시작 — 손잡이는 "Pick up …") */
const block = (name: string) =>
  screen.getByRole("button", { name: new RegExp(`^${name.replace(/[()]/g, "\\$&")} `) });
const live = () => document.querySelector("[aria-live=polite]")!;
const create = () => screen.getByRole("button", { name: "Create" });

/** 이름을 짓고 "보통 크기 Linux 서버로 시작"(보통 + Linux + ceph 새 디스크 + lan NIC) */
async function quickVm(user: ReturnType<typeof userEvent.setup>, name = "web") {
  await user.type(await screen.findByLabelText("Name"), name);
  await user.click(screen.getByRole("button", { name: "Start with a medium Linux server" }));
}

/** 키보드로 부품을 들어 그 칸까지 Tab으로 가서 Space로 놓는다. */
function keyDrop(from: HTMLElement, to: () => HTMLElement) {
  from.focus();
  fireEvent.keyDown(from, { key: " " });
  for (let i = 0; i < 30 && document.activeElement !== to(); i += 1) {
    fireEvent.keyDown(document.activeElement!, { key: "Tab" });
  }
  expect(to()).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: " " });
}

afterEach(() => {
  vi.restoreAllMocks();
  localStorage.clear();
});

it("starts with an empty chassis, a parts box and a quick start, without axe violations", async () => {
  world();
  const { container } = open();
  expect(
    await screen.findByRole("heading", { name: "Create a virtual machine" }),
  ).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "Builder" })).toHaveAttribute("aria-current", "page");
  expect(screen.getByRole("link", { name: "Form" })).toHaveAttribute("href", "/vms/-new");
  expect(
    within(slot(/^Disks \(top boots first\)/)).getByText("Drop a pool or a volume here."),
  ).toBeInTheDocument();
  expect(await screen.findByRole("button", { name: /^New disk in ceph/ })).toHaveAttribute(
    "aria-roledescription",
    "draggable part",
  );
  expect(part(/^Medium, 2 vCPU · 4 GiB/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /^Zone b/ })).toBeInTheDocument();
  // 동작 줄이기: 빌더의 모양에는 움직임(키프레임·애니메이션)이 없다.
  const style = container.querySelector("style")!.textContent!;
  expect(style).not.toMatch(/@keyframes|animation|transition/);
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("builds a VM with the keyboard only and gives focus back on Escape", async () => {
  world();
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  keyDrop(part(/^Medium/), () => slot("Compute"));
  expect(within(slot("Compute")).getByText("2 vCPU · 4 GiB · CPU host")).toBeInTheDocument();
  expect(live()).toHaveTextContent("Dropped Medium on Compute.");

  keyDrop(part(/^New disk in ceph/), () => slot(/^Disks \(top boots first\)/));
  // 놓으면 새 블록으로 초점이 가고 개수를 읽는다.
  const disk = screen.getByRole("button", {
    name: /^Disk 1 \(web-disk0\) web-disk0 · 20 GiB · ceph/,
  });
  expect(disk).toHaveFocus();
  expect(live()).toHaveTextContent("1 disk.");

  keyDrop(part(/^lan/), () => slot("NICs"));
  keyDrop(part(/^web, 0 rules/), () => slot("NIC 1 (lan)"));
  expect(within(slot("NICs")).getByText("web")).toBeInTheDocument();

  // 들고 Esc: 취소하고 초점은 든 부품으로
  const iso = part(/^alpine/);
  iso.focus();
  fireEvent.keyDown(iso, { key: "Enter" });
  expect(live()).toHaveTextContent(/Picked up alpine\. 1 place to drop/);
  fireEvent.keyDown(document.activeElement!, { key: "Tab" });
  expect(slot("CD drives")).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(iso).toHaveFocus();
  expect(live()).toHaveTextContent("Cancelled.");
  expect(screen.queryByText("alpine ·")).toBeNull();
});

it("moves between parts of a group with the arrow keys (one tab stop per group)", async () => {
  world();
  open();
  const small = await screen.findByRole("button", { name: /^Small/ });
  const medium = part(/^Medium/);
  expect(small).toHaveAttribute("tabindex", "0");
  expect(medium).toHaveAttribute("tabindex", "-1");
  small.focus();
  fireEvent.keyDown(small, { key: "ArrowDown" });
  expect(medium).toHaveFocus();
  expect(medium).toHaveAttribute("tabindex", "0");
});

it("picks with a tap and drops with a tap on a lit slot", async () => {
  world();
  open();
  const lan = await screen.findByRole("button", { name: /^lan/ });
  fireEvent.pointerDown(lan, { button: 0, isPrimary: true, clientX: 10, clientY: 10 });
  fireEvent.pointerUp(window, { clientX: 10, clientY: 10 });
  const nics = slot("NICs");
  expect(nics).toHaveAttribute("data-drop", "ok");
  expect(within(nics).getByText("Drop here")).toBeInTheDocument();
  expect(slot("Compute")).toHaveAttribute("data-drop", "no");
  fireEvent.click(nics);
  expect(block("NIC 1 (lan)")).toBeInTheDocument();
});

/** 디스크 둘(새 디스크 web-disk0, 있는 볼륨 spare)을 메뉴로 더한다. 블록 이름을 차례로 돌려주는 함수를 준다. */
async function twoDisks(user: ReturnType<typeof userEvent.setup>) {
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "Ways to add New disk in ceph" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Disks (top boots first)" }));
  await user.click(screen.getByRole("button", { name: "Ways to add spare" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Disks (top boots first)" }));
  const names = () =>
    within(slot(/^Disks \(top boots first\)/))
      .getAllByRole("button", { name: /^Disk \d/ })
      .map((b) => /^Disk \d \([^)]*\)/.exec(b.textContent ?? "")?.[0]);
  expect(names()).toEqual(["Disk 1 (web-disk0)", "Disk 2 (spare)"]);
  return names;
}

it("picks a block with a tap on its grip without moving it (the click after the tap)", async () => {
  world();
  open();
  const user = userEvent.setup();
  const names = await twoDisks(user);
  const grip = screen.getByRole("button", { name: "Pick up Disk 1 (web-disk0)" });
  // 실제 누르기: pointerdown → pointerup → 누른 원소의 click(바깥 칸들로 올라감)
  fireEvent.pointerDown(grip, { button: 0, isPrimary: true, clientX: 10, clientY: 10 });
  fireEvent.pointerUp(window, { clientX: 10, clientY: 10 });
  fireEvent.click(grip);
  expect(names()).toEqual(["Disk 1 (web-disk0)", "Disk 2 (spare)"]);
  expect(grip).toHaveAttribute("data-held");
  expect(live()).toHaveTextContent(/Picked up Disk 1 \(web-disk0\)/);
  // 다른 블록 줄을 누르면 그 자리에 놓는다(가장 안쪽 칸만 받는다).
  fireEvent.click(block("Disk 2 (spare)"));
  expect(names()).toEqual(["Disk 1 (spare)", "Disk 2 (web-disk1)"]);
});

it("does not undo while a block is held, and refuses a drop whose block is gone", async () => {
  world();
  open();
  const user = userEvent.setup();
  const names = await twoDisks(user);
  const grip = screen.getByRole("button", { name: "Pick up Disk 2 (spare)" });
  grip.focus();
  fireEvent.keyDown(grip, { key: " " });
  expect(screen.getByRole("button", { name: "Undo" })).toBeDisabled();
  fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
  expect(names()).toEqual(["Disk 1 (web-disk0)", "Disk 2 (spare)"]);
  // 첫 블록 자리에 놓는다(되돌리기가 블록을 지웠다면 페이지가 깨졌을 것).
  fireEvent.keyDown(document.activeElement!, { key: "Tab" });
  expect(slot("Disk 1 (web-disk0)")).toHaveFocus();
  fireEvent.keyDown(document.activeElement!, { key: " " });
  expect(names()).toEqual(["Disk 1 (spare)", "Disk 2 (web-disk1)"]);
  // 옮긴 블록으로 초점
  expect(block("Disk 1 (spare)")).toHaveFocus();
  expect(screen.getByRole("button", { name: "Undo" })).toBeEnabled();
  fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
  expect(names()).toEqual(["Disk 1 (web-disk0)", "Disk 2 (spare)"]);
});

it("gives focus to the slot heading after a keyboard drop on a slot that is not a list", async () => {
  world();
  open();
  await screen.findByLabelText("Name");
  keyDrop(part(/^Medium/), () => slot("Compute"));
  expect(screen.getByRole("heading", { name: "Compute" })).toHaveFocus();
});

it("drags with the mouse onto a slot and reads why another slot refuses", async () => {
  world();
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  const rects: Record<string, DOMRect> = {
    vm: new DOMRect(300, 0, 600, 1000),
    compute: new DOMRect(320, 100, 200, 100),
    disks: new DOMRect(320, 400, 500, 100),
    nics: new DOMRect(320, 600, 500, 150),
    "nics.0": new DOMRect(330, 620, 480, 40),
  };
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (
    this: HTMLElement,
  ) {
    return rects[this.dataset.slot ?? ""] ?? new DOMRect(0, 0, 0, 0);
  });
  const ceph = part(/^New disk in ceph/);
  fireEvent.pointerDown(ceph, { button: 0, isPrimary: true, clientX: 50, clientY: 50 });
  fireEvent.pointerMove(window, { clientX: 450, clientY: 450 });
  expect(slot(/^Disks \(top boots first\)/)).toHaveAttribute("data-over");
  expect(live()).toHaveTextContent("Disks (top boots first). Can drop here.");
  fireEvent.pointerUp(window, { clientX: 450, clientY: 450 });
  expect(block("Disk 1 (web-disk0)")).toBeInTheDocument();

  // NAT(user) NIC에는 보안 그룹을 걸 수 없다: 그 위에서 까닭을 읽고 놓지 않는다.
  fireEvent.pointerDown(part(/^user/), { button: 0, isPrimary: true, clientX: 50, clientY: 60 });
  fireEvent.pointerMove(window, { clientX: 450, clientY: 700 });
  fireEvent.pointerUp(window, { clientX: 450, clientY: 700 });
  fireEvent.pointerDown(part(/^web, 0 rules/), {
    button: 0,
    isPrimary: true,
    clientX: 50,
    clientY: 70,
  });
  fireEvent.pointerMove(window, { clientX: 450, clientY: 640 });
  expect(live()).toHaveTextContent(
    "NIC 1 (user). Cannot drop here: The user network (NAT) takes no security groups.",
  );
  fireEvent.pointerUp(window, { clientX: 450, clientY: 640 });
  expect(within(slot("NICs")).queryByText("web")).toBeNull();
});

it("adds a part from its menu, reorders and removes blocks from theirs, and undoes", async () => {
  world();
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "Ways to add New disk in ceph" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Disks (top boots first)" }));
  await user.click(screen.getByRole("button", { name: "Ways to add spare" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Disks (top boots first)" }));
  const names = () =>
    within(slot(/^Disks \(top boots first\)/))
      .getAllByRole("button", { name: /^Disk \d/ })
      .map((b) => /^Disk \d \([^)]*\)/.exec(b.textContent ?? "")?.[0]);
  expect(names()).toEqual(["Disk 1 (web-disk0)", "Disk 2 (spare)"]);
  await user.click(screen.getByRole("button", { name: "Actions for Disk 2 (spare)" }));
  await user.click(screen.getByRole("menuitem", { name: "Move up" }));
  expect(names()).toEqual(["Disk 1 (spare)", "Disk 2 (web-disk1)"]);
  await user.click(screen.getByRole("button", { name: "Actions for Disk 1 (spare)" }));
  await user.click(screen.getByRole("menuitem", { name: "Remove" }));
  expect(names()).toEqual(["Disk 1 (web-disk0)"]);
  await user.click(screen.getByRole("button", { name: "Undo" }));
  expect(names()).toEqual(["Disk 1 (spare)", "Disk 2 (web-disk1)"]);
  // ⌘Z·Ctrl+Z(입력 칸 밖)
  fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
  expect(names()).toEqual(["Disk 1 (web-disk0)", "Disk 2 (spare)"]);
});

it("edits the chosen block in the inspector: sliders, numbers and the form's cloud-init", async () => {
  world();
  open();
  const user = userEvent.setup();
  await quickVm(user);
  await user.click(within(slot("Compute")).getByRole("button", { name: "Inspect" }));
  const memory = screen.getByLabelText("Memory (MiB)", { selector: "input:not([type=range])" });
  await user.clear(memory);
  await user.type(memory, "8192");
  expect(within(slot("Compute")).getByText("2 vCPU · 8 GiB · CPU host")).toBeInTheDocument();
  // 메모리 밀대는 256 MiB 단위·로그 눈금
  fireEvent.change(screen.getByRole("slider", { name: "Memory (MiB)" }), {
    target: { value: "11" },
  });
  expect(memory).toHaveValue("2048");
  await user.click(screen.getByRole("button", { name: "Close settings" }));

  await user.click(screen.getByRole("button", { name: "Ways to add User and SSH keys" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to cloud-init" }));
  // cloud-init 조각을 놓으면 살피기가 폼의 cloud-init 칸으로 열린다.
  expect(
    await screen.findByRole("heading", { name: "Settings of cloud-init" }),
  ).toBeInTheDocument();
  await user.type(screen.getByLabelText("User"), "ops");
  expect(within(slot("cloud-init")).getByText("ops")).toBeInTheDocument();
});

it("checks with the server after a pause, shows problems on their slots and blocks Create on errors", async () => {
  world({ features: true });
  const seen: unknown[] = [];
  let reply: () => Response = () =>
    HttpResponse.json({
      ok: false,
      node: null,
      machine: null,
      cpu_model: null,
      ips: [],
      problems: [
        { at: "disks.0", code: "pool_space", message: "pool ceph is nearly full", level: "warn" },
        { at: "quota", code: "quota_exceeded", message: "over the quota", level: "error" },
      ],
    });
  server.use(
    http.post(apiUrl("/vm-checks"), async ({ request }) => {
      seen.push(await request.json());
      return reply();
    }),
  );
  open();
  const user = userEvent.setup();
  await quickVm(user);
  expect(
    await screen.findByText("Server check: 2 problems.", { selector: "p" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  // 하나만 날았다(입력마다가 아니라 쉰 뒤).
  expect(seen).toHaveLength(1);
  expect(seen[0]).toMatchObject({
    name: "web",
    volumes: [{ name: "web-disk0", spec: { pool: "ceph" } }],
    rules: [],
  });
  const disk = block("Disk 1 (web-disk0)").closest<HTMLElement>("[data-slot]")!;
  expect(disk).toHaveAttribute("data-level", "warn");
  expect(within(disk).getByText("pool ceph is nearly full")).toBeInTheDocument();
  expect(slot(/^VM, 1 problem/)).toHaveAttribute("data-level", "error");
  expect(create()).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Cannot create yet: over the quota" }),
  ).toBeInTheDocument();
  expect(live()).toHaveTextContent("Server check: 2 problems.");

  // 업그레이드 중(409 cluster_upgrading)은 알리기만 하고 만들기를 막지 않는다.
  reply = () =>
    HttpResponse.json({ code: "cluster_upgrading", message: "upgrading" }, { status: 409 });
  await user.type(screen.getByLabelText("Name"), "2");
  expect(await screen.findByText("Values changed; checking again.")).toBeInTheDocument();
  expect(
    await screen.findByText(/works after all nodes are upgraded/, {}, { timeout: 3000 }),
  ).toBeInTheDocument();
  expect(create()).toBeEnabled();
});

it("does not show a passed server check next to browser errors", async () => {
  world({ features: true });
  server.use(
    http.post(apiUrl("/vm-checks"), () =>
      HttpResponse.json({ ok: true, node: "n1", ips: [], problems: [] }),
    ),
  );
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  expect(
    await screen.findByText("Server check: passed.", { selector: "p" }, { timeout: 3000 }),
  ).toBeInTheDocument();
  await user.clear(screen.getByLabelText("Name"));
  expect(
    screen.getByText("The server check runs when the browser finds no errors."),
  ).toBeInTheDocument();
  expect(screen.queryByText("Server check: passed.", { selector: "p" })).toBeNull();
});

it("does not ask the server while the browser finds errors, or when a node lacks the feature", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    world({ features: false });
    open();
    // 이름이 없다(브라우저 오류) → 묻지 않는다.
    await screen.findByLabelText("Name");
    await act(() => vi.advanceTimersByTimeAsync(CHECK_DELAY_MS * 2));
    expect(
      screen.getByText("The server check runs when the browser finds no errors."),
    ).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "web" } });
    await act(() => vi.advanceTimersByTimeAsync(CHECK_DELAY_MS * 2));
    // 기능 `vm-check`가 없는 노드가 있다(msw가 모르는 요청은 실패한다 — 보내지 않았다).
    expect(screen.getByText(/works after all nodes are upgraded/)).toBeInTheDocument();
  } finally {
    vi.useRealTimers();
  }
});

it("creates the new volumes, then the zone rule, then the VM", async () => {
  world();
  const order: string[] = [];
  const bodies: Record<string, unknown> = {};
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      order.push("volume");
      bodies.volume = await request.json();
      return HttpResponse.json(task("t-vol", "volume-create", "v1"), { status: 202 });
    }),
    http.post(apiUrl("/placement-rules"), async ({ request }) => {
      order.push("rule");
      bodies.rule = await request.json();
      return HttpResponse.json({}, { status: 201 });
    }),
    http.post(apiUrl("/vms"), async ({ request }) => {
      order.push("vm");
      bodies.vm = await request.json();
      return HttpResponse.json(vm("web", "stopped"), { status: 201 });
    }),
  );
  const { queryClient } = open();
  const user = userEvent.setup();
  await quickVm(user);
  await user.click(screen.getByRole("button", { name: "Ways to add Zone b" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Placement" }));
  expect(within(slot("Placement")).getByText("Zone b")).toBeInTheDocument();
  await user.click(create());
  await waitFor(() => expect(order).toEqual(["volume"]));
  expect(bodies.volume).toEqual({
    name: "web-disk0",
    spec: { pool: "ceph", size_bytes: 20 * 1024 ** 3, format: "qcow2" },
  });
  // 볼륨 Task가 끝나야 규칙과 VM을 만든다.
  const done = {
    ...task("t-vol", "volume-create", "v1"),
    status: { state: "succeeded", error: null },
  };
  queryClient.setQueryData<List<Task>>(["list", "task"], { revision: 2, items: [done as Task] });
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(order).toEqual(["volume", "rule", "vm"]);
  expect(bodies.rule).toEqual({
    name: "web-zone",
    spec: { kind: "node-affinity", vms: ["web"], node_tag: "zone=b", enabled: true },
  });
  expect(bodies.vm).toMatchObject({
    name: "web",
    spec: {
      cpu: { cores: 2 },
      memory_mib: 4096,
      disks: [{ volume: "web-disk0" }],
      nics: [{ network: "lan" }],
    },
  });
});

it("says what was already made when the VM cannot be created, and does not make it again", async () => {
  world();
  let volumes = 0;
  server.use(
    http.post(apiUrl("/volumes"), () => {
      volumes += 1;
      return HttpResponse.json(task("t-vol", "volume-create", "v1"), { status: 202 });
    }),
    http.post(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "quota_exceeded", message: "over the quota" }, { status: 409 }),
    ),
  );
  const { queryClient } = open();
  const user = userEvent.setup();
  await quickVm(user);
  const done = {
    ...task("t-vol", "volume-create", "v1"),
    status: { state: "succeeded", error: null },
  };
  queryClient.setQueryData<List<Task>>(["list", "task"], { revision: 2, items: [done as Task] });
  await user.click(create());
  expect(await screen.findByRole("alert")).toHaveTextContent(/Already created: web-disk0\./);
  await user.click(create());
  await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("over the quota"));
  // 두 번째 실패에도 이미 만든 볼륨을 알린다.
  expect(screen.getByRole("alert")).toHaveTextContent(/Already created: web-disk0\./);
  expect(volumes).toBe(1);
});

it("reuses the zone rule of a failed create only while it is the same, and deletes a stale one", async () => {
  world({ features: true });
  const checks: { rules?: unknown[] }[] = [];
  const rules: unknown[] = [];
  const deleted: string[] = [];
  let vmReply = () =>
    HttpResponse.json({ code: "quota_exceeded", message: "over the quota" }, { status: 409 });
  server.use(
    http.post(apiUrl("/vm-checks"), async ({ request }) => {
      checks.push((await request.json()) as { rules?: unknown[] });
      return HttpResponse.json({ ok: true, node: null, ips: [], problems: [] });
    }),
    http.post(apiUrl("/placement-rules"), async ({ request }) => {
      const body = (await request.json()) as { name: string };
      rules.push(body);
      return HttpResponse.json({ ...body, id: `r${rules.length}` }, { status: 201 });
    }),
    http.delete(apiUrl("/placement-rules/:key"), ({ params }) => {
      deleted.push(String(params.key));
      return new HttpResponse(null, { status: 204 });
    }),
    http.post(apiUrl("/vms"), () => vmReply()),
  );
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "Ways to add Zone b" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Placement" }));
  await user.click(create());
  expect(await screen.findByRole("alert")).toHaveTextContent(/Already created: web-zone\./);
  // 만든 규칙은 확인 본문의 "만들 규칙"에서 빠진다(name_taken이 만들기를 막지 않게).
  await waitFor(() => expect(checks.at(-1)?.rules).toEqual([]), { timeout: 3000 });
  // 같은 것으로 다시: 규칙을 다시 만들지 않는다. 만든 것은 두 번째 실패에도 알린다.
  await user.click(create());
  await waitFor(() =>
    expect(screen.getByRole("alert")).toHaveTextContent(/Already created: web-zone\./),
  );
  expect(rules).toHaveLength(1);
  // hard → soft: 지난 규칙(r1)을 곧 지우고, 만들 때 새로 만든다.
  await user.click(within(slot("Placement")).getByRole("button", { name: "Inspect" }));
  await user.selectOptions(screen.getByLabelText("Zone b"), "soft");
  await waitFor(() => expect(deleted).toEqual(["r1"]));
  vmReply = () => HttpResponse.json(vm("web", "stopped"), { status: 201 });
  await user.click(create());
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(rules).toHaveLength(2);
  expect(rules[1]).toMatchObject({ name: "web-zone", spec: { kind: "soft-node-affinity" } });
});

it("deletes the zone rule of a failed create when the zone is removed or the VM renamed", async () => {
  world();
  const deleted: string[] = [];
  let made = 0;
  server.use(
    http.post(apiUrl("/placement-rules"), async ({ request }) => {
      made += 1;
      const body = (await request.json()) as { name: string };
      return HttpResponse.json({ ...body, id: `r${made}` }, { status: 201 });
    }),
    http.delete(apiUrl("/placement-rules/:key"), ({ params }) => {
      deleted.push(String(params.key));
      return new HttpResponse(null, { status: 204 });
    }),
    http.post(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "quota_exceeded", message: "over the quota" }, { status: 409 }),
    ),
  );
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(screen.getByRole("button", { name: "Ways to add Zone b" }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Placement" }));
  await user.click(create());
  expect(await screen.findByRole("alert")).toHaveTextContent(/Already created: web-zone\./);
  // 이름을 바꾸면 old 이름의 규칙(web-zone)을 지운다.
  await user.type(screen.getByLabelText("Name"), "2");
  await waitFor(() => expect(deleted).toEqual(["r1"]));
  await user.click(create());
  expect(await screen.findByRole("alert")).toHaveTextContent(/Already created: web2-zone\./);
  // 영역을 빼면(자동) 그 규칙도 지운다.
  await user.click(screen.getByRole("button", { name: /^Ways to add Automatic/ }));
  await user.click(screen.getByRole("menuitem", { name: "Add to Placement" }));
  await waitFor(() => expect(deleted).toEqual(["r1", "r2"]));
});

it("does not navigate after the VM is created when the user already left", async () => {
  world();
  let release!: () => void;
  const wait = new Promise<void>((r) => (release = r));
  server.use(
    http.post(apiUrl("/vms"), async () => {
      await wait;
      return HttpResponse.json(vm("web", "stopped"), { status: 201 });
    }),
  );
  const { router } = open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "web");
  await user.click(create());
  await act(() => router.navigate("/elsewhere"));
  release();
  await act(() => new Promise((r) => setTimeout(r, 50)));
  expect(router.state.location.pathname).toBe("/elsewhere");
});

it("keeps no draft or view in this tab when the view switch opens a new tab", async () => {
  world();
  open();
  await user_type_name();
  const form = screen.getByRole("link", { name: "Form" });
  fireEvent.click(form, { metaKey: true });
  fireEvent.click(form, { ctrlKey: true });
  fireEvent.click(form, { shiftKey: true });
  expect(readDraft()).toBeNull();
  expect(localStorage.getItem(`opl.${ADMIN.user}.vmCreate.mode`)).toBe("builder");
});

async function user_type_name() {
  await userEvent.setup().type(await screen.findByLabelText("Name"), "web");
}

it("deploys from a template with the clone request and keeps its devices fixed", async () => {
  world();
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl(`/vms/${TEMPLATE.id}/clone`), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(task("t-clone", "vm-clone", "from-tmpl"), { status: 202 });
    }),
  );
  open();
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "from-tmpl");
  await user.click(screen.getByRole("button", { name: /^Ways to add tmpl/ }));
  await user.click(screen.getByRole("menuitem", { name: "Add to VM" }));
  expect(screen.getByText("Base: template tmpl (deployed as a copy)")).toBeInTheDocument();
  expect(block("Disk 1 (tmpl-disk0)")).toBeInTheDocument();
  // 배포는 디스크를 더하지 못한다.
  await user.click(screen.getByRole("button", { name: "Ways to add New disk in ceph" }));
  expect(screen.getByRole("menuitem", { name: "Nothing takes this part now." })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await user.keyboard("{Escape}");
  // 배포는 HA·NIC 모델·노드 고정을 싣지 못한다: 놓지 못하고 살피기에도 없다.
  await user.click(screen.getByRole("button", { name: /^Ways to add High availability/ }));
  expect(screen.getByRole("menuitem", { name: "Nothing takes this part now." })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await user.keyboard("{Escape}");
  await user.click(within(slot("Protection")).getByRole("button", { name: "Inspect" }));
  expect(
    screen.getByText("A template deployment does not set HA. Turn it on after creating."),
  ).toBeInTheDocument();
  await user.click(block("NIC 1 (lan)"));
  expect(screen.queryByLabelText("Model")).toBeNull();
  expect(
    screen.getByText("The server checks a template deployment when you create it."),
  ).toBeInTheDocument();
  await user.click(create());
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(sent).toEqual([{ name: "from-tmpl", start: true }]);
});

it("shows the JSON and CLI of the same requests, with the password masked", async () => {
  world();
  open();
  const user = userEvent.setup();
  await quickVm(user);
  await user.click(screen.getByRole("tab", { name: "JSON" }));
  const json = screen.getByRole("tabpanel").textContent!;
  expect(json).toContain("# POST /api/v1/volumes");
  expect(json).toContain("# POST /api/v1/vms");
  expect(json.indexOf("/volumes")).toBeLessThan(json.indexOf("/api/v1/vms"));
  expect(screen.getByRole("button", { name: "Copy the JSON requests" })).toBeInTheDocument();
  await user.click(screen.getByRole("tab", { name: "CLI" }));
  expect(screen.getByRole("tabpanel")).toHaveTextContent(
    "orbivisorctl volume create web-disk0 --pool ceph --size 20G",
  );
  expect(screen.getByRole("tabpanel")).toHaveTextContent(/orbivisorctl vm create web/);
  // 고른 탭을 기억한다.
  expect(localStorage.getItem(`opl.${ADMIN.user}.vmBuilder.panel`)).toBe("cli");
});

it("hands the values to the form and back without putting the password in history or storage", async () => {
  world();
  renderPage(<VmCreatePage />, { path: "/vms/-new" });
  const user = userEvent.setup();
  await user.type(await screen.findByLabelText("Name"), "carry");
  await user.click(screen.getByRole("checkbox", { name: "Configure the guest with cloud-init" }));
  await user.type(screen.getByLabelText("Password"), "s3cret!");
  const builder = screen.getByRole("link", { name: "Builder" });
  expect(builder).toHaveAttribute("href", "/vms/-new/builder");
  await user.click(builder);
  expect(await screen.findByText("moved")).toBeInTheDocument();
  expect(localStorage.getItem(`opl.${ADMIN.user}.vmCreate.mode`)).toBe("builder");
  expect(readDraft()?.values.name).toBe("carry");
  expect(JSON.stringify(window.history.state ?? null)).not.toContain("s3cret");
  expect(sessionStorage.length).toBe(0);
  expect(JSON.stringify({ ...localStorage })).not.toContain("s3cret");

  // 빌더가 초안을 받는다(폼처럼 디스크 하나·NIC 하나).
  open();
  expect(await screen.findByLabelText("Name")).toHaveValue("carry");
  expect(block("NIC 1 (user)")).toBeInTheDocument();
  expect(readDraft()).toBeNull();
});

it("opens the builder from /vms/-new when it was the last view", async () => {
  world();
  writeUserPref(ADMIN.user, "vmCreate.mode", "builder");
  renderPage(<VmCreatePage />, { path: "/vms/-new", url: "/vms/-new?folder=x" });
  expect(await screen.findByText("moved")).toBeInTheDocument();
});

describe("on a phone", () => {
  beforeEach(() => {
    vi.spyOn(window, "matchMedia").mockImplementation(
      (query: string) =>
        ({
          matches: query.includes("max-width"),
          media: query,
          addEventListener: () => {},
          removeEventListener: () => {},
        }) as unknown as MediaQueryList,
    );
  });

  it("adds parts from a bottom drawer: press a part, then Drop here", async () => {
    world();
    const { container } = open();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Add parts" }));
    const drawer = screen.getByRole("dialog", { name: "Parts" });
    await expectNoAxeViolations(drawer);
    const lan = within(drawer).getByRole("button", { name: /^lan/ });
    fireEvent.pointerDown(lan, { button: 0, isPrimary: true, clientX: 10, clientY: 10 });
    fireEvent.pointerUp(window, { clientX: 10, clientY: 10 });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const nics = slot("NICs");
    expect(within(nics).getByText("Drop here")).toBeInTheDocument();
    await expectNoAxeViolations(container, { disable: ["region"] });
    fireEvent.click(within(nics).getByText("Drop here"));
    expect(block("NIC 1 (lan)")).toBeInTheDocument();
  });
});

it("has no axe violations with a full chassis and the inspector open", async () => {
  world();
  const { container } = open();
  const user = userEvent.setup();
  await quickVm(user);
  await user.click(block("NIC 1 (lan)"));
  expect(screen.getByRole("heading", { name: "Settings of NIC 1 (lan)" })).toBeInTheDocument();
  await expectNoAxeViolations(container, { disable: ["region"] });
});

it("reads in Korean", async () => {
  await i18n.changeLanguage("ko");
  world();
  open();
  expect(
    await screen.findByRole("button", { name: "보통 크기 Linux 서버로 시작" }),
  ).toBeInTheDocument();
  expect(screen.getByRole("link", { name: "빌더" })).toHaveAttribute("aria-current", "page");
});

it("has the same builder strings in English and Korean", () => {
  const keys = (o: object, p = ""): string[] =>
    Object.entries(o).flatMap(([k, v]) =>
      typeof v === "object" ? keys(v as object, `${p}${k}.`) : [`${p}${k}`],
    );
  expect(keys(builderKo).sort()).toEqual(keys(builderEn).sort());
});
