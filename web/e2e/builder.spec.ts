import { expect, type Locator, type Page, test } from "@playwright/test";

// 계획 71: VM › 만들기의 보기 "빌더"로 VM을 셋 만든다 — 넓은 화면 마우스 끌기(b1, 폼에서 넘어온 NIC 블록을 "빼기"에 끌어
// 놓기·폴더·작게 포함), 키보드만(b2), 휴대폰 폭 서랍의 누르기 들기와 "여기에 놓기"(b3). 모두 꺼진 채 만든다(QEMU를 켜지
// 않음). b1은 문제가 칸에 붙는지 본다: 이미 있는 볼륨 `b1-disk0`과 이름이 겹침(브라우저 오류 — 서버에 묻지 않음), 풀 여유보다
// 큰 디스크(서버 경고 — 디스크 줄), vCPU 할당량 1인 폴더 `/q`(서버 오류 — VM 틀, 만들기 막힘). 클러스터·풀·그 볼륨·폴더는
// Rust 테스트(`crates/orbivisord/tests/web.rs`)가 준비하고 만든 VM을 확인한다.
const user = process.env.OPL_USER ?? "admin";
const password = process.env.OPL_PASSWORD ?? "";
/** 새 디스크를 만들 dir 풀 이름 */
const pool = process.env.OPL_POOL ?? "";
if (!password || !pool) {
  throw new Error("Set OPL_PASSWORD and OPL_POOL (the Rust test sets them).");
}

async function signIn(page: Page) {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Username").fill(user);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
}

/** VM › VM의 "만들기" → 보기 "빌더"(늦게 읽는 조각) */
async function openBuilder(page: Page) {
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: "Virtual machines", exact: true })
    .click();
  await page
    .getByRole("navigation", { name: "Virtual machines menu" })
    .getByRole("link", { name: "VMs", exact: true })
    .click();
  await page.getByRole("link", { name: "Create", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  await page.getByRole("link", { name: "Builder", exact: true }).click();
  await expect(page).toHaveURL(/\/vms\/-new\/builder$/);
  await expect(page.getByRole("link", { name: "Builder", exact: true })).toHaveAttribute(
    "aria-current",
    "page",
  );
}

const part = (page: Page, name: RegExp) => page.getByRole("button", { name });
const slot = (page: Page, name: string | RegExp) =>
  page.getByRole("group", { name: typeof name === "string" ? new RegExp(`^${esc(name)}`) : name });
const esc = (s: string) => s.replace(/[()]/g, "\\$&");
/** 끌기 틀의 알림 줄(지도의 노드 줄 `<p aria-live>`와 다름) */
const announcer = (page: Page) => page.locator("div[role=status][aria-live=polite]");
/** 확인 판의 상태 줄(알림 줄에도 같은 글이 있다) */
const checkStatus = (page: Page, text: string) =>
  page.getByRole("tabpanel").getByText(text, { exact: true });
const create = (page: Page) => page.getByRole("button", { name: "Create", exact: true });

/** 진짜 마우스로 부품을 칸 가운데로 끈다(문턱 4px을 넘게 여러 걸음). */
async function drag(page: Page, from: Locator, to: Locator) {
  await from.scrollIntoViewIfNeeded();
  const a = (await from.boundingBox())!;
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2);
  await page.mouse.down();
  await page.mouse.move(a.x + a.width / 2 + 10, a.y + a.height / 2 + 10, { steps: 3 });
  const b = (await to.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2, { steps: 10 });
  await page.mouse.up();
}

/** 키보드로 든다(Space) → 놓을 칸까지 Tab(틀이 놓을 수 있는 칸 사이만 옮김) → Space로 놓는다. */
async function keyDrop(page: Page, from: Locator, to: Locator) {
  await from.focus();
  await page.keyboard.press("Space");
  await expect(announcer(page)).toContainText("Picked up");
  for (let i = 0; i < 30; i += 1) {
    if (await to.evaluate((el) => el === document.activeElement)) {
      break;
    }
    await page.keyboard.press("Tab");
  }
  await expect(to).toBeFocused();
  await page.keyboard.press("Space");
}

/** 만든 VM의 상세 화면에 왔다. */
async function created(page: Page, name: string) {
  await expect(page).toHaveURL(new RegExp(`/vms/${name}$`), { timeout: 60_000 });
  await expect(page.getByRole("heading", { name, exact: true })).toBeVisible();
}

test.use({ locale: "en-US" });

test("builds a VM by dragging with the mouse, with server problems on their slots", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  await signIn(page);
  await openBuilder(page);
  // 폼에서 넘어온 값(폼의 기본 NIC 하나)을 받는다. 그 블록을 손잡이로 끌어 "빼기" 칸에 놓는다.
  await expect(page.getByRole("button", { name: /^NIC 1 \(user\) / })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill("b1");
  const grip = page.getByRole("button", { name: "Pick up NIC 1 (user)" });
  await grip.scrollIntoViewIfNeeded();
  const g = (await grip.boundingBox())!;
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await page.mouse.down();
  await page.mouse.move(g.x + g.width / 2 + 10, g.y + g.height / 2 + 10, { steps: 3 });
  const trash = page.getByRole("group", { name: "Remove", exact: true });
  const tb = (await trash.boundingBox())!;
  await page.mouse.move(tb.x + tb.width / 2, tb.y + tb.height / 2, { steps: 10 });
  await page.mouse.up();
  await expect(page.getByRole("button", { name: /^NIC 1 \(user\) / })).toHaveCount(0);
  await expect(trash).toBeHidden();

  await drag(page, part(page, /^Medium, 2 vCPU · 4 GiB/), slot(page, "Compute"));
  await expect(slot(page, "Compute")).toContainText("2 vCPU · 4 GiB");
  await expect(announcer(page)).toContainText("Dropped Medium on Compute.");
  // 거르기 칸으로 부품을 좁힌 뒤 끈다.
  const filter = page.getByRole("searchbox", { name: "Filter parts" });
  await filter.fill(pool);
  await drag(page, part(page, new RegExp(`^New disk in ${pool}`)), slot(page, "Disks"));
  await expect(page.getByRole("button", { name: /^Disk 1 \(b1-disk0\) / })).toBeVisible();
  await filter.fill("user");
  await drag(page, part(page, /^user/), slot(page, "NICs"));
  await expect(page.getByRole("button", { name: /^NIC 1 \(user\) / })).toBeVisible();
  await filter.fill("");
  await page.getByRole("checkbox", { name: "Start after creating" }).uncheck();

  // 브라우저가 아는 문제: 볼륨 b1-disk0이 이미 있다 → 디스크 줄에 오류, 만들기 막힘, 서버에는 아직 묻지 않음
  const disk = slot(page, "Disk 1 (b1-disk0)");
  await expect(disk).toContainText("A volume with this name already exists.");
  await expect(create(page)).toBeDisabled();
  await expect(
    checkStatus(page, "The server check runs when the browser finds no errors."),
  ).toBeVisible();

  // 살피기에서 볼륨 이름을 바꾸고 디스크를 풀 여유보다 크게(4 TiB) → 서버 확인의 경고가 그 디스크 줄에(만들기는 막지 않음)
  await page.getByRole("button", { name: /^Disk 1 \(b1-disk0\) / }).click();
  await expect(page.getByRole("heading", { name: "Settings of Disk 1 (b1-disk0)" })).toBeVisible();
  await page.getByLabel("Volume name").fill("b1-root");
  const size = page.getByRole("textbox", { name: "Size (GiB)" });
  await size.fill("4096");
  const big = slot(page, "Disk 1 (b1-root)");
  await expect(big).toContainText(`storage pool "${pool}" has`, { timeout: 15_000 });
  await expect(big).toHaveAttribute("data-level", "warn");
  await expect(create(page)).toBeEnabled();
  await size.fill("1");
  await page.getByRole("button", { name: "Close settings" }).click();
  await expect(checkStatus(page, "Server check: passed.")).toBeVisible({ timeout: 15_000 });

  // 폴더 /q(할당량 vCPU 1)를 폴더 칸에 끌어 놓으면 서버 확인이 할당량 오류를 VM 틀에 붙이고 만들기를 막는다.
  await filter.fill("/q");
  await drag(page, part(page, /^\/q/), slot(page, "Folder"));
  await filter.fill("");
  await expect(slot(page, "Folder")).toContainText("/q");
  const whole = slot(page, /^VM, \d problems?$/);
  await expect(whole).toContainText("folder /q would use 2 of 1 vCPUs", { timeout: 15_000 });
  await expect(whole).toHaveAttribute("data-level", "error");
  await expect(checkStatus(page, "Server check: 1 problem.")).toBeVisible();
  await expect(create(page)).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Cannot create yet: folder /q would use 2 of 1 vCPUs" }),
  ).toBeVisible();
  // "작게"(1 vCPU)를 계산 칸에 놓으면 통과한다.
  await drag(page, part(page, /^Small, 1 vCPU/), slot(page, "Compute"));
  await expect(slot(page, "Compute")).toContainText("1 vCPU · 1 GiB");
  await expect(checkStatus(page, "Server check: passed.")).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/^Machine (virt|q35)$/)).toBeVisible();
  await expect(create(page)).toBeEnabled();

  // 보낼 요청 그대로: JSON(볼륨 먼저)과 CLI
  await page.getByRole("tab", { name: "JSON" }).click();
  const panel = page.getByRole("tabpanel");
  await expect(panel).toContainText("# POST /api/v1/volumes");
  await expect(panel).toContainText("# POST /api/v1/vms");
  await expect(panel).toContainText('"name": "b1-root"');
  await page.getByRole("tab", { name: "CLI" }).click();
  await expect(panel).toContainText(`orbivisorctl volume create b1-root --pool ${pool} --size 1G`);
  await expect(panel).toContainText("orbivisorctl vm create b1");
  await page.getByRole("tab", { name: "Check" }).click();

  await create(page).click();
  await created(page, "b1");
});

test("builds a VM with the keyboard only", async ({ page }) => {
  await signIn(page);
  // 주소로 바로 열면 폼의 초안 없이 빈 틀이다.
  await page.goto("/vms/-new/builder");
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  await expect(slot(page, "NICs")).toContainText("Drop a network here.");
  await page.getByLabel("Name", { exact: true }).fill("b2");

  await keyDrop(page, part(page, /^Medium/), slot(page, "Compute"));
  await expect(announcer(page)).toContainText("Dropped Medium on Compute.");
  await expect(slot(page, "Compute")).toContainText("2 vCPU · 4 GiB");
  await keyDrop(page, part(page, new RegExp(`^New disk in ${pool}`)), slot(page, "Disks"));
  // 놓으면 새 블록에 초점이 가고 개수를 읽는다.
  await expect(page.getByRole("button", { name: /^Disk 1 \(b2-disk0\) / })).toBeFocused();
  await expect(announcer(page)).toContainText("1 disk.");

  // 들고 Esc: 취소하고 초점은 든 부품으로
  const user = part(page, /^user/);
  await user.focus();
  await page.keyboard.press("Enter");
  await expect(announcer(page)).toContainText("Picked up user.");
  await page.keyboard.press("Escape");
  await expect(announcer(page)).toContainText("Cancelled.");
  await expect(user).toBeFocused();

  await keyDrop(page, user, slot(page, "NICs"));
  await expect(page.getByRole("button", { name: /^NIC 1 \(user\) / })).toBeFocused();
  await expect(announcer(page)).toContainText("1 NIC.");

  const start = page.getByRole("checkbox", { name: "Start after creating" });
  await start.focus();
  await page.keyboard.press("Space");
  await expect(start).not.toBeChecked();
  await expect(checkStatus(page, "Server check: passed.")).toBeVisible({
    timeout: 15_000,
  });
  await create(page).focus();
  await page.keyboard.press("Enter");
  await created(page, "b2");
});

test("builds a VM at phone width from the parts drawer", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  // 휴대폰 폭의 사이드바는 메뉴 단추 뒤에 있다 — 빌더 주소로 바로 간다(빈 틀).
  await page.goto("/vms/-new/builder");
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill("b3");

  // 서랍을 열고 부품을 누르면(들기) 서랍이 닫히고 놓을 수 있는 칸에 "여기에 놓기"가 나온다.
  async function tapDrop(name: RegExp, to: string) {
    await page.getByRole("button", { name: "Add parts" }).click();
    const drawer = page.getByRole("dialog", { name: "Parts" });
    await drawer.getByRole("button", { name }).click();
    await expect(drawer).toBeHidden();
    const target = slot(page, to);
    await target.getByText("Drop here", { exact: true }).click();
  }
  await tapDrop(/^Medium/, "Compute");
  await expect(slot(page, "Compute")).toContainText("2 vCPU · 4 GiB");
  await tapDrop(new RegExp(`^New disk in ${pool}`), "Disks");
  await expect(page.getByRole("button", { name: /^Disk 1 \(b3-disk0\) / })).toBeVisible();
  await tapDrop(/^user/, "NICs");
  await expect(page.getByRole("button", { name: /^NIC 1 \(user\) / })).toBeVisible();

  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
    ),
  ).toBe(true);
  await page.getByRole("checkbox", { name: "Start after creating" }).uncheck();
  await expect(checkStatus(page, "Server check: passed.")).toBeVisible({
    timeout: 15_000,
  });
  await create(page).click();
  await created(page, "b3");
});

// 계획 72: 가져오기 탭의 새 보기(VMware 이전·원본·내보내기)와 새 계획 마법사가 휴대폰 폭에서 가로로 넘치지 않는다(어둠
// 테마도). 원본이 없는 클러스터라 빈 보기다 — 원본·마법사의 흐름은 계획 72 스테이지 8의 `vmware.spec.ts`가 본다.
test("opens the VMware and export views at phone width", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  for (const [path, heading] of [
    ["/vmware-migrations", "VMware migrations"],
    ["/vmware-sources", "VMware sources"],
    ["/vm-exports", "Exports"],
    ["/vmware-migrations/-new", "New VMware migration"],
  ] as const) {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1, name: heading })).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
        ),
        `${path} (${colorScheme}) scrolls sideways`,
      ).toBe(true);
    }
  }
  await page.emulateMedia({ colorScheme: null });
});

// 계획 73: 감사 탭의 설정 기록·선언 묶음 보기와 보안 탭의 인증서 보기가 휴대폰 폭에서 가로로 넘치지 않는다(어둠 테마도).
// 이 파일의 앞 시험이 만든 VM이 설정 기록에 줄로 남는다(만들기 — 웹 출처). 되돌리기 흐름은 스테이지 7의 `history.spec.ts`가 본다.
test("opens the config history, apply sets and certificates views at phone width", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signIn(page);
  for (const [path, heading] of [
    ["/history", "Config history"],
    ["/apply-sets", "Apply sets"],
    ["/settings/certificates", "Certificates"],
  ] as const) {
    for (const colorScheme of ["light", "dark"] as const) {
      await page.emulateMedia({ colorScheme });
      await page.goto(path);
      await expect(page.getByRole("heading", { level: 1, name: heading })).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
        ),
        `${path} (${colorScheme}) scrolls sideways`,
      ).toBe(true);
    }
  }
  // 설정 기록에 앞 시험이 만든 VM의 "만듦" 줄이 있고, 누르면 서랍이 열린다(휴대폰 폭에서도 넘치지 않음).
  await page.emulateMedia({ colorScheme: null });
  await page.goto("/history?kind=vm");
  const row = page.getByRole("button", { name: /Created.*vm b1/ });
  await expect(row).toBeVisible();
  await row.click();
  await expect(page.getByRole("dialog", { name: "vm b1" })).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
    ),
  ).toBe(true);
});
