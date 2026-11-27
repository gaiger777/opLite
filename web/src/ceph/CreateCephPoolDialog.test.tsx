import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";
import { useState } from "react";
import { toast } from "sonner";

import { CreateCephPoolDialog } from "@/ceph/CreateCephPoolDialog";
import { resetTasks } from "@/tasks/tasks";
import { task } from "@/test/fixtures";
import { renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

afterEach(() => {
  toast.dismiss();
  resetTasks();
});

function setup(hosts: number | undefined) {
  const sent: unknown[] = [];
  server.use(
    http.post(apiUrl("/ceph/pools"), async ({ request }) => {
      sent.push(await request.json());
      return HttpResponse.json(task("t-pool", "ceph-pool-create", "c1"), { status: 202 });
    }),
  );
  renderPage(<CreateCephPoolDialog hosts={hosts} onClose={() => {}} />);
  return { sent, user: userEvent.setup() };
}

it("creates a pool with three copies when three hosts have OSDs", async () => {
  const { sent, user } = setup(4);
  expect(await screen.findByLabelText("Copies (size)")).toHaveValue("3");
  expect(screen.getByText(/4 nodes have Ceph disks up/)).toBeInTheDocument();
  await user.type(screen.getByLabelText("Name"), "vms");
  await user.click(screen.getByRole("button", { name: "Create" }));
  expect(
    await screen.findByText("Create Ceph pool vms as storage pool vms (size 3)"),
  ).toBeInTheDocument();
  expect(sent).toEqual([{ name: "vms", size: 3 }]);
});

it("warns about two copies and sends the Ceph pool name", async () => {
  const { sent, user } = setup(2);
  expect(await screen.findByLabelText("Copies (size)")).toHaveValue("2");
  expect(screen.getByText(/Size 2 can lose data/)).toBeInTheDocument();
  await user.type(screen.getByLabelText("Name"), "vms");
  await user.type(screen.getByLabelText("Ceph pool"), "rbd-vms");
  await user.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(sent).toEqual([{ name: "vms", size: 2, pool: "rbd-vms" }]));
});

it("asks before keeping a single copy", async () => {
  const { sent, user } = setup(1);
  expect(await screen.findByLabelText("Copies (size)")).toHaveValue("1");
  expect(screen.getByText(/Size 1 keeps a single copy/)).toBeInTheDocument();
  await user.type(screen.getByLabelText("Name"), "scratch");
  const create = screen.getByRole("button", { name: "Create" });
  expect(create).toBeDisabled();
  await user.click(screen.getByLabelText("I understand that there is no redundancy"));
  await user.click(create);
  await waitFor(() => expect(sent).toEqual([{ name: "scratch", size: 1 }]));
});

it("follows the host count that arrives after opening until a size is chosen", async () => {
  // 상태(호스트 수)가 대화상자를 연 뒤에 온다.
  let arrive: (hosts: number) => void = () => {};
  function Late() {
    const [hosts, setHosts] = useState<number>();
    arrive = setHosts;
    return <CreateCephPoolDialog hosts={hosts} onClose={() => {}} />;
  }
  renderPage(<Late />);
  const user = userEvent.setup();
  expect(await screen.findByLabelText("Copies (size)")).toHaveValue("3");
  act(() => arrive(1));
  expect(screen.getByLabelText("Copies (size)")).toHaveValue("1");
  await user.selectOptions(screen.getByLabelText("Copies (size)"), "2");
  act(() => arrive(3));
  expect(screen.getByLabelText("Copies (size)")).toHaveValue("2");
});

it("checks the names before sending", async () => {
  const { sent, user } = setup(undefined);
  // 호스트 수를 모르면 3이다(서버가 호스트 수로 거절한다).
  expect(await screen.findByLabelText("Copies (size)")).toHaveValue("3");
  await user.type(screen.getByLabelText("Ceph pool"), "-bad");
  await user.click(screen.getByRole("button", { name: "Create" }));
  expect(screen.getByLabelText("Name")).toHaveAttribute("aria-invalid", "true");
  expect(screen.getByLabelText("Ceph pool")).toHaveAttribute("aria-invalid", "true");
  expect(sent).toEqual([]);
});
