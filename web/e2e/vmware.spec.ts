import { readFile } from "node:fs/promises";

import { expect, type Page, test } from "@playwright/test";
import { openItem, openPageTab } from "./nav";

// 계획 72: 가상 머신 › 가져오기의 VMware 보기로 따뜻한 이전을 끝까지 한다 — 원본 더하기(지문을 보고 믿기) → 마법사(나무에서
// web01 고르기·네트워크 둘·기본 풀·켜지 않기·미리 확인) → 시작 → 동기화 중 → 전환 → 끝남·실제 내린 시간 → VM 링크. 그 VM을 메뉴의
// "OVA로 내보내기…"로 내보내 내려받은 파일이 ustar(OVA)인지 본다. 가짜 vSphere·클러스터·풀은 Rust 테스트
// (`crates/orbivisord/tests/web.rs`)가 준비하고 끝난 뒤 원본·대상을 확인한다.
const user = process.env.ORBIVISOR_USER ?? "admin";
const password = process.env.ORBIVISOR_PASSWORD ?? "";
const pool = process.env.ORBIVISOR_POOL ?? "";
/** VMware 네트워크 둘을 잇는 우리 네트워크(브리지 — VM은 켜지 않는다) */
const network = process.env.ORBIVISOR_NETWORK ?? "";
const vsphere = {
  host: process.env.ORBIVISOR_VSPHERE_HOST ?? "",
  port: process.env.ORBIVISOR_VSPHERE_PORT ?? "",
  user: process.env.ORBIVISOR_VSPHERE_USER ?? "",
  password: process.env.ORBIVISOR_VSPHERE_PASSWORD ?? "",
  fingerprint: process.env.ORBIVISOR_VSPHERE_FINGERPRINT ?? "",
};
if (!password || !pool || !network || !vsphere.host || !vsphere.fingerprint) {
  throw new Error(
    "Set ORBIVISOR_PASSWORD, ORBIVISOR_POOL, ORBIVISOR_NETWORK and ORBIVISOR_VSPHERE_* (the Rust test sets them).",
  );
}

async function signIn(page: Page) {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
}

/** 이미지·이전 묶음의 화면 하나(VMware 이전·원본은 "VMware migration (copy while running)" 항목의 화면 안 탭) */
async function importsView(page: Page, view: string) {
  if (view === "Exports") {
    await openItem(page, "Images & migration", "Exports");
    return;
  }
  await openItem(page, "Images & migration", "VMware migration (copy while running)");
  await openPageTab(page, "VMware migration (copy while running)", view);
}

test("migrates a running VMware VM warm, cuts it over and exports it as OVA", async ({ page }) => {
  await signIn(page);

  // ① 원본: 지문 없이 연결하면 서버가 인증서 지문을 보이고, 믿으면 저장한다.
  await importsView(page, "VMware sources");
  await page.getByRole("button", { name: "Connect vCenter or ESXi" }).click();
  const add = page.getByRole("dialog", { name: "Connect a vCenter or ESXi" });
  await add.getByLabel("Name", { exact: true }).fill("vc1");
  await add.getByLabel("Host", { exact: true }).fill(vsphere.host);
  await add.getByLabel("Port", { exact: true }).fill(vsphere.port);
  await add.getByLabel("User", { exact: true }).fill(vsphere.user);
  await add.getByLabel("Password", { exact: true }).fill(vsphere.password);
  await add.getByRole("button", { name: "Connect" }).click();
  await expect(add.getByText(vsphere.fingerprint)).toBeVisible();
  await add.getByRole("button", { name: "Trust and connect" }).click();
  await expect(add).toBeHidden();
  const sources = page.getByRole("table", { name: "Connected vCenter & ESXi" });
  await expect(sources.getByRole("row", { name: /vc1/ })).toContainText("Connected");

  // ② 마법사: 원본·이름 → 나무에서 Prod 폴더의 web01 → 기본 풀·켜지 않기 → 한도 → 미리 확인 → 시작
  await importsView(page, "VMware migrations");
  await page.getByRole("link", { name: "New migration" }).click();
  await expect(page.getByRole("heading", { name: "New VMware migration" })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill("wave1");
  await page
    .getByRole("combobox", { name: "Source" })
    .selectOption({ label: `vc1 (${vsphere.host})` });
  await page.getByRole("button", { name: "Next" }).click();
  const vms = page.getByRole("table", { name: "Source VMs" });
  await page
    .getByRole("navigation", { name: "Inventory" })
    .getByRole("button", { name: /^Prod/ })
    .click();
  await vms.getByRole("checkbox", { name: "web01" }).check();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByLabel("VM Network", { exact: true }).selectOption(network);
  await page.getByLabel("dvpg-web", { exact: true }).selectOption(network);
  await page.getByLabel("Default pool", { exact: true }).selectOption(pool);
  await page.getByLabel("Start the new VM after cutover", { exact: true }).uncheck();
  await page.getByRole("button", { name: "Next" }).click();
  await page.getByRole("button", { name: "Next" }).click();
  const results = page.getByRole("table", { name: "Check results" });
  await expect(results.getByRole("row", { name: /web01/ })).not.toContainText("Blocked");
  await page.getByRole("button", { name: "Start 1 VM" }).click();
  await expect(page).toHaveURL(/\/vmware-migrations\/wave1$/);

  // ③ 전체 복사 뒤 동기화 중 → 전환(대화상자가 원본 처리를 알린다) → 끝남
  const table = page.getByRole("table", { name: "wave1" });
  const row = table.getByRole("row", { name: /web01/ });
  await expect(row).toContainText("Syncing", { timeout: 120_000 });
  await table.getByRole("checkbox", { name: "Select web01" }).check();
  await page.getByRole("button", { name: "Cut over", exact: true }).click();
  const confirm = page.getByRole("alertdialog", { name: "Cut over 1 VM?" });
  await expect(confirm).toContainText("shut down through VMware Tools");
  await expect(confirm).toContainText("Source NICs stay disconnected at power-on");
  await confirm.getByRole("button", { name: "Cut over", exact: true }).click();
  await expect(row).toContainText("Done", { timeout: 120_000 });
  await expect(row).toContainText("(actual)");
  await page.getByRole("button", { name: "Open web01" }).click();
  const drawer = page.getByRole("dialog", { name: "web01" });
  await expect(drawer).toContainText("Downtime");
  await expect(drawer).toContainText("Disconnected at power-on (protected)");
  await page.keyboard.press("Escape");

  // ④ VM 링크 → VM 상세 ⋯ "Export as OVA…" → 내보내기 보기의 대화상자 → 준비됨 → 내려받기(ustar)
  await table.getByRole("link", { name: "web01", exact: true }).click();
  await expect(page).toHaveURL(/\/vms\/web01/);
  await page.getByRole("button", { name: "Actions for web01", exact: true }).click();
  await page.getByRole("menuitem", { name: "Export as OVA…", exact: true }).click();
  await expect(page).toHaveURL(/\/vm-exports/);
  const dialog = page.getByRole("dialog", { name: "Export web01 as OVA" });
  await dialog.getByLabel("Pool", { exact: true }).selectOption({ label: `${pool} (dir)` });
  await dialog.getByLabel("Name", { exact: true }).fill("web01-ova");
  await dialog.getByRole("button", { name: "Export", exact: true }).click();
  await expect(dialog).toBeHidden();
  const exports = page.getByRole("table", { name: "Exports" });
  await expect(exports.getByRole("row", { name: /web01-ova/ })).toContainText("Ready", {
    timeout: 120_000,
  });
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    exports.getByRole("button", { name: "Download web01-ova" }).click(),
  ]);
  // 파일 이름은 VM 이름(`{VM}.ova`)
  expect(download.suggestedFilename()).toBe("web01.ova");
  const bytes = await readFile(await download.path());
  // ustar 머리: 첫 항목은 OVF 서술자, 257번째 바이트부터 "ustar"
  expect(bytes.subarray(257, 262).toString()).toBe("ustar");
  expect(bytes.subarray(0, 100).toString().replace(/\0+$/, "")).toMatch(/\.ovf$/);
});
