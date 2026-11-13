import { emptyRow, fenceErrors, fromView, toMethod } from "@/nodes/fence";

it("reads the saved methods and writes them back without the stored password", () => {
  const ipmi = fromView({
    kind: "ipmi",
    address: "10.0.0.11",
    port: 623,
    user: "admin",
    has_password: true,
    lanplus: false,
  });
  expect(ipmi).toMatchObject({
    address: "10.0.0.11",
    port: "623",
    hasPassword: true,
    password: "",
  });
  // 비밀번호를 비우면 빼서 저장된 값을 둔다.
  expect(toMethod(ipmi)).toEqual({
    kind: "ipmi",
    address: "10.0.0.11",
    port: 623,
    user: "admin",
    lanplus: false,
  });
  expect(toMethod({ ...ipmi, password: "s3cret" })).toMatchObject({ password: "s3cret" });
  // 지우기는 빈 글을 보낸다.
  expect(toMethod({ ...ipmi, clearPassword: true })).toMatchObject({ password: "" });
  const redfish = fromView({
    kind: "redfish",
    url: "https://bmc",
    user: "root",
    has_password: false,
    insecure: true,
  });
  // 빈 시스템 ID·CA는 보내지 않는다.
  expect(toMethod(redfish)).toEqual({
    kind: "redfish",
    url: "https://bmc",
    user: "root",
    insecure: true,
  });
  expect(toMethod({ ...redfish, systemId: " 1 ", caPem: "PEM" })).toMatchObject({
    system_id: "1",
    ca_pem: "PEM",
  });
  const command = fromView({ kind: "command", off: "pdu off $X", status: "pdu status $X" });
  expect(toMethod(command)).toEqual({
    kind: "command",
    off: "pdu off $X",
    status: "pdu status $X",
  });
});

it("checks the required fields of each kind", () => {
  expect(fenceErrors([emptyRow("ipmi"), emptyRow("redfish"), emptyRow("command")])).toEqual({
    "0.address": "nodes.fence.errors.required",
    "0.user": "nodes.fence.errors.required",
    "1.address": "nodes.fence.errors.required",
    "1.user": "nodes.fence.errors.required",
    "2.off": "nodes.fence.errors.required",
    "2.status": "nodes.fence.errors.required",
  });
  expect(
    fenceErrors([
      { ...emptyRow("ipmi"), address: "bmc", user: "u", port: "0" },
      { ...emptyRow("redfish"), address: "ftp://bmc", user: "u" },
    ]),
  ).toEqual({ "0.port": "nodes.fence.errors.port", "1.address": "nodes.fence.errors.url" });
  expect(
    fenceErrors([
      { ...emptyRow("ipmi"), address: "bmc", user: "u" },
      { ...emptyRow("redfish"), address: "http://bmc:8000", user: "u" },
    ]),
  ).toEqual({});
});
