import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { hardware, NODE, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { VmDetailPage } from "@/vms/VmDetailPage";

const NODES = [NODE, { ...NODE, id: "n2", name: "node-b" }, { ...NODE, id: "n3", name: "node-c" }];

/** 한 번 켠 Windows VM: TPM·Secure Boot, node-a의 상태와 node-b·node-c의 사본 */
function win(phase: "running" | "stopped" = "stopped", extra: Record<string, unknown> = {}) {
  const base = vm("win", phase);
  return vm("win", phase, {
    spec: { ...base.spec, tpm: true, secure_boot: true },
    status: {
      ...base.status,
      firmware_node: "n1",
      tpm_node: "n1",
      tpm_sha256: "ab".repeat(32),
      firmware_copies: ["n2", "n3"],
      ...extra,
    },
  });
}

function security() {
  return screen.getByRole("heading", { name: "Security" }).closest("[data-slot=card]")!;
}

it("shows where the TPM state and the UEFI variables are", async () => {
  mockLists({ vm: [win()], node: NODES });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/win" });
  // 노드 목록을 읽으면 id 대신 이름을 보인다.
  expect(await screen.findByText(/copies on node-b, node-c/)).toBeInTheDocument();
  const card = security();
  expect(card).toHaveTextContent("TPM 2.0OnState on node node-a, copies on node-b, node-c");
  expect(card).toHaveTextContent("Secure BootOn");
  expect(card).toHaveTextContent("UEFI variablesOn node node-a");
});

it("says a TPM VM has no state until it starts and that turned-off TPM state is kept", async () => {
  const fresh = vm("fresh", "stopped", {
    spec: { ...vm("fresh", "stopped").spec, tpm: true },
  });
  const off = vm("off", "stopped", {
    status: {
      ...vm("off", "stopped").status,
      firmware_node: "n1",
      tpm_node: "n1",
      tpm_sha256: "cd".repeat(32),
    },
  });
  mockLists({ vm: [fresh, off], node: NODES });
  const { router } = renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/fresh" });
  expect(await screen.findByText("No state yet (made on first start)")).toBeInTheDocument();
  expect(screen.getByText("None yet (made on first start)")).toBeInTheDocument();
  await router.navigate("/vms/off");
  expect(await screen.findByText("State kept on node node-a")).toBeInTheDocument();
});

it("shows TPM and Secure Boot changes that wait for a restart", async () => {
  const running = win("running", {
    hardware: hardware({
      pending: [{ field: "tpm", running: "off", wanted: "on", reason: "restart" }],
    }),
  });
  mockLists({ vm: [running], node: NODES });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/win" });
  expect(await screen.findByText("TPM 2.0: off → on at the next restart")).toBeInTheDocument();
  // 요약 아래의 다음 재시작 목록도 이름을 붙인다.
  expect(screen.getByRole("list", { name: "Applies at the next power cycle" })).toHaveTextContent(
    "TPM 2.0",
  );
});

it("resets the firmware after the VM name is typed and tracks the task", async () => {
  const stopped = win();
  mockLists({ vm: [stopped], node: NODES });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${stopped.id}/firmware/reset`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t-fw", "vm-firmware-reset", stopped.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/win" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Reset firmware" }));
  const dialog = await screen.findByRole("alertdialog");
  const confirm = within(dialog).getByRole("button", { name: "Reset firmware" });
  // 아무것도 고르지 않으면 보내지 않는다.
  await user.type(within(dialog).getByLabelText("Type win to confirm"), "win");
  expect(confirm).toBeDisabled();
  await user.click(within(dialog).getByLabelText("TPM state"));
  expect(within(dialog).getByText(/BitLocker will ask for its recovery key/)).toBeInTheDocument();
  await user.click(within(dialog).getByLabelText("UEFI variables"));
  await user.click(confirm);
  expect(await screen.findByText("Reset the firmware of win")).toBeInTheDocument();
  expect(body).toEqual({ uefi_vars: true, tpm: true });
});

it("turns Secure Boot off through the firmware reset", async () => {
  const stopped = win();
  mockLists({ vm: [stopped], node: NODES });
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${stopped.id}/firmware/reset`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(task("t-fw", "vm-firmware-reset", stopped.id), { status: 202 });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/win" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Turn Secure Boot off" }));
  const dialog = await screen.findByRole("alertdialog");
  const sb = within(dialog).getByLabelText("Turn Secure Boot off (resets the UEFI variables)");
  expect(sb).toBeChecked();
  // Secure Boot를 바꾸면 변수도 지운다.
  expect(within(dialog).getByLabelText("UEFI variables")).toBeChecked();
  expect(within(dialog).getByLabelText("UEFI variables")).toBeDisabled();
  await user.type(within(dialog).getByLabelText("Type win to confirm"), "win");
  await user.click(within(dialog).getByRole("button", { name: "Reset firmware" }));
  await waitFor(() => expect(body).toEqual({ uefi_vars: true, secure_boot: false }));
});

it("says why the firmware of a running VM cannot be reset", async () => {
  const running = win("running");
  mockLists({ vm: [running], node: NODES });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/win" });
  const reset = await screen.findByRole("button", { name: "Reset firmware" });
  expect(reset).toHaveAttribute("aria-disabled", "true");
  expect(reset).toHaveAccessibleDescription(
    "Stop the VM to reset its firmware or change Secure Boot.",
  );
});

it("shows the server's reason when the reset is refused", async () => {
  const stopped = win();
  mockLists({ vm: [stopped], node: NODES });
  server.use(
    http.post(apiUrl(`/vms/${stopped.id}/firmware/reset`), () =>
      HttpResponse.json(
        { code: "node_unavailable", message: 'node "node-a" holding the TPM state is offline' },
        { status: 409 },
      ),
    ),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/win" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Reset firmware" }));
  const dialog = await screen.findByRole("alertdialog");
  await user.click(within(dialog).getByLabelText("UEFI variables"));
  await user.type(within(dialog).getByLabelText("Type win to confirm"), "win");
  await user.click(within(dialog).getByRole("button", { name: "Reset firmware" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    'The node is unavailable: node "node-a" holding the TPM state is offline',
  );
});

it("hides the firmware reset from users who cannot change VMs", async () => {
  mockLists({ vm: [win()], node: NODES });
  renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/win",
    as: { ...ADMIN, role: "viewer" },
  });
  expect(await screen.findByRole("heading", { name: "Security" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Reset firmware" })).not.toBeInTheDocument();
});
