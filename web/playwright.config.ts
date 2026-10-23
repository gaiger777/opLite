import { defineConfig, devices } from "@playwright/test";

// 실제 opld(QEMU)에 붙는 E2E. opld와 준비는 Rust 테스트가 한다:
// `pnpm -C web build && cargo test -p opld --features web-it --test web`
export default defineConfig({
  testDir: "e2e",
  timeout: 120_000,
  expect: { timeout: 30_000 },
  retries: 0,
  workers: 1,
  reporter: [["list"]],
  use: {
    baseURL: process.env.OPL_URL,
    // opld는 클러스터 CA가 서명한 인증서를 쓴다.
    ignoreHTTPSErrors: true,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
