import { can } from "@/auth/permissions";

it("mirrors the server roles", () => {
  expect(can("admin", "write", "network")).toBe(true);
  expect(can("operator", "operate", "vm")).toBe(true);
  expect(can("operator", "write", "volume")).toBe(true);
  expect(can("operator", "write", "network")).toBe(false);
  expect(can("viewer", "read", "vm")).toBe(true);
  expect(can("viewer", "operate", "vm")).toBe(false);
});

it("keeps accounts to admins", () => {
  expect(can("admin", "read", "user")).toBe(true);
  expect(can("admin", "write", "join-token")).toBe(true);
  for (const role of ["operator", "viewer"] as const) {
    expect(can(role, "read", "user")).toBe(false);
    expect(can(role, "read", "api-token")).toBe(false);
    expect(can(role, "write", "join-token")).toBe(false);
  }
});
