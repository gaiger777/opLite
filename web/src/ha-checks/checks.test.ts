import type { Node } from "@/api/types";
import {
  allowedActions,
  type CheckForm,
  checkErrors,
  emptyCheckForm,
  emptyTarget,
  failingChecks,
  fromSpec,
  interfaceNames,
  sameSpec,
  sharedPools,
  targetText,
  toSpec,
  validIp,
} from "@/ha-checks/checks";
import { haCheck, NET_NODE, NODE, pool } from "@/test/fixtures";

function form(part: Partial<CheckForm>): CheckForm {
  return { ...emptyCheckForm(), name: "stor", ...part };
}

const ping = (addresses: string, need = "", via = "") => ({
  ...emptyTarget(),
  addresses,
  need,
  via,
});

it("reads a saved check with the defaults the server leaves out", () => {
  const f = fromSpec(haCheck("stor", 1));
  expect(f).toEqual({
    name: "stor",
    role: "storage",
    kind: "link",
    target: { ...emptyTarget(), iface: "bond1" },
    onFail: "",
    grace: "10",
    overrides: [],
    enabled: true,
  });
  // 기본값만 다른 스펙은 같다.
  expect(sameSpec(haCheck("stor", 1).spec, toSpec(f))).toBe(true);
  expect(sameSpec(haCheck("stor", 1).spec, toSpec({ ...f, grace: "11" }))).toBe(false);
});

it("writes ping targets, node overrides and the chosen reaction", () => {
  const spec = toSpec(
    form({
      role: "vm",
      kind: "ping",
      target: ping("10.0.0.1, 10.0.0.2 fd00::1", "2", "bond1"),
      onFail: "fence",
      grace: "",
      overrides: [{ node: "node-b", target: ping("10.1.0.1") }],
      enabled: false,
    }),
  );
  expect(spec).toEqual({
    role: "vm",
    target: {
      kind: "ping",
      addresses: ["10.0.0.1", "10.0.0.2", "fd00::1"],
      need: 2,
      via: "bond1",
    },
    on_fail: "fence",
    grace_secs: 10,
    nodes: { "node-b": { kind: "ping", addresses: ["10.1.0.1"], need: 1 } },
    enabled: false,
  });
  // 저장된 것을 다시 읽으면 같은 스펙이다.
  const saved = haCheck("lan", 2, spec);
  expect(toSpec(fromSpec(saved))).toEqual(spec);
  expect(sameSpec(saved.spec, toSpec(fromSpec(saved)))).toBe(true);
});

it("checks the form like the server", () => {
  expect(checkErrors(form({ target: { ...emptyTarget(), iface: "bond1" } }), [])).toEqual({});
  expect(checkErrors(form({ name: "Bad" }), []).name).toBe("forms.name");
  expect(checkErrors(form({ name: "cluster-link-1" }), []).name).toBe("haChecks.errors.reserved");
  expect(checkErrors(form({}), ["stor"]).name).toBe("haChecks.errors.exists");
  // 바꾸기는 이름을 보지 않는다.
  expect(
    checkErrors(form({ name: "Bad", target: { ...emptyTarget(), iface: "eth0" } }), null),
  ).toEqual({});
  const target = (f: Partial<CheckForm>) => checkErrors(form(f), null).target;
  expect(target({ target: { ...emptyTarget(), iface: "" } })).toBe("haChecks.errors.iface");
  expect(target({ target: { ...emptyTarget(), iface: "a/b" } })).toBe("haChecks.errors.iface");
  expect(target({ target: { ...emptyTarget(), iface: "abcdefghijklmnop" } })).toBe(
    "haChecks.errors.iface",
  );
  expect(target({ kind: "ping", target: ping("") })).toBe("haChecks.errors.addressCount");
  expect(
    target({
      kind: "ping",
      target: ping(Array.from({ length: 9 }, (_, i) => `10.0.0.${i}`).join()),
    }),
  ).toBe("haChecks.errors.addressCount");
  expect(target({ kind: "ping", target: ping("10.0.0.256") })).toBe("haChecks.errors.address");
  expect(target({ kind: "ping", target: ping("10.0.0.1, 10.0.0.1") })).toBe(
    "haChecks.errors.duplicate",
  );
  expect(target({ kind: "ping", target: ping("10.0.0.1", "0") })).toBe("haChecks.errors.need");
  expect(target({ kind: "ping", target: ping("10.0.0.1", "2") })).toBe("haChecks.errors.need");
  expect(target({ kind: "ping", target: ping("10.0.0.1", "", "a:b") })).toBe(
    "haChecks.errors.iface",
  );
  expect(target({ kind: "pool", target: emptyTarget() })).toBe("haChecks.errors.pool");
  expect(target({ role: "vm", kind: "pool", target: { ...emptyTarget(), pool: "nas" } })).toBe(
    "haChecks.errors.poolRole",
  );
  const ok = { target: { ...emptyTarget(), iface: "eth0" } };
  expect(checkErrors(form({ ...ok, role: "storage", onFail: "migrate" }), null).onFail).toBe(
    "haChecks.errors.action",
  );
  expect(checkErrors(form({ ...ok, role: "management", onFail: "migrate" }), null).onFail).toBe(
    "haChecks.errors.action",
  );
  expect(checkErrors(form({ ...ok, grace: "0" }), null).grace).toBe("haChecks.errors.grace");
  // 검사 두 회차(10초)보다 짧은 grace는 받지 않는다.
  expect(checkErrors(form({ ...ok, grace: "9" }), null).grace).toBe("haChecks.errors.grace");
  expect(checkErrors(form({ ...ok, grace: "10" }), null).grace).toBeUndefined();
  expect(checkErrors(form({ ...ok, grace: "601" }), null).grace).toBe("haChecks.errors.grace");
  expect(checkErrors(form({ ...ok, grace: "600" }), null).grace).toBeUndefined();
  // 덮기 줄마다 따로 본다.
  expect(
    checkErrors(
      form({
        ...ok,
        overrides: [
          { node: "node-b", target: { ...emptyTarget(), iface: "eth1" } },
          { node: "", target: { ...emptyTarget(), iface: "eth1" } },
          { node: "node-c", target: emptyTarget() },
        ],
      }),
      null,
    ),
  ).toEqual({
    "override.1": "haChecks.errors.overrideNode",
    "override.2": "haChecks.errors.iface",
  });
});

it("allows the reactions of each role", () => {
  expect(allowedActions("management")).toEqual(["warn", "fence"]);
  expect(allowedActions("storage")).toEqual(["warn", "restart_elsewhere", "fence"]);
  expect(allowedActions("vm")).toEqual(["warn", "migrate", "restart_elsewhere", "fence"]);
});

it("reads IP addresses like the server", () => {
  expect(validIp("10.0.0.1")).toBe(true);
  expect(validIp("fd00::1")).toBe(true);
  expect(validIp("010.0.0.1")).toBe(false);
  expect(validIp("10.0.0")).toBe(false);
  expect(validIp("nas.example")).toBe(false);
});

it("shows targets like the CLI", () => {
  expect(targetText({ kind: "link", iface: "eth0" })).toBe("link:eth0");
  expect(targetText({ kind: "pool", name: "nas" })).toBe("pool:nas");
  expect(targetText({ kind: "ping", addresses: ["10.0.0.1"], need: 1 })).toBe("ping:10.0.0.1");
  expect(
    targetText({ kind: "ping", addresses: ["10.0.0.1", "10.0.0.2"], via: "bond1", need: 2 }),
  ).toBe("ping:10.0.0.1,10.0.0.2@bond1 need=2");
});

it("offers the interface names of all nodes and only shared pools", () => {
  const other: Node = {
    ...NODE,
    id: "n2",
    status: { ...NODE.status, interfaces: [{ name: "eth9", kind: "ethernet", state: "up" }] },
  };
  expect(interfaceNames([NET_NODE, other, NODE])).toEqual([
    "bond0",
    "eno1",
    "eno2",
    "eth9",
    "vmbr0",
  ]);
  expect(
    sharedPools([
      pool("local"),
      pool("shared-dir", { shared: true }),
      pool("nas", { backend: { kind: "nfs", server: "nas", export: "/x" } }),
    ]),
  ).toEqual(["nas", "shared-dir"]);
  expect(failingChecks(NET_NODE).map((c) => c.name)).toEqual(["stor"]);
});
