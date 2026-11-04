import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { ImportsPage } from "@/imports/ImportsPage";
import { resetUploads } from "@/storage/upload";
import { FakeXhr } from "@/test/fakeXhr";
import { NODE, vm, vmImport } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

beforeEach(() => vi.stubGlobal("XMLHttpRequest", FakeXhr));
afterEach(() => {
  vi.unstubAllGlobals();
  resetUploads();
});

const web = vm("web", "stopped");
const done = vmImport("web01", 1, { phase: "done", vm: web.id });
const retry = vmImport("db01", 2, { error: "checksum mismatch for db01-disk1.vmdk" });
const gone = vmImport("old", 3, { phase: "done", vm: "019a0f3c-0000-7000-8000-00000000dead" });

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists imports with their source, state and VM", async () => {
  mockLists({ "vm-import": [done, retry, gone], vm: [web], node: [NODE] });
  renderPage(<ImportsPage />);
  const table = await screen.findByRole("table", { name: "Imports" });
  await within(table).findByText("web01");
  expect(within(table).getByRole("link", { name: "web01" })).toHaveAttribute(
    "href",
    "/imports/web01",
  );
  const row = cells(table, "web01");
  expect(row("Node")).toHaveTextContent("node-a");
  expect(row("Source VM")).toHaveTextContent("Web 01web01.ova");
  expect(row("State")).toHaveTextContent("Done");
  expect(await within(row("VM")!).findByRole("link", { name: "web" })).toHaveAttribute(
    "href",
    "/vms/web",
  );
  // 실패한 실행의 오류는 상태 아래에 보인다.
  const db = cells(table, "db01");
  expect(db("State")).toHaveTextContent("Readychecksum mismatch for db01-disk1.vmdk");
  expect(db("VM")).toHaveTextContent("—");
  expect(cells(table, "old")("VM")).toHaveTextContent("Deleted VM");
});

it("hides the upload from viewers", async () => {
  mockLists({ "vm-import": [done] });
  renderPage(<ImportsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("web01")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Upload OVA" })).not.toBeInTheDocument();
});

it("uploads an OVA under a name taken from the file", async () => {
  mockLists({ "vm-import": [done], node: [NODE] });
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
    http.post(apiUrl("/vm-imports/ova/check"), () => new HttpResponse(null, { status: 204 })),
  );
  renderPage(<ImportsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Upload OVA" }));
  const dialog = await screen.findByRole("dialog", { name: "Upload an OVA" });
  expect(await within(dialog).findByText(/connected to \(node-a\)/)).toBeInTheDocument();
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("Choose a file.")).toBeVisible();

  // 이름은 확장자를 빼고 이름 규칙에 맞춘다. 이미 있는 이름과 예약한 이름은 받지 않는다.
  await userEvent.upload(within(dialog).getByLabelText("OVA file"), new File(["ova"], "Web01.ova"));
  expect(within(dialog).getByLabelText("Name")).toHaveValue("web01");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("An import with this name already exists.")).toBeVisible();
  await userEvent.upload(within(dialog).getByLabelText("OVA file"), new File(["ova"], "OVA.ova"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(within(dialog).getByText("The names ova and esxi are reserved.")).toBeVisible();

  await userEvent.upload(
    within(dialog).getByLabelText("OVA file"),
    new File(["ova"], "Web Server 2019.ova"),
  );
  expect(within(dialog).getByLabelText("Name")).toHaveValue("web-server-2019");
  await userEvent.click(within(dialog).getByRole("button", { name: "Upload" }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  // 확인을 통과한 뒤에 보낸다.
  await vi.waitFor(() => expect(FakeXhr.last).toBeDefined());
  expect(FakeXhr.last!.url).toContain(
    "/vm-imports/ova?name=web-server-2019&file_name=Web+Server+2019.ova",
  );
  expect(await screen.findByText(/^Uploading web-server-2019: 0%/)).toBeInTheDocument();
});
