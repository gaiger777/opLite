import { can } from "@/auth/permissions";

it("mirrors the server roles", () => {
  expect(can("admin", "write", "network")).toBe(true);
  expect(can("operator", "operate", "vm")).toBe(true);
  expect(can("operator", "write", "volume")).toBe(true);
  expect(can("operator", "write", "network")).toBe(false);
  expect(can("viewer", "read", "vm")).toBe(true);
  expect(can("viewer", "operate", "vm")).toBe(false);
});
