import { builderCtx, TEMPLATE } from "@/test/builder";
import { drop } from "@/vmBuilder/model/drop";
import { parts } from "@/vmBuilder/model/parts";
import {
  checkBody,
  jsonText,
  masked,
  requestPath,
  requests,
  zoneRule,
} from "@/vmBuilder/model/requests";
import { type BuilderState, initialState, setName } from "@/vmBuilder/model/state";
import { PASSWORD_MASK } from "@/vms/cloudInit";
import { toSpec } from "@/vms/form";

const ctx = builderCtx();

function build(steps: [string, string][], from = initialState("q35")): BuilderState {
  return steps.reduce((s, [id, at]) => {
    const p = parts(s, ctx).find((x) => x.id === id)!;
    const next = drop(s, p, at, ctx);
    expect(next, `${id} on ${at}`).not.toBe(s);
    return next;
  }, from);
}
const GIB = 1024 ** 3;

it("creates new volumes, then the VM with the form's spec", () => {
  const s = setName(
    build([
      ["size:m", "compute"],
      ["pool:ceph", "disks"],
      ["volume:spare", "disks"],
      ["pool:local", "disks"],
      ["network:lan", "nics"],
    ]),
    "web",
  );
  const reqs = requests(s, ctx);
  expect(reqs.map((r) => `${r.method} ${requestPath(r)}`)).toEqual([
    "POST /api/v1/volumes",
    "POST /api/v1/volumes",
    "POST /api/v1/vms",
  ]);
  expect(reqs[0]!.body).toEqual({
    name: "web-disk0",
    spec: { pool: "ceph", size_bytes: 20 * GIB, format: "qcow2" },
  });
  expect(reqs[1]!.body).toMatchObject({ name: "web-disk2", spec: { pool: "local" } });
  // 만들기와 같은 `toSpec` — 켠 채로 만든다(폼의 처음 값).
  expect(reqs[2]!.body).toEqual({ name: "web", spec: toSpec(s.values, "running", [false]) });
  expect(reqs[2]!.body).toMatchObject({
    spec: {
      cpu: { sockets: 1, cores: 2, model: "host" },
      memory_mib: 4096,
      machine: "q35",
      disks: [
        { volume: "web-disk0", bus: "virtio-blk", boot_order: null },
        { volume: "spare", bus: "virtio-blk", boot_order: null },
        { volume: "web-disk2", bus: "virtio-blk", boot_order: null },
      ],
      nics: [{ network: "lan", model: "virtio", mac: null }],
      power: "running",
    },
  });
});

it("creates the zone rule before the VM", () => {
  const s = setName(
    build([
      ["zone:b", "placement"],
      ["network:lan", "nics"],
    ]),
    "web",
  );
  expect(zoneRule(s)).toEqual({
    name: "web-zone",
    spec: { kind: "node-affinity", vms: ["web"], node_tag: "zone=b", enabled: true },
  });
  expect(zoneRule({ ...s, zone: { ...s.zone!, hard: false } })!.spec.kind).toBe(
    "soft-node-affinity",
  );
  expect(requests(s, ctx).map((r) => r.path)).toEqual(["/placement-rules", "/vms"]);
  expect(zoneRule(initialState("q35"))).toBeNull();
});

it("deploys a template with only the changed fields", () => {
  let s = setName(build([[`template:${TEMPLATE.id}`, "vm"]]), "web");
  expect(requests(s, ctx)).toEqual([
    {
      method: "POST",
      path: "/vms/{key}/clone",
      key: TEMPLATE.id,
      body: { name: "web", start: true },
    },
  ]);
  s = build(
    [
      ["size:l", "compute"],
      ["network:tenant", "nics.0"],
      ["node:n1", "placement"],
      ["cpu:x86-64-v3", "compute"],
      ["ci:user", "cloudInit"],
      ["zone:a", "placement"],
    ],
    s,
  );
  s = {
    ...s,
    base: { kind: "template", id: TEMPLATE.id, linked: true, pool: "" },
    values: {
      ...s.values,
      start: false,
      disks: [{ ...s.values.disks[0]!, sizeGib: "20.5" }],
      nics: [{ ...s.values.nics[0]!, ips: "10.10.0.7" }],
      cloudInit: { ...s.values.cloudInit, user: "ops", password: "secret" },
    },
  };
  const reqs = requests(s, ctx);
  expect(reqs.map((r) => r.path)).toEqual(["/placement-rules", "/vms/{key}/clone"]);
  expect(requestPath(reqs[1]!)).toBe(`/api/v1/vms/${TEMPLATE.id}/clone`);
  expect(reqs[1]!.body).toEqual({
    name: "web",
    linked: true,
    vcpus: 4,
    memory_mib: 8192,
    nics: ["tenant"],
    ips: [["10.10.0.7"]],
    disks: [Math.ceil((20.5 * GIB) / 1024 ** 2) * 1024 ** 2],
    cloud_init: { user: "ops", password: "secret" },
    cpu_model: "x86-64-v3",
  });
});

it("checks without the plain password and not for templates", () => {
  let s = setName(
    build([
      ["pool:ceph", "disks"],
      ["zone:a", "placement"],
      ["ci:password", "cloudInit"],
    ]),
    "web",
  );
  s = { ...s, values: { ...s.values, cloudInit: { ...s.values.cloudInit, password: "secret" } } };
  const body = checkBody(s, ctx)!;
  expect(body.name).toBe("web");
  expect(body.labels).toEqual({});
  expect(body.volumes?.map((v) => v.name)).toEqual(["web-disk0"]);
  expect(body.rules?.map((r) => r.name)).toEqual(["web-zone"]);
  expect(JSON.stringify(body)).not.toContain("secret");
  expect(body.spec.cloud_init).toBeDefined();
  expect(checkBody(build([[`template:${TEMPLATE.id}`, "vm"]]), ctx)).toBeNull();
  // 이 화면이 이미 만든 규칙은 "만들 규칙"이 아니다(보내면 서버가 name_taken).
  expect(checkBody(s, ctx, (name) => name === "web-zone")!.rules).toEqual([]);
});

it("masks the cloud-init password in the JSON preview", () => {
  let s = setName(build([["ci:password", "cloudInit"]]), "web");
  s = { ...s, values: { ...s.values, cloudInit: { ...s.values.cloudInit, password: "hunter2" } } };
  const reqs = requests(s, ctx);
  const text = jsonText(reqs, "masked!");
  expect(text).not.toContain("hunter2");
  expect(text).toContain(`"password": "${PASSWORD_MASK}"`);
  expect(text.split("\n")[0]).toBe("# masked!");
  expect(text).toContain('# POST /api/v1/vms\n{\n  "name": "web",');
  // 원래 요청(만들기)은 그대로다.
  expect(JSON.stringify(reqs)).toContain("hunter2");
  const plain = requests(setName(initialState("q35"), "web"), ctx);
  expect(jsonText(plain, "masked!").startsWith("# POST")).toBe(true);
  expect(masked(plain[0]!).masked).toBe(false);
});
