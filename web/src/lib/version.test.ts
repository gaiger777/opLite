import { compareVersions } from "@/lib/version";

// opl_types::version의 비교 표와 같다.
it.each([
  ["1.2.3", "1.2.3", 0],
  ["1.01", "1.1", 0],
  ["1.0", "1.0-0", 0],
  ["0:1.0", "1.0", 0],
  ["0.10.0", "0.9.0", 1],
  ["1.0~ci12", "1.0~ci5", 1],
  ["1.2", "1.2.1", -1],
  ["1.0~rc1", "1.0", -1],
  ["1.0~~", "1.0~", -1],
  ["1.0", "1.0a", -1],
  ["1.0a", "1.0+", -1],
  ["1.0", "1.0+1", -1],
  ["1.0b", "1.0a", 1],
  ["1:0.1", "2.0", 1],
  ["2:1.0", "10:0.1", -1],
  ["1.0-1", "1.0-2", -1],
  ["1.0-10", "1.0-9", 1],
  ["1.0-rc-2", "1.0-rc-10", -1],
  ["1.0-1", "1.0", 1],
  ["99999999999999999999999", "99999999999999999999998", 1],
])("compares %s and %s", (a, b, want) => {
  expect(compareVersions(a, b)).toBe(want);
  expect(compareVersions(b, a)).toBe(-want || 0);
});

it("compares odd strings without throwing", () => {
  for (const a of ["", ":", "a:b", "-", "1:", "~", "1.0-", ":-:", "é1", "1:2:3"]) {
    for (const b of ["", "1.0", "~", a]) {
      expect(Math.abs(compareVersions(a, b))).toBeLessThanOrEqual(1);
    }
    expect(compareVersions(a, a)).toBe(0);
  }
  expect(compareVersions("", "1")).toBe(-1);
  expect(compareVersions("~", "")).toBe(-1);
});
