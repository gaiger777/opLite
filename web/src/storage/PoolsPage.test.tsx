import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { PoolsPage } from "@/storage/PoolsPage";
import { NODE, pool, poolState, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

it("lists pools with their nodes, space and volumes", async () => {
  mockLists({
    "storage-pool": [
      pool("local"),
      pool("busy", { nodes: ["n1"] }),
      pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } }),
      pool("tank", { backend: { kind: "zfs", dataset: "tank/opl" } }),
    ],
    node: [
      {
        ...NODE,
        status: { ...NODE.status, pools: { local: poolState("local"), busy: poolState("busy") } },
      },
    ],
    volume: [volume("disk", "busy")],
  });
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const local = (await within(table).findByText("local")).closest("tr")!;
  expect(local).toHaveTextContent("Directory");
  expect(local).toHaveTextContent("/srv/local");
  expect(local).toHaveTextContent("All nodes");
  expect(local).toHaveTextContent("node-a: Ready");
  // 용량은 노드가 알린 값이다.
  expect(local).toHaveTextContent("64 GiB / 128 GiB");
  const busy = within(table).getByText("busy").closest("tr")!;
  expect(busy).toHaveTextContent("node-a");
  expect(busy).not.toHaveTextContent("All nodes");
  expect(within(busy).getByRole("button", { name: "Delete busy" })).toBeDisabled();
  // 지울 수 없는 이유를 행에 글로 보인다.
  expect(busy).toHaveTextContent("Delete its volumes first.");
  expect(within(local).getByRole("button", { name: "Delete local" })).toBeEnabled();
  expect(local).not.toHaveTextContent("Delete its volumes first.");
  const fast = within(table).getByText("fast").closest("tr")!;
  expect(fast).toHaveTextContent("LVM-thin");
  expect(fast).toHaveTextContent("vg0/tp");
  expect(fast).toHaveTextContent("node-a: No status reported yet");
  const tank = within(table).getByText("tank").closest("tr")!;
  expect(tank).toHaveTextContent("ZFS");
  expect(tank).toHaveTextContent("tank/opl");
});

it("shows each node's pool state and space", async () => {
  const nas = pool("nas", { backend: { kind: "nfs", server: "fd00::5", export: "/srv/vm" } });
  const b = { ...NODE, id: "n2", name: "node-b" };
  const c = { ...NODE, id: "n3", name: "node-c" };
  // 꺼진 노드는 준비됐다고 알렸어도 오프라인이고 용량을 보이지 않는다.
  const d = {
    ...NODE,
    id: "n0",
    name: "node-d",
    status: {
      ...NODE.status,
      online: false,
      pools: { nas: poolState("nas", { available_bytes: 9 * 1024 ** 3 }) },
    },
  };
  mockLists({
    "storage-pool": [nas, pool("local")],
    node: [
      d,
      {
        ...NODE,
        status: {
          ...NODE.status,
          pools: {
            nas: poolState("nas", { ready: false, error: "access denied by server" }),
            local: poolState("local", { available_bytes: 1024 ** 3 }),
          },
        },
      },
      {
        ...b,
        status: {
          ...b.status,
          pools: {
            nas: poolState("nas"),
            local: poolState("local", { available_bytes: 2 * 1024 ** 3 }),
          },
        },
      },
      c,
    ],
    volume: [],
  });
  renderPage(<PoolsPage />);
  const table = await screen.findByRole("table", { name: "Storage pools" });
  const row = (await within(table).findByText("nas")).closest("tr")!;
  expect(row).toHaveTextContent("NFS");
  expect(row).toHaveTextContent("[fd00::5]:/srv/vm");
  expect(row).toHaveTextContent("node-a: Not ready");
  expect(row).toHaveTextContent("access denied by server");
  expect(row).toHaveTextContent("node-b: Ready");
  expect(row).toHaveTextContent("node-c: No status reported yet");
  expect(row).toHaveTextContent("node-d: Offline");
  // 공유 풀은 첫 준비된 노드의 값 하나다.
  const space = within(row).getAllByRole("cell")[4];
  expect(space).toHaveTextContent(/^64 GiB \/ 128 GiB$/);
  // 로컬 풀은 준비된 노드마다 값을 보인다.
  const local = within(table).getByText("local").closest("tr")!;
  expect(within(local).getAllByRole("cell")[4]).toHaveTextContent(
    "node-a: 1 GiB / 128 GiBnode-b: 2 GiB / 128 GiB",
  );
});

// 볼륨 목록을 읽기 전·읽지 못했을 때는 쓰지 않는 풀인지 모른다.
it("does not offer a delete without the volumes", async () => {
  mockLists({ "storage-pool": [pool("local")] });
  server.use(
    http.get(apiUrl("/volumes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<PoolsPage />);
  const local = (await screen.findByText("local")).closest("tr")!;
  expect(within(local).getByRole("button", { name: "Delete local" })).toBeDisabled();
  expect(within(local).getAllByRole("cell").at(-2)).toHaveTextContent("—");
});

it("creates a pool on the chosen nodes", async () => {
  mockLists({ node: [NODE, { ...NODE, id: "n2", name: "node-b" }] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("fast"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "Fast");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "srv/fast");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText(/lowercase letters/)).toBeVisible();
  expect(within(dialog).getByText("Enter an absolute path (starting with /).")).toBeVisible();
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
  await userEvent.clear(within(dialog).getByLabelText("Directory"));
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/srv/fast");
  await userEvent.click(await within(dialog).findByRole("checkbox", { name: "node-b" }));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() => expect(body).toBeDefined());
  expect(await screen.findByText("Created storage pool fast.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "fast",
    spec: { backend: { kind: "dir", path: "/srv/fast" }, nodes: ["n2"] },
  });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("creates a directory pool on shared storage", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("shared"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "shared");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/mnt/vm");
  const shared = within(dialog).getByRole("checkbox", { name: "Shared storage" });
  expect(shared).toHaveAccessibleDescription(
    "Check when every node sees the same files at this path, such as an NFS share mounted outside opLite.",
  );
  await userEvent.click(shared);
  // NFS 풀은 늘 공유라 고를 것이 없다.
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "nfs");
  expect(within(dialog).queryByRole("checkbox", { name: "Shared storage" })).toBeNull();
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "dir");
  expect(within(dialog).getByRole("checkbox", { name: "Shared storage" })).toBeChecked();
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "shared",
      spec: { backend: { kind: "dir", path: "/mnt/vm" }, nodes: [], shared: true },
    }),
  );
});

it("creates an NFS pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("nas"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "nas");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "nfs");
  expect(within(dialog).queryByLabelText("Directory")).not.toBeInTheDocument();
  await userEvent.type(within(dialog).getByLabelText("NFS server"), "[[fd00::5]"); // `[[`는 글자 `[`다.
  await userEvent.type(within(dialog).getByLabelText("Export path"), "srv/vm");
  await userEvent.type(within(dialog).getByLabelText("Mount options (optional)"), "vers=4.2, hard");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("NFS server")).toHaveAccessibleDescription(
    "Enter a host name or IP address without spaces, commas, slashes or brackets.",
  );
  expect(within(dialog).getByLabelText("Export path")).toHaveAccessibleDescription(
    "Enter an absolute path (starting with /) without control characters.",
  );
  const optionsError =
    "Separate the options with commas, without spaces. bind, rbind, remount, move, suid, dev, exec, helper, bg and x-* options are not allowed.";
  expect(within(dialog).getByLabelText("Mount options (optional)")).toHaveAccessibleDescription(
    optionsError,
  );
  // 서버처럼 마운트의 성격을 바꾸는 옵션도 받지 않는다.
  await userEvent.clear(within(dialog).getByLabelText("Mount options (optional)"));
  await userEvent.type(within(dialog).getByLabelText("Mount options (optional)"), "vers=4.2,Exec");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Mount options (optional)")).toHaveAccessibleDescription(
    optionsError,
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("NFS server"));
  await userEvent.type(within(dialog).getByLabelText("NFS server"), "fd00::5");
  await userEvent.clear(within(dialog).getByLabelText("Export path"));
  await userEvent.type(within(dialog).getByLabelText("Export path"), "/srv/vm");
  await userEvent.clear(within(dialog).getByLabelText("Mount options (optional)"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Created storage pool nas.")).toBeInTheDocument();
  // 옵션을 비우면 보내지 않고, 공유 여부는 서버가 정한다.
  expect(body).toEqual({
    name: "nas",
    spec: { backend: { kind: "nfs", server: "fd00::5", export: "/srv/vm" }, nodes: [] },
  });
});

it("creates an LVM-thin pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("fast"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Shared storage" }));
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "lvm-thin");
  expect(within(dialog).getByLabelText("Kind")).toHaveAccessibleDescription(
    "Create the volume group and thin pool on each node first. Volumes are raw thin volumes kept on the node that creates them, and their snapshots hold disks only. Needs Linux nodes running as root.",
  );
  // 노드 로컬 풀이라 공유를 고를 수 없고, 디렉터리·NFS 입력도 없다.
  expect(within(dialog).queryByRole("checkbox", { name: "Shared storage" })).toBeNull();
  expect(within(dialog).queryByLabelText("Directory")).toBeNull();
  expect(within(dialog).queryByLabelText("NFS server")).toBeNull();
  await userEvent.type(within(dialog).getByLabelText("Volume group"), "-vg");
  await userEvent.type(within(dialog).getByLabelText("Thin pool"), "thin pool");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  const nameError =
    "Use 1 to 127 letters, digits, +, _, . or -, not starting with -, snapshot or pvmove. LVM also reserves . and .. and names containing parts like _tdata or _tmeta.";
  expect(within(dialog).getByLabelText("Volume group")).toHaveAccessibleDescription(nameError);
  expect(within(dialog).getByLabelText("Thin pool")).toHaveAccessibleDescription(nameError);
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Volume group"));
  await userEvent.type(within(dialog).getByLabelText("Volume group"), "vg0");
  await userEvent.clear(within(dialog).getByLabelText("Thin pool"));
  await userEvent.type(within(dialog).getByLabelText("Thin pool"), "tp");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  // 디렉터리 풀에서 고른 공유는 보내지 않는다.
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "fast",
      spec: { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" }, nodes: [] },
    }),
  );
});

it("creates a ZFS pool", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("tank"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "tank");
  await userEvent.click(within(dialog).getByRole("checkbox", { name: "Shared storage" }));
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "zfs");
  expect(within(dialog).getByLabelText("Kind")).toHaveAccessibleDescription(
    "Create the dataset on each node first. Volumes are raw zvols kept on the node that creates them, and their snapshots hold disks only. Disks can only revert to their latest snapshot. Needs Linux nodes running as root.",
  );
  // 노드 로컬 풀이라 공유를 고를 수 없고, 다른 종류의 입력도 없다.
  expect(within(dialog).queryByRole("checkbox", { name: "Shared storage" })).toBeNull();
  expect(within(dialog).queryByLabelText("Directory")).toBeNull();
  expect(within(dialog).queryByLabelText("Volume group")).toBeNull();
  await userEvent.type(within(dialog).getByLabelText("Dataset"), "tank/-opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByLabelText("Dataset")).toHaveAccessibleDescription(
    "Use a dataset name such as tank/opl, at most 177 characters: parts separated by / with letters, digits, _, ., : or -, none empty, . or .. or starting with -. The pool name comes first and starts with a letter.",
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("Dataset"));
  await userEvent.type(within(dialog).getByLabelText("Dataset"), "tank/opl");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  // 디렉터리 풀에서 고른 공유는 보내지 않는다.
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "tank",
      spec: { backend: { kind: "zfs", dataset: "tank/opl" }, nodes: [] },
    }),
  );
});

it("sends NFS mount options", async () => {
  mockLists({ node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("nas"), { status: 201 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "nas");
  await userEvent.selectOptions(within(dialog).getByLabelText("Kind"), "nfs");
  await userEvent.type(within(dialog).getByLabelText("NFS server"), "nas.lan");
  await userEvent.type(within(dialog).getByLabelText("Export path"), "/srv/vm");
  await userEvent.type(within(dialog).getByLabelText("Mount options (optional)"), "vers=4.2,hard");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "nas",
      spec: {
        backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm", options: "vers=4.2,hard" },
        nodes: [],
      },
    }),
  );
});

// 고른 노드가 모두 빠진 채로 보내면 빈 목록이 되어 모든 노드의 풀이 된다.
it("does not create a pool when the chosen nodes are gone", async () => {
  mockLists({ node: [NODE, { ...NODE, id: "n2", name: "node-b" }] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/storage-pools"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(pool("fast"), { status: 201 });
    }),
  );
  const { queryClient } = renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "fast");
  await userEvent.type(within(dialog).getByLabelText("Directory"), "/srv/fast");
  await userEvent.click(await within(dialog).findByRole("checkbox", { name: "node-b" }));
  act(() => {
    queryClient.setQueryData(["list", "node"], { revision: 2, items: [NODE] });
  });
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByRole("group", { name: "Nodes" })).toHaveAccessibleDescription(
    "The chosen nodes are no longer in the cluster. Choose again.",
  );
  expect(body).toBeUndefined();
  // 빠진 노드는 고른 목록에서도 지웠으므로 다시 보내면 모든 노드의 풀이 된다.
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  await vi.waitFor(() =>
    expect(body).toEqual({
      name: "fast",
      spec: { backend: { kind: "dir", path: "/srv/fast" }, nodes: [] },
    }),
  );
});

it("says when the nodes cannot be read", async () => {
  mockLists({});
  server.use(
    http.get(apiUrl("/nodes"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create a storage pool" });
  expect(await within(dialog).findByRole("alert")).toBeInTheDocument();
  const nodes = within(dialog).getByRole("group", { name: "Nodes" });
  expect(within(nodes).queryByRole("checkbox")).not.toBeInTheDocument();
});

it("keeps the confirmation open when the pool is still in use", async () => {
  const local = pool("local");
  mockLists({ "storage-pool": [local] });
  server.use(
    http.delete(apiUrl(`/storage-pools/${local.id}`), () =>
      HttpResponse.json({ code: "pool_in_use", message: "in use" }, { status: 409 }),
    ),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete local" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "The pool still has volumes. Delete them first.",
  );
  expect(dialog).toBeVisible();
});

it("deletes an unused pool after confirming", async () => {
  const local = pool("local");
  mockLists({ "storage-pool": [local] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/storage-pools/${local.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<PoolsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete local" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Deleted storage pool local.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("hides changes from operators", async () => {
  mockLists({ "storage-pool": [pool("local")] });
  renderPage(<PoolsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByText("local")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete local" })).not.toBeInTheDocument();
});
