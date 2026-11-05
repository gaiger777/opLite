import {
  formatBytes,
  formatCapacity,
  formatDate,
  formatMib,
  formatTime,
  uuidTime,
} from "@/lib/format";

it("reads the time from a UUIDv7", () => {
  expect(uuidTime("019a0f3c-8a10-7000-8000-000000000000").toISOString()).toBe(
    "2025-10-23T04:03:37.104Z",
  );
});

it("formats memory sizes", () => {
  expect(formatMib(512)).toBe("512 MiB");
  expect(formatMib(4096)).toBe("4 GiB");
  expect(formatMib(1536)).toBe("1536 MiB");
});

it("formats capacities to one decimal", () => {
  expect(formatCapacity(512)).toBe("512 MiB");
  expect(formatCapacity(1024)).toBe("1 GiB");
  expect(formatCapacity(1536)).toBe("1.5 GiB");
  expect(formatCapacity(15890)).toBe("15.5 GiB");
});

it("shows the full year", () => {
  const time = new Date("2025-10-23T04:03:37.104Z");
  expect(formatTime(time, "en")).toContain("2025");
  expect(formatTime(time, "ko")).toContain("2025");
  expect(formatDate(time, "en")).toContain("2025");
  expect(formatDate(time, "en")).not.toContain(":");
});

it("shows dates in UTC like the server and oplctl", () => {
  // 어느 시간대의 브라우저에서도 같은 날이다(둘 중 하나는 로컬 시간대에서 다른 날이 된다).
  expect(formatDate(new Date("2026-01-01T00:30:00Z"), "en")).toBe("Jan 1, 2026");
  expect(formatDate(new Date("2025-12-31T23:30:00Z"), "en")).toBe("Dec 31, 2025");
});

it("formats byte counts", () => {
  expect(formatBytes(0)).toBe("0 B");
  expect(formatBytes(512 * 1024 ** 2)).toBe("512 MiB");
  expect(formatBytes(1.5 * 1024 ** 3)).toBe("1.5 GiB");
  expect(formatBytes(64 * 1024 ** 4)).toBe("64 TiB");
});

it("moves to the next unit when rounding reaches 1024", () => {
  expect(formatBytes(1048570)).toBe("1 MiB");
  expect(formatBytes(1024 ** 3 - 1)).toBe("1 GiB");
  expect(formatBytes(1023)).toBe("1023 B");
});
