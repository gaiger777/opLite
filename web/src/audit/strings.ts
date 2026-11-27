// 감사 로그의 동작 이름 표(계획 77-7): 라우트 225개의 사람 말이라 첫 화면 묶음에 넣지 않고 감사 화면을 열 때 두 언어를 함께
// 더한다(맨 위 키 `auditOps` 하나라 얕게 더한다 — `vmware/strings.ts`와 같은 규약). 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

export async function loadAuditStrings(): Promise<void> {
  if (i18n.exists("auditOps.route.POST /vms", { lng: "en" })) {
    return;
  }
  const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}
