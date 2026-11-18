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

export function removePref(key: string) {
  try {
    window.localStorage.removeItem(`opl.${key}`);
  } catch {
    // 지우지 못해도 사용자별 키가 먼저 읽힌다.
  }
}

/**
 * 사용자별 설정 `{user}.{key}`(한 브라우저를 여럿이 써도 남의 검색·보기가 넘어가지 않게). 없으면 예전의 공용 키
 * `{key}`를 한 번 옮긴다: 이 사용자 키로 쓰고 공용 키는 지운다.
 */
export function readUserPref(user: string, key: string): string | null {
  const own = readPref(`${user}.${key}`);
  if (own !== null) {
    return own;
  }
  const old = readPref(key);
  if (old !== null) {
    writePref(`${user}.${key}`, old);
    removePref(key);
  }
  return old;
}

export function writeUserPref(user: string, key: string, value: string) {
  writePref(`${user}.${key}`, value);
}
