import { hardware, vm } from "@/test/fixtures";
import {
  agentChannel,
  type GuestInfo,
  guestAddresses,
  guestOs,
  guestPasswordErrors,
} from "@/vms/guest";

it("knows when a VM has a guest agent channel", () => {
  const base = vm("web", "running");
  expect(agentChannel(base)).toBe(false);
  expect(agentChannel({ ...base, spec: { ...base.spec, guest_agent: { freeze_fs: true } } })).toBe(
    true,
  );
  // HA 게스트 감시도 채널을 붙인다.
  expect(
    agentChannel({
      ...base,
      spec: {
        ...base.spec,
        ha: {
          state: "started",
          max_restart: 1,
          max_relocate: 1,
          priority: 0,
          guest_watch: { timeout_secs: 60 },
        },
      },
    }),
  ).toBe(true);
  // 끈 채널은 다시 켤 때까지 도는 QEMU에 남는다.
  expect(
    agentChannel({
      ...base,
      status: { ...base.status, hardware: hardware({ guest_agent: true }) },
    }),
  ).toBe(true);
});

it("lists addresses without loopback and names the OS", () => {
  const info: GuestInfo = {
    collected_at: 1,
    os: { name: "Ubuntu", version: "24.04" },
    interfaces: [
      {
        name: "lo",
        addresses: [
          { ip: "127.0.0.1", prefix: 8 },
          { ip: "::1", prefix: 128 },
        ],
      },
      {
        name: "eth0",
        addresses: [
          { ip: "10.0.0.5", prefix: 24 },
          { ip: "fd00::5", prefix: 64 },
        ],
      },
    ],
  };
  expect(guestAddresses(info)).toEqual(["10.0.0.5/24", "fd00::5/64"]);
  expect(guestOs(info)).toBe("Ubuntu 24.04");
  expect(guestOs({ collected_at: 1, os: { name: "Microsoft Windows 11", version: "11" } })).toBe(
    "Microsoft Windows 11",
  );
  expect(guestOs({ collected_at: 1, os: { id: "mswindows" } })).toBe("mswindows");
  expect(guestOs({ collected_at: 1 })).toBeNull();
});

it("checks guest passwords like the server", () => {
  expect(guestPasswordErrors("root", "pw", "pw")).toEqual({});
  expect(guestPasswordErrors("", "", "")).toEqual({
    user: "vms.guest.errors.user",
    password: "vms.guest.errors.password",
  });
  expect(guestPasswordErrors("a\tb", "x".repeat(257), "")).toEqual({
    user: "vms.guest.errors.user",
    password: "vms.guest.errors.password",
  });
  // 바이트로 센다(한글은 세 바이트).
  expect(guestPasswordErrors("가".repeat(22), "pw", "pw")).toEqual({
    user: "vms.guest.errors.user",
  });
  expect(guestPasswordErrors("root", "pw", "pW")).toEqual({ again: "vms.guest.errors.again" });
});
