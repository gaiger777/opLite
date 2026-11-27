import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { ApiError, NetworkError } from "@/api/client";
import mapEn from "@/clusterMap/i18n/en.json";
import mapKo from "@/clusterMap/i18n/ko.json";
import dndEn from "@/dnd/i18n/en.json";
import dndKo from "@/dnd/i18n/ko.json";
import en from "@/i18n/en.json";
import { errorMessage, FIXED_MESSAGE } from "@/i18n/errors";
import i18n from "@/i18n";
import ko from "@/i18n/ko.json";
import builderEn from "@/vmBuilder/i18n/en.json";
import builderKo from "@/vmBuilder/i18n/ko.json";
import exportsEn from "@/exports/i18n/en.json";
import exportsKo from "@/exports/i18n/ko.json";
import vmwareEn from "@/vmware/i18n/en.json";
import vmwareKo from "@/vmware/i18n/ko.json";
import historyEn from "@/history/i18n/en.json";
import historyKo from "@/history/i18n/ko.json";
import declarativeEn from "@/declarative/i18n/en.json";
import declarativeKo from "@/declarative/i18n/ko.json";
import certsEn from "@/certs/i18n/en.json";
import certsKo from "@/certs/i18n/ko.json";
import physicalEn from "@/physical/i18n/en.json";
import physicalKo from "@/physical/i18n/ko.json";
import drEn from "@/dr/i18n/en.json";
import drKo from "@/dr/i18n/ko.json";
import helpEn from "@/help/i18n/en.json";
import helpKo from "@/help/i18n/ko.json";

function keys(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null) {
    return [prefix];
  }
  return Object.entries(value).flatMap(([k, v]) => keys(v, prefix ? `${prefix}.${k}` : k));
}

it("has the same keys in every language", () => {
  expect(keys(ko).sort()).toEqual(keys(en).sort());
  // 늦게 받는 클러스터 지도 문구(`clusterMap/strings.ts`)도
  expect(keys(mapKo).sort()).toEqual(keys(mapEn).sort());
  expect(Object.keys(mapEn)).toEqual(["clusterMap"]);
  expect(Object.keys(en)).not.toContain("clusterMap");
  // 끌기 틀(`dnd/strings.ts`)과 VM 빌더(`vmBuilder/strings.ts`) 문구도
  expect(keys(dndKo).sort()).toEqual(keys(dndEn).sort());
  expect(keys(builderKo).sort()).toEqual(keys(builderEn).sort());
  expect(Object.keys(dndEn)).toEqual(["dnd"]);
  expect(Object.keys(builderEn)).toEqual(["vmBuilder"]);
  expect(Object.keys(en)).not.toContain("dnd");
  expect(Object.keys(en)).not.toContain("vmBuilder");
  // VMware 이전(`vmware/strings.ts`)과 VM 내보내기(`exports/strings.ts`) 문구도(계획 72)
  expect(keys(vmwareKo).sort()).toEqual(keys(vmwareEn).sort());
  expect(keys(exportsKo).sort()).toEqual(keys(exportsEn).sort());
  expect(Object.keys(vmwareEn)).toEqual(["vmware"]);
  expect(Object.keys(exportsEn)).toEqual(["vmExports"]);
  expect(Object.keys(en)).not.toContain("vmware");
  expect(Object.keys(en)).not.toContain("vmExports");
  // 설정 기록·선언 묶음·인증서(계획 73 — `declarative/strings.ts`)
  for (const [one, two, top] of [
    [historyEn, historyKo, "history"],
    [declarativeEn, declarativeKo, "declarative"],
    [certsEn, certsKo, "certs"],
    // 운영자 대시보드(계획 75 — `physical/strings.ts`)
    [physicalEn, physicalKo, "physical"],
    // DR(계획 76 — `dr/strings.ts`)
    [drEn, drKo, "dr"],
    // 도움말 패널·그림(계획 77-4 — `help/strings.ts`)
    [helpEn, helpKo, "helpInfo"],
  ] as const) {
    expect(keys(two).sort()).toEqual(keys(one).sort());
    expect(Object.keys(one)).toEqual([top]);
    expect(Object.keys(en)).not.toContain(top);
  }
});

// `common.*`으로 옮긴 키. 같은 뜻의 개별 키를 다시 만들지 않는다.
const MOVED_TO_COMMON = [
  "confirm.cancel",
  "users.save",
  "vmForm.save",
  "tokens.done",
  "tasks.node",
  "tasks.stateColumn",
  "dashboard.state",
  ...["vms", "volumes", "pools", "networks", "users", "tokens"].map((s) => `${s}.create`),
  ...["snapshots", "volumes", "pools", "networks", "users", "tokens"].map((s) => `${s}.delete`),
  ...["volumes", "pools", "networks", "tokens"].map((s) => `${s}.deleteNamed`),
  ...["vms", "snapshots", "volumes", "pools", "networks", "nodes", "users", "tokens"].map(
    (s) => `${s}.name`,
  ),
  ...["vms", "volumes", "nodes"].map((s) => `${s}.state`),
  ...["vms", "volumes"].map((s) => `${s}.node`),
];

it("keeps common words only under common", () => {
  for (const language of [en, ko]) {
    const present = keys(language);
    expect(MOVED_TO_COMMON.filter((key) => present.includes(key))).toEqual([]);
  }
  expect(en.common).toMatchObject({ cancel: "Cancel", deleteNamed: "Delete {{name}}" });
  expect(ko.common).toMatchObject({ cancel: "취소", deleteNamed: "{{name}} 삭제" });
});

/** `dir` 아래의 `.rs` 파일(시험 파일·`tests` 디렉터리는 빼고) */
function rustSources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const path = join(dir, e.name);
    if (e.isDirectory()) {
      return e.name === "tests" || e.name === "target" ? [] : rustSources(path);
    }
    return e.name.endsWith(".rs") && e.name !== "tests.rs" ? [path] : [];
  });
}

/**
 * 계획 77-1 리뷰 S7: 설명을 덧붙이지 않는 code(`FIXED_MESSAGE`)는 서버의 설명이 늘 글자 그대로여야 한다. 서버 코드에서
 * `"<code>", ` 바로 다음이 `format!`이나 변수(소문자 이름)면 이름·값이 든 설명이라 덧붙여야 한다 — 목록에서 뺀다.
 */
it("only skips the server detail for codes whose server message never changes", () => {
  const root = join(import.meta.dirname, "../../../crates");
  const dynamic: string[] = [];
  const seen = new Set<string>();
  for (const file of rustSources(root)) {
    const text = readFileSync(file, "utf8");
    for (const [, code = "", next] of text.matchAll(
      /"([a-z_0-9]+)",\s*(format!|[a-z_][a-z_0-9.]*)/g,
    )) {
      seen.add(code);
      if (FIXED_MESSAGE.has(code)) {
        dynamic.push(`${file.slice(root.length + 1)}: ${code} ${next}`);
      }
    }
    for (const [, code = ""] of text.matchAll(/"([a-z_0-9]+)",\s*"/g)) {
      seen.add(code);
    }
  }
  expect(dynamic).toEqual([]);
  // 서버에 없는 code(지워졌거나 잘못 적음)도 없다.
  expect([...FIXED_MESSAGE].filter((c) => !seen.has(c))).toEqual([]);
  // `forbidden`은 구체 설명을 주기도 하므로 덧붙이고, 기본 글("permission denied")만 뺀다.
  expect(
    errorMessage(
      i18n.t,
      new ApiError(403, "forbidden", "cannot grant privileges you do not hold: vm.power"),
    ),
  ).toContain("Details: cannot grant privileges you do not hold: vm.power");
  expect(errorMessage(i18n.t, new ApiError(403, "forbidden", "permission denied"))).not.toContain(
    "Details",
  );
});

it("translates API error codes and falls back to the server message", async () => {
  const t = i18n.t;
  // 번역이 서버 설명을 담지 않으면 설명을 "자세히"로 덧붙인다(계획 77).
  expect(errorMessage(t, new ApiError(409, "vm_running", "cannot delete VM"))).toBe(
    "Stop the VM first. Details: cannot delete VM",
  );
  // 늘 같은 설명(로그인 실패 등)은 덧붙이지 않는다.
  expect(errorMessage(t, new ApiError(401, "invalid_credentials", "invalid credentials"))).toBe(
    "Wrong username or password.",
  );
  // 서버 문장이 이미 "another SDN central task is running"이라 앞말을 되풀이하지 않는다(리뷰 1 #11).
  expect(
    errorMessage(
      t,
      new ApiError(409, "sdn_busy", "another SDN central task is running (task 019a)"),
    ),
  ).toBe("another SDN central task is running (task 019a)");
  expect(errorMessage(t, new ApiError(409, "no_node", "no node has bridge br0"))).toBe(
    "No node can run this VM: no node has bridge br0",
  );
  expect(errorMessage(t, new ApiError(429, "too_many_attempts", "try again in 30 seconds"))).toBe(
    "Too many attempts. Try again later. Details: try again in 30 seconds",
  );
  expect(
    errorMessage(t, new ApiError(409, "pool_unavailable", 'storage pool "nas" is not ready')),
  ).toBe('storage pool "nas" is not ready');
  expect(errorMessage(t, new ApiError(400, "machine_arch_mismatch", "virt on x86_64"))).toBe(
    "The machine type doesn't match the selected node's architecture. Details: virt on x86_64",
  );
  expect(
    errorMessage(t, new ApiError(409, "firewall_unsupported", 'node "n1" has no firewall')),
  ).toBe(
    'The node has no firewall (nftables) and cannot run VMs with security groups. Details: node "n1" has no firewall',
  );
  expect(errorMessage(t, new ApiError(409, "backup_not_ready", 'backup "b" is not ready'))).toBe(
    'The backup is not ready. Only finished backups can be restored or copied. Details: backup "b" is not ready',
  );
  expect(errorMessage(t, new ApiError(409, "job_disabled", 'backup job "n" is disabled'))).toBe(
    'The backup job is off. Turn on "Run on schedule" first. Details: backup job "n" is disabled',
  );
  expect(errorMessage(t, new ApiError(400, "invalid_ova", "the OVA has no .ovf file"))).toBe(
    "The file is not a usable OVA: the OVA has no .ovf file",
  );
  expect(
    errorMessage(t, new ApiError(409, "placement_rule", 'node "b" breaks placement rules: spread')),
  ).toBe('Placement rules leave no node for this VM: node "b" breaks placement rules: spread');
  expect(
    errorMessage(
      t,
      new ApiError(
        409,
        "restart_required",
        'cannot migrate VM "web": cloud-init changed while running',
      ),
    ),
  ).toBe(
    'The VM must be restarted first: cannot migrate VM "web": cloud-init changed while running',
  );
  expect(
    errorMessage(
      t,
      new ApiError(409, "template_in_use", 'template "tmpl" has 1 linked clone(s) (web-1)'),
    ),
  ).toBe(
    'The template still has linked clones. Delete or flatten them first: template "tmpl" has 1 linked clone(s) (web-1)',
  );
  expect(
    errorMessage(
      t,
      new ApiError(
        409,
        "tpm_state_unavailable",
        'the TPM state of VM "win" is on node "a" (offline)',
      ),
    ),
  ).toBe(
    'The VM cannot start because its TPM state is not available: the TPM state of VM "win" is on node "a" (offline)',
  );
  expect(
    errorMessage(t, new ApiError(409, "guest_agent_unavailable", 'VM "win" has no guest agent')),
  ).toBe('The guest agent cannot be used: VM "win" has no guest agent');
  // 서버의 설명은 CLI 명령을 말하므로 웹의 길을 알린다.
  expect(
    errorMessage(t, new ApiError(409, "firmware_reset_required", "vm set win --reset-uefi-vars")),
  ).toMatch(/^The VM has UEFI variables .* "Reset firmware" on the VM page/);
  expect(errorMessage(t, new ApiError(409, "cluster_upgrading", 'node "b" lacks vtpm'))).toBe(
    'The cluster is being upgraded: node "b" lacks vtpm',
  );
  // HA 용량 예약은 서버의 셈으로 남은 자리를 알린다(details가 없으면 앞 문장만).
  const capacity = { capacity_mib: 16384, reserve_mib: 8192, used_mib: 7168, need_mib: 2048 };
  expect(errorMessage(t, new ApiError(409, "ha_capacity", "x", capacity))).toBe(
    "Starting this HA VM would use memory reserved for HA recovery. It needs 2 GiB, but only 1 GiB of 16 GiB is left outside the 8 GiB reserve.",
  );
  expect(errorMessage(t, new ApiError(409, "ha_capacity", "x"))).toBe(
    "Starting this HA VM would use memory reserved for HA recovery. Details: x",
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
    "memory_mib must be > 0",
  );
  expect(errorMessage(t, new NetworkError(new TypeError("Failed to fetch")))).toBe(
    "Cannot reach the server.",
  );
  // 코드 버그의 TypeError는 네트워크 오류로 숨기지 않는다.
  expect(errorMessage(t, new TypeError("x is undefined"))).toBe("x is undefined");
  await i18n.changeLanguage("ko");
  expect(errorMessage(i18n.t, new ApiError(409, "vm_running", "x"))).toBe(
    "먼저 VM을 정지하세요. 자세히: x",
  );
});

it("shows untranslated server messages verbatim", () => {
  const message = "a {{x}} b $t(errors.network) c";
  expect(errorMessage(i18n.t, new ApiError(400, "invalid_request", message))).toBe(message);
});

it("keeps <html lang> in sync with the language", async () => {
  await i18n.changeLanguage("ko");
  expect(document.documentElement.lang).toBe("ko");
  await i18n.changeLanguage("en");
  expect(document.documentElement.lang).toBe("en");
});

it("words routing in Korean like the English and with a particle that fits any name", () => {
  // 리뷰 2 웹 m7: "주인"은 en "active"와 맞지 않고, "{{physnet}}이"는 `ext`에서 "ext이"가 된다.
  expect(ko.router.activeNow).toBe("활성");
  expect(ko.router.standby).toBe("대기");
  expect(ko.sdn.physnet.noNode).not.toMatch(/\}\}이 /);
  // 서버의 까닭을 옮긴 글은 두 말 모두에 있다.
  expect(Object.keys(ko.routing.server)).toEqual(Object.keys(en.routing.server));
});

it("translates the plan-67 error codes (review 2 m2)", () => {
  const codes = [
    "qos_unsupported",
    "qos_dscp_needs_sdn",
    "invalid_qos",
    "invalid_load_balancer",
    "lb_network_not_routed",
    "lb_member_unreachable",
    "lb_in_use",
    "invalid_migration",
    "migration_in_progress",
    "migration_finished",
    "migration_not_running",
    "migration_bridge_laid",
    "network_migrating",
    "nic_network_live",
    "lockdown_peers_outside",
  ];
  for (const code of codes) {
    expect(en.errors).toHaveProperty(code);
    expect(ko.errors).toHaveProperty(code);
  }
  const t = i18n.getFixedT("ko");
  expect(
    errorMessage(t, new ApiError(409, "qos_unsupported", 'node "mac" cannot rate-limit NICs')),
  ).toBe(`${ko.errors.qos_unsupported} 자세히: node "mac" cannot rate-limit NICs`);
  expect(errorMessage(t, new ApiError(400, "invalid_load_balancer", "listener tcp/80 twice"))).toBe(
    "부하 분산기 설정이 올바르지 않습니다: listener tcp/80 twice",
  );
});

it("translates the declarative and history error codes in both languages", async () => {
  const stale = new ApiError(412, "plan_stale", 'vm "web" changed after the plan was made');
  const gone = new ApiError(404, "plan_not_found", "plan 019a not found");
  const t = i18n.getFixedT("ko");
  expect(errorMessage(t, stale)).toBe(
    '계획을 세운 뒤 바뀐 것이 있습니다. 다시 계획해 확인하세요: vm "web" changed after the plan was made',
  );
  expect(errorMessage(t, gone)).toMatch(/^계획이 없습니다/);
  for (const code of [
    "plan_stale",
    "plan_not_found",
    "plan_blocked",
    "plan_not_started",
    "history_unavailable",
    "manifest_unsupported",
    "http01_unavailable",
    "apply_set_too_large",
  ]) {
    expect(i18n.getFixedT("en")(`errors.${code}`)).not.toBe(`errors.${code}`);
    expect(i18n.getFixedT("ko")(`errors.${code}`)).not.toBe(`errors.${code}`);
  }
});
