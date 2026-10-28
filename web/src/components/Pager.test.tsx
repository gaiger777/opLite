import { act, render, renderHook, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Pager, usePage } from "@/components/Pager";

const range = (n: number) => Array.from({ length: n }, (_, i) => i + 1);

it("pages through items", () => {
  const { result } = renderHook(() => usePage(range(120), 50));
  expect(result.current.page).toBe(0);
  expect(result.current.pageCount).toBe(3);
  expect(result.current.pageItems).toEqual(range(50));
  act(() => result.current.setPage(2));
  expect(result.current.pageItems).toEqual(range(120).slice(100));
});

it("moves to the last page when items shrink", () => {
  const { result, rerender } = renderHook(({ items }) => usePage(items, 50), {
    initialProps: { items: range(120) },
  });
  act(() => result.current.setPage(2));
  rerender({ items: range(60) });
  expect(result.current.page).toBe(1);
  expect(result.current.pageItems).toEqual([51, 52, 53, 54, 55, 56, 57, 58, 59, 60]);
  // 다시 늘어도 옮긴 페이지에 머문다.
  rerender({ items: range(120) });
  expect(result.current.page).toBe(1);
});

function Paged({ total }: { total: number }) {
  const pager = usePage(range(total), 50);
  return (
    <>
      <p>first {pager.pageItems[0]}</p>
      <Pager {...pager} pageSize={50} total={total} />
    </>
  );
}

it("shows the range and moves between pages", async () => {
  render(<Paged total={120} />);
  expect(screen.getByText("1–50 of 120")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Previous" })).toHaveAttribute("aria-disabled", "true");
  await userEvent.click(screen.getByRole("button", { name: "Next" }));
  expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
  expect(screen.getByText("first 51")).toBeInTheDocument();
  const next = screen.getByRole("button", { name: "Next" });
  await userEvent.click(next);
  expect(screen.getByText("101–120 of 120")).toBeInTheDocument();
  // 마지막 페이지에서 막혀도 포커스는 그대로이고, 또 눌러도 넘어가지 않는다.
  expect(next).toHaveAttribute("aria-disabled", "true");
  expect(next).toHaveFocus();
  await userEvent.click(next);
  expect(screen.getByText("101–120 of 120")).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Previous" }));
  expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
});

it("draws nothing without items", () => {
  render(<Paged total={0} />);
  expect(screen.queryByRole("navigation")).not.toBeInTheDocument();
  expect(screen.queryByRole("button")).not.toBeInTheDocument();
});
