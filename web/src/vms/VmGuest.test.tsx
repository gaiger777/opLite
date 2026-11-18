import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import type { GuestInfoResponse } from "@/vms/guest";
import { NODE, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";
import { GUEST_REFRESH_MS } from "@/vms/guest";
import { VmDetailPage } from "@/vms/VmDetailPage";

const web = vm("web", "running", {
  spec: { ...vm("web", "running").spec, guest_agent: { freeze_fs: true } },
});

const ANSWER: GuestInfoResponse = {
  state: "answered",
  info: {
    collected_at: 1_790_000_000,
    agent_version: "8.2.2",
    os: { id: "ubuntu", name: "Ubuntu", version: "24.04", kernel: "6.8.0-45-generic" },
    hostname: "web.lab",
    interfaces: [
      { name: "lo", addresses: [{ ip: "127.0.0.1", prefix: 8 }] },
      {
        name: "eth0",
        mac: "52:54:00:12:34:56",
        addresses: [
          { ip: "10.0.0.5", prefix: 24 },
          { ip: "fd00::5", prefix: 64 },
        ],
      },
    ],
    filesystems: [
      { mountpoint: "/", fstype: "ext4", used_bytes: 4 * 1024 ** 3, total_bytes: 16 * 1024 ** 3 },
    ],
  },
};

/** 게스트 정보 응답. 새로 묻기(`refresh=true`)의 수를 센다. */
function guest(answer: GuestInfoResponse, refreshed: GuestInfoResponse = answer) {
  const calls = { refresh: 0 };
  server.use(
    http.get(apiUrl(`/vms/${web.id}/guest`), ({ request }) => {
      if (new URL(request.url).searchParams.get("refresh") === "true") {
        calls.refresh += 1;
        return HttpResponse.json(refreshed);
      }
      return HttpResponse.json(answer);
    }),
  );
  return calls;
}

it("shows what the guest agent reported on the summary", async () => {
  mockLists({ vm: [web], node: [NODE] });
  guest(ANSWER);
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web" });
  const card = (await screen.findByRole("heading", { name: "Guest agent" })).closest(
    "[data-slot=card]",
  )!;
  expect(await within(card as HTMLElement).findByText("web.lab")).toBeInTheDocument();
  expect(card).toHaveTextContent("SettingsOn (freezes file systems, shuts down through the agent)");
  expect(card).toHaveTextContent("Operating systemUbuntu 24.04");
  // 루프백은 빼고 보인다.
  expect(card).toHaveTextContent("IP addresses10.0.0.5/24, fd00::5/64");
});

it("does not ask a VM without an agent channel", async () => {
  const plain = vm("plain", "running");
  mockLists({ vm: [plain], node: [NODE] });
  let asked = false;
  server.use(
    http.get(apiUrl(`/vms/${plain.id}/guest`), () => {
      asked = true;
      return HttpResponse.json({ state: "no_agent" });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/plain" });
  const card = (await screen.findByRole("heading", { name: "Guest agent" })).closest(
    "[data-slot=card]",
  )!;
  expect(card).toHaveTextContent("SettingsOff");
  expect(asked).toBe(false);
});

it("shows the guest tab with interfaces, file systems and when it was collected", async () => {
  mockLists({ vm: [web], node: [NODE] });
  guest(ANSWER);
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=guest" });
  const nics = await screen.findByRole("table", { name: "Network interfaces" });
  expect(within(nics).getByText("eth0").closest("tr")).toHaveTextContent(
    "eth052:54:00:12:34:5610.0.0.5/24, fd00::5/64",
  );
  const fs = screen.getByRole("table", { name: "File systems" });
  expect(within(fs).getByText("/").closest("tr")).toHaveTextContent("ext44 GiB of 16 GiB (25%)");
  expect(screen.getByText("6.8.0-45-generic")).toBeInTheDocument();
  expect(screen.getByText("8.2.2")).toBeInTheDocument();
  expect(screen.getByText(/^Collected /)).toBeInTheDocument();
});

it.each([
  ["no_agent", /install qemu-guest-agent in the guest/],
  ["not_running", /^The VM is not running\.$/],
  ["unreachable", /Cannot reach the node the VM runs on/],
  ["unsupported", /runs an older Orbivisor that cannot read guest information/],
] as const)("says why there is no guest information (%s)", async (state, message) => {
  mockLists({ vm: [web], node: [NODE] });
  guest({ state });
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=guest" });
  expect(await screen.findAllByText(message)).not.toHaveLength(0);
  expect(screen.queryByRole("table", { name: "File systems" })).not.toBeInTheDocument();
});

it("refreshes at most once every five seconds", async () => {
  mockLists({ vm: [web], node: [NODE] });
  const calls = guest({ state: "unreachable" }, ANSWER);
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=guest" });
  const user = userEvent.setup();
  const refresh = await screen.findByRole("button", { name: "Refresh" });
  await user.click(refresh);
  expect(await screen.findByRole("table", { name: "File systems" })).toBeInTheDocument();
  expect(calls.refresh).toBe(1);
  expect(refresh).toBeDisabled();
  await act(() => new Promise((resolve) => setTimeout(resolve, GUEST_REFRESH_MS + 100)));
  expect(refresh).toBeEnabled();
}, 10_000);

it("sets a guest password without keeping it", async () => {
  mockLists({ vm: [web], node: [NODE] });
  guest(ANSWER);
  let body: unknown;
  server.use(
    http.post(apiUrl(`/vms/${web.id}/guest/password`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json({ user: "root" });
    }),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=guest" });
  const user = userEvent.setup();
  await screen.findByRole("table", { name: "File systems" });
  await user.click(screen.getByRole("button", { name: "Set guest password" }));
  const dialog = await screen.findByRole("dialog");
  expect(within(dialog).getByLabelText("User")).toHaveValue("root");
  await user.type(within(dialog).getByLabelText("New password"), "s3cret!");
  await user.type(within(dialog).getByLabelText("New password again"), "s3cret?");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(within(dialog).getByText("The passwords do not match.")).toBeInTheDocument();
  expect(body).toBeUndefined();
  await user.clear(within(dialog).getByLabelText("New password again"));
  await user.type(within(dialog).getByLabelText("New password again"), "s3cret!");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Set the password of root on web")).toBeInTheDocument();
  expect(body).toEqual({ user: "root", password: "s3cret!" });
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  // 다시 열면 비어 있다.
  await user.click(screen.getByRole("button", { name: "Set guest password" }));
  expect(await screen.findByLabelText("New password")).toHaveValue("");
});

it("shows the server's reason when the guest password cannot be set", async () => {
  mockLists({ vm: [web], node: [NODE] });
  guest({ state: "unreachable" });
  server.use(
    http.post(apiUrl(`/vms/${web.id}/guest/password`), () =>
      HttpResponse.json(
        {
          code: "guest_agent_unavailable",
          message: 'the guest agent of VM "web" did not answer: timed out',
        },
        { status: 409 },
      ),
    ),
  );
  renderPage(<VmDetailPage />, { path: "/vms/:name", url: "/vms/web?tab=guest" });
  const user = userEvent.setup();
  await user.click(await screen.findByRole("button", { name: "Set guest password" }));
  const dialog = await screen.findByRole("dialog");
  // Windows가 아니면 root를 채워 둔다. 이름을 바꿀 수 있다.
  await user.clear(within(dialog).getByLabelText("User"));
  await user.type(within(dialog).getByLabelText("User"), "Administrator");
  await user.type(within(dialog).getByLabelText("New password"), "pw");
  await user.type(within(dialog).getByLabelText("New password again"), "pw");
  await user.click(within(dialog).getByRole("button", { name: "Save" }));
  const alert = await within(dialog).findByRole("alert");
  expect(alert).toHaveTextContent(
    'The guest agent cannot be used: the guest agent of VM "web" did not answer: timed out',
  );
  expect(alert).not.toHaveTextContent("pw ");
});

it("lets only writers set a guest password, and only on a running VM", async () => {
  const stopped = vm("web", "stopped", { spec: { ...web.spec, power: "stopped" } });
  mockLists({ vm: [stopped], node: [NODE] });
  guest({ state: "not_running" });
  const { unmount } = renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/web?tab=guest",
  });
  const button = await screen.findByRole("button", { name: "Set guest password" });
  expect(button).toHaveAttribute("aria-disabled", "true");
  expect(button).toHaveAccessibleDescription("The VM must be running to set a guest password.");
  unmount();
  renderPage(<VmDetailPage />, {
    path: "/vms/:name",
    url: "/vms/web?tab=guest",
    as: { ...ADMIN, role: "viewer" },
  });
  expect(await screen.findByRole("button", { name: "Refresh" })).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Set guest password" })).not.toBeInTheDocument();
});
