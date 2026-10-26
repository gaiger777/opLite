import { fireEvent, render, screen } from "@testing-library/react";

import { CopyButton } from "@/components/CopyButton";
import { Toaster } from "@/components/ui/sonner";
import { ThemeProvider } from "@/lib/theme";

function setup() {
  render(
    <ThemeProvider>
      <CopyButton text="secret" label="Copy the token" />
      <Toaster />
    </ThemeProvider>,
  );
  fireEvent.click(screen.getByRole("button", { name: "Copy the token" }));
}

// navigator 전체를 바꾸지 않고 clipboard만 바꾼 뒤 되돌린다.
const original = Object.getOwnPropertyDescriptor(navigator, "clipboard");

function setClipboard(value: unknown) {
  Object.defineProperty(navigator, "clipboard", { configurable: true, value });
}

afterEach(() => {
  if (original) {
    Object.defineProperty(navigator, "clipboard", original);
  } else {
    Reflect.deleteProperty(navigator, "clipboard");
  }
});

it("says it copied", async () => {
  const writeText = vi.fn(() => Promise.resolve());
  setClipboard({ writeText });
  setup();
  expect(await screen.findByText("Copied.")).toBeInTheDocument();
  expect(writeText).toHaveBeenCalledWith("secret");
});

it("says it could not copy without a clipboard", async () => {
  // 보안 맥락(HTTPS)이 아니면 navigator.clipboard가 없다.
  setClipboard(undefined);
  setup();
  expect(
    await screen.findByText("Could not copy. Select the text and copy it yourself."),
  ).toBeInTheDocument();
});
