// 도움말 패널·그림의 문구(계획 77-4): 첫 화면 묶음에 넣지 않고 패널이나 그림을 처음 그릴 때 두 언어를 함께 더한다
// (`dr/strings.ts`와 같은 규약 — 맨 위 키 `helpInfo` 하나라 얕게 더한다). 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

export async function loadHelpStrings(): Promise<void> {
  if (i18n.exists("helpInfo.panel.what", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
