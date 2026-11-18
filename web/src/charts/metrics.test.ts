import { fieldStats, formatValue, pointTimes, refetchMs } from "@/charts/metrics";

it("formats values with their unit", () => {
  expect(formatValue(12.345, "percent")).toBe("12.3 %");
  expect(formatValue(1536, "mib")).toBe("1.5 GiB");
  expect(formatValue(2048, "bytesRate")).toBe("2 KiB/s");
  expect(formatValue(3.25, "opsRate")).toBe("3.3/s");
  // 모르는 값
  expect(formatValue(null, "percent")).toBe("—");
  expect(formatValue(undefined, "mib")).toBe("—");
  expect(formatValue(Number.NaN, "mib")).toBe("—");
});

it("times every point from the start and step", () => {
  const series = {
    start: 1_000,
    step_ms: 5_000,
    columns: { cpu: { avg: [1, null, 3] }, mem_used: { avg: [1] } },
  };
  expect(pointTimes(series, ["cpu", "mem_used"])).toEqual([1_000, 6_000, 11_000]);
  expect(pointTimes(series, ["missing"])).toEqual([]);
});

it("uses the server stats, else computes them from the AVG column", () => {
  const given = { avg: 5, max: 9, min: 1 };
  expect(fieldStats({ start: 0, step_ms: 1, columns: {}, stats: { cpu: given } }, "cpu")).toEqual(
    given,
  );
  // 클러스터 모으기는 AVG 열만 있다. 모르는 칸(null)은 빼고 센다.
  const cluster = { start: 0, step_ms: 1, columns: { cpu: { avg: [2, null, 6, 4] } } };
  expect(fieldStats(cluster, "cpu")).toEqual({ avg: 4, max: 6, min: 2 });
  expect(fieldStats(cluster, "net_rx")).toEqual({ avg: null, max: null, min: null });
});

it("follows the last hour closely and longer ranges every minute", () => {
  expect(refetchMs("hour")).toBe(5_000);
  expect(refetchMs("year")).toBe(60_000);
});
