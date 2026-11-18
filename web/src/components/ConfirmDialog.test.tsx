import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ConfirmDialog } from "@/components/ConfirmDialog";
import { expectNoAxeViolations } from "@/test/axe";

function open(props: Partial<React.ComponentProps<typeof ConfirmDialog>> = {}) {
  const onConfirm = vi.fn(() => Promise.resolve());
  render(
    <ConfirmDialog
      open
      onOpenChange={() => {}}
      title="Delete 3 things"
      description="They are gone for good."
      confirmLabel="Delete"
      onConfirm={onConfirm}
      {...props}
    />,
  );
  return { dialog: screen.getByRole("alertdialog"), onConfirm };
}

it("lists the impact and asks for the count of many", async () => {
  const { dialog, onConfirm } = open({
    typeToConfirm: 3,
    impact: ["VMs using it: 2", "Snapshots: 4"],
  });
  const impact = within(dialog).getByRole("list", { name: "What this affects" });
  expect(
    within(impact)
      .getAllByRole("listitem")
      .map((li) => li.textContent),
  ).toEqual(["VMs using it: 2", "Snapshots: 4"]);
  // 기본 포커스는 취소, 위험 단추는 오른쪽 끝이다.
  expect(within(dialog).getByRole("button", { name: "Cancel" })).toHaveFocus();
  const buttons = within(dialog).getAllByRole("button");
  expect(buttons.at(-1)).toHaveTextContent("Delete");
  const confirm = within(dialog).getByRole("button", { name: "Delete" });
  const input = within(dialog).getByLabelText("Type 3 to confirm");
  expect(input).toHaveAttribute("inputmode", "numeric");
  await userEvent.type(input, "2");
  expect(confirm).toBeDisabled();
  await userEvent.clear(input);
  await userEvent.type(input, " 3 ");
  expect(confirm).toBeEnabled();
  await userEvent.click(confirm);
  expect(onConfirm).toHaveBeenCalledOnce();
  await expectNoAxeViolations(document.body, { disable: ["region"] });
});

it("asks for the exact name of one", async () => {
  const { dialog } = open({ typeToConfirm: "web" });
  const input = within(dialog).getByLabelText("Type web to confirm");
  await userEvent.type(input, " web");
  expect(within(dialog).getByRole("button", { name: "Delete" })).toBeDisabled();
  expect(within(dialog).queryByRole("list")).not.toBeInTheDocument();
});
