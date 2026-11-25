import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { env } from "node:process";

import { toYaml } from "@/declarative/toYaml";

const VM = {
  apiVersion: "orbivisor.io/v1",
  kind: "Vm",
  metadata: {
    name: "web01",
    folder: "/prod/web",
    labels: { "orbivisor.io/apply-set": "web-prod", tier: "front end" },
    annotations: { "orbivisor.io/prune": "false" },
  },
  spec: {
    vcpus: 4,
    memory_mib: 8192,
    cpu: { model: "x86-64-v2", flags: [] },
    disks: [{ volume: "web01-disk0", bus: "virtio", boot_order: 1 }],
    nics: [{ network: "lan", mac: "52:54:00:12:34:56", security_groups: ["web", "ssh"] }],
    cloud_init: { user: "ubuntu", password: { secretRef: "web01-password" }, ssh_keys: [] },
    description: "",
    ha: null,
    tpm: false,
  },
  state: "running",
};

it("writes manifests as block YAML with a --- line per document", () => {
  expect(
    toYaml([{ apiVersion: "orbivisor.io/v1", kind: "Folder", metadata: { name: "web" } }]),
  ).toBe("---\napiVersion: orbivisor.io/v1\nkind: Folder\nmetadata:\n  name: web\n");
  expect(toYaml([VM])).toContain(
    [
      "  disks:",
      "    - volume: web01-disk0",
      "      bus: virtio",
      "      boot_order: 1",
      "  nics:",
      "    - network: lan",
      '      mac: "52:54:00:12:34:56"',
      "      security_groups:",
      "        - web",
      "        - ssh",
    ].join("\n"),
  );
  expect(toYaml([VM])).toContain('    orbivisor.io/prune: "false"');
  expect(toYaml([VM])).toContain("  flags: []\n");
  expect(toYaml([VM])).toContain('  description: ""\n');
  expect(toYaml([{}, []])).toBe("---\n{}\n---\n[]\n");
});

it("quotes strings YAML would read as something else", () => {
  const doc = {
    s: ["true", "No", "null", "~", "123", "1.5", "0x1f", ".inf", "a: b", "#x", "- x", "x ", " x"],
    multi: "line 1\nline 2",
    ko: "웹 서버",
    "key with: colon": 1,
    "123": "number key",
    true: "bool key",
  };
  const yaml = toYaml([doc]);
  for (const s of doc.s) {
    expect(yaml).toContain(`- ${JSON.stringify(s)}\n`);
  }
  expect(yaml).toContain('multi: "line 1\\nline 2"');
  expect(yaml).toContain('ko: "웹 서버"');
  expect(yaml).toContain('"key with: colon": 1');
  expect(yaml).toContain('"123": number key');
  expect(yaml).toContain('"true": bool key');
});

it("escapes characters YAML does not allow in a double-quoted string", () => {
  const yaml = toYaml([{ s: "del\u007f c1\u0080\u009f nel\u0085 ls\u2028 bom\ufeff ok é" }]);
  expect(yaml).toBe('---\ns: "del\\u007f c1\\u0080\\u009f nel\\u0085 ls\\u2028 bom\\ufeff ok é"\n');
});

it("writes nested lists and odd numbers", () => {
  expect(toYaml([{ m: [[1, 2], [], [{ a: -3.25 }]], n: [Number.NaN] }])).toBe(
    '---\nm:\n  -\n    - 1\n    - 2\n  - []\n  -\n    - a: -3.25\n"n":\n  - null\n',
  );
});

// CLI 교차 고정 파일(설계 §14 웹): CLI 시험(`oplctl` `web_cross.rs`)이 `yaml`을 serde-saphyr로 읽어 `value`와 같은지 본다.
// 바꾸려면 `UPDATE_FIXTURES=1 pnpm -C web test toYaml`.
it("matches the YAML fixture the CLI reads back", () => {
  const cases = [
    { name: "vm", value: [VM] },
    {
      name: "many",
      value: [
        { apiVersion: "orbivisor.io/v1", kind: "Folder", metadata: { name: "prod" }, spec: {} },
        {
          apiVersion: "orbivisor.io/v1",
          kind: "Network",
          metadata: { name: "lan", folder: "/prod" },
          spec: { kind: "bridge", bridge: "br0", vlan: 100, subnets: [] },
        },
      ],
    },
    {
      name: "awkward",
      value: [
        {
          strings: ["true", "No", "null", "~", "123", "1.5", "0x1f", ".inf", "a: b", "#x", "- x"],
          spaces: ["x ", " x", "a  b", ""],
          multi: 'line 1\nline 2\ttab "quoted" \\',
          ko: "웹 서버 — 설명",
          "key with: colon": 1,
          "123": "number key",
          numbers: [0, -1, 1.5, 4294967296],
          nested: [[1, 2], [], [{ a: false }]],
          empty: {},
          nothing: null,
          controls: "del\u007f c1\u0080\u009f nel\u0085 ls\u2028\u2029 bom\ufeff",
        },
      ],
    },
  ].map((c) => ({ ...c, yaml: toYaml(c.value) }));
  const text = `${JSON.stringify({ cases }, null, 2)}\n`;
  // 시험은 web/에서 돈다(`pnpm -C web test`).
  const path = resolve("src/declarative/__fixtures__/yaml.json");
  if (env.UPDATE_FIXTURES === "1") {
    writeFileSync(path, text);
  }
  expect(readFileSync(path, "utf8")).toBe(text);
});
