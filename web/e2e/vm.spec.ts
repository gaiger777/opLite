import { expect, type Page, test } from "@playwright/test";

const user = process.env.OPL_USER ?? "admin";
const password = process.env.OPL_PASSWORD ?? "";
/** 노드에 미리 만든 빈 디렉터리(풀)와 올릴 디스크 이미지. Rust 테스트가 준비한다. */
const poolPath = process.env.OPL_POOL_PATH ?? "";
const image = process.env.OPL_IMAGE ?? "";
if (!password || !poolPath || !image) {
  throw new Error("Set OPL_PASSWORD, OPL_POOL_PATH and OPL_IMAGE (the Rust test sets them).");
}

async function signIn(page: Page) {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
}

/**
 * noVNC 캔버스에 검은색이 아닌 화소가 있다(콘솔이 화면을 받아 그린다). aarch64에서는 게스트가 화면을
 * 준비하기 전에 QEMU가 직접 글자를 그리므로 펌웨어 화면이라고까지는 보지 않는다.
 */
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

test("upload an image, run a VM from it, then delete everything", async ({ page }) => {
  await signIn(page);
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();

  // 풀을 만들고 이미지를 올린다. 볼륨은 가져오기 Task가 끝나면 준비된다(watch).
  await page.getByRole("link", { name: "Storage pools" }).click();
  await page.getByRole("button", { name: "Create" }).click();
  let dialog = page.getByRole("dialog", { name: "Create a storage pool" });
  await dialog.getByLabel("Name").fill("local");
  await dialog.getByLabel("Directory").fill(poolPath);
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("cell", { name: "local", exact: true })).toBeVisible();

  await page.getByRole("link", { name: "Volumes" }).click();
  await page.getByRole("button", { name: "Upload image" }).click();
  dialog = page.getByRole("dialog", { name: "Upload a disk image" });
  await dialog.getByLabel("Image file").setInputFiles(image);
  await expect(dialog.getByLabel("File format")).toHaveValue("raw");
  await dialog.getByLabel("Name").fill("root");
  await dialog.getByLabel("Storage pool").selectOption("local");
  await dialog.getByRole("button", { name: "Upload" }).click();
  const root = page.getByRole("row").filter({ has: page.getByRole("cell", { name: "root" }) });
  await expect(root).toContainText("Ready", { timeout: 60_000 });
  await expect(root).toContainText("16 MiB");

  await page.getByRole("link", { name: "Virtual machines" }).click();
  await page.getByRole("link", { name: "Create" }).click();
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await page.getByLabel("Memory (MiB)").fill("256");
  await page.getByRole("button", { name: "Attach a volume" }).click();
  await page.getByRole("group", { name: "Disk 1" }).getByLabel("Volume").selectOption("root");
  await page.getByRole("button", { name: "Create", exact: true }).click();

  // 만든 뒤 시작한다. watch가 상태를 바꿔 준다.
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  // 요약의 "원하는 전원 상태"가 아니라 머리글의 단계 표시를 본다. 상세 화면에는 하나뿐이다.
  await expect(page.getByTestId("vm-phase")).toHaveCount(1);
  await expect(page.getByTestId("vm-phase")).toHaveText("Running", { timeout: 60_000 });

  await page.getByRole("tab", { name: "Console" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Connected" })).toBeVisible();
  await expect.poll(() => consoleShowsSomething(page), { timeout: 60_000 }).toBe(true);

  // OS가 없는 게스트는 ACPI 종료에 응하지 않으므로 강제로 끈다.
  await page.getByRole("button", { name: "Force stop" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Force stop" }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Stopped", { timeout: 60_000 });

  await page.getByRole("button", { name: "Delete" }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0);

  // 볼륨과 풀을 지운다. 볼륨은 삭제 Task가 끝나야 목록에서 사라진다.
  await page.getByRole("link", { name: "Volumes" }).click();
  await page.getByRole("button", { name: "Delete root" }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type root to confirm").fill("root");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("cell", { name: "root" })).toHaveCount(0, { timeout: 60_000 });

  await page.getByRole("link", { name: "Storage pools" }).click();
  await page.getByRole("button", { name: "Delete local" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No storage pools.")).toBeVisible();

  // 로그아웃하면 보호된 화면은 로그인으로 보낸다.
  await page.getByRole("button", { name: /^Account/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login/);
  await page.goto("/vms");
  await expect(page).toHaveURL(/\/login\?next=%2Fvms/);
});
