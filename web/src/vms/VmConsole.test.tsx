import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";

import { vm } from "@/test/fixtures";
import { keyEvents, keysym, SHIFT } from "@/vms/keysyms";
import VmConsole, { PASTE_LIMIT } from "@/vms/VmConsole";

// vi.mock은 파일 맨 위로 올라가므로 가짜 클래스도 함께 올린다.
const { FakeRfb, created } = vi.hoisted(() => {
  const created: InstanceType<typeof FakeRfb>[] = [];
  class FakeRfb extends EventTarget {
    scaleViewport = false;
    resizeSession = false;
    disconnected = false;
    ctrlAltDel = 0;
    keys: [number, string | null, boolean | undefined][] = [];
    focused = 0;
    constructor(
      readonly target: HTMLElement,
      readonly url: string,
    ) {
      super();
      created.push(this);
    }
    disconnect() {
      this.disconnected = true;
    }
    sendCtrlAltDel() {
      this.ctrlAltDel += 1;
    }
    sendKey(sym: number, code: string | null, down?: boolean) {
      this.keys.push([sym, code, down]);
    }
    focus() {
      this.focused += 1;
    }
  }
  return { FakeRfb, created };
});

vi.mock("@novnc/novnc", () => ({ default: FakeRfb }));

beforeEach(() => {
  created.length = 0;
});

it("connects to the VM console and reconnects after a disconnect", async () => {
  const web = vm("web", "running");
  const { unmount } = render(<VmConsole vm={web} />);
  expect(created).toHaveLength(1);
  expect(created[0]!.url).toBe(`ws://${window.location.host}/api/v1/vms/${web.id}/console`);
  expect(created[0]!.scaleViewport).toBe(true);

  created[0]!.dispatchEvent(new Event("connect"));
  expect(await screen.findByText("Connected")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Ctrl+Alt+Del" }));
  expect(created[0]!.ctrlAltDel).toBe(1);

  created[0]!.dispatchEvent(new Event("disconnect"));
  await userEvent.click(await screen.findByRole("button", { name: "Reconnect" }));
  expect(created).toHaveLength(2);
  expect(created[0]!.disconnected).toBe(true);

  unmount();
  expect(created[1]!.disconnected).toBe(true);
});

it("ignores a late disconnect from the previous connection", async () => {
  render(<VmConsole vm={vm("web", "running")} />);
  created[0]!.dispatchEvent(new Event("disconnect"));
  await userEvent.click(await screen.findByRole("button", { name: "Reconnect" }));
  created[1]!.dispatchEvent(new Event("connect"));
  expect(await screen.findByText("Connected")).toBeInTheDocument();

  // 앞 연결의 disconnect()가 뒤늦게 알린다.
  act(() => {
    created[0]!.dispatchEvent(new Event("disconnect"));
  });
  expect(screen.getByRole("status")).toHaveTextContent("Connected");
  expect(screen.queryByRole("button", { name: "Reconnect" })).not.toBeInTheDocument();
});

it("names the screen and tells how to release the keyboard", () => {
  render(<VmConsole vm={vm("web", "running")} />);
  expect(screen.getByRole("region", { name: "VM screen" })).toBeInTheDocument();
  expect(screen.getByText(/Click outside it to release the keyboard/)).toBeInTheDocument();
});

it("types the clipboard with Shift and focuses the screen", async () => {
  const user = userEvent.setup();
  vi.spyOn(navigator.clipboard, "readText").mockResolvedValue("aB!\r\n");
  render(<VmConsole vm={vm("web", "running")} />);
  created[0]!.dispatchEvent(new Event("connect"));
  await user.click(await screen.findByRole("button", { name: "Type clipboard" }));
  await vi.waitFor(() => expect(created[0]!.focused).toBe(1));
  expect(created[0]!.keys).toEqual([
    [0x61, null, undefined],
    [SHIFT, "ShiftLeft", true],
    [0x42, null, undefined],
    [SHIFT, "ShiftLeft", false],
    [SHIFT, "ShiftLeft", true],
    [0x21, null, undefined],
    [SHIFT, "ShiftLeft", false],
    [0xff0d, null, undefined],
  ]);
});

it("types at most PASTE_LIMIT characters and says so", async () => {
  const user = userEvent.setup();
  const warning = vi.spyOn(toast, "warning");
  vi.spyOn(navigator.clipboard, "readText").mockResolvedValue("a".repeat(PASTE_LIMIT + 10));
  render(<VmConsole vm={vm("web", "running")} />);
  created[0]!.dispatchEvent(new Event("connect"));
  await user.click(await screen.findByRole("button", { name: "Type clipboard" }));
  await vi.waitFor(() => expect(created[0]!.focused).toBe(1));
  expect(created[0]!.keys).toHaveLength(PASTE_LIMIT);
  expect(warning).toHaveBeenCalledExactlyOnceWith(
    `Only the first ${PASTE_LIMIT} characters were typed.`,
  );
  warning.mockRestore();
});

it("does not connect to a stopped VM", () => {
  render(<VmConsole vm={vm("web", "stopped")} />);
  expect(created).toHaveLength(0);
  expect(screen.getByRole("status")).toHaveTextContent("The VM is not running.");
});

it("maps characters to keysyms", () => {
  expect(keysym("a")).toBe(0x61);
  expect(keysym("é")).toBe(0xe9);
  expect(keysym("\n")).toBe(0xff0d);
  expect(keysym("\r")).toBe(0xff0d);
  expect(keysym("\b")).toBe(0xff08);
  expect(keysym("한")).toBe(0x0100d55c);
  expect(keysym("\x1b")).toBeNull();
  expect(keysym("\x7f")).toBeNull();
});

it("turns text into key events", () => {
  const press = (sym: number) => [sym, null];
  const shifted = (sym: number) => [
    [SHIFT, "ShiftLeft", true],
    [sym, null],
    [SHIFT, "ShiftLeft", false],
  ];
  // 대문자와 Shift 기호는 Shift로 감싼다. 숫자와 Shift 없는 기호는 그대로다.
  expect(keyEvents("Z1~-")).toEqual([...shifted(0x5a), press(0x31), ...shifted(0x7e), press(0x2d)]);
  expect(keyEvents(`~!@#$%^&*()_+{}|:"<>?`)).toHaveLength(21 * 3);
  // \r\n은 Return 한 번, \n만 있어도 Return, 다른 제어 문자는 버린다.
  expect(keyEvents("a\r\nb\nc\x00\x1b\x7f\b")).toEqual([
    press(0x61),
    press(0xff0d),
    press(0x62),
    press(0xff0d),
    press(0x63),
    press(0xff08),
  ]);
  expect(keyEvents("\r\r")).toEqual([press(0xff0d), press(0xff0d)]);
});
