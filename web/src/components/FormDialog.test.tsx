import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ApiError } from "@/api/client";
import { FormDialog } from "@/components/FormDialog";

function setup(onSubmit: () => Promise<boolean | void>) {
  const onClose = vi.fn();
  render(
    <FormDialog title="Create" submitLabel="Save" onClose={onClose} onSubmit={onSubmit}>
      <input aria-label="Name" />
    </FormDialog>,
  );
  return onClose;
}

it("closes after a successful submit", async () => {
  const onClose = setup(async () => {});
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(onClose).toHaveBeenCalledOnce();
});

it("stays open on input errors and shows server errors", async () => {
  let fail = false;
  const onClose = setup(async () => {
    if (fail) {
      throw new ApiError(409, "name_taken", "exists");
    }
    return false;
  });
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(onClose).not.toHaveBeenCalled();

  fail = true;
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("That name is already in use.");
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByLabelText("Name")).toBeEnabled();
});

it("locks inputs and closing while submitting", async () => {
  const onClose = setup(() => new Promise(() => {}));
  await userEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(screen.getByLabelText("Name")).toBeDisabled();
  expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
  await userEvent.keyboard("{Escape}");
  expect(onClose).not.toHaveBeenCalled();
});

it("submits once when submitted twice in the same tick", () => {
  const onSubmit = vi.fn(() => new Promise<void>(() => {}));
  setup(onSubmit);
  const form = screen.getByRole("button", { name: "Save" }).closest("form")!;
  fireEvent.submit(form);
  fireEvent.submit(form);
  expect(onSubmit).toHaveBeenCalledOnce();
});
