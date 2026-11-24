// 끌어 놓기 틀의 문구(계획 71 설계 §12): 첫 화면 묶음에 넣지 않고 빌더·지도 경로를 열 때 두 언어를 함께 더한다(계획 70
// `clusterMap/strings.ts`와 같은 규약 — 맨 위 키 `dnd` 하나라 얕게 더한다). 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

export async function loadDndStrings(): Promise<void> {
  if (i18n.exists("dnd.role", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
