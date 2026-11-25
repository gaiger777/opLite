// VM 내보내기(계획 72 §12·§13)의 문구: 첫 화면 묶음에 넣지 않고 내보내기 보기나 VM 메뉴의 내보내기 대화상자를 열 때 두 언어를
// 함께 더한다(맨 위 키 `vmExports` 하나라 얕게 더한다 — `vmware/strings.ts`와 같은 규약). 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

export async function loadExportStrings(): Promise<void> {
  if (i18n.exists("vmExports.dialog.title", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
