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

afterEach(() => {
  vi.unstubAllGlobals();
});

it("says it copied", async () => {
  const writeText = vi.fn(() => Promise.resolve());
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
  setup();
  expect(await screen.findByText("Copied.")).toBeInTheDocument();
  expect(writeText).toHaveBeenCalledWith("secret");
});

it("says it could not copy without a clipboard", async () => {
  // 보안 맥락(HTTPS)이 아니면 navigator.clipboard가 없다.
  vi.stubGlobal("navigator", { ...navigator, clipboard: undefined });
  setup();
  expect(
    await screen.findByText("Could not copy. Select the text and copy it yourself."),
  ).toBeInTheDocument();
});
