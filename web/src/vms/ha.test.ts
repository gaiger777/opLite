import { DEFAULT_HA, haEnabled, haPolicy, haSetting } from "@/vms/ha";

it("reads the old boolean and the policy object", () => {
  const policy = { state: "started", max_restart: 1, max_relocate: 1 } as const;
  expect(haEnabled({})).toBe(false);
  expect(haEnabled({ ha: false })).toBe(false);
  expect(haEnabled({ ha: true })).toBe(true);
  expect(haEnabled({ ha: policy })).toBe(true);
  expect(haEnabled({ ha: { ...policy, state: "stopped", group: "rack-a" } })).toBe(true);
  expect(haEnabled({ ha: { ...policy, state: "ignored" } })).toBe(false);
});

it("reads a policy with defaults and writes the default policy as the old true", () => {
  expect(haPolicy({ ha: false })).toBeNull();
  expect(haPolicy({})).toBeNull();
  expect(haPolicy({ ha: true })).toEqual(DEFAULT_HA);
  expect(
    haPolicy({ ha: { state: "stopped", group: "g", max_restart: 2, max_relocate: 0 } }),
  ).toEqual({ state: "stopped", group: "g", max_restart: 2, max_relocate: 0 });
  expect(haSetting(null)).toBe(false);
  expect(haSetting(DEFAULT_HA)).toBe(true);
  // 빈 그룹 이름은 그룹 없음이다.
  expect(haSetting({ ...DEFAULT_HA, group: "" })).toBe(true);
  expect(haSetting({ ...DEFAULT_HA, group: "rack-a" })).toEqual({ ...DEFAULT_HA, group: "rack-a" });
  expect(haSetting({ ...DEFAULT_HA, state: "ignored", group: "" })).toEqual({
    ...DEFAULT_HA,
    state: "ignored",
  });
  expect(haSetting({ ...DEFAULT_HA, max_restart: 0 })).toEqual({ ...DEFAULT_HA, max_restart: 0 });
});
