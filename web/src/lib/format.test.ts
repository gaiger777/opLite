import { formatMib, uuidTime } from "@/lib/format";

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
