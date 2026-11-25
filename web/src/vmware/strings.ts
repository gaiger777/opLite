// VMware 이전 보기(계획 72 §13)의 문구: 첫 화면 묶음에 넣지 않고 원본·계획 경로를 열 때 두 언어를 함께 더한다(계획 70
// `clusterMap/strings.ts`와 같은 규약 — 맨 위 키 `vmware` 하나라 얕게 더한다). 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

export async function loadVmwareStrings(): Promise<void> {
  if (i18n.exists("vmware.sources.title", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
