import { readFile } from "node:fs/promises";

import { expect, type Page, test } from "@playwright/test";

// 계획 69: 백업 안 파일 찾아보기. 실제 도우미 이미지·QEMU로 연 세션에서 디렉터리에 들어가고, 파일 하나와 폴더(zip)를
// 내려받고, 닫는다. 고정물 ext4 디스크 VM과 그 저장소 백업은 Rust 테스트(`crates/opld/tests/web.rs`)가 준비한다.
const user = process.env.OPL_USER ?? "admin";
const password = process.env.OPL_PASSWORD ?? "";
/** 찾아볼 백업 이름 */
const backup = process.env.OPL_BACKUP ?? "";
if (!password || !backup) {
  throw new Error("Set OPL_PASSWORD and OPL_BACKUP (the Rust test sets them).");
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
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Protection", exact: true })
    .click();
  await page
    .getByRole("navigation", { name: "Protection menu" })
    .getByRole("link", { name: "Backups", exact: true })
    .click();
  await page.getByRole("link", { name: `Browse files of ${backup}` }).click();
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
  await expect(root.getByText("hello.txt", { exact: true })).toBeVisible();
  await expect(root.getByText("→ hello.txt")).toBeVisible();
  // 문서용 화면(설정했을 때만): `OPL_SCREENSHOTS=<디렉터리>`
  if (process.env.OPL_SCREENSHOTS) {
    await page.screenshot({ path: `${process.env.OPL_SCREENSHOTS}/file-restore.png` });
  }
  // 디렉터리에 들어갔다가 이동 경로로 돌아온다.
  await root.getByRole("button", { name: "dir", exact: true }).click();
  const dir = page.getByRole("table", { name: "Files in /dir" });
  await expect(dir.getByRole("button", { name: "nested", exact: true })).toBeVisible();
  await page
    .getByRole("navigation", { name: "Folder path" })
    .getByRole("button", { name: "Disk 0" })
    .click();
  await expect(root.getByText("hello.txt", { exact: true })).toBeVisible();

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
  await expect(page.getByText("This browsing session is closed")).toBeVisible();
  await expect(page.getByRole("button", { name: "Open again" })).toBeVisible();
});
