import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { resetUploads } from "@/storage/upload";
import { VolumesPage } from "@/storage/VolumesPage";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, pool, poolState } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

const nas = pool("nas", { backend: { kind: "nfs", server: "nas.lan", export: "/srv/vm" } });

it("uploads a chosen file into a new volume", async () => {
  // 접속한 노드(n1)에서 쓸 수 없는 풀(다른 노드의 풀, 준비되지 않은 NFS 풀)은 고를 수 없다.
  const here = {
    ...NODE,
    status: { ...NODE.status, pools: { nas: poolState("nas", { ready: false, error: "down" }) } },
  };
  mockLists({
    "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] }), nas],
    node: [here],
  });
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
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("Choose a file.")).toBeVisible();

  await userEvent.upload(
    within(dialog).getByLabelText("Image file"),
    new File(["disk"], "Ubuntu 24.04.qcow2"),
  );
  expect(within(dialog).getByLabelText("Name")).toHaveValue("ubuntu-24-04");
  expect(within(dialog).getByLabelText("File format")).toHaveValue("qcow2");
  const pools = within(within(dialog).getByLabelText("Storage pool")).getAllByRole("option");
  expect(pools.map((o) => o.textContent)).toEqual(["—", "local"]);
  await userEvent.selectOptions(within(dialog).getByLabelText("Storage pool"), "local");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));

  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  // 확인을 통과한 뒤에 보낸다.
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  expect(FakeXhr.last!.url).toContain(
    "/volumes/import?name=ubuntu-24-04&pool=local&format=qcow2&source_format=qcow2",
  );
  expect(await screen.findByText(/^Uploading ubuntu-24-04: 0%/)).toBeInTheDocument();
});

it("imports into LVM-thin pools as raw", async () => {
  const fast = pool("fast", { backend: { kind: "lvm-thin", vg: "vg0", thinpool: "tp" } });
  mockLists({
    "storage-pool": [pool("local"), fast],
    node: [{ ...NODE, status: { ...NODE.status, pools: { fast: poolState("fast") } } }],
  });
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
    http.post(apiUrl("/volumes/import/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  await userEvent.upload(within(dialog).getByLabelText("Image file"), new File(["disk"], "a.vmdk"));
  await userEvent.selectOptions(await within(dialog).findByLabelText("Storage pool"), "fast");
  // LVM-thin 풀은 raw 볼륨만 둔다.
  const format = within(dialog).getByLabelText("Format");
  expect(format).toHaveValue("raw");
  expect(
    within(format)
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["raw"]);
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  expect(FakeXhr.last!.url).toContain("/volumes/import?name=a&pool=fast&format=raw");
});

it("says when it cannot tell the node it is connected to", async () => {
  mockLists({ "storage-pool": [pool("local")], node: [NODE] });
  server.use(
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Cannot tell which node you are connected to. You do not have permission to do this.",
  );
});

it("says when the pools cannot be read instead of saying there are none", async () => {
  mockLists({ node: [NODE] });
  server.use(
    http.get(apiUrl("/storage-pools"), () =>
      HttpResponse.json({ code: "forbidden", message: "no" }, { status: 403 }),
    ),
    http.get(apiUrl("/cluster"), () =>
      HttpResponse.json({
        name: "lab",
        ca_fingerprint: "x",
        leader: "n1",
        node: "n1",
        members: [],
      }),
    ),
  );
  renderPage(<VolumesPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload image" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload a disk image" });
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "You do not have permission to do this.",
  );
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  expect(
    within(dialog).queryByText("No storage pool is available on this node."),
  ).not.toBeInTheDocument();
});
