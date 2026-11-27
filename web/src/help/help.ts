// 도움말 패널(계획 77-4, 진단 §2.0)이 지금 보이는 주제. 칸 옆 ⓘ·화면 제목 옆 ⓘ·빈 상태의 "자세히"가 모두 이것을 바꾸고,
// 셸의 패널 하나(`HelpPanel`)가 읽는다. 대화상자 안에서도 열 수 있게 맥락 대신 모듈 상태로 둔다(`tasks/center.ts`와 같은 꼴).
import { useSyncExternalStore } from "react";

/**
 * 패널에 보일 주제. `topic`은 늦게 받는 문구 `helpInfo.<topic>`(화면.칸, 예: `pools.kind`)의 키, `label`은 패널 제목이
 * 없을 때 쓸 칸 이름이다. `line`은 칸의 한 줄 설명(패널 맨 위에 다시 보인다).
 */
export interface HelpTopic {
  topic: string;
  label: string;
  line?: string;
}

let current: HelpTopic | null = null;
/** 패널을 연 요소(닫으면 그리로 포커스를 돌려준다) */
let opener: HTMLElement | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of listeners) {
    listener();
  }
}

/** 도움말 패널을 연다(이미 열려 있으면 주제만 바꾼다). */
export function openHelp(topic: HelpTopic) {
  if (current === null) {
    opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  }
  current = topic;
  emit();
}

/** 도움말 패널을 닫는다. */
export function closeHelp() {
  if (current !== null) {
    current = null;
    emit();
  }
}

/** 패널을 연 요소를 한 번 돌려준다(닫힐 때 포커스를 옮기려고). */
export function takeHelpOpener(): HTMLElement | null {
  const element = opener;
  opener = null;
  return element;
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** 지금 보이는 도움말 주제(없으면 닫힘) */
export function useHelpTopic(): HelpTopic | null {
  return useSyncExternalStore(
    subscribe,
    () => current,
    () => null,
  );
}
