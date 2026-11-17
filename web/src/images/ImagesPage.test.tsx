import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Network, Principal } from "@/api/types";
import { type CatalogEntry, defaultVolumeName, digestError, validCatalogId } from "@/images/images";
import { ImagesPage } from "@/images/ImagesPage";
import { resetTasks } from "@/tasks/tasks";
import { NODE, pool, task, vm } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const OPERATOR: Principal = {
  user: "00000000-0000-0000-0000-000000000002",
  name: "ops",
  role: "operator",
};

const lan = { id: "019a0f3c-0000-7000-8004-000000000001", name: "lan" } as Network;

const UBUNTU_URL =
  "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img";

function entry(extra: Partial<CatalogEntry> = {}): CatalogEntry {
  return {
    id: "ubuntu-24.04",
    title: "Ubuntu 24.04 LTS",
    os: "ubuntu",
    version: "24.04",
    arch: "x86_64",
    url: UBUNTU_URL,
    checksum: {
      kind: "sums",
      url: "https://cloud-images.ubuntu.com/releases/24.04/release/SHA256SUMS",
    },
    source_format: "qcow2",
    firmware: "uefi",
    default_user: "ubuntu",
    min_size_bytes: 3758096384,
    source: "builtin",
    volumes: [],
    ...extra,
  };
}

const custom = entry({
  id: "myos-1.0",
  title: "My OS 1.0",
  os: "myos",
  version: "1.0",
  arch: "aarch64",
  url: "https://mirror.lan/myos-1.0.qcow2",
  checksum: { kind: "sha256", value: "a".repeat(64) },
  source: "custom",
  volumes: ["myos-1-0-arm64"],
});

/** 카탈로그 응답. 받은 질의의 `arch`를 모은다. */
function mockCatalog(items: CatalogEntry[]) {
  const arches: (string | null)[] = [];
  server.use(
    http.get(apiUrl("/image-catalog"), ({ request }) => {
      const arch = new URL(request.url).searchParams.get("arch");
      arches.push(arch);
      return HttpResponse.json({
        revision: 1,
        items: items.filter((i) => arch === null || i.arch === arch),
      });
    }),
  );
  return arches;
}

it("checks catalog input like the server", () => {
  expect(defaultVolumeName({ id: "ubuntu-24.04", arch: "x86_64" })).toBe("ubuntu-24-04");
  expect(defaultVolumeName({ id: "ubuntu-24.04", arch: "aarch64" })).toBe("ubuntu-24-04-arm64");
  expect(validCatalogId("myos-1.0")).toBe(true);
  expect(validCatalogId("-x")).toBe(false);
  expect(validCatalogId("MyOS")).toBe(false);
  // 기본 볼륨 이름이 리소스 이름이 되어야 한다(서버와 같다).
  for (const id of [
    "my_os",
    "myos.",
    "myos-",
    "x".repeat(58),
    "01234567-89ab-cdef-0123-456789abcdef",
  ]) {
    expect(validCatalogId(id)).toBe(false);
  }
  expect(validCatalogId("x".repeat(57))).toBe(true);
  expect(digestError("sha256", "a".repeat(64))).toBeUndefined();
  expect(digestError("sha512", "a".repeat(64))).toBe("images.errors.sha512");
});

it("lists catalog images by architecture with downloads in progress", async () => {
  mockLists({
    node: [NODE],
    task: [
      {
        ...task("t-dl", "volume-download", "v1"),
        spec: {
          op: { kind: "volume-download", url: UBUNTU_URL } as never,
          target: "v1",
          node: "n1",
        },
        status: {
          state: "running",
          error: null,
          progress: { stage: "checksum", done_bytes: 0, total_bytes: 0 },
        },
      },
    ],
  });
  const arches = mockCatalog([entry(), custom]);
  renderPage(<ImagesPage />, { as: OPERATOR });
  const table = await screen.findByRole("table", { name: "Image library" });
  const ubuntu = (await within(table).findByText("Ubuntu 24.04 LTS")).closest("tr")!;
  expect(ubuntu).toHaveTextContent("Built-in");
  expect(ubuntu).toHaveTextContent("Checksum file");
  expect(await within(ubuntu).findByText("Fetching the checksum file")).toBeInTheDocument();
  const mine = within(table).getByText("My OS 1.0").closest("tr")!;
  expect(mine).toHaveTextContent("myos-1-0-arm64");
  // 관리자 항목은 admin만 지운다.
  expect(within(mine).queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Add entry" })).not.toBeInTheDocument();

  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "aarch64" }));
  await waitFor(() =>
    expect(within(table).queryByText("Ubuntu 24.04 LTS")).not.toBeInTheDocument(),
  );
  expect(arches).toContain("aarch64");
  expect(screen.getByRole("button", { name: "aarch64" })).toHaveAttribute("aria-pressed", "true");
});

it("downloads an image and creates a template from it", async () => {
  mockLists({
    node: [NODE],
    network: [lan],
    vm: [vm("ubuntu-tmpl", "stopped")],
    "storage-pool": [pool("local")],
  });
  mockCatalog([entry()]);
  let body: unknown;
  server.use(
    http.post(apiUrl("/image-catalog/ubuntu-24.04/download"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t1", "volume-download", "v9"), { status: 202 });
    }),
  );
  renderPage(<ImagesPage />);
  const user = userEvent.setup();
  await user.click(
    await screen.findByRole("button", { name: "Download Ubuntu 24.04 LTS (x86_64)" }),
  );
  const dialog = await screen.findByRole("dialog", { name: "Download Ubuntu 24.04 LTS (x86_64)" });
  expect(within(dialog).getByLabelText("Volume name")).toHaveAttribute(
    "placeholder",
    "ubuntu-24-04",
  );
  await user.click(within(dialog).getByLabelText("Create a template after downloading"));
  const name = within(dialog).getByLabelText("Template name");
  expect(name).toHaveValue("ubuntu-24-04-tmpl");
  await user.clear(name);
  await user.type(name, "ubuntu-tmpl");
  await user.type(within(dialog).getByLabelText("Disk size (GiB)"), "2");
  await user.click(within(dialog).getByRole("button", { name: "Download" }));
  expect(within(dialog).getByText("Choose a storage pool.")).toBeVisible();
  expect(within(dialog).getByText("A VM with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Choose a network.")).toBeVisible();
  expect(within(dialog).getByText("Smaller than the image's disk size.")).toBeVisible();

  await user.selectOptions(await within(dialog).findByLabelText("Storage pool"), "local");
  await user.clear(name);
  await user.type(name, "noble");
  await user.selectOptions(within(dialog).getByLabelText("Network"), "lan");
  await user.clear(within(dialog).getByLabelText("Disk size (GiB)"));
  await user.type(within(dialog).getByLabelText("Disk size (GiB)"), "20");
  await user.type(within(dialog).getByLabelText("SSH public keys"), "ssh-ed25519 AAAA me");
  await user.click(within(dialog).getByRole("button", { name: "Download" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(body).toEqual({
    pool: "local",
    arch: "x86_64",
    template: {
      name: "noble",
      network: "lan",
      vcpus: 2,
      memory_mib: 2048,
      size_bytes: 20 * 1024 ** 3,
      cloud_init: { ssh_keys: ["ssh-ed25519 AAAA me"] },
    },
  });
  expect(
    await screen.findByText("Download volume ubuntu-24-04 and create template noble"),
  ).toBeInTheDocument();
});

it("lets admins add and remove custom entries", async () => {
  mockLists({ node: [NODE] });
  mockCatalog([entry(), custom]);
  let added: unknown;
  let removed: string | null = null;
  server.use(
    http.post(apiUrl("/image-catalog"), async ({ request }) => {
      const image = (await request.json()) as CatalogEntry;
      added = image;
      return HttpResponse.json(image, { status: 201 });
    }),
    http.delete(apiUrl("/image-catalog/myos-1.0"), ({ request }) => {
      removed = new URL(request.url).searchParams.get("arch");
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<ImagesPage />);
  const user = userEvent.setup();
  const table = await screen.findByRole("table", { name: "Image library" });
  const ubuntu = (await within(table).findByText("Ubuntu 24.04 LTS")).closest("tr")!;
  // 내장 항목은 지울 수 없다.
  expect(within(ubuntu).queryByRole("button", { name: /^Delete/ })).not.toBeInTheDocument();
  await user.click(within(table).getByRole("button", { name: "Delete My OS 1.0 (aarch64)" }));
  const confirm = await screen.findByRole("alertdialog");
  await user.click(within(confirm).getByRole("button", { name: "Delete" }));
  await waitFor(() => expect(removed).toBe("aarch64"));

  await user.click(screen.getByRole("button", { name: "Add entry" }));
  const dialog = await screen.findByRole("dialog", { name: "Add a catalog entry" });
  await user.type(within(dialog).getByLabelText("id"), "Bad");
  await user.selectOptions(within(dialog).getByLabelText("Checksum"), "sha512");
  await user.type(within(dialog).getByLabelText("Hash (hex)"), "abc");
  await user.click(within(dialog).getByRole("button", { name: "Add entry" }));
  expect(within(dialog).getByText("Use 128 hex digits.")).toBeVisible();
  expect(within(dialog).getAllByText("Enter a value.")).toHaveLength(3);
  expect(added).toBeUndefined();

  await user.clear(within(dialog).getByLabelText("id"));
  await user.type(within(dialog).getByLabelText("id"), "myos-2.0");
  await user.type(within(dialog).getByLabelText("Title"), "My OS 2.0");
  await user.type(within(dialog).getByLabelText("Distribution"), "myos");
  await user.type(within(dialog).getByLabelText("Version"), "2.0");
  await user.type(within(dialog).getByLabelText("URL"), "https://mirror.lan/myos-2.0.qcow2");
  await user.clear(within(dialog).getByLabelText("Hash (hex)"));
  await user.type(within(dialog).getByLabelText("Hash (hex)"), "B".repeat(128));
  await user.click(within(dialog).getByRole("button", { name: "Add entry" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(added).toEqual({
    id: "myos-2.0",
    title: "My OS 2.0",
    os: "myos",
    version: "2.0",
    arch: "x86_64",
    url: "https://mirror.lan/myos-2.0.qcow2",
    checksum: { kind: "sha512", value: "b".repeat(128) },
    source_format: "qcow2",
    firmware: "uefi",
  });
});
