import { formatCapacity, formatMib, formatTime, uuidTime } from "@/lib/format";

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
});
