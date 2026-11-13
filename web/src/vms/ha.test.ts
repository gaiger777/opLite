import { haEnabled } from "@/vms/ha";

it("reads the old boolean and the policy object", () => {
  const policy = { state: "started", max_restart: 1, max_relocate: 1 } as const;
  expect(haEnabled({})).toBe(false);
  expect(haEnabled({ ha: false })).toBe(false);
  expect(haEnabled({ ha: true })).toBe(true);
  expect(haEnabled({ ha: policy })).toBe(true);
  expect(haEnabled({ ha: { ...policy, state: "stopped", group: "rack-a" } })).toBe(true);
  expect(haEnabled({ ha: { ...policy, state: "ignored" } })).toBe(false);
});
