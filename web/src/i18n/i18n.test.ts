import { ApiError } from "@/api/client";
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

it("translates API error codes and falls back to the server message", async () => {
  const t = i18n.t;
  expect(errorMessage(t, new ApiError(409, "vm_running", "cannot delete VM"))).toBe(
    "Stop the VM first.",
  );
  expect(errorMessage(t, new ApiError(409, "no_node", "no node has bridge br0"))).toBe(
    "No node can run this VM: no node has bridge br0",
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
    "memory_mib must be > 0",
  );
  expect(errorMessage(t, new TypeError("Failed to fetch"))).toBe("Cannot reach the server.");
  await i18n.changeLanguage("ko");
  expect(errorMessage(i18n.t, new ApiError(409, "vm_running", "x"))).toBe("먼저 VM을 정지하세요.");
});
