import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { List } from "@/api/live";
import type { Volume } from "@/api/types";
import { volumeNameError } from "@/storage/volumes";
import { VolumesPage } from "@/storage/VolumesPage";
import { resetTasks } from "@/tasks/tasks";
import { installVm, isoVolume, NODE, pool, poolState, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(resetTasks);

it("checks new volume names like the server", () => {
  expect(volumeNameError("data", [])).toBeUndefined();
  expect(volumeNameError("Data", [])).toBe("forms.name");
  expect(volumeNameError("import", [])).toBe("volumes.errors.reserved");
  expect(volumeNameError("download", [])).toBe("volumes.errors.reserved");
  expect(volumeNameError("data", ["data"])).toBe("volumes.errors.exists");
});

it("lists volumes with their size, state and VM", async () => {
  const web = vm("web", "stopped", {
    spec: { ...vm("web", "stopped").spec, disks: [{ volume: "root", bus: "virtio-blk" }] },
  });
  const importing = {
    ...volume("img", "local", { phase: "pending", allocated_bytes: null }),
    spec: { pool: "local", size_bytes: 0, format: "qcow2" as const },
  };
  mockLists({ volume: [volume("root", "local"), importing], vm: [web], node: [NODE] });
  renderPage(<VolumesPage />);
  const table = await screen.findByRole("table", { name: "Volumes" });
  const root = (await within(table).findByText("root")).closest("tr")!;
  expect(root).toHaveTextContent("16 GiB");
  expect(root).toHaveTextContent("1 GiB");
  expect(root).toHaveTextContent("Ready");
  expect(within(root).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(root).getByRole("button", { name: "Delete root" })).toHaveAccessibleDescription(
    "Remove it from the VM first.",
  );
  const img = within(table).getByText("img").closest("tr")!;
  expect(img).toHaveTextContent("Creating");
  const sizeColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Size");
  expect(within(img).getAllByRole("cell")[sizeColumn]).toHaveTextContent("—");
  expect(within(img).getByRole("button", { name: "Delete img" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(within(img).getByRole("button", { name: "Resize img" })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  expect(within(img).getByRole("button", { name: "Resize img" })).toHaveAccessibleDescription(
    "Available once ready.",
  );
  // 두 버튼이 같은 이유면 한 번만 보인다.
  expect(within(img).getAllByText("Available once ready.")).toHaveLength(1);
  expect(within(root).getByRole("button", { name: "Resize root" })).not.toHaveAttribute(
    "aria-disabled",
  );
  expect(root).not.toHaveTextContent("Available once ready.");
});

it("creates an empty volume and follows its task", async () => {
  // 어느 노드에서도 쓸 수 없는 풀(상태를 알린 노드가 없는 NFS 풀)은 고를 수 없다.
  const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm" } });
  mockLists({
    "storage-pool": [pool("local"), nas],
    volume: [volume("root", "local")],
    node: [NODE],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "volume-create", "v9"), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create an empty volume" });
  await userEvent.type(within(dialog).getByLabelText("Name"), "root");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("A volume with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();

  const pools = within(within(dialog).getByLabelText("Storage pool")).getAllByRole("option");
  expect(pools.map((o) => o.textContent)).toEqual(["—", "local"]);

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "data");
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  await userEvent.clear(within(dialog).getByLabelText("Size (GiB)"));
  await userEvent.type(within(dialog).getByLabelText("Size (GiB)"), "0.5");
  await userEvent.selectOptions(within(dialog).getByLabelText("Format"), "raw");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Create volume data")).toBeInTheDocument();
  expect(body).toEqual({
    name: "data",
    spec: { pool: "local", size_bytes: 512 * 1024 ** 2, format: "raw" },
  });
});

it("makes volumes in LVM-thin pools raw", async () => {
  const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
  mockLists({
    "storage-pool": [pool("local"), fast],
    volume: [],
    node: [{ ...NODE, status: { ...NODE.status, pools: { fast: poolState("fast") } } }],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl("/volumes"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "volume-create", "v9"), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create an empty volume" });
  const format = within(dialog).getByLabelText("Format");
  const formats = () =>
    within(format)
      .getAllByRole("option")
      .map((o) => o.textContent);
  expect(format).toHaveValue("qcow2");
  // LVM-thin 풀은 raw 볼륨만 둔다.
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "fast");
  expect(format).toHaveValue("raw");
  expect(formats()).toEqual(["raw"]);
  // 디렉터리 풀로 바꾸면 그 풀의 기본 형식(qcow2)으로 돌아간다.
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  expect(format).toHaveValue("qcow2");
  expect(formats()).toEqual(["qcow2", "raw"]);
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "fast");
  await userEvent.type(within(dialog).getByLabelText("Name"), "data");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Create volume data")).toBeInTheDocument();
  expect(body).toEqual({
    name: "data",
    spec: { pool: "fast", size_bytes: 16 * 1024 ** 3, format: "raw" },
  });
});

it("says when the pools cannot be read", async () => {
  mockLists({});
  server.use(
    http.get(apiUrl("/storage-pools"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create an empty volume" });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
});

// VM 목록을 읽지 못하면 쓰는 VM이 없는지 모른다. 모르는 노드는 id 끝으로 보인다.
it("does not offer a delete without the VMs", async () => {
  mockLists({
    volume: [volume("data", "local", { node: "019a0f3c-0000-7000-8000-00000000abcd" })],
  });
  server.use(
    http.get(apiUrl("/vms"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VolumesPage />);
  const data = (await screen.findByText("data")).closest("tr")!;
  expect(within(data).getByRole("button", { name: "Delete data" })).toHaveAccessibleDescription(
    "Cannot tell which VMs use it.",
  );
  expect(data).toHaveTextContent("0000abcd");
});

it("keeps the confirmation open when a VM uses the volume", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  server.use(
    http.delete(apiUrl(`/volumes/${data.id}`), () =>
      HttpResponse.json({ code: "volume_in_use", message: "in use" }, { status: 409 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete data" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type data to confirm"), "data");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "A volume is attached to another VM.",
  );
  expect(dialog).toBeVisible();
});

it("deletes a volume after typing its name", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  server.use(
    http.delete(apiUrl(`/volumes/${data.id}`), () =>
      HttpResponse.json(task("t2", "volume-delete", data.id), { status: 202 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete data" }));
  const dialog = await screen.findByRole("alertdialog");
  await userEvent.type(within(dialog).getByLabelText("Type data to confirm"), "data");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Delete volume data")).toBeInTheDocument();
});

it("lets viewers only read", async () => {
  mockLists({ volume: [volume("data", "local")] });
  renderPage(<VolumesPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("data")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Delete data" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Resize data" })).not.toBeInTheDocument();
});

it("grows a volume and follows its task", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/volumes/${data.id}/resize`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t3", "volume-resize", data.id), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  const trigger = await screen.findByRole("button", { name: "Resize data" });
  await userEvent.click(trigger);
  const dialog = await screen.findByRole("dialog", { name: "Resize volume data" });
  expect(dialog).toHaveTextContent("Current size: 16 GiB");
  // 지금 크기와 같거나 작으면 보내지 않는다.
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "16");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(within(dialog).getByLabelText("New size (GiB)")).toHaveAccessibleDescription(
    expect.stringContaining("Enter a size larger than the current size."),
  );
  expect(body).toBeUndefined();

  await userEvent.clear(within(dialog).getByLabelText("New size (GiB)"));
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "20.5");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(await screen.findByText("Resize volume data")).toBeInTheDocument();
  expect(body).toEqual({ size_bytes: 20.5 * 1024 ** 3 });
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  await waitFor(() => expect(trigger).toHaveFocus());
});

it("grows to the latest size rounded up to a whole MiB", async () => {
  const GIB = 1024 ** 3;
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/volumes/${data.id}/resize`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t3", "volume-resize", data.id), { status: 202 });
    }),
  );
  const { queryClient } = renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Resize data" }));
  const dialog = await screen.findByRole("dialog", { name: "Resize volume data" });
  expect(dialog).toHaveTextContent("Current size: 16 GiB");
  // 열어 둔 사이 크기가 바뀌면 지금 크기를 새로 보이고 그것과 비교한다.
  const grown = { ...data, spec: { ...data.spec, size_bytes: 20 * GIB + 512 } };
  queryClient.setQueryData<List<Volume>>(["list", "volume"], { revision: 2, items: [grown] });
  expect(await within(dialog).findByText("Current size: 20 GiB")).toBeVisible();
  // 20 GiB + 107 바이트는 지금보다 작지만 1 MiB 단위로 올리면 크다(qcow2는 512 배수만 받는다).
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "20.0000001");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(await screen.findByText("Resize volume data")).toBeInTheDocument();
  expect(body).toEqual({ size_bytes: 20 * GIB + 1024 ** 2 });
});

it("keeps the resize dialog open when the server refuses", async () => {
  const data = volume("data", "local");
  mockLists({ volume: [data] });
  server.use(
    http.post(apiUrl(`/volumes/${data.id}/resize`), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Resize data" }));
  const dialog = await screen.findByRole("dialog", { name: "Resize volume data" });
  await userEvent.type(within(dialog).getByLabelText("New size (GiB)"), "32");
  await userEvent.click(within(dialog).getByRole("button", { name: "Resize" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
});

it("leaves ISO images out unless asked", async () => {
  mockLists({
    volume: [volume("root", "local"), isoVolume("alpine", "local")],
    vm: [installVm("web", "stopped")],
    node: [NODE],
  });
  renderPage(<VolumesPage />);
  const table = await screen.findByRole("table", { name: "Volumes" });
  await within(table).findByText("root");
  expect(within(table).queryByText("alpine")).not.toBeInTheDocument();
  const user = userEvent.setup();
  const chip = screen.getByRole("button", { name: "Show ISO images" });
  expect(chip).toHaveAttribute("aria-pressed", "false");
  await user.click(chip);
  expect(chip).toHaveAttribute("aria-pressed", "true");
  const alpine = within(table).getByText("alpine").closest("tr")!;
  expect(alpine).toHaveTextContent("ISO");
  // CD-ROM에 넣은 VM이 쓰는 VM이다. ISO는 크기를 바꾸지 않는다.
  expect(within(alpine).getByRole("link", { name: "web" })).toBeInTheDocument();
  expect(within(alpine).getByRole("button", { name: "Resize alpine" })).toHaveAccessibleDescription(
    "ISO images cannot be resized.",
  );
});

it("downloads a disk image from a URL", async () => {
  mockLists({ "storage-pool": [pool("local")], volume: [], node: [NODE] });
  let body: unknown;
  server.use(
    http.post(apiUrl("/volumes/download"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "volume-download", "v9"), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Download from URL" }));
  const dialog = await screen.findByRole("dialog", { name: "Download a disk image" });
  await user.type(
    within(dialog).getByLabelText("URL"),
    "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img",
  );
  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "noble");
  await user.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  expect(within(dialog).getByLabelText("File format")).toHaveValue("qcow2");
  await user.selectOptions(within(dialog).getByLabelText("Format"), "raw");
  await user.click(within(dialog).getByRole("button", { name: "Download" }));
  expect(await screen.findByText("Download volume noble")).toBeInTheDocument();
  expect(body).toEqual({
    name: "noble",
    pool: "local",
    url: "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img",
    source_format: "qcow2",
    format: "raw",
  });
});

it("shows linked clones' bases and flattens a stopped VM's disk", async () => {
  const base = volume("tmpl-root", "local");
  const linked = (name: string) => {
    const v = volume(name, "local");
    return { ...v, spec: { ...v.spec, base: base.id } };
  };
  const withDisk = (v: ReturnType<typeof vm>, disk: string, template = false) => ({
    ...v,
    spec: { ...v.spec, template, disks: [{ volume: disk, bus: "virtio-blk" as const }] },
  });
  mockLists({
    volume: [base, linked("web-1-root"), linked("web-2-root")],
    vm: [
      withDisk(vm("tmpl", "stopped"), "tmpl-root", true),
      withDisk(vm("web-1", "stopped"), "web-1-root"),
      withDisk(vm("web-2", "running"), "web-2-root"),
    ],
    node: [NODE],
  });
  let flattened = false;
  server.use(
    http.post(apiUrl(`/volumes/${linked("web-1-root").id}/flatten`), () => {
      flattened = true;
      return HttpResponse.json(task("t-f", "volume-flatten", "x"), { status: 202 });
    }),
  );
  renderPage(<VolumesPage />);
  const table = await screen.findByRole("table", { name: "Volumes" });
  const baseColumn = within(table)
    .getAllByRole("columnheader")
    .findIndex((h) => h.textContent === "Clone source (template)");
  const tmplRow = (await within(table).findByRole("button", { name: "Resize tmpl-root" })).closest(
    "tr",
  )!;
  // 템플릿의 디스크는 크기를 바꾸거나 지울 수 없고, 평탄화할 것도 없다.
  expect(
    within(tmplRow).getByRole("button", { name: "Resize tmpl-root" }),
  ).toHaveAccessibleDescription("It is a disk of a template.");
  expect(
    within(tmplRow).queryByRole("button", { name: "Flatten tmpl-root" }),
  ).not.toBeInTheDocument();
  const web2 = within(table).getByText("web-2-root").closest("tr")!;
  expect(within(web2).getAllByRole("cell")[baseColumn]).toHaveTextContent("tmpl-root");
  expect(
    within(web2).getByRole("button", { name: "Flatten web-2-root" }),
  ).toHaveAccessibleDescription("Stop the VM using it first.");
  const web1 = within(table).getByText("web-1-root").closest("tr")!;
  const user = userEvent.setup();
  await user.click(within(web1).getByRole("button", { name: "Flatten web-1-root" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Flatten volume web-1-root?" });
  await user.click(within(dialog).getByRole("button", { name: "Flatten" }));
  await waitFor(() => expect(flattened).toBe(true));
  expect(await screen.findByText("Flatten volume web-1-root")).toBeInTheDocument();
});

it("explains an empty volume list with a create button and more help", async () => {
  mockLists({ node: [NODE], volume: [] });
  renderPage(<VolumesPage />);
  expect(await screen.findByText("No volumes.")).toBeVisible();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Create an empty volume" }));
  expect(await screen.findByRole("dialog", { name: "Create an empty volume" })).toBeInTheDocument();
});

it("keeps the table when only ISO images are hidden", async () => {
  mockLists({ node: [NODE], volume: [isoVolume("alpine.iso", "local")] });
  renderPage(<VolumesPage />);
  expect(await screen.findByRole("button", { name: "Show ISO images" })).toBeVisible();
  expect(screen.queryByRole("button", { name: "Learn more" })).not.toBeInTheDocument();
});
