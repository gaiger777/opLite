import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { vm } from "@/test/fixtures";
import { keysym } from "@/vms/keysyms";
import VmConsole from "@/vms/VmConsole";

// vi.mock은 파일 맨 위로 올라가므로 가짜 클래스도 함께 올린다.
const { FakeRfb, created } = vi.hoisted(() => {
  const created: InstanceType<typeof FakeRfb>[] = [];
  class FakeRfb extends EventTarget {
    scaleViewport = false;
    resizeSession = false;
    disconnected = false;
    ctrlAltDel = 0;
    keys: number[] = [];
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
    sendKey(sym: number) {
      this.keys.push(sym);
    }
    focus() {}
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

it("does not connect to a stopped VM", () => {
  render(<VmConsole vm={vm("web", "stopped")} />);
  expect(created).toHaveLength(0);
  expect(screen.getByText("The VM is not running.")).toBeInTheDocument();
});

it("maps characters to keysyms", () => {
  expect(keysym("a")).toBe(0x61);
  expect(keysym("é")).toBe(0xe9);
  expect(keysym("\n")).toBe(0xff0d);
  expect(keysym("한")).toBe(0x0100d55c);
});
