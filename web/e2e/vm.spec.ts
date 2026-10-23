import { expect, type Page, test } from "@playwright/test";

const user = process.env.OPL_USER ?? "admin";
const password = process.env.OPL_PASSWORD ?? "";
/** Rust 테스트가 미리 만든 빈 볼륨 */
const volume = process.env.OPL_VOLUME ?? "root";

async function signIn(page: Page) {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
}

/** noVNC 캔버스에 검은색이 아닌 화소가 있다(펌웨어 화면이 그려졌다). */
async function consoleShowsSomething(page: Page) {
  return page.evaluate(() => {
    const canvas = document.querySelector<HTMLCanvasElement>("[data-testid=console-screen] canvas");
    if (!canvas || canvas.width === 0 || canvas.height === 0) {
      return false;
    }
    const data = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height).data;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i]! > 16 || data[i + 1]! > 16 || data[i + 2]! > 16) {
        return true;
      }
    }
    return false;
  });
}

test.use({ locale: "en-US" });

test("create a VM, open its console, stop and delete it", async ({ page }) => {
  await signIn(page);
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();

  await page.getByRole("link", { name: "Virtual machines" }).click();
  await page.getByRole("link", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await page.getByLabel("Memory (MiB)").fill("256");
  await page.getByRole("button", { name: "Attach a volume" }).click();
  await page.getByRole("group", { name: "Disk 1" }).getByLabel("Volume").selectOption(volume);
  await page.getByRole("button", { name: "Create", exact: true }).click();

  // 만든 뒤 시작한다. watch가 상태를 바꿔 준다.
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  await expect(page.getByText("Running", { exact: true })).toBeVisible({ timeout: 60_000 });

  await page.getByRole("tab", { name: "Console" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Connected" })).toBeVisible();
  await expect.poll(() => consoleShowsSomething(page), { timeout: 60_000 }).toBe(true);

  // OS가 없는 게스트는 ACPI 종료에 응하지 않으므로 강제로 끈다.
  await page.getByRole("button", { name: "Force stop" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Force stop" }).click();
  await expect(page.getByText("Stopped", { exact: true })).toBeVisible({ timeout: 60_000 });

  await page.getByRole("button", { name: "Delete" }).click();
  const dialog = page.getByRole("alertdialog");
  await dialog.getByLabel("Type web to confirm").fill("web");
  await dialog.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0);

  // 로그아웃하면 보호된 화면은 로그인으로 보낸다.
  await page.getByRole("button", { name: "Account" }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await page.goto("/vms");
  await expect(page).toHaveURL(/\/login\?next=%2Fvms/);
});
