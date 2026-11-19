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
  await expect(page.getByRole("heading", { name: "Home" })).toBeVisible();
}

/**
 * 왼쪽 사이드바의 영역을 누른 뒤 본문 위 영역 탭을 누른다. 사이드바에는 영역만 있고 하위 화면은 영역 탭이다
 * (설계 §4). 이동 경로에도 같은 이름의 링크가 있으므로 내비게이션 안에서 찾는다.
 */
async function openTab(page: Page, area: string, tab: string) {
  await page
    .getByRole("navigation", { name: "Main" })
    .getByRole("link", { name: area, exact: true })
    .click();
  await page
    .getByRole("navigation", { name: `${area} menu` })
    .getByRole("link", { name: tab, exact: true })
    .click();
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

test.use({ locale: "en-US" });

test("upload an image, run a VM from it, clone and snapshot it, then delete everything", async ({
  page,
}) => {
  await signIn(page);
  await expect(page.getByRole("status").filter({ hasText: "Live" })).toBeVisible();

  // 풀을 만들고 이미지를 올린다. 볼륨은 가져오기 Task가 끝나면 준비된다(watch).
  await openTab(page, "Infrastructure", "Storage pools");
  // 머리줄의 "+ 만들기"(Create new)와 겹치지 않게 정확히 찾는다.
  await page.getByRole("button", { name: "Create", exact: true }).click();
  let dialog = page.getByRole("dialog", { name: "Create a storage pool" });
  await dialog.getByLabel("Name").fill("local");
  await dialog.getByLabel("Directory").fill(poolPath);
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog).toBeHidden();
  // 풀 이름 칸에는 즐겨찾기 ★도 있다.
  await expect(page.getByRole("button", { name: "Favorite local", exact: true })).toBeVisible();

  await openTab(page, "Infrastructure", "Volumes");
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

  await openTab(page, "Virtual machines", "VMs");
  await page.getByRole("link", { name: "Create", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Create a virtual machine" })).toBeVisible();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await page.getByLabel("Memory (MiB)", { exact: true }).fill("256");
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
  await expect(before.getByText("Current", { exact: true })).toBeVisible({ timeout: 60_000 });
  await expect(before.getByRole("cell", { name: "Included", exact: true })).toBeVisible();

  // 되돌리면 VM을 껐다가 저장한 메모리에서 다시 실행한다. Task가 끝날 때는 다시 실행 중이다.
  await page.getByRole("button", { name: "Revert to before" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Revert" }).click();
  await expect(page.getByText("Revert web to before: done")).toBeVisible({ timeout: 60_000 });
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
  await openTab(page, "Virtual machines", "VMs");
  await page.getByRole("link", { name: "copy", exact: true }).click();
  await expect(page.getByRole("heading", { name: "copy" })).toBeVisible();
  await expect(page.getByTestId("vm-phase")).toHaveText("Stopped");
  await page.getByRole("tab", { name: "Hardware" }).click();
  await expect(page.getByRole("cell", { name: "copy-root", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByTestId("vm-phase")).toHaveText("Running", { timeout: 60_000 });

  // 목록에서 골라 일괄로 끈다(설계 §5.3). 강제 정지는 고른 수를 입력해야 하고, VM마다 Task 결과를 보인다.
  await openTab(page, "Virtual machines", "VMs");
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
  await openTab(page, "Infrastructure", "Volumes");
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
  await dialog.getByLabel("Name").fill("off");
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
  await page.getByLabel("Take snapshots").selectOption("hourly");
  await page.getByLabel("Snapshots to keep").fill("2");
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
    await page.getByLabel("Take snapshots").selectOption("off");
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
  await openTab(page, "Infrastructure", "Volumes");
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

  await openTab(page, "Infrastructure", "Storage pools");
  await page.getByRole("button", { name: "Delete local" }).click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("No storage pools.")).toBeVisible();

  // 계획 63: 노드 상세의 CPU 모델(노드가 자기 QEMU에 물어 알린 것)과 설정 › 일반의 클러스터 CPU 기준
  await page.goto("/nodes/node1");
  const cpu = page.getByRole("region", { name: "CPU models" });
  await expect(cpu.getByText("QEMU", { exact: true })).toBeVisible();
  await expect(cpu.getByRole("button", { name: "Re-read CPU models" })).toBeEnabled();
  // 계획 64: 노드 상세의 NUMA 토폴로지(Linux 노드는 탐침 결과, macOS는 없다고 말한다)
  const topology = page.getByRole("region", { name: "NUMA topology" });
  await expect(
    topology.getByText(/NUMA nodes ·|has not reported a NUMA topology/).first(),
  ).toBeVisible();
  await page.goto("/settings");
  await expect(page.getByLabel("Cluster CPU baseline")).toHaveValue("off");

  // 로그아웃하면 보호된 화면은 로그인으로 보낸다.
  await page.getByRole("button", { name: /^Account/ }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/login/);
  await page.goto("/vms");
  await expect(page).toHaveURL(/\/login\?next=%2Fvms/);
});
