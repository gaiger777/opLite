import { expect, type Page, test } from "@playwright/test";

// 계획 73: 꺼진 VM 하나(Rust 테스트가 CLI로 만듦 — 메모리 256 MiB)의 메모리를 웹 편집으로 바꾸고, VM ⋯ 메뉴의 "설정 기록…"
// 서랍에서 두 줄(만듦·바뀜)과 바뀜 줄의 전후 표를 본 뒤, 만든 줄로 되돌린다(서버 계획의 diff를 보고 "Restore"). 되돌린 줄이
// "restored from"과 함께 생긴다. 보안 탭의 인증서 보기는 빈 목록만 연다(ACME 디렉터리 없음). 메모리가 돌아왔는지는 Rust
// 테스트(`crates/opld/tests/web.rs`)가 본다.
const user = process.env.OPL_USER ?? "admin";
const password = process.env.OPL_PASSWORD ?? "";
/** 되돌릴 VM 이름 */
const vm = process.env.OPL_VM ?? "";
if (!password || !vm) {
  throw new Error("Set OPL_PASSWORD and OPL_VM (the Rust test sets them).");
}

async function signIn(page: Page) {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
}

/** VM 상세 머리의 케밥 메뉴("Actions for {vm}")에서 항목을 고른다. */
async function vmMenu(page: Page, item: string) {
  await page.getByRole("button", { name: `Actions for ${vm}`, exact: true }).click();
  await page.getByRole("menuitem", { name: item, exact: true }).click();
}

test("changes a VM in the web, sees it in the config history and restores it", async ({ page }) => {
  await signIn(page);
  await page.goto(`/vms/${vm}`);
  await expect(page.getByRole("heading", { name: vm, exact: true })).toBeVisible();

  // 웹에서 메모리를 바꾼다.
  await vmMenu(page, "Edit");
  await expect(page.getByRole("heading", { name: `Edit ${vm}` })).toBeVisible();
  await page.getByLabel("Memory (MiB)", { exact: true }).fill("512");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { name: vm, exact: true })).toBeVisible();

  // ⋯ "설정 기록…": 가장 새 것 먼저 두 줄 — 바뀜(웹)·만듦
  await vmMenu(page, "Config history…");
  const drawer = page.getByRole("dialog", { name: `Config history of ${vm}` });
  const list = drawer.getByRole("list", { name: `Config history of ${vm}` });
  await expect(list.getByRole("button")).toHaveCount(2);
  await expect(list.getByRole("button").nth(0)).toHaveAccessibleName(/Changed.*Web.*memory_mib/);
  await expect(list.getByRole("button").nth(1)).toHaveAccessibleName(/Created/);

  // 바뀜 줄의 전후 표: memory_mib 256 → 512
  await list.getByRole("button").nth(0).click();
  const changes = drawer.getByRole("table", { name: `Changed fields of ${vm}` });
  const row = changes.getByRole("row").filter({ hasText: "memory_mib" });
  await expect(row.getByRole("cell", { name: "256", exact: true })).toBeVisible();
  await expect(row.getByRole("cell", { name: "512", exact: true })).toBeVisible();

  // 만든 줄로 되돌린다: 서버 계획의 diff(512 → 256)를 보고 "Restore"
  await drawer.getByRole("button", { name: "All changes" }).click();
  await list.getByRole("button").nth(1).click();
  await drawer.getByRole("button", { name: "Restore this revision…" }).click();
  const restore = page.getByRole("dialog", {
    name: new RegExp(`^Restore ${vm} to revision \\d+\\?$`),
  });
  await expect(restore).toBeVisible();
  await expect(restore.getByText("memory_mib").first()).toBeVisible();
  await restore.getByRole("button", { name: "Restore", exact: true }).click();
  await expect(restore).toBeHidden();

  // 되돌린 줄(적용 Task가 쓴다 — "restored from")이 맨 위에 생긴다. Task가 끝나기 전에 읽었으면 서랍을 다시 연다.
  await expect(async () => {
    if (await drawer.isVisible()) {
      await page.keyboard.press("Escape");
      await expect(drawer).toBeHidden();
    }
    await vmMenu(page, "Config history…");
    await expect(list.getByRole("button")).toHaveCount(3, { timeout: 3_000 });
  }).toPass({ timeout: 60_000 });
  await expect(list.getByRole("button").nth(0)).toHaveAccessibleName(/Changed.*restored from/);
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();

  // 설정 › 보안 탭의 인증서 보기: 빈 목록
  await page.goto("/settings/certificates");
  await expect(page.getByRole("heading", { level: 1, name: "Certificates" })).toBeVisible();
  await expect(page.getByText("No certificates.")).toBeVisible();
  await expect(page.getByText("No ACME accounts.")).toBeVisible();
});
