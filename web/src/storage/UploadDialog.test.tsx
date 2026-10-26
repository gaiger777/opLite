import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { poolsOnNode } from "@/storage/UploadDialog";
import { resetUploads } from "@/storage/upload";
import { VolumesPage } from "@/storage/VolumesPage";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, pool } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

it("offers only the pools of the node it is connected to", () => {
  const pools = [pool("all"), pool("here", { nodes: ["n1"] }), pool("far", { nodes: ["n2"] })];
  expect(poolsOnNode(pools, "n1").map((p) => p.name)).toEqual(["all", "here"]);
  expect(poolsOnNode(pools, undefined)).toEqual([]);
});

it("uploads a chosen file into a new volume", async () => {
  mockLists({ "storage-pool": [pool("local"), pool("far", { nodes: ["n2"] })], node: [NODE] });
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
  expect(FakeXhr.last!.url).toContain(
    "/volumes/import?name=ubuntu-24-04&pool=local&format=qcow2&source_format=qcow2",
  );
  expect(await screen.findByText(/^Uploading ubuntu-24-04: 0%/)).toBeInTheDocument();
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
