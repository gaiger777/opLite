import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { type Column, DataTable } from "@/components/data-table/DataTable";
import { useTableState } from "@/components/data-table/state";
import { SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { expectNoAxeViolations } from "@/test/axe";
import { mockLists, renderPage } from "@/test/render";

interface Row {
  id: string;
  name: string;
  size: number;
  kind: string;
}

const ROWS: Row[] = [
  { id: "1", name: "alpha", size: 3, kind: "a" },
  { id: "2", name: "beta", size: 1, kind: "b" },
  { id: "3", name: "gamma", size: 4, kind: "a" },
  { id: "4", name: "delta", size: 2, kind: "b" },
];

const COLUMNS: Column<Row>[] = [
  { id: "name", header: "Name", value: (r) => r.name, cell: (r) => r.name },
  { id: "size", header: "Size", value: (r) => r.size, cell: (r) => `${r.size} GiB`, priority: 2 },
  { id: "kind", header: "Kind", cell: (r) => r.kind, priority: 3 },
];

function Harness({ rows = ROWS, pending = false }: { rows?: Row[]; pending?: boolean }) {
  const state = useTableState("demo", ["kind"]);
  return (
    <>
      <h1 id="demo-title">Demo</h1>
      <DataTable
        id="demo"
        rows={rows}
        columns={COLUMNS}
        rowId={(r) => r.id}
        rowName={(r) => r.name}
        query={{ isPending: pending, isError: false, error: null }}
        empty="Nothing."
        labelledBy="demo-title"
        state={state}
        search={(r) => r.name}
        filters={[
          {
            id: "kind",
            label: "Kind",
            options: [
              { value: "a", label: "A" },
              { value: "b", label: "B" },
            ],
            test: (r, value) => r.kind === value,
          },
        ]}
        views={[{ name: "Only A", query: "kind=a" }]}
        bulk={(chosen) => <span>bulk:{chosen.map((r) => r.name).join(",")}</span>}
        rowHref={(r) => `/demo/${r.name}`}
        peek={(r) => (
          <SheetHeader>
            <SheetTitle>{r.name}</SheetTitle>
            <SheetDescription>{r.size} GiB</SheetDescription>
          </SheetHeader>
        )}
        keys
      />
    </>
  );
}

const names = () =>
  within(screen.getByRole("table", { name: "Demo" }))
    .getAllByRole("row")
    .slice(1)
    .map((row) => within(row).getAllByRole("cell")[1]!.textContent);

beforeEach(() => mockLists({}));
afterEach(() => window.localStorage.clear());

it("keeps search, filter and sort in the address", async () => {
  const { router } = renderPage(<Harness />, { url: "/?q=a&sort=-size" });
  await screen.findByRole("table", { name: "Demo" });
  expect(names()).toEqual(["gamma", "alpha", "delta", "beta"]);
  expect(screen.getByRole("columnheader", { name: /Size/ })).toHaveAttribute(
    "aria-sort",
    "descending",
  );

  await userEvent.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "b");
  expect(names()).toEqual(["delta", "beta"]);
  expect(router.state.location.search).toBe("?q=a&sort=-size&kind=b");

  await userEvent.clear(screen.getByRole("textbox", { name: "Search" }));
  await userEvent.type(screen.getByRole("textbox", { name: "Search" }), "del");
  expect(names()).toEqual(["delta"]);

  // 정렬을 바꾸면 주소와 마지막 정렬(`cols.demo`)에 남긴다.
  await userEvent.click(screen.getByRole("button", { name: /Name/ }));
  expect(router.state.location.search).toContain("sort=name");
  expect(JSON.parse(window.localStorage.getItem("opl.cols.demo")!)).toEqual({
    hidden: [],
    sort: "name",
  });
});

it("selects a range with Shift and drops rows a filter hides", async () => {
  renderPage(<Harness />);
  await screen.findByRole("table", { name: "Demo" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select alpha" }));
  const user = userEvent.setup();
  await user.keyboard("{Shift>}");
  await user.click(screen.getByRole("checkbox", { name: "Select gamma" }));
  await user.keyboard("{/Shift}");
  const bar = screen.getByRole("region", { name: "Bulk actions" });
  expect(bar).toHaveTextContent("3 selected");
  expect(bar).toHaveTextContent("bulk:alpha,beta,gamma");
  expect(screen.getByRole("checkbox", { name: "Select all" })).toBePartiallyChecked();

  await userEvent.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "a");
  expect(screen.getByRole("region", { name: "Bulk actions" })).toHaveTextContent(
    "bulk:alpha,gamma",
  );
  await userEvent.selectOptions(screen.getByRole("combobox", { name: "Kind" }), "");
  // 가려졌던 beta는 다시 보여도 고른 채가 아니다.
  expect(screen.getByRole("checkbox", { name: "Select beta" })).not.toBeChecked();

  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  expect(screen.getByRole("region", { name: "Bulk actions" })).toHaveTextContent("4 selected");
  await userEvent.click(screen.getByRole("checkbox", { name: "Select all" }));
  expect(screen.queryByRole("region", { name: "Bulk actions" })).not.toBeInTheDocument();
});

it("moves, selects, peeks and opens rows from the keyboard", async () => {
  const { router } = renderPage(<Harness />);
  await screen.findByRole("table", { name: "Demo" });
  await userEvent.keyboard("j");
  expect(document.activeElement).toHaveTextContent("alpha");
  await userEvent.keyboard("jj");
  await userEvent.keyboard("k");
  expect(document.activeElement).toHaveTextContent("beta");
  await userEvent.keyboard("x");
  expect(screen.getByRole("checkbox", { name: "Select beta" })).toBeChecked();

  // Space는 미리 보기를 열고, 열린 채 j/k로 행을 옮기면 내용이 바뀐다.
  await userEvent.keyboard(" ");
  const peek = await screen.findByRole("dialog", { name: "beta" });
  expect(peek).toHaveTextContent("1 GiB");
  await userEvent.keyboard("j");
  expect(await screen.findByRole("dialog", { name: "gamma" })).toBeInTheDocument();
  await userEvent.keyboard("{Escape}");
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());

  await userEvent.keyboard("{Enter}");
  await waitFor(() => expect(router.state.location.pathname).toBe("/demo/gamma"));
});

it("opens the peek from the eye button", async () => {
  renderPage(<Harness />);
  await screen.findByRole("table", { name: "Demo" });
  await userEvent.click(screen.getByRole("button", { name: "Preview delta" }));
  const peek = await screen.findByRole("dialog", { name: "delta" });
  await expectNoAxeViolations(peek, { disable: ["region"] });
});

it("saves, applies and deletes views", async () => {
  const { router } = renderPage(<Harness />, { url: "/?q=al" });
  await screen.findByRole("table", { name: "Demo" });
  await userEvent.click(screen.getByRole("button", { name: "Views" }));
  await userEvent.type(screen.getByLabelText("View name"), "Mine");
  await userEvent.click(screen.getByRole("button", { name: "Save view" }));
  expect(JSON.parse(window.localStorage.getItem("opl.views.demo")!)).toEqual([
    { name: "Mine", query: "q=al" },
  ]);
  expect(screen.getByRole("button", { name: "Mine" })).toHaveAttribute("aria-pressed", "true");

  await userEvent.click(screen.getByRole("button", { name: "Only A" }));
  expect(router.state.location.search).toBe("?kind=a");
  expect(names()).toEqual(["alpha", "gamma"]);
  await userEvent.click(screen.getByRole("button", { name: "All" }));
  expect(names()).toHaveLength(4);

  await userEvent.click(screen.getByRole("button", { name: "Delete view Mine" }));
  expect(screen.queryByRole("button", { name: "Mine" })).not.toBeInTheDocument();
  expect(JSON.parse(window.localStorage.getItem("opl.views.demo")!)).toEqual([]);
});

it("hides chosen columns and narrow-screen columns by priority", async () => {
  renderPage(<Harness />);
  await screen.findByRole("table", { name: "Demo" });
  expect(screen.getByRole("columnheader", { name: /Size/ })).toHaveClass("max-lg:hidden");
  expect(screen.getByRole("columnheader", { name: "Kind" })).toHaveClass("max-xl:hidden");
  await userEvent.click(screen.getByRole("button", { name: "Columns" }));
  await userEvent.click(screen.getByRole("checkbox", { name: "Size" }));
  expect(screen.queryByRole("columnheader", { name: /Size/ })).not.toBeInTheDocument();
  expect(JSON.parse(window.localStorage.getItem("opl.cols.demo")!)).toEqual({ hidden: ["size"] });
});

it("shows skeleton rows while the first read is pending", async () => {
  renderPage(<Harness rows={[]} pending />);
  const table = await screen.findByRole("table", { name: "Demo" });
  expect(within(table).getByText("Loading…")).toBeInTheDocument();
  expect(table.querySelectorAll("[data-slot=skeleton]").length).toBeGreaterThan(0);
});

it("draws cards on a phone", async () => {
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    ...original(query),
    matches: query === "(max-width: 767px)",
  })) as typeof window.matchMedia;
  try {
    const { container } = renderPage(<Harness />);
    const list = await screen.findByRole("list", { name: "Demo" });
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    const cards = within(list).getAllByRole("listitem");
    expect(cards).toHaveLength(4);
    // 우선순위 1이 아닌 열(크기·종류)은 카드에 넣지 않는다.
    expect(cards[0]).toHaveTextContent("alpha");
    expect(cards[0]).not.toHaveTextContent("3 GiB");
    expect(within(cards[0]!).getByRole("button", { name: "Preview alpha" })).toBeInTheDocument();
    await expectNoAxeViolations(container, { disable: ["region"] });
  } finally {
    window.matchMedia = original;
  }
});

it("has no accessibility violations with rows selected", async () => {
  const { container } = renderPage(<Harness />);
  await screen.findByRole("table", { name: "Demo" });
  await userEvent.click(screen.getByRole("checkbox", { name: "Select alpha" }));
  await expectNoAxeViolations(container, { disable: ["region"] });
});
