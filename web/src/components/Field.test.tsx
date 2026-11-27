import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Field } from "@/components/Field";
import { HelpPanel } from "@/help/HelpPanel";
import { closeHelp } from "@/help/help";

afterEach(() => closeHelp());

function setup(error?: string, info?: string) {
  render(
    <>
      <Field id="name" label="Name" help="Lowercase letters." info={info} error={error}>
        {(a11y) => <input id="name" {...a11y} />}
      </Field>
      <HelpPanel />
    </>,
  );
  return screen.getByRole("textbox", { name: "Name" });
}

it("describes the input with the help line", () => {
  const input = setup();
  expect(input).toHaveAccessibleDescription("Lowercase letters.");
  expect(input).not.toBeInvalid();
});

it("keeps the help line under the error (plan 77-4)", () => {
  const input = setup("pools.errors.path");
  expect(input).toHaveAccessibleDescription(
    "Lowercase letters. Enter an absolute path (starting with /).",
  );
  expect(input).toBeInvalid();
  expect(screen.getByText("Lowercase letters.")).toBeVisible();
});

it("opens the help panel from the ⓘ and returns focus on Escape", async () => {
  const user = userEvent.setup();
  setup(undefined, "test.none");
  const info = screen.getByRole("button", { name: "Help: Name" });
  await user.click(info);
  const panel = await screen.findByRole("dialog", { name: "Name" });
  // 주제가 없으면 칸의 한 줄 설명을 다시 보인다.
  expect(await within(panel).findByText("Lowercase letters.")).toBeInTheDocument();
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(info).toHaveFocus();
});
