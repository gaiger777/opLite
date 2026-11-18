import {
  formatSort,
  parseSort,
  pickQuery,
  readCols,
  readViews,
  writeCols,
  writeViews,
} from "@/components/data-table/state";

const U = "00000000-0000-0000-0000-0000000000a1";

afterEach(() => window.localStorage.clear());

it("reads and writes a one-column sort", () => {
  expect(parseSort("name")).toEqual({ id: "name", desc: false });
  expect(parseSort("-memory")).toEqual({ id: "memory", desc: true });
  expect(parseSort("")).toBeNull();
  expect(parseSort("-")).toBeNull();
  expect(formatSort({ id: "memory", desc: true })).toBe("-memory");
  expect(formatSort(null)).toBe("");
});

it("keeps only the table's own query keys", () => {
  const params = new URLSearchParams("q=we&tab=x&phase=running&node=");
  expect(pickQuery(params, ["q", "sort", "phase", "node"])).toBe("q=we&phase=running");
});

it("moves the old VM sort into the column settings", () => {
  window.localStorage.setItem("opl.vms.sort", JSON.stringify([{ id: "cpu", desc: false }]));
  expect(readCols(U, "vms")).toEqual({ hidden: [], sort: "cpu" });
  // 다른 표는 옛 정렬을 보지 않는다.
  expect(readCols(U, "nodes")).toEqual({ hidden: [] });
  // 새 설정이 있으면 그것이 이긴다.
  writeCols(U, "vms", { hidden: ["memory"], sort: "-name" });
  expect(readCols(U, "vms")).toEqual({ hidden: ["memory"], sort: "-name" });
});

it("drops broken saved settings", () => {
  window.localStorage.setItem(`opl.${U}.vms.sort`, "{");
  expect(readCols(U, "vms")).toEqual({ hidden: [] });
  window.localStorage.setItem(`opl.${U}.cols.pools`, JSON.stringify({ hidden: [1, "kind"] }));
  expect(readCols(U, "pools")).toEqual({ hidden: ["kind"] });
  window.localStorage.setItem(
    `opl.${U}.views.vms`,
    JSON.stringify([{ name: 1 }, { name: "a", query: "q=1" }]),
  );
  expect(readViews(U, "vms")).toEqual([{ name: "a", query: "q=1" }]);
  window.localStorage.setItem(`opl.${U}.views.vms`, "nope");
  expect(readViews(U, "vms")).toEqual([]);
  writeViews(U, "vms", [{ name: "b", query: "" }]);
  expect(readViews(U, "vms")).toEqual([{ name: "b", query: "" }]);
});

it("keeps views and columns per user and moves the old shared ones to the first reader", () => {
  const other = "00000000-0000-0000-0000-0000000000a2";
  window.localStorage.setItem("opl.views.vms", JSON.stringify([{ name: "old", query: "q=1" }]));
  window.localStorage.setItem("opl.cols.vms", JSON.stringify({ hidden: ["node"] }));
  expect(readViews(U, "vms")).toEqual([{ name: "old", query: "q=1" }]);
  expect(readCols(U, "vms")).toEqual({ hidden: ["node"] });
  expect(window.localStorage.getItem("opl.views.vms")).toBeNull();
  expect(window.localStorage.getItem("opl.cols.vms")).toBeNull();
  // 옮긴 뒤 다른 사용자는 보지 않는다.
  expect(readViews(other, "vms")).toEqual([]);
  expect(readCols(other, "vms")).toEqual({ hidden: [] });
  writeViews(other, "vms", [{ name: "mine", query: "" }]);
  expect(readViews(U, "vms")).toEqual([{ name: "old", query: "q=1" }]);
});
