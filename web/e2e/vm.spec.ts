import { expect, type Page, test } from "@playwright/test";
import { openItem, openPageTab } from "./nav";

const user = process.env.ORBIVISOR_USER ?? "admin";
const password = process.env.ORBIVISOR_PASSWORD ?? "";
/** 노드에 미리 만든 빈 디렉터리(풀)와 올릴 디스크 이미지. Rust 테스트가 준비한다. */
const poolPath = process.env.ORBIVISOR_POOL_PATH ?? "";
const image = process.env.ORBIVISOR_IMAGE ?? "";
/** 백업 저장소 줄기(계획 68)의 백업 풀 디렉터리. 저장소를 지워도 데이터가 남으므로 위 풀과 따로 둔다. */
const backupPoolPath = process.env.ORBIVISOR_BACKUP_POOL_PATH ?? "";
if (!password || !poolPath || !image || !backupPoolPath) {
  throw new Error(
    "Set ORBIVISOR_PASSWORD, ORBIVISOR_POOL_PATH, ORBIVISOR_IMAGE and ORBIVISOR_BACKUP_POOL_PATH (the Rust test sets them).",
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

/** VM 상세 머리의 케밥 메뉴("Actions for {vm}")에서 항목을 고른다. 주 동작(시작·정지·콘솔·옮기기) 밖은 메뉴에 있다. */
async function vmMenu(page: Page, vm: string, item: string) {
  await page.getByRole("button", { name: `Actions for ${vm}`, exact: true }).click();
  await page.getByRole("menuitem", { name: item, exact: true }).click();
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

/** 스냅샷 탭의 예약 스냅샷 행: 이름(뒤 배지 글은 뺀다)과 만든 시각(만드는 중이면 null). */
async function scheduledSnapshots(page: Page) {
  const rows = page.getByRole("row").filter({ has: page.getByText("Scheduled", { exact: true }) });
  const cells = await rows.evaluateAll((trs) =>
    trs.map((tr) => [...(tr as HTMLTableRowElement).cells].map((c) => c.textContent ?? "")),
  );
  return cells.map(([name, created]) => ({
    name: /^auto-[0-9-]+/.exec(name ?? "")?.[0] ?? name ?? "",
    created: created === "Creating" ? null : (created ?? null),
  }));
}

/**
 * 계획 70: 인프라 › 노드의 보기 "지도"(늦게 읽는 조각). 켜 둔 VM `vm`이 노드 블록 위 타일로 보이고, 키보드(`]`·화살표·Enter)와
 * 진짜 브라우저의 타일 누르기(SVG 점 적중 검사)가 서랍을 열며, 목록 보기·동작 줄이기·어둠 테마·휴대폰 폭(가로 스크롤 없음)을 본다.
 * 끝에 보기 "노드"로 돌아가 인프라를 다시 누르면 기억한 보기로 온다.
 */
async function clusterMap(page: Page, vm: string) {
  await openItem(page, "Overview", "Infrastructure map");
  await expect(page).toHaveURL(/\/cluster\/map$/);
  const picture = page.getByRole("group", { name: "Cluster map" });
  const node = picture.getByRole("button", { name: /^Node node1, .*\bonline\b/ });
  await expect(node).toBeVisible();
  await expect(node).toHaveAttribute("aria-label", /\b1 VM \(1 running\)/);
  // 한 노드·VM 하나는 맞추기 배율이 커서 "자세히" 단계 — 타일 윗면에 VM 이름이 있다.
  await expect(page.getByRole("status").filter({ hasText: "Detail." })).toBeVisible();
  const tileText = picture.locator("text").filter({ hasText: new RegExp(`^${vm}$`) });
  await expect(tileText).toBeVisible();

  // 키보드: 노드에 초점 → `]`로 그 노드의 VM 타일(화면 초점은 노드에 남고 aria-live 줄이 읽는다) → Enter로 서랍
  const live = page.locator("p[aria-live=polite]").filter({ hasText: /^(Node|VM) / });
  // 처음 초점은 노드 자신의 이름(aria-label)으로 읽고, 옮겨 갈 때부터 aria-live 줄이 읽는다.
  await node.focus();
  await expect(node).toBeFocused();
  await page.keyboard.press("]");
  await expect(live).toHaveText(new RegExp(`^VM ${vm}, Running, node node1`));
  await expect(node).toBeFocused();
  await page.keyboard.press("Enter");
  // 서랍 조각은 처음 열 때 받는다.
  let sheet = page.getByRole("dialog");
  await expect(sheet.getByRole("heading", { name: vm, exact: true })).toBeVisible();
  await expect(sheet.getByRole("button", { name: "Migrate…" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(sheet).toBeHidden();
  await expect(node).toBeFocused();
  // `[`는 그림 안에서 노드로 나온다(그림 밖에서는 전역 단축키 — 사이드바 접기).
  await page.keyboard.press("[");
  await expect(live).toHaveText(/^Node node1, /);
  await page.keyboard.press("Enter");
  sheet = page.getByRole("dialog");
  await expect(sheet.getByRole("heading", { name: "node1", exact: true })).toBeVisible();
  await expect(sheet.getByRole("button", { name: "Node actions" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(sheet).toBeHidden();

  // 계획 71: VM 타일에서 `m`(끌어 옮기기의 키보드 길). 판정 함수는 서랍 조각과 함께 오는데 위에서 서랍을 열었으니 왔다.
  // 노드가 하나라 놓을 곳이 없다고 끌기 틀의 알림 줄이 읽고, Esc는 취소하고 초점을 노드에 돌려준다.
  const announcer = page.locator("div[role=status][aria-live=polite]");
  await expect(node).toBeFocused();
  await page.keyboard.press("]");
  await expect(live).toHaveText(new RegExp(`^VM ${vm}, `));
  await page.keyboard.press("m");
  await expect(announcer).toHaveText(`Moving ${vm}: no node can take it. Escape cancels.`);
  await expect(page.locator("html")).toHaveAttribute("data-dnd-held", /.*/);
  await page.keyboard.press("Escape");
  await expect(announcer).toHaveText("Cancelled.");
  await expect(page.locator("html")).not.toHaveAttribute("data-dnd-held", /.*/);
  await expect(node).toBeFocused();
  await page.keyboard.press("[");
  await expect(live).toHaveText(/^Node node1, /);

  // 진짜 브라우저의 가리키기·누르기: 타일은 합친 경로라 DOM 원소가 없고, 적중 검사가 그 점의 타일을 찾는다.
  const box = (await tileText.boundingBox())!;
  const at = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  await page.mouse.move(at.x, at.y);
  await expect(page.locator("[data-map-card]")).toContainText(vm);
  await page.mouse.click(at.x, at.y);
  sheet = page.getByRole("dialog");
  await expect(sheet.getByRole("heading", { name: vm, exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(sheet).toBeHidden();

  // 목록 보기에 같은 노드·VM
  await page.getByRole("radio", { name: "List" }).click();
  const list = page.getByRole("region", { name: "Cluster map as a list" });
  await expect(list.getByRole("button", { name: "node1", exact: true })).toBeVisible();
  await expect(list.getByRole("button", { name: vm, exact: true })).toBeVisible();
  await page.getByRole("radio", { name: "Picture" }).click();
  await expect(node).toBeVisible();

  // 동작 줄이기면 움직임 키프레임이 없다(그림의 <style>). 어둠 테마에서도 그린다.
  const motion = () =>
    picture.evaluate((svg) =>
      [...svg.querySelectorAll("style")].some((s) => /@keyframes/.test(s.textContent ?? "")),
    );
  expect(await motion()).toBe(true);
  await page.emulateMedia({ reducedMotion: "reduce", colorScheme: "dark" });
  await expect.poll(motion).toBe(false);
  await expect(page.locator("html")).toHaveClass(/\bdark\b/);
  await expect(node).toBeVisible();
  await expect(tileText).toBeVisible();
  await page.emulateMedia({ reducedMotion: null, colorScheme: null });

  // 휴대폰 폭: 도구 띠는 그림 | 목록과 "보기 설정" 하나, 문서는 가로로 넘치지 않는다.
  const wide = page.viewportSize()!;
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("checkbox", { name: "Storage" })).toBeHidden();
  await page.getByRole("button", { name: "View settings" }).click();
  sheet = page.getByRole("dialog", { name: "View settings" });
  await expect(sheet.getByRole("checkbox", { name: "Storage" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(sheet).toBeHidden();
  await expect(node).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= document.documentElement.clientWidth,
    ),
  ).toBe(true);
  await page.setViewportSize(wide);

  // 클러스터·가용성 › 노드로 간다.
  await openItem(page, "Cluster & availability", "Nodes");
  await expect(page).toHaveURL(/\/nodes$/);
}

test.use({ locale: "en-US" });

test("upload an image, run a VM from it, clone and snapshot it, then delete everything", async ({
  page,
}) => {
  await signIn(page);
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();

  // 풀을 만들고 이미지를 올린다. 볼륨은 가져오기 Task가 끝나면 준비된다(watch).
  await openItem(page, "Storage", "Storage pools");
  // 머리줄의 "+ 만들기"(Create new)와 겹치지 않게 정확히 찾는다.
  await page.getByRole("button", { name: "Create", exact: true }).click();
  let dialog = page.getByRole("dialog", { name: "Create a storage pool" });
  await dialog.getByLabel("Name").fill("local");
  await dialog.getByLabel("Directory").fill(poolPath);
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).toBeHidden();
  // 풀 이름 칸에는 즐겨찾기 ★도 있다.
  await expect(page.getByRole("button", { name: "Favorite local", exact: true })).toBeVisible();

  await openItem(page, "Storage", "Disks (volumes)");
  await page.getByRole("button", { name: "Upload image" }).click();
  dialog = page.getByRole("dialog", { name: "Upload a disk image" });
  await dialog.getByLabel("Image file").setInputFiles(image);
  await expect(dialog.getByLabel("File format")).toHaveValue("raw");
  await dialog.getByLabel("Name").fill("root");
  await dialog.getByLabel("Storage pool").selectOption("local");
  await dialog.getByRole("button", { name: "Upload" }).click();
  const root = page
    .getByRole("row")
    .filter({ has: page.getByRole("cell", { name: "root", exact: true }) });
  await expect(root).toContainText("Ready", { timeout: 60_000 });
  await expect(root).toContainText("16 MiB");
  // raw 파일을 기본 형식 qcow2로 바꿔 가져왔다.
  await expect(root.getByRole("cell", { name: "qcow2", exact: true })).toBeVisible();

  await openItem(page, "Virtual machines", "All VMs");
  await page.getByRole("link", { name: "Create VM", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  // VM 만들기 한 화면(계획 77-5): 용도 카드 "빈 VM" → 그림에 이름·메모리(살피기 칸)·올린 볼륨 root(부품을 누르면 디스크 칸에 붙음)
  await page.getByRole("button", { name: /^Blank VM/ }).click();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await page.getByRole("button", { name: "Edit CPU & memory" }).click();
  await page.getByRole("textbox", { name: "Memory (MiB)" }).fill("256");
  await page.getByRole("button", { name: "Close settings" }).click();
  await page.getByRole("searchbox", { name: "Filter parts" }).fill("root");
  await page.getByRole("button", { name: /^root\b/ }).click();
  await expect(page.getByRole("button", { name: /^Disk 1 \(root\) / })).toBeVisible();
  await expect(page.getByRole("checkbox", { name: "Start after creating" })).toBeChecked();
  await expect(
    page.getByRole("complementary").getByText("Server check: passed.", { exact: true }),
  ).toBeVisible({ timeout: 15_000 });
  await page.getByRole("button", { name: "Create", exact: true }).click();

  // 만든 뒤 시작한다. watch가 상태를 바꿔 준다.
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  // 요약의 "원하는 전원 상태"가 아니라 머리글의 단계 표시를 본다. 상세 화면에는 하나뿐이다.
  await expect(page.getByTestId("vm-phase")).toHaveCount(1);
  await expect(page.getByTestId("vm-phase")).toHaveText("Running", { timeout: 60_000 });

  // 계획 70: 켜진 VM을 클러스터 지도에서 본 뒤 VM 화면으로 돌아온다.
  await clusterMap(page, "web");
  await openItem(page, "Virtual machines", "All VMs");
  await page.getByRole("link", { name: "web", exact: true }).click();
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();

  await page.getByRole("tab", { name: "Console" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Connected" })).toBeVisible();
  await expect.poll(() => consoleShowsSomething(page), { timeout: 60_000 }).toBe(true);

  // 실행 중에 메모리까지 담은 스냅샷을 만든다. 만들기 Task가 끝나면 현재 위치가 된다.
  // 스냅샷은 qcow2 디스크에만 만들 수 있다. 위에서 올린 raw 이미지를 qcow2로 가져왔다는 것이 전제다.
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await page.getByRole("button", { name: "Take snapshot" }).click();
  dialog = page.getByRole("dialog", { name: "Take a snapshot of web" });
  await dialog.getByLabel("Name").fill("before");
  await expect(dialog.getByRole("checkbox", { name: "Include memory" })).toBeChecked();
  await dialog.getByRole("button", { name: "Take snapshot" }).click();
  await expect(dialog).toBeHidden();
  const before = page.getByRole("row").filter({ has: page.getByRole("cell", { name: /^before/ }) });
  // 메모리 스냅샷은 느린 CI 디스크에서 2분 넘게 걸린 적이 있다(그동안 VM은 "작업 중"으로 남는다).
  await expect(before.getByText("Current", { exact: true })).toBeVisible({ timeout: 180_000 });
  await expect(before.getByRole("cell", { name: "Included", exact: true })).toBeVisible();

  // 되돌리면 VM을 껐다가 저장한 메모리에서 다시 실행한다. Task가 끝날 때는 다시 실행 중이다.
  await page.getByRole("button", { name: "Revert to before" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Revert" }).click();
  await expect(page.getByText("Revert web to before: done")).toBeVisible({ timeout: 180_000 });
  await expect(page.getByTestId("vm-phase")).toHaveText("Running");
  await expect(before.getByText("Current", { exact: true })).toBeVisible();

  // 실행 중인 VM은 지금 상태를 복사하지 않으므로 복제 대화상자는 최근 스냅샷을 원본으로 고른다.
  // 디스크를 QEMU가 쥐고 있어도 스냅샷에서는 복사할 수 있다.
  await vmMenu(page, "web", "Clone");
  dialog = page.getByRole("dialog", { name: "Clone web" });
  await expect(dialog.getByLabel("Source").locator("option:checked")).toHaveText("Snapshot before");
  await dialog.getByLabel("New VM name").fill("copy");
  await dialog.getByRole("button", { name: "Clone", exact: true }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByText("Clone web to copy: done")).toBeVisible({ timeout: 60_000 });

  // 복제한 VM은 꺼진 채로 생기고, 새 볼륨으로 시작한다.
  await openItem(page, "Virtual machines", "All VMs");
  await page.getByRole("link", { name: "copy", exact: true }).click();
  await expect(page.getByRole("heading", { name: "copy" })).toBeVisible();
  await expect(page.getByTestId("vm-phase")).toHaveText("Stopped");
  await page.getByRole("tab", { name: "Hardware" }).click();
  await expect(page.getByRole("cell", { name: "copy-root", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Running", { timeout: 60_000 });

  // 목록에서 골라 일괄로 끈다(설계 §5.3). 강제 정지는 고른 수를 입력해야 하고, VM마다 Task 결과를 보인다.
  await openItem(page, "Virtual machines", "All VMs");
  await page.getByRole("checkbox", { name: "Select copy", exact: true }).check();
  await page
    .getByRole("region", { name: "Bulk actions" })
    .getByRole("button", { name: /^Force stop \(/ })
    .click();
  const bulk = page.getByRole("dialog", { name: "Force stop 1 VM" });
  await bulk.getByLabel("Type 1 to confirm").fill("1");
  await bulk.getByRole("button", { name: "Force stop", exact: true }).click();
  await expect(bulk.getByText("Succeeded", { exact: true })).toBeVisible({ timeout: 60_000 });
  await bulk.getByRole("button", { name: "Done", exact: true }).click();
  const copyRow = page
    .getByRole("row")
    .filter({ has: page.getByRole("link", { name: "copy", exact: true }) });
  await expect(copyRow.getByTestId("vm-phase")).toHaveText("Stopped", { timeout: 60_000 });
  await page.getByRole("link", { name: "copy", exact: true }).click();
  await expect(page.getByRole("heading", { name: "copy" })).toBeVisible();
  await vmMenu(page, "copy", "Delete");
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type copy to confirm").fill("copy");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "copy", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });

  await page.getByRole("link", { name: "web", exact: true }).click();
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await page.getByRole("button", { name: "Delete snapshot before" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No snapshots.")).toBeVisible({ timeout: 60_000 });

  // 실행 중인 VM의 볼륨을 1 GiB로 늘린다.
  await openItem(page, "Storage", "Disks (volumes)");
  await page.getByRole("button", { name: "Resize root" }).click();
  dialog = page.getByRole("dialog", { name: "Resize volume root" });
  await expect(dialog.getByText("Current size: 16 MiB")).toBeVisible();
  await dialog.getByLabel("New size (GiB)").fill("1");
  await dialog.getByRole("button", { name: "Resize" }).click();
  await expect(dialog).toBeHidden();
  await expect(root.getByRole("cell", { name: "1 GiB", exact: true })).toBeVisible({
    timeout: 60_000,
  });

  // ⌘K 팔레트로 VM을 연다(설계 §5.2). CI는 Linux라 Ctrl+K다(ControlOrMeta가 둘 다 맞춘다).
  // VM 이름 그대로 치면 그 VM의 동작들이 앞에 오므로 "가상 머신" 묶음의 항목을 누른다.
  await page.keyboard.press("ControlOrMeta+k");
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Command palette" }).fill("web");
  await palette
    .getByRole("group", { name: "Virtual machines" })
    .getByRole("option", { name: /^web/ })
    .click();
  await expect(palette).toBeHidden();
  await expect(page).toHaveURL(/\/vms\/web$/);
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  await expect(page.getByTestId("vm-phase")).toHaveText("Running");

  // OS가 없는 게스트는 ACPI 종료에 응하지 않으므로 강제로 끈다.
  await vmMenu(page, "web", "Force stop");
  await page.getByRole("alertdialog").getByRole("button", { name: "Force stop" }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Stopped", { timeout: 60_000 });

  // 꺼진 VM에는 디스크만 담는 스냅샷을 만든다(메모리 칸이 없다).
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await page.getByRole("button", { name: "Take snapshot" }).click();
  dialog = page.getByRole("dialog", { name: "Take a snapshot of web" });
  await dialog.getByLabel("Name", { exact: true }).fill("off");
  await expect(dialog.getByRole("checkbox", { name: "Include memory" })).toHaveCount(0);
  await dialog.getByRole("button", { name: "Take snapshot" }).click();
  await expect(dialog).toBeHidden();
  const off = page.getByRole("row").filter({ has: page.getByRole("cell", { name: /^off/ }) });
  await expect(off.getByText("Current", { exact: true })).toBeVisible({ timeout: 60_000 });
  await expect(off.getByRole("cell", { name: "No", exact: true })).toBeVisible();

  // 예약 스냅샷을 켠다. 하네스가 한 시간을 2초로 줄였다.
  await expect(page.getByText("No snapshot schedule.")).toBeVisible();
  await page.getByRole("link", { name: "Change schedule" }).click();
  await expect(page.getByRole("heading", { name: "Edit web" })).toBeVisible();
  await page.getByLabel("Take snapshots", { exact: true }).selectOption("hourly");
  await page.getByLabel("Snapshots to keep", { exact: true }).fill("2");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await expect(page.getByText("Snapshot schedule: Hourly, keep 2")).toBeVisible();

  // 넷 이상 만들어질 때까지 본다. 넘친 것은 다음 tick에 지우고 그동안 새로 만들지 않으므로
  // 예약 스냅샷은 많아야 keep+1개다. 이름은 지운 뒤 다시 쓰일 수 있어 만든 시각(초)과 함께 센다.
  const seen: string[] = [];
  let most = 0;
  await expect
    .poll(
      async () => {
        const rows = await scheduledSnapshots(page);
        most = Math.max(most, rows.length);
        for (const { name, created } of rows) {
          if (created !== null && !seen.includes(`${name} ${created}`)) {
            seen.push(`${name} ${created}`);
          }
        }
        return seen.length;
      },
      { timeout: 90_000, intervals: [200] },
    )
    .toBeGreaterThanOrEqual(4);
  expect(most).toBeLessThanOrEqual(3);

  // 예약을 끈다. 예약 스냅샷 Task가 도는 동안에는 VM을 고칠 수 없어(vm_busy) 될 때까지 다시 한다.
  await expect(async () => {
    if (await page.getByRole("heading", { name: "Edit web" }).isVisible()) {
      await page.getByRole("link", { name: "Cancel" }).click();
    }
    await vmMenu(page, "web", "Edit");
    await page.getByLabel("Take snapshots", { exact: true }).selectOption("off");
    await page.getByRole("button", { name: "Save" }).click();
    await expect(page.getByRole("heading", { name: "Edit web" })).toHaveCount(0, {
      timeout: 5_000,
    });
  }).toPass({ timeout: 60_000 });
  await page.getByRole("tab", { name: "Snapshots" }).click();
  await expect(page.getByText("No snapshot schedule.")).toBeVisible();
  // 도는 Task가 끝나면 남은 예약 스냅샷은 keep개(마지막이 만들기였으면 keep+1개)이고 처음 것은 지웠다.
  // 사용자가 만든 스냅샷은 그대로다.
  await expect(page.getByText("Creating", { exact: true })).toHaveCount(0, { timeout: 60_000 });
  const left = await scheduledSnapshots(page);
  expect(left.length).toBeGreaterThanOrEqual(2);
  expect(left.length).toBeLessThanOrEqual(3);
  expect(left.map(({ name, created }) => `${name} ${created}`)).not.toContain(seen[0]);
  await expect(off).toBeVisible();
  await expect(page.getByText("Last scheduled snapshot failed:")).toHaveCount(0);

  // 계획 64: 꺼진 VM을 소켓 둘·게스트 NUMA 노드 둘로 바꿔 저장하면 요약의 "CPU 배치"가 그것을 보인다.
  await vmMenu(page, "web", "Edit");
  await expect(page.getByRole("heading", { name: "Edit web" })).toBeVisible();
  // 접힌 칸의 요약 줄(제목 뒤에 지금 값이 붙는다)
  await page.locator("summary", { hasText: "CPU placement and NUMA" }).click();
  await page.getByLabel("Sockets", { exact: true }).selectOption("2");
  await page.getByLabel("Guest NUMA nodes", { exact: true }).selectOption("count:2");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("heading", { name: "web" })).toBeVisible();
  await expect(
    page.getByRole("region", { name: "CPU placement" }).getByText("2 NUMA nodes", { exact: true }),
  ).toBeVisible();

  // 스냅샷이 있는 VM은 스냅샷도 지우겠다고 골라야 지울 수 있다.
  await vmMenu(page, "web", "Delete");
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type web to confirm").fill("web");
  await expect(confirm.getByRole("button", { name: "Delete" })).toBeDisabled();
  await confirm.getByRole("checkbox", { name: /^Also delete all \d+ of its snapshots$/ }).check();
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page).toHaveURL(/\/vms$/);
  await expect(page.getByRole("link", { name: "web", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });

  // 볼륨과 풀을 지운다. 볼륨은 삭제 Task가 끝나야 목록에서 사라진다.
  await openItem(page, "Storage", "Disks (volumes)");
  await page.getByRole("button", { name: "Delete root" }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type root to confirm").fill("root");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("cell", { name: "root", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });
  await page.getByRole("button", { name: "Delete copy-root" }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type copy-root to confirm").fill("copy-root");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("cell", { name: "copy-root", exact: true })).toHaveCount(0, {
    timeout: 60_000,
  });

  await openItem(page, "Storage", "Storage pools");
  await page.getByRole("button", { name: "Delete local" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No storage pools.")).toBeVisible();

  // 계획 63: 노드 상세의 CPU 모델(노드가 자기 QEMU에 물어 알린 것)과 설정 › 일반의 클러스터 CPU 기준
  await page.goto("/nodes/node1?tab=hardware");
  const cpu = page.getByRole("region", { name: "CPU models" });
  await expect(cpu.getByText("QEMU", { exact: true })).toBeVisible();
  await expect(cpu.getByRole("button", { name: "Re-read CPU models" })).toBeEnabled();
  // 계획 64: 노드 상세의 NUMA 토폴로지(Linux 노드는 탐침 결과, macOS는 없다고 말한다)
  const topology = page.getByRole("region", { name: "NUMA topology" });
  await expect(
    topology.getByText(/NUMA nodes ·|has not reported a NUMA topology/).first(),
  ).toBeVisible();
  await page.goto("/settings");
  await expect(page.getByLabel("Cluster CPU baseline", { exact: true })).toHaveValue("off");

  // 로그아웃하면 보호된 화면은 로그인으로 보낸다.
  await page.getByRole("button", { name: /^Account/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login/);
  await page.goto("/vms");
  await expect(page).toHaveURL(/\/login\?next=%2Fvms/);
});

// SDN(계획 65): 설정 전에는 네트워크 탭의 SDN 보기가 빈 상태이고, 네트워크 만들기의 SDN 종류는 막혀 있다.
test("SDN is not set up: the empty state and the blocked SDN type", async ({ page }) => {
  await signIn(page);
  await openItem(page, "Networking", "SDN (virtual networks)");
  await expect(page).toHaveURL(/\/sdn$/);
  await expect(page.getByText("SDN is not set up")).toBeVisible();
  await expect(page.getByRole("button", { name: "Start the SDN setup" })).toBeVisible();
  await openItem(page, "Networking", "Networks");
  // 머리줄의 전역 "Create" 메뉴가 아니라 목록의 만들기 단추
  await page.getByRole("main").getByRole("button", { name: "Create", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Create a network" });
  await expect(dialog.getByRole("option", { name: "SDN (OVN) — not set up" })).toBeDisabled();
});

// 라우터·유동 IP(계획 66): 네트워크 탭의 보기이고, SDN 설정 전에는 빈 상태와 막힌 만들기다.
test("routers and floating IPs before SDN is set up", async ({ page }) => {
  await signIn(page);
  await openItem(page, "Networking", "Routers & floating IPs");
  await expect(page).toHaveURL(/\/routers$/);
  await expect(page.getByText("No routers")).toBeVisible();
  await expect(
    page.getByText(
      "Routers and floating IPs need SDN set up first, under Networking › SDN (virtual networks).",
    ),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Create a router" })).toBeDisabled();
  await openPageTab(page, "Routers & floating IPs", "Floating IPs");
  await expect(page).toHaveURL(/\/floating-ips$/);
  await expect(page.getByText("No floating IPs")).toBeVisible();
  await expect(page.getByRole("button", { name: "Create a floating IP" })).toBeDisabled();
});

// 계획 67: 2.5D 토폴로지(네트워크 탭의 보기, 늦게 읽는 조각)는 단순 네트워크만 있는 클러스터에서도 그림을 그리고 목록 보기로
// 바뀐다. 부하 분산은 SDN 설정 전에 빈 상태와 막힌 만들기다.
test("network topology and load balancers before SDN is set up", async ({ page }) => {
  await signIn(page);
  await openItem(page, "Networking", "Networks");
  await page.getByRole("main").getByRole("button", { name: "Create", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Create a network" });
  await dialog.getByLabel("Name", { exact: true }).fill("e2e-lan");
  await dialog.getByLabel("Bridge", { exact: true }).fill("e2ebr0");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(page.getByRole("cell", { name: "e2e-lan", exact: true })).toBeVisible();

  await openItem(page, "Networking", "Topology diagram");
  await expect(page).toHaveURL(/\/networks\/topology$/);
  const picture = page.getByRole("group", { name: "Network topology" });
  await expect(
    picture.getByRole("button", { name: /^Simple network e2e-lan, bridge/ }),
  ).toBeVisible();
  await page.getByRole("radio", { name: "List" }).click();
  const list = page.getByRole("region", { name: "Network topology as a list" });
  await expect(list.getByRole("button", { name: "e2e-lan" })).toBeVisible();
  // 다음에 열 때를 위해 그림으로 되돌린다(브라우저에 기억한다).
  await page.getByRole("radio", { name: "Picture" }).click();

  await openItem(page, "Networking", "Load balancers");
  await expect(page).toHaveURL(/\/load-balancers$/);
  await expect(page.getByText("No load balancers")).toBeVisible();
  await expect(
    page.getByText(
      "Load balancers need SDN set up first, under Networking › SDN (virtual networks).",
    ),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Create a load balancer" })).toBeDisabled();

  await openItem(page, "Networking", "Networks");
  await page.getByRole("button", { name: "Delete e2e-lan" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("cell", { name: "e2e-lan", exact: true })).toHaveCount(0);
});

// 계획 68: 보호 영역의 백업 저장소 탭(늦게 읽는 조각). 백업 풀 안에 로컬 저장소를 만들면 목록에 보이고 초기화가 끝나면
// 준비된다. 저장소 리소스를 지우고 풀도 지운다(저장소 데이터는 풀 디렉터리에 남는다 — Rust 테스트가 본다).
test("create a local backup store and see it in the list", async ({ page }) => {
  await signIn(page);
  await openItem(page, "Storage", "Storage pools");
  await page.getByRole("button", { name: "Create", exact: true }).click();
  const poolDialog = page.getByRole("dialog", { name: "Create a storage pool" });
  await poolDialog.getByLabel("Name", { exact: true }).fill("e2e-bk");
  await poolDialog.getByLabel("Directory", { exact: true }).fill(backupPoolPath);
  await poolDialog.getByLabel("Store backups", { exact: true }).check();
  await poolDialog.getByRole("button", { name: "Create" }).click();
  await expect(poolDialog).toBeHidden();

  await openItem(page, "Backup & recovery", "Backup stores");
  await expect(page).toHaveURL(/\/backup-stores$/);
  await expect(page.getByText("No backup stores")).toBeVisible();
  await page.getByRole("button", { name: "Create a backup store" }).click();
  const wizard = page.getByRole("dialog", { name: "Create a backup store" });
  await wizard.getByLabel("Name", { exact: true }).fill("e2e-st");
  await expect(wizard.getByLabel("Backup pool", { exact: true })).toHaveValue("e2e-bk");
  await wizard.getByRole("button", { name: "Next" }).click();
  await wizard.getByRole("button", { name: "Next" }).click();
  await wizard.getByRole("button", { name: "Create" }).click();
  await expect(wizard).toBeHidden();
  const row = page
    .getByRole("row")
    .filter({ has: page.getByRole("button", { name: "Open e2e-st" }) });
  await expect(row).toContainText("e2e-bk");
  await expect(row).toContainText("Not encrypted");
  // 로컬 저장소 준비(디렉터리 65,536개)는 macOS에서 십몇 초 걸린다.
  await expect(row).toContainText("Ready", { timeout: 120_000 });

  await page.getByRole("button", { name: "Delete e2e-st" }).click();
  let confirm = page.getByRole("alertdialog");
  await confirm.getByLabel("Type e2e-st to confirm").fill("e2e-st");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No backup stores")).toBeVisible();

  await openItem(page, "Storage", "Storage pools");
  await page.getByRole("button", { name: "Delete e2e-bk" }).click();
  confirm = page.getByRole("alertdialog");
  await confirm.getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No storage pools.")).toBeVisible();
});
