// 설정 기록·계획의 칸 바뀜을 화면 글로(순수). 비밀 칸은 이력에 평문이 없고 표시(`{"$secret": "set|same|changed|cleared"}`)만
// 있다(계획 73 §7.2) — 그 표시는 번역한 글로 보인다. 계획의 비밀 칸은 서버가 `(changed)`·`(secret)` 글로 준다.

/** 비밀 표시(`$secret`)의 상태. 아니면 null */
export function secretMark(value: unknown): string | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  const keys = Object.keys(value);
  const mark = (value as Record<string, unknown>).$secret;
  return keys.length === 1 && typeof mark === "string" ? mark : null;
}

/** 값 안의 비밀 표시를 찾는다(전체 spec 펼침의 안내 — 비밀은 되돌리지 않는다) */
export function hasSecret(value: unknown): boolean {
  if (secretMark(value) !== null) {
    return true;
  }
  if (Array.isArray(value)) {
    return value.some(hasSecret);
  }
  if (typeof value === "object" && value !== null) {
    return Object.values(value).some(hasSecret);
  }
  return false;
}
