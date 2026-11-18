import {
  formatSort,
  parseSort,
  pickQuery,
  readCols,
  readViews,
  writeCols,
  writeViews,
} from "@/components/data-table/state";

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
  expect(readCols("vms")).toEqual({ hidden: [], sort: "cpu" });
  // 다른 표는 옛 정렬을 보지 않는다.
  expect(readCols("nodes")).toEqual({ hidden: [] });
  // 새 설정이 있으면 그것이 이긴다.
  writeCols("vms", { hidden: ["memory"], sort: "-name" });
  expect(readCols("vms")).toEqual({ hidden: ["memory"], sort: "-name" });
});

it("drops broken saved settings", () => {
  window.localStorage.setItem("opl.vms.sort", "{");
  expect(readCols("vms")).toEqual({ hidden: [] });
  window.localStorage.setItem("opl.cols.pools", JSON.stringify({ hidden: [1, "kind"] }));
  expect(readCols("pools")).toEqual({ hidden: ["kind"] });
  window.localStorage.setItem(
    "opl.views.vms",
    JSON.stringify([{ name: 1 }, { name: "a", query: "q=1" }]),
  );
  expect(readViews("vms")).toEqual([{ name: "a", query: "q=1" }]);
  window.localStorage.setItem("opl.views.vms", "nope");
  expect(readViews("vms")).toEqual([]);
  writeViews("vms", [{ name: "b", query: "" }]);
  expect(readViews("vms")).toEqual([{ name: "b", query: "" }]);
});
