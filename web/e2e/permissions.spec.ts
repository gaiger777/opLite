import { createHmac } from "node:crypto";

import { expect, type Page, test } from "@playwright/test";
import { openItem } from "./nav";

// 계획 62: 전역 역할이 없는 사용자(alice, 그룹으로 /teamA operator)는 VM 목록·폴더 트리·팔레트에 자기 폴더만
// 보고, + 만들기가 그 폴더를 골라 둔다. 인증 앱을 켜면 로그인에 둘째 단계가 생긴다. 클러스터·폴더·VM·사용자·grant는
// Rust 테스트(`crates/orbivisord/tests/web.rs`)가 준비한다.
const user = process.env.ORBIVISOR_USER ?? "";
const password = process.env.ORBIVISOR_PASSWORD ?? "";
if (!user || !password) {
  throw new Error("Set ORBIVISOR_USER and ORBIVISOR_PASSWORD (the Rust test sets them).");
}

/** RFC 4648 base32 → 바이트 */
function base32(text: string): Buffer {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of text.replace(/=+$/, "").toUpperCase()) {
    bits += alphabet.indexOf(c).toString(2).padStart(5, "0");
  }
  const bytes = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) {
    bytes.push(parseInt(bits.slice(i, i + 8), 2));
  }
  return Buffer.from(bytes);
}

const step = () => Math.floor(Date.now() / 1000 / 30);

/** RFC 6238 TOTP(SHA-1, 6자리, 30초) */
function totp(secret: string, at: number): string {
  const counter = Buffer.alloc(8);
  counter.writeBigUInt64BE(BigInt(at));
  const mac = createHmac("sha1", base32(secret)).update(counter).digest();
  const offset = mac[mac.length - 1]! & 0x0f;
  const value = (mac.readUInt32BE(offset) & 0x7fffffff) % 1_000_000;
  return String(value).padStart(6, "0");
}

async function signIn(page: Page) {
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
}

test.use({ locale: "en-US" });

test("a folder user sees only their folder and signs in with a second factor", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await signIn(page);
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();

  // VM 목록: /teamA의 a1만. 다른 폴더(b1)와 루트(r1)의 VM은 없다.
  await openItem(page, "Virtual machines", "All VMs");
  await expect(page.getByRole("link", { name: "a1", exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: "b1", exact: true })).toHaveCount(0);
  await expect(page.getByRole("link", { name: "r1", exact: true })).toHaveCount(0);

  // 폴더 보기(목록 오른쪽 위 토글): 트리에 teamA만
  await page
    .getByRole("navigation", { name: "Show as" })
    .getByRole("link", { name: "Show by folder" })
    .click();
  const tree = page.getByRole("tree", { name: "VM folders" });
  await expect(tree.getByRole("treeitem")).toHaveText(["All VMs", "teamA"]);
  await tree.getByRole("treeitem", { name: "teamA" }).click();
  await expect(page.getByRole("heading", { name: "/teamA" })).toBeVisible();
  await expect(page.getByRole("link", { name: "a1", exact: true })).toBeVisible();

  // 팔레트: b1은 찾지 못하고 a1은 찾는다.
  await page.keyboard.press("ControlOrMeta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  const input = palette.getByRole("combobox", { name: "Command palette" });
  await input.fill("b1");
  await expect(palette.getByRole("option", { name: /^b1/ })).toHaveCount(0);
  await input.fill("a1");
  await expect(
    palette.getByRole("group", { name: "Virtual machines" }).getByRole("option", { name: /^a1/ }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(palette).toBeHidden();

  // + 만들기 → 가상 머신: 루트에는 만들 수 없으니 /teamA가 골라져 있고 노드 칸이 없다(머신은 서버가 고른다).
  await page.getByRole("button", { name: "Create new" }).click();
  await page.getByRole("menuitem", { name: "Virtual machine" }).click();
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  // VM 만들기 한 화면(계획 77-5): 용도 카드 "빈 VM" → 그림과 오른쪽 요약(폴더·노드)
  await page.getByRole("button", { name: /^Blank VM/ }).click();
  await expect(page.getByLabel("Folder", { exact: true })).toHaveValue(/.+/);
  await expect(page.getByLabel("Folder", { exact: true }).locator("option:checked")).toHaveText(
    "/teamA",
  );
  await expect(page.getByLabel("Node", { exact: true })).toHaveCount(0);
  await page.getByLabel("Name", { exact: true }).fill("a2");
  await page.getByRole("button", { name: "Edit CPU & memory" }).click();
  await page.getByRole("textbox", { name: "Memory (MiB)" }).fill("256");
  await page.getByRole("button", { name: "Close settings" }).click();
  await page.getByRole("checkbox", { name: "Start after creating" }).uncheck();
  await expect(
    page.getByRole("complementary").getByText("Server check: passed.", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Create", exact: true }).click();
  await expect(page.getByRole("heading", { name: "a2" })).toBeVisible();

  // 인증 앱을 켠다(사용자 메뉴 › 내 2단계 인증). 비밀로 코드를 셈한다.
  await page.getByRole("button", { name: /^Account / }).click();
  await page.getByRole("menuitem", { name: "My two-step verification" }).click();
  await page.getByRole("button", { name: "Set up an authenticator app" }).click();
  await expect(page.getByRole("img", { name: "QR code for the authenticator app" })).toBeVisible();
  const secret = (await page.getByTestId("totp-secret").textContent()) ?? "";
  const confirmed = step();
  await page.getByLabel("Code from the app").fill(totp(secret, confirmed));
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(
    page.getByRole("list", { name: "Recovery codes" }).getByRole("listitem"),
  ).toHaveCount(10);
  await page.getByRole("button", { name: "I saved them" }).click();
  await expect(page.getByText("An authenticator app is set up.")).toBeVisible();

  // 로그아웃하고 다시 로그인하면 둘째 단계가 있다. 확인에 쓴 칸은 다시 못 쓰므로 다음 칸을 기다린다.
  await page.getByRole("button", { name: /^Account/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login/);
  await signIn(page);
  const code = page.getByLabel("Authentication code");
  await expect(code).toBeVisible();
  await code.fill("000000");
  await page.getByRole("button", { name: "Verify" }).click();
  await expect(page.getByRole("alert")).toHaveText("That code is not valid.");
  await expect.poll(step, { timeout: 40_000, intervals: [500] }).toBeGreaterThan(confirmed);
  await code.fill(totp(secret, step()));
  await page.getByRole("button", { name: "Verify" }).click();
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
});
