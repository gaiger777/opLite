import type { Page } from "@playwright/test";

/**
 * 왼쪽 2단 사이드바에서 묶음 › 항목을 연다(계획 77). 묶음이 접혀 있으면 먼저 펼친다. 이동 경로에도 같은 이름의 링크가
 * 있으므로 사이드바 안에서 찾는다. 브라우저 폭 1280px 이상(Playwright 기본)은 펼친 사이드바다.
 */
export async function openItem(page: Page, group: string, item: string) {
  const nav = page.getByRole("navigation", { name: "Main" });
  const head = nav.getByRole("button", { name: group, exact: true });
  if ((await head.getAttribute("aria-expanded")) !== "true") {
    await head.click();
  }
  // 숫자가 붙은 항목은 이름 뒤에 " (울리는 경보 n개)"가 붙는다 — 이름 앞부분으로 찾는다.
  const escaped = item.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  await nav.getByRole("link", { name: new RegExp(`^${escaped}( \\(.*\\))?$`) }).click();
}

/** 한 항목의 화면 안 탭(라우터 | 공인 IP 등) */
export async function openPageTab(page: Page, item: string, tab: string) {
  await page
    .getByRole("navigation", { name: `${item} pages` })
    .getByRole("link", { name: tab, exact: true })
    .click();
}
