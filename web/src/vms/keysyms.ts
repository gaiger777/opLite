/** 글자를 X11 keysym으로. Latin-1은 코드 포인트 그대로, 나머지 유니코드는 0x01000000을 더한다. */
export function keysym(char: string): number {
  if (char === "\n") {
    return 0xff0d; // Return
  }
  if (char === "\t") {
    return 0xff09; // Tab
  }
  const code = char.codePointAt(0) ?? 0;
  return code < 0x100 ? code : 0x01000000 + code;
}
