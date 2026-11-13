import { ApiError, NetworkError } from "@/api/client";
import en from "@/i18n/en.json";
import { errorMessage } from "@/i18n/errors";
import i18n from "@/i18n";
import ko from "@/i18n/ko.json";

function keys(value: unknown, prefix = ""): string[] {
  if (typeof value !== "object" || value === null) {
    return [prefix];
  }
  return Object.entries(value).flatMap(([k, v]) => keys(v, prefix ? `${prefix}.${k}` : k));
}

it("has the same keys in every language", () => {
  expect(keys(ko).sort()).toEqual(keys(en).sort());
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

it("translates API error codes and falls back to the server message", async () => {
  const t = i18n.t;
  expect(errorMessage(t, new ApiError(409, "vm_running", "cannot delete VM"))).toBe(
    "Stop the VM first.",
  );
  expect(errorMessage(t, new ApiError(409, "no_node", "no node has bridge br0"))).toBe(
    "No node can run this VM: no node has bridge br0",
  );
  expect(errorMessage(t, new ApiError(429, "too_many_attempts", "try again in 30 seconds"))).toBe(
    "Too many attempts. Try again later.",
  );
  expect(
    errorMessage(t, new ApiError(409, "pool_unavailable", 'storage pool "nas" is not ready')),
  ).toBe('storage pool "nas" is not ready');
  expect(errorMessage(t, new ApiError(400, "machine_arch_mismatch", "virt on x86_64"))).toBe(
    "The machine type doesn't match the selected node's architecture.",
  );
  expect(
    errorMessage(t, new ApiError(409, "firewall_unsupported", 'node "n1" has no firewall')),
  ).toBe("The node has no firewall (nftables) and cannot run VMs with security groups.");
  expect(errorMessage(t, new ApiError(409, "backup_not_ready", 'backup "b" is not ready'))).toBe(
    "The backup is not ready. Only finished backups can be restored or copied.",
  );
  expect(errorMessage(t, new ApiError(409, "job_disabled", 'backup job "n" is disabled'))).toBe(
    'The backup job is off. Turn on "Run on schedule" first.',
  );
  expect(errorMessage(t, new ApiError(400, "invalid_ova", "the OVA has no .ovf file"))).toBe(
    "The file is not a usable OVA: the OVA has no .ovf file",
  );
  expect(
    errorMessage(t, new ApiError(409, "placement_rule", 'node "b" breaks placement rules: spread')),
  ).toBe('Placement rules leave no node for this VM: node "b" breaks placement rules: spread');
  // HA 용량 예약은 서버의 셈으로 남은 자리를 알린다(details가 없으면 앞 문장만).
  const capacity = { capacity_mib: 16384, reserve_mib: 8192, used_mib: 7168, need_mib: 2048 };
  expect(errorMessage(t, new ApiError(409, "ha_capacity", "x", capacity))).toBe(
    "Starting this HA VM would use memory reserved for HA recovery. It needs 2 GiB, but only 1 GiB of 16 GiB is left outside the 8 GiB reserve.",
  );
  expect(errorMessage(t, new ApiError(409, "ha_capacity", "x"))).toBe(
    "Starting this HA VM would use memory reserved for HA recovery.",
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
  expect(errorMessage(i18n.t, new ApiError(409, "vm_running", "x"))).toBe("먼저 VM을 정지하세요.");
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
