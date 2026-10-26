import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { volumeNameError } from "@/storage/volumes";
import { VolumesPage } from "@/storage/VolumesPage";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, task, vm, volume } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(resetTasks);

it("checks new volume names like the server", () => {
  expect(volumeNameError("data", [])).toBeUndefined();
  expect(volumeNameError("Data", [])).toBe("forms.name");
  expect(volumeNameError("import", [])).toBe("volumes.errors.reserved");
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
  expect(within(root).getByRole("button", { name: "Delete root" })).toBeDisabled();
  const img = within(table).getByText("img").closest("tr")!;
  expect(img).toHaveTextContent("Creating");
  expect(within(img).getByRole("button", { name: "Delete img" })).toBeDisabled();
});

it("creates an empty volume and follows its task", async () => {
  mockLists({ "storage-pool": [pool("local")], volume: [volume("root", "local")] });
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
});
