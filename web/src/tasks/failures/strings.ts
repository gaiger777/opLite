// Task 실패 원인·해결 사전과 경보 원인·해결(계획 77-2): 첫 화면 묶음에 넣지 않고 실패한 Task나 경보를 처음 그릴 때 두 언어를
// 함께 더한다(`help/strings.ts`와 같은 규약 — 맨 위 키 `taskErrors`·`alertRunbook`이라 얕게 더한다). 시험은 `test/setup.ts`가
// 미리 더한다.
import { useQuery } from "@tanstack/react-query";

import i18n from "@/i18n";

export async function loadFailureStrings(): Promise<true> {
  if (!i18n.exists("taskErrors.task_timeout.title", { lng: "en" })) {
    const [en, ko] = await Promise.all([import("./i18n/en.json"), import("./i18n/ko.json")]);
    i18n.addResourceBundle("en", "translation", en.default, false, false);
    i18n.addResourceBundle("ko", "translation", ko.default, false, false);
  }
  return true;
}

/** 사전이 필요할 때(`needed`) 받고, 받았으면 true(받는 동안·실패면 false — 그때는 원문만 보인다) */
export function useFailureStrings(needed: boolean): boolean {
  const query = useQuery({
    queryKey: ["strings", "failures"],
    queryFn: loadFailureStrings,
    enabled: needed,
    staleTime: Infinity,
    retry: false,
  });
  return query.data === true || i18n.exists("taskErrors.task_timeout.title", { lng: "en" });
}
