import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { env } from "node:process";

import i18n from "@/i18n";
import { builderCtx, TEMPLATE } from "@/test/builder";
import { folderId } from "@/test/fixtures";
import { cliPlan, cliText, commandText, shq, sizeArg } from "@/vmBuilder/model/cli";
import { drop } from "@/vmBuilder/model/drop";
import { parts } from "@/vmBuilder/model/parts";
import { requests } from "@/vmBuilder/model/requests";
import { type BuilderState, initialState, setName } from "@/vmBuilder/model/state";
import type { VmFormValues } from "@/vms/form";

const ctx = builderCtx();

function build(name: string, steps: [string, string][], patch: Partial<VmFormValues> = {}) {
  const s = steps.reduce(
    (s, [id, at]) => {
      const p = parts(s, ctx).find((x) => x.id === id)!;
      const next = drop(s, p, at, ctx);
      expect(next, `${id} on ${at}`).not.toBe(s);
      return next;
    },
    setName(initialState("q35"), name),
  );
  return { ...s, values: { ...s.values, ...patch } };
}
const t = i18n.getFixedT("en");

it.each([
  ["web-01", "web-01"],
  ["zone=a", "zone=a"],
  ["nic0=10.0.0.5/24,gw=10.0.0.1", "nic0=10.0.0.5/24,gw=10.0.0.1"],
  ["ssh-ed25519 AAAA me@host", "'ssh-ed25519 AAAA me@host'"],
  ["it's", "'it'\\''s'"],
  ["$(rm -rf /)", "'$(rm -rf /)'"],
  ["", "''"],
  ["*/15", "'*/15'"],
])("quotes %j for the shell as %s", (raw, quoted) => {
  expect(shq(raw)).toBe(quoted);
});

it("writes sizes in the biggest whole unit", () => {
  expect(sizeArg(20 * 1024 ** 3)).toBe("20G");
  expect(sizeArg(1536 * 1024 ** 2)).toBe("1536M");
  expect(sizeArg(2 * 1024 ** 4)).toBe("2T");
  expect(sizeArg(512 * 1024)).toBe("512K");
  expect(sizeArg(1000)).toBe("1000");
});

it("breaks long commands before flags", () => {
  const text = commandText([
    "orbivisorctl",
    "vm",
    "create",
    "web",
    "--cores",
    "2",
    "--memory",
    "4096",
    "--machine",
    "q35",
    "--disk",
    "web-disk0",
    "--nic",
    "lan",
    "--start",
  ]);
  expect(text.split("\n").length).toBeGreaterThan(1);
  for (const line of text.split("\n").slice(0, -1)) {
    expect(line.endsWith(" \\")).toBe(true);
  }
  expect(text.replaceAll(" \\\n  ", " ")).toBe(
    "orbivisorctl vm create web --cores 2 --memory 4096 --machine q35 --disk web-disk0 --nic lan --start",
  );
});

it("prints volumes, the zone rule and the VM with notes for missing flags", () => {
  let s = build("web", [
    ["size:m", "compute"],
    ["pool:ceph", "disks"],
    ["network:lan", "nics"],
    ["zone:a", "placement"],
    ["ci:packages", "cloudInit"],
    ["ci:password", "cloudInit"],
  ]);
  s = {
    ...s,
    values: {
      ...s.values,
      nics: [{ ...s.values.nics[0]!, model: "rtl8139" as never, mac: "52:54:00:12:34:56" }],
      cloudInit: {
        ...s.values.cloudInit,
        password: "hunter2",
        sshKeys: "ssh-ed25519 AAAA me@host",
      },
    },
  };
  const text = cliText(s, ctx, t).replaceAll(" \\\n  ", " ");
  expect(text).not.toContain("hunter2");
  expect(text.split("\n").slice(0, 3)).toEqual([
    "# No CLI flag for NIC model per NIC — use the JSON request instead.",
    "# No CLI flag for fixed MAC addresses — use the JSON request instead.",
    "# The password is asked for by --ci-password-prompt (it is not shown here).",
  ]);
  expect(text).toContain(
    "cat > user-data.yaml <<'EOF'\n#cloud-config\npackage_update: true\npackages: [qemu-guest-agent]\nEOF",
  );
  expect(text).toContain("orbivisorctl volume create web-disk0 --pool ceph --size 20G");
  expect(text).toContain(
    "orbivisorctl placement-rule create web-zone --kind node-affinity --vm web --node-tag zone=a",
  );
  const vm = text.slice(text.indexOf("orbivisorctl vm create")).replaceAll(" \\\n  ", " ");
  expect(vm).toBe(
    "orbivisorctl vm create web --cores 2 --memory 4096 --machine q35 --guest-agent on --disk web-disk0 " +
      "--nic lan --ci-password-prompt --ci-ssh-key 'ssh-ed25519 AAAA me@host' --ci-user-data user-data.yaml --start",
  );
});

it("picks another heredoc end when the data has one", () => {
  const s = build("web", [["ci:user", "cloudInit"]]);
  s.values.cloudInit.userData = "#cloud-config\nEOF\n";
  expect(cliText(s, ctx, t)).toContain("<<'EOF_'\n#cloud-config\nEOF\nEOF_");
});

it("speaks the notes in Korean", () => {
  const s = build("web", [["network:lan", "nics"]], {
    snapshotSchedule: { every: "daily", keep: "7", memory: false },
  });
  expect(cliText(s, ctx, i18n.getFixedT("ko")).split("\n")[0]).toBe(
    "# 스냅샷 예약은(는) CLI 플래그가 없습니다 — JSON 요청을 쓰세요.",
  );
});

/**
 * 대표 상태 열둘(설계 §7.6 교차 시험): 웹이 짓는 CLI 명령과 `POST /vms`(또는 배포) 본문을 고정 파일로 남긴다. Rust 시험
 * (`crates/orbivisorctl`)이 이 파일의 명령을 CLI 파서에 넣어 가짜 API가 받은 본문이 웹의 본문과 같은지 본다. 파일이 다르면
 * 실패한다 — 바꿀 때는 `UPDATE_FIXTURES=1 pnpm -C web test src/vmBuilder`.
 */
const CASES: [string, () => BuilderState][] = [
  [
    "minimal",
    () =>
      build("web", [
        ["size:m", "compute"],
        ["pool:ceph", "disks"],
        ["network:lan", "nics"],
      ]),
  ],
  [
    "existing-volume-user-net",
    () =>
      build(
        "vol",
        [
          ["volume:spare", "disks"],
          ["network:user", "nics"],
        ],
        { start: false },
      ),
  ],
  [
    "windows",
    () =>
      build("win", [
        ["pool:ceph", "disks"],
        ["network:lan", "nics"],
        ["os:windows", "vm"],
        ["iso:alpine", "cdroms"],
      ]),
  ],
  [
    "two-disks-cd",
    () =>
      build("multi", [
        ["pool:ceph", "disks"],
        ["volume:spare", "disks"],
        ["iso:alpine", "cdroms"],
        ["network:lan", "nics"],
      ]),
  ],
  [
    "sdn-and-groups",
    () => {
      const s = build("sdn", [
        ["pool:ceph", "disks"],
        ["network:lan", "nics"],
        ["sg:web", "nics.0"],
        ["sg:ssh", "nics.0"],
        ["network:tenant", "nics"],
      ]);
      s.values.nics[1] = { ...s.values.nics[1]!, ips: "10.10.0.5" };
      return s;
    },
  ],
  [
    "cloud-init",
    () => {
      const s = build("ci", [
        ["pool:ceph", "disks"],
        ["network:lan", "nics"],
        ["ci:user", "cloudInit"],
        ["addr:static", "nics.0"],
      ]);
      s.values.cloudInit = {
        ...s.values.cloudInit,
        hostname: "ci.example",
        user: "ops",
        sshKeys: "ssh-ed25519 AAAAC3Nza ops@laptop\nssh-rsa AAAAB3Nza other",
        dns: "1.1.1.1, 8.8.8.8",
        search: "example.com",
        networks: [
          {
            ...s.values.cloudInit.networks[0]!,
            ipv4Address: "192.168.1.10/24",
            ipv4Gateway: "192.168.1.1",
            ipv6: "slaac",
          },
        ],
      };
      return s;
    },
  ],
  [
    "packages",
    () =>
      build("pkg", [
        ["pool:ceph", "disks"],
        ["network:lan", "nics"],
        ["ci:packages", "cloudInit"],
      ]),
  ],
  [
    "node-cpu",
    () =>
      build(
        "pin",
        [
          ["pool:local", "disks"],
          ["network:lan", "nics"],
          ["node:n1", "placement"],
          ["cpu:x86-64-v3", "compute"],
        ],
        { pinned: true, cpuFlags: ["-avx512f", "+aes"] },
      ),
  ],
  [
    "ha",
    () =>
      build(
        "ha",
        [
          ["pool:ceph", "disks"],
          ["network:lan", "nics"],
          ["ha", "protection"],
          ["haGroup:pair", "protection"],
        ],
        {
          haPriority: "70",
          guestWatch: true,
          guestWatchSecs: "120",
        },
      ),
  ],
  [
    "zone",
    () =>
      build("zoned", [
        ["pool:ceph", "disks"],
        ["network:lan", "nics"],
        ["zone:b", "placement"],
      ]),
  ],
  [
    "folder-carried",
    () =>
      build(
        "team",
        [
          ["size:l", "compute"],
          ["pool:ceph", "disks"],
          ["network:lan", "nics"],
          [`folder:${folderId("prod")}`, "folder"],
        ],
        {
          restart: "on-failure",
          maxVcpus: "8",
          maxMemory: "16384",
          start: false,
        },
      ),
  ],
  [
    "template",
    () => {
      const s = build("from-tmpl", [
        [`template:${TEMPLATE.id}`, "vm"],
        ["size:l", "compute"],
        ["network:tenant", "nics.0"],
        ["node:n1", "placement"],
        ["ci:user", "cloudInit"],
      ]);
      s.values.disks[0] = { ...s.values.disks[0]!, sizeGib: "40" };
      s.values.cloudInit.user = "ops";
      return { ...s, base: { kind: "template", id: TEMPLATE.id, linked: false, pool: "ceph" } };
    },
  ],
];

it("keeps the CLI cross-check fixture (web CLI lines → same POST body as the web)", () => {
  const cases = CASES.map(([name, make]) => {
    const s = make();
    const plan = cliPlan(s, ctx);
    const all = requests(s, ctx);
    return {
      name,
      files: plan.files,
      commands: plan.commands,
      missing: plan.missing,
      // 마지막 요청(VM 만들기·배포) 앞에 보낼 볼륨·배치 규칙 만들기(차례대로)
      before: all.slice(0, -1),
      request: all.at(-1)!,
    };
  });
  expect(cases).toHaveLength(12);
  // 다 쓸 수 있는 CLI(플래그 없는 칸 없음)
  for (const c of cases) {
    expect(c.missing, c.name).toEqual([]);
  }
  const text = `${JSON.stringify({ nodes: { n1: "node-a", n2: "node-b", n3: "node-arm" }, cases }, null, 2)}\n`;
  // 시험은 web/에서 돈다(`pnpm -C web test`).
  const path = resolve("src/vmBuilder/model/__fixtures__/cli.json");
  if (env.UPDATE_FIXTURES === "1") {
    writeFileSync(path, text);
  }
  expect(readFileSync(path, "utf8")).toBe(text);
});

it("writes the commands each case needs", () => {
  const argv = (name: string) =>
    cliPlan(CASES.find(([n]) => n === name)![1](), ctx).commands.at(-1)!;
  expect(argv("windows")).toEqual(
    expect.arrayContaining(["--preset", "windows11", "--cdrom", "alpine"]),
  );
  expect(argv("windows")).not.toContain("--tpm");
  expect(argv("two-disks-cd")).toEqual(expect.arrayContaining(["--boot", "disk0,disk1,cdrom0"]));
  expect(argv("sdn-and-groups")).toEqual(
    expect.arrayContaining(["--nic", "lan,sg=web+ssh", "--nic", "tenant,ip=10.10.0.5"]),
  );
  expect(argv("cloud-init")).toEqual(
    expect.arrayContaining([
      "--ci-ip",
      "nic0=192.168.1.10/24,gw=192.168.1.1",
      "--ci-ip6",
      "nic0=slaac",
      "--ci-dns",
      "8.8.8.8",
    ]),
  );
  expect(argv("node-cpu")).toEqual(
    expect.arrayContaining([
      "--node",
      "node-a",
      "--pinned",
      "--cpu-model",
      "x86-64-v3",
      "--cpu-flags=-avx512f,+aes",
    ]),
  );
  expect(argv("ha")).toEqual(
    expect.arrayContaining([
      "--ha",
      "--ha-group",
      "pair",
      "--ha-priority",
      "70",
      "--guest-watch",
      "120",
    ]),
  );
  expect(argv("ha")).not.toContain("--max-restart");
  expect(argv("folder-carried")).toEqual(
    expect.arrayContaining([
      "--folder",
      "/team/prod",
      "--restart",
      "on-failure",
      "--max-vcpus",
      "8",
    ]),
  );
  expect(argv("folder-carried")).not.toContain("--start");
  expect(argv("template")).toEqual([
    "orbivisorctl",
    "template",
    "deploy",
    "tmpl",
    "--name",
    "from-tmpl",
    "--full",
    "--node",
    "node-a",
    "--pool",
    "ceph",
    "--cpus",
    "4",
    "--memory",
    "8192",
    "--network",
    "nic0=tenant",
    "--disk-size",
    "disk0=40G",
    "--ci-user",
    "ops",
    "--start",
  ]);
});

it("notes what a template deployment cannot take by CLI", () => {
  const s = CASES.find(([n]) => n === "template")![1]();
  s.values.folder = folderId("team");
  expect(cliPlan(s, ctx).missing).toEqual(["folder"]);
});
