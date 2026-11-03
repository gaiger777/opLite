import {
  cidrFamily,
  fromRule,
  moveRule,
  type RuleRow,
  toRule,
  validateRules,
  validPorts,
} from "@/security-groups/rules";

const row = (patch: Partial<RuleRow>): RuleRow => ({
  direction: "in",
  action: "accept",
  protocol: "tcp",
  ports: "",
  remote: "",
  ...patch,
});

it("round-trips rules and leaves out empty values", () => {
  const full = {
    direction: "in",
    action: "accept",
    protocol: "tcp",
    ports: "22",
    remote: "10.0.0.0/8",
  } as const;
  expect(toRule(fromRule(full))).toEqual(full);
  expect(fromRule({ direction: "out", action: "drop" })).toEqual(
    row({ direction: "out", action: "drop", protocol: "any" }),
  );
  expect(toRule(row({ protocol: "any" }))).toEqual({ direction: "in", action: "accept" });
  // 포트는 tcp·udp에서만 보낸다. 앞뒤 빈칸은 뺀다.
  expect(toRule(row({ protocol: "icmp", ports: "22" }))).toEqual({
    direction: "in",
    action: "accept",
    protocol: "icmp",
  });
  expect(toRule(row({ protocol: "udp", ports: " 53 ", remote: " ::1 " }))).toEqual({
    direction: "in",
    action: "accept",
    protocol: "udp",
    ports: "53",
    remote: "::1",
  });
});

it("follows the server's port range rule", () => {
  for (const ok of ["1", "22", "65535", "8000-8080", "80-80"]) {
    expect(validPorts(ok)).toBe(true);
  }
  for (const bad of ["", "0", "65536", "8080-8000", "22-", "-22", "a", "1 - 2", "0-10"]) {
    expect(validPorts(bad)).toBe(false);
  }
});

it("tells the CIDR family", () => {
  expect(cidrFamily("10.0.0.0/8")).toBe(4);
  expect(cidrFamily("192.168.1.10")).toBe(4);
  expect(cidrFamily("0.0.0.0/0")).toBe(4);
  expect(cidrFamily("2001:db8::/32")).toBe(6);
  expect(cidrFamily("::/0")).toBe(6);
  expect(cidrFamily("fe80::1")).toBe(6);
  for (const bad of ["", "10.0.0/8", "256.0.0.0", "10.0.0.0/33", "::/129", "1::2::3", "a/b/c"]) {
    expect(cidrFamily(bad)).toBeUndefined();
  }
  expect(cidrFamily("10.0.0.0/x")).toBeUndefined();
});

it("checks ports, addresses and ICMP families", () => {
  expect(
    validateRules([
      row({ ports: "22" }),
      row({ ports: "99999" }),
      // tcp·udp가 아니면 포트는 보내지 않으므로 보지 않는다.
      row({ protocol: "any", ports: "x" }),
      row({ remote: "10.0.0.0/40" }),
      row({ protocol: "icmp", remote: "2001:db8::/32" }),
      row({ protocol: "icmpv6", remote: "10.0.0.0/8" }),
      row({ protocol: "icmpv6", remote: "2001:db8::/32" }),
    ]),
  ).toEqual({
    "rules.1.ports": "securityGroups.errors.ports",
    "rules.3.remote": "securityGroups.errors.remote",
    "rules.4.remote": "securityGroups.errors.icmpRemote",
    "rules.5.remote": "securityGroups.errors.icmpv6Remote",
  });
});

it("moves a rule up or down within the list", () => {
  expect(moveRule(["a", "b", "c"], 1, -1)).toEqual(["b", "a", "c"]);
  expect(moveRule(["a", "b", "c"], 1, 1)).toEqual(["a", "c", "b"]);
  expect(moveRule(["a", "b", "c"], 0, -1)).toEqual(["a", "b", "c"]);
  expect(moveRule(["a", "b", "c"], 2, 1)).toEqual(["a", "b", "c"]);
});
