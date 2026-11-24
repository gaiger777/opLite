// 지도의 문구(계획 70 §12): 첫 화면 묶음에 넣지 않고 지도 경로를 열 때 두 언어를 함께 받아 i18n에 더한다(언어를 바꿔도
// 바로 보이게). 키는 `clusterMap.*` 그대로다 — 시험은 `test/setup.ts`가 미리 더한다. 얕게 더한다(깊게 합치면 첫 화면
// 묶음의 문구 객체를 고친다 — 맨 위 키 `clusterMap` 하나라 같은 결과).
import i18n from "@/i18n";

export async function loadMapStrings(): Promise<void> {
  if (i18n.exists("clusterMap.pictureLabel", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
