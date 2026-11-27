import { readFile } from "node:fs/promises";

import { expect, type Page, test } from "@playwright/test";
import { openItem } from "./nav";

// 계획 69: 백업 안 파일 찾아보기. 실제 도우미 이미지·QEMU로 연 세션에서 디렉터리에 들어가고, 파일 하나와 폴더(zip)를
// 내려받고, 닫는다. 고정물 ext4 디스크 VM과 그 저장소 백업은 Rust 테스트(`crates/orbivisord/tests/web.rs`)가 준비한다.
const user = process.env.ORBIVISOR_USER ?? "admin";
const password = process.env.ORBIVISOR_PASSWORD ?? "";
/** 찾아볼 백업 이름 */
const backup = process.env.ORBIVISOR_BACKUP ?? "";
if (!password || !backup) {
  throw new Error("Set ORBIVISOR_PASSWORD and ORBIVISOR_BACKUP (the Rust test sets them).");
}

async function signIn(page: Page) {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
}

test.use({ locale: "en-US", acceptDownloads: true });

test("browses a backup, downloads a file and a folder, and closes the session", async ({
  page,
}) => {
  await signIn(page);
  await openItem(page, "Backup & recovery", "Backups");
  // 행 동작은 "복원" + ⋯ 메뉴(계획 77-6)
  await page.getByRole("button", { name: `Actions for ${backup}` }).click();
  await page.getByRole("menuitem", { name: `Browse files of ${backup}` }).click();
  await expect(page).toHaveURL(/\/backups\/[^/]+\/files$/);
  await expect(page.getByRole("heading", { name: `Files in ${backup}` })).toBeVisible();
  // 도우미 VM이 뜨면(TCG면 1분까지) 준비됨
  await expect(page.locator('[data-slot="status-chip"]', { hasText: "Ready" })).toBeVisible({
    timeout: 180_000,
  });
  // 이동 경로의 백업 id는 백업 이름이다.
  await expect(
    page.getByRole("navigation", { name: "Breadcrumb" }).getByText(backup, { exact: true }),
  ).toBeVisible();
  const tree = page.getByRole("navigation", { name: "Volumes" });
  await expect(tree.getByRole("button", { name: /Disk 0/ })).toContainText("ext4");

  const root = page.getByRole("table", { name: "Files in /" });
  // 이름은 <bdi> 안이고 링크 대상(→ hello.txt)도 같은 글이라 행의 체크 상자로 찾는다.
  await expect(root.getByRole("checkbox", { name: "Select hello.txt" })).toBeVisible();
  await expect(root.getByText("→ hello.txt")).toBeVisible();
  // 문서용 화면(설정했을 때만): `ORBIVISOR_SCREENSHOTS=<디렉터리>`
  if (process.env.ORBIVISOR_SCREENSHOTS) {
    await page.screenshot({ path: `${process.env.ORBIVISOR_SCREENSHOTS}/file-restore.png` });
  }
  // 디렉터리에 들어갔다가 이동 경로로 돌아온다.
  await root.getByRole("button", { name: "dir", exact: true }).click();
  const dir = page.getByRole("table", { name: "Files in /dir" });
  await expect(dir.getByRole("button", { name: "nested", exact: true })).toBeVisible();
  await page
    .getByRole("navigation", { name: "Folder path" })
    .getByRole("button", { name: "Disk 0" })
    .click();
  await expect(root.getByRole("checkbox", { name: "Select hello.txt" })).toBeVisible();

  // 파일 하나는 그대로(raw)
  await root.getByRole("checkbox", { name: "Select hello.txt" }).click();
  const [file] = await Promise.all([
    page.waitForEvent("download"),
    page.getByRole("button", { name: "Download", exact: true }).click(),
  ]);
  expect(file.suggestedFilename()).toMatch(/hello\.txt$/);
  expect(await readFile(await file.path())).toEqual(Buffer.from("hello from orbivisor\n"));

  // 고른 것을 풀면 지금 폴더 전체를 zip으로
  await root.getByRole("checkbox", { name: "Select hello.txt" }).click();
  const [zip] = await Promise.all([
    page.waitForEvent("download"),
    page.getByRole("button", { name: "Download folder" }).click(),
  ]);
  expect(zip.suggestedFilename()).toMatch(/\.zip$/);
  const bytes = await readFile(await zip.path());
  expect(bytes.subarray(0, 2).toString()).toBe("PK");
  expect(bytes.includes(Buffer.from("dir/nested/deep.txt"))).toBe(true);

  await page.getByRole("button", { name: "Close", exact: true }).click();
  // 서버가 닫힘을 알릴 때까지 "닫는 중"이고 2초마다 묻는다.
  await expect(page.getByText("This browsing session is closed")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("button", { name: "Open again" })).toBeVisible();
});
