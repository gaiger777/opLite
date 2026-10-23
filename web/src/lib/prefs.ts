// 브라우저에 남기는 개인 설정(언어, 테마, 표 정렬). 비밀값은 두지 않는다. 저장소를 쓸 수 없는 브라우저
// (사생활 보호 창 등)에서도 화면은 그대로 동작해야 하므로 실패는 무시한다.

export function readPref(key: string): string | null {
  try {
    return window.localStorage.getItem(`opl.${key}`);
  } catch {
    return null;
  }
}

export function writePref(key: string, value: string) {
  try {
    window.localStorage.setItem(`opl.${key}`, value);
  } catch {
    // 저장하지 못해도 이번 화면에는 반영됐다.
  }
}
