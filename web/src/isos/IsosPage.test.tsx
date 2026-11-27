import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { IsosPage } from "@/isos/IsosPage";
import { resetUploads } from "@/storage/upload";
import { resetTasks } from "@/tasks/tasks";
import { FakeXhr } from "@/test/fakeXhr";
import { installVm, isoVolume, NODE, pool, poolState, task, volume } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  resetTasks();
  vi.unstubAllGlobals();
  resetUploads();
});

const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/iso" } });
const here = { ...NODE, status: { ...NODE.status, pools: { nas: poolState("nas") } } };

it("lists ISO images with their pool, node and VMs", async () => {
  const downloading = isoVolume("ubuntu", "nas", { phase: "pending", allocated_bytes: null });
  mockLists({
    volume: [volume("root", "local"), isoVolume("alpine", "local"), downloading],
    vm: [installVm("web", "stopped")],
    node: [here],
    "storage-pool": [pool("local"), nas],
    task: [
      {
        ...task("t-dl", "volume-download", downloading.id),
        status: {
          state: "running",
          error: null,
          progress: { stage: "downloading", done_bytes: 1024 ** 3, total_bytes: 2 * 1024 ** 3 },
        },
      },
    ],
  });
  renderPage(<IsosPage />);
  const table = await screen.findByRole("table", { name: "ISO files" });
  // 디스크 볼륨은 보이지 않는다.
  await within(table).findByText("alpine");
  expect(within(table).queryByText("root")).not.toBeInTheDocument();
  const alpine = within(table).getByText("alpine").closest("tr")!;
  expect(alpine).toHaveTextContent("600 MiB");
  // 노드 로컬 풀의 ISO는 그 노드를 보인다.
  expect(alpine).toHaveTextContent("node-a");
  expect(within(alpine).getByRole("link", { name: "web" })).toHaveAttribute("href", "/vms/web");
  expect(within(alpine).getByRole("button", { name: "Delete alpine" })).toHaveAccessibleDescription(
    "Eject it from the VMs first.",
  );
  const ubuntu = within(table).getByText("ubuntu").closest("tr")!;
  expect(await within(ubuntu).findByText("Downloading 50% (1.0/2.0 GiB)")).toBeInTheDocument();
  expect(within(ubuntu).getByRole("progressbar", { name: "Download of ubuntu" })).toHaveAttribute(
    "value",
    String(1024 ** 3),
  );
});

it("downloads an ISO into a directory or NFS pool", async () => {
  const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
  mockLists({
    volume: [isoVolume("alpine", "local")],
    node: [
      {
        ...here,
        status: { ...here.status, pools: { ...here.status.pools, fast: poolState("fast") } },
      },
    ],
    "storage-pool": [pool("local"), nas, fast],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl("/volumes/download"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "volume-download", "v9"), { status: 202 });
    }),
  );
  renderPage(<IsosPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Download from URL" }));
  const dialog = await screen.findByRole("dialog", { name: "Download an ISO image" });
  // 블록 풀에는 ISO를 두지 않는다.
  const pools = within(within(dialog).getByLabelText("Storage pool")).getAllByRole("option");
  expect(pools.map((o) => o.textContent)).toEqual(["—", "local", "nas"]);
  expect(within(dialog).queryByLabelText("File format")).not.toBeInTheDocument();

  await user.type(within(dialog).getByLabelText("URL"), "ftp://mirror/alpine.iso");
  await user.type(within(dialog).getByLabelText("SHA-256 (optional)"), "abc");
  await user.click(within(dialog).getByRole("button", { name: "Download" }));
  expect(within(dialog).getByText("Enter an http or https URL.")).toBeVisible();
  expect(within(dialog).getByText("Use 64 hex digits.")).toBeVisible();
  expect(within(dialog).getByText("A volume with this name already exists.")).toBeVisible();

  await user.clear(within(dialog).getByLabelText("URL"));
  await user.type(
    within(dialog).getByLabelText("URL"),
    "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/x86_64/alpine-virt-3.20.3-x86_64.iso",
  );
  // URL 끝에서 이름을 제안한다.
  expect(within(dialog).getByLabelText("Name")).toHaveValue("alpine-virt-3-20-3-x86-64");
  await user.clear(within(dialog).getByLabelText("SHA-256 (optional)"));
  await user.type(within(dialog).getByLabelText("SHA-256 (optional)"), "AB".repeat(32));
  await user.selectOptions(within(dialog).getByLabelText("Storage pool"), "nas");
  await user.click(within(dialog).getByRole("button", { name: "Download" }));
  expect(await screen.findByText("Download volume alpine-virt-3-20-3-x86-64")).toBeInTheDocument();
  expect(body).toEqual({
    name: "alpine-virt-3-20-3-x86-64",
    pool: "nas",
    url: "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/x86_64/alpine-virt-3.20.3-x86_64.iso",
    sha256: "ab".repeat(32),
    content: "iso",
  });
});

it("uploads an ISO as raw with the iso content", async () => {
  vi.stubGlobal("XMLHttpRequest", FakeXhr);
  mockLists({ node: [here], "storage-pool": [pool("local"), nas] });
  let checked: URLSearchParams | undefined;
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
    http.post(apiUrl("/volumes/import/check"), ({ request }) => {
      checked = new URL(request.url).searchParams;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<IsosPage />);
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Upload ISO" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload an ISO image" });
  const file = within(dialog).getByLabelText("Image file");
  expect(file).toHaveAttribute("accept", ".iso");
  await user.upload(file, new File(["iso"], "Alpine Virt.iso"));
  expect(within(dialog).getByLabelText("Name")).toHaveValue("alpine-virt");
  expect(within(dialog).queryByLabelText("Format")).not.toBeInTheDocument();
  await user.selectOptions(await within(dialog).findByLabelText("Storage pool"), "nas");
  await user.click(within(dialog).getByRole("button", { name: "Upload" }));
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  expect(FakeXhr.last!.url).toContain(
    "/volumes/import?name=alpine-virt&pool=nas&format=raw&source_format=raw&content=iso",
  );
  await waitFor(() => expect(checked?.get("content")).toBe("iso"));
});

it("explains an empty ISO library with upload and download buttons and more help", async () => {
  mockLists({ node: [NODE], volume: [volume("disk0", "local")] });
  renderPage(<IsosPage />);
  expect(await screen.findByText("No ISO images.")).toBeVisible();
  expect(screen.queryByRole("table")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Learn more" })).toBeInTheDocument();
  const [, upload] = screen.getAllByRole("button", { name: "Upload ISO" });
  await userEvent.click(upload!);
  expect(await screen.findByRole("dialog", { name: "Upload an ISO image" })).toBeInTheDocument();
});
