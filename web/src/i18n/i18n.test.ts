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
  expect(errorMessage(t, new ApiError(400, "machine_arch_mismatch", "virt on x86_64"))).toBe(
    "The machine type doesn't match the selected node's architecture.",
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
