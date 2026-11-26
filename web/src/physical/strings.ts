// 운영자 대시보드의 문구(계획 75): 첫 화면 묶음에 넣지 않고 홈의 대시보드 칸을 읽을 때 두 언어를 함께 받아 i18n에 더한다(지도
// `clusterMap/strings.ts`와 같은 방식). 키는 `physical.*` — 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

export async function loadPhysicalStrings(): Promise<void> {
  if (i18n.exists("physical.table.title", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
