/** 왼쪽 Shift의 keysym */
export const SHIFT = 0xffe1;

/** US 배열에서 Shift를 눌러야 나오는 기호 */
const SHIFTED = new Set('~!@#$%^&*()_+{}|:"<>?');

/** `sendKey`에 넘길 인자: keysym, 물리 키 코드, 누름(없으면 눌렀다 뗀다) */
export type KeyEvent = [keysym: number, code: string | null, down?: boolean];

/**
 * 글자를 X11 keysym으로. Latin-1은 코드 포인트 그대로, 나머지 유니코드는 0x01000000을 더한다.
 * 보낼 수 없는 제어 문자는 null이다.
 */
export function keysym(char: string): number | null {
  switch (char) {
    case "\r":
    case "\n":
      return 0xff0d; // Return
    case "\t":
      return 0xff09; // Tab
    case "\b":
      return 0xff08; // BackSpace
  }
  const code = char.codePointAt(0) ?? 0;
  if (code < 0x20 || code === 0x7f) {
    return null;
  }
  return code < 0x100 ? code : 0x01000000 + code;
}

/**
 * 문자열을 키 입력 순서로. 대문자와 Shift 기호는 Shift를 누른 채 보낸다(QEMU는 keysym만으로
 * Shift를 붙이지 않는다). `\r\n`은 Return 한 번이다.
 */
export function keyEvents(text: string): KeyEvent[] {
  const events: KeyEvent[] = [];
  let previous = "";
  for (const char of text) {
    const skip = char === "\n" && previous === "\r";
    previous = char;
    const sym = keysym(char);
    if (skip || sym === null) {
      continue;
    }
    if ((char >= "A" && char <= "Z") || SHIFTED.has(char)) {
      events.push([SHIFT, "ShiftLeft", true], [sym, null], [SHIFT, "ShiftLeft", false]);
    } else {
      events.push([sym, null]);
    }
  }
  return events;
}
