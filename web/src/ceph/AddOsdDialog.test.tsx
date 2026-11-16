import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { toast } from "sonner";

import type { Node } from "@/api/types";
import { AddOsdDialog } from "@/ceph/AddOsdDialog";
import { resetTasks } from "@/tasks/tasks";
import { DISKS, NODE, task } from "@/test/fixtures";
import { mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

const NODE_B: Node = { ...NODE, id: "n2", name: "node-b" };

/** 노드마다 디스크 목록을 돌려주고 `POST /ceph/osds`의 본문을 모은다. */
function setup(onClose = vi.fn()) {
  mockLists({ node: [NODE, NODE_B] });
  const sent: unknown[] = [];
  server.use(
    http.get(apiUrl("/nodes/:key/disks"), ({ params }) =>
      HttpResponse.json(params.key === "n1" ? DISKS : [DISKS[1]]),
    ),
    http.post(apiUrl("/ceph/osds"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(task("t-osd", "ceph-osd-add", "c1"), { status: 202 });
    }),
  );
  renderPage(<AddOsdDialog onClose={onClose} />);
  return { sent, onClose, user: userEvent.setup() };
}

it("adds an OSD on an empty disk", async () => {
  const { sent, onClose, user } = setup();
  const table = await screen.findByRole("table", { name: "Disks" });
  // 쓰는 중인 디스크는 고를 수 없고 이유를 보인다.
  const sda = within(table).getByText("/dev/sda").closest("tr")!;
  expect(sda).toHaveTextContent("In usesystem disk (/ is on it)");
  expect(within(sda).queryByRole("radio")).not.toBeInTheDocument();
  expect(within(table).getByText("/dev/sdb").closest("tr")).toHaveTextContent(
    "/dev/sdbwwn-0x5000c500a1b2c3d420 GiBHDDST2000 · Z1X2Available",
  );
  const add = screen.getByRole("button", { name: "Add OSD" });
  expect(add).toBeDisabled();
  await user.click(screen.getByRole("radio", { name: "/dev/sdb" }));
  await user.click(add);
  expect(await screen.findByText("Add a Ceph OSD on node-a /dev/sdb")).toBeInTheDocument();
  // 확인한 디스크는 by-id 경로로 보낸다.
  expect(sent).toEqual([
    { node: "n1", device: "/dev/disk/by-id/wwn-0x5000c500a1b2c3d4", wipe: false },
  ]);
  expect(onClose).toHaveBeenCalled();
});

it("erases a disk that is not empty only after its name is typed", async () => {
  const { sent, user } = setup();
  await user.click(await screen.findByRole("radio", { name: "/dev/sdc" }));
  const add = screen.getByRole("button", { name: "Erase and add OSD" });
  expect(add).toBeDisabled();
  expect(
    screen.getByText(
      "The disk is not empty: gpt partition table, ext4 filesystem. Everything on it is erased.",
    ),
  ).toBeInTheDocument();
  const confirm = screen.getByLabelText("Type /dev/sdc to erase all data on it");
  await user.type(confirm, "sdb");
  expect(add).toBeDisabled();
  await user.clear(confirm);
  await user.type(confirm, "sdc");
  await user.click(add);
  await waitFor(() => expect(sent).toEqual([{ node: "n1", device: "/dev/sdc", wipe: true }]));
});

it("reads the disks of the chosen node", async () => {
  const { user } = setup();
  await screen.findByRole("radio", { name: "/dev/sdc" });
  await user.selectOptions(screen.getByLabelText("Node"), "node-b");
  await waitFor(() =>
    expect(screen.queryByRole("radio", { name: "/dev/sdc" })).not.toBeInTheDocument(),
  );
  expect(screen.getByRole("radio", { name: "/dev/sdb" })).not.toBeChecked();
});

it("shows why the server refused the disk", async () => {
  const { user } = setup();
  server.use(
    http.post(apiUrl("/ceph/osds"), () =>
      HttpResponse.json(
        { code: "disk_in_use", message: "/dev/sdb cannot hold an OSD: mounted at /srv" },
        { status: 409 },
      ),
    ),
  );
  await user.click(await screen.findByRole("radio", { name: "/dev/sdb" }));
  await user.click(screen.getByRole("button", { name: "Add OSD" }));
  expect(
    await screen.findByText(
      "The disk cannot hold an OSD: /dev/sdb cannot hold an OSD: mounted at /srv",
    ),
  ).toBeInTheDocument();
});
