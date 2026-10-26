import { render, screen } from "@testing-library/react";

import { Field } from "@/components/Field";

function setup(error?: string) {
  render(
    <Field id="name" label="Name" hint="Lowercase letters." error={error}>
      {(a11y) => <input id="name" {...a11y} />}
    </Field>,
  );
  return screen.getByRole("textbox", { name: "Name" });
}

it("describes the input with the hint", () => {
  const input = setup();
  expect(input).toHaveAccessibleDescription("Lowercase letters.");
  expect(input).not.toBeInvalid();
});

it("describes the input with the error instead of the hint", () => {
  const input = setup("pools.errors.path");
  expect(input).toHaveAccessibleDescription("Enter an absolute path (starting with /).");
  expect(input).toBeInvalid();
  expect(screen.queryByText("Lowercase letters.")).not.toBeInTheDocument();
});
