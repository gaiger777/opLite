// 선언형 적용·설정 기록·인증서(계획 73 §13)의 문구: 첫 화면 묶음에 넣지 않고 그 보기(또는 ⋯ 메뉴의 서랍·대화상자)를 열 때 두
// 언어를 함께 더한다(맨 위 키 하나씩이라 얕게 더한다 — `vmware/strings.ts`와 같은 규약). 계획 표(`PlanTable`)는 선언 문구를,
// 되돌리기 대화상자는 설정 기록 문구를 쓰므로 설정 기록·선언 묶음은 둘을 함께 받는다. 시험은 `test/setup.ts`가 미리 더한다.
import i18n from "@/i18n";

async function add(
  probe: string,
  load: () => Promise<[{ default: object }, { default: object }]>,
): Promise<void> {
  if (i18n.exists(probe, { lng: "en" })) {
    return;
  }
  const [en, ko] = await load();
  i18n.addResourceBundle("en", "translation", en.default, false, false);
  i18n.addResourceBundle("ko", "translation", ko.default, false, false);
}

/** 설정 기록·선언 묶음 문구(`history.*`·`declarative.*`) */
export async function loadDeclarativeStrings(): Promise<void> {
  await Promise.all([
    add("history.pageHint", () =>
      Promise.all([import("@/history/i18n/en.json"), import("@/history/i18n/ko.json")]),
    ),
    add("declarative.sets.title", () =>
      Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]),
    ),
  ]);
}

/** 인증서 보기 문구(`certs.*`) */
export async function loadCertStrings(): Promise<void> {
  await add("certs.pageHint", () =>
    Promise.all([import("@/certs/i18n/en.json"), import("@/certs/i18n/ko.json")]),
  );
}
