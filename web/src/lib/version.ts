/** 비숫자 부분 문자의 순서: `~` < 끝·숫자 < 문자 < 그 밖 */
function order(c: number | undefined): number {
  if (c === undefined) return 0;
  if (c === 0x7e) return -1; // ~
  if (isDigit(c)) return 0;
  if ((c >= 0x41 && c <= 0x5a) || (c >= 0x61 && c <= 0x7a)) return c;
  return c + 256;
}

function isDigit(c: number | undefined): boolean {
  return c !== undefined && c >= 0x30 && c <= 0x39;
}

/** 숫자 문자열을 수로 비교한다(앞 0 무시, 길이에 제한 없음). */
function compareNumber(a: string, b: string): number {
  const x = a.replace(/^0+/, "");
  const y = b.replace(/^0+/, "");
  if (x.length !== y.length) return x.length < y.length ? -1 : 1;
  return x < y ? -1 : x > y ? 1 : 0;
}

/** 업스트림·리비전을 비숫자 부분과 숫자 부분을 번갈아 비교한다(dpkg `verrevcmp`). */
function comparePart(a: Uint8Array, b: Uint8Array): number {
  let i = 0;
  let j = 0;
  const digits = (s: Uint8Array, from: number): [string, number] => {
    let end = from;
    while (isDigit(s[end])) end++;
    return [String.fromCharCode(...s.subarray(from, end)), end];
  };
  while (i < a.length || j < b.length) {
    while ((i < a.length && !isDigit(a[i])) || (j < b.length && !isDigit(b[j]))) {
      const x = order(a[i]);
      const y = order(b[j]);
      if (x !== y) return x < y ? -1 : 1;
      i++;
      j++;
    }
    const [x, ni] = digits(a, i);
    const [y, nj] = digits(b, j);
    const ord = compareNumber(x, y);
    if (ord !== 0) return ord;
    i = ni;
    j = nj;
  }
  return 0;
}

/** 에포크(숫자가 아니면 없는 것으로 본다), 업스트림, 리비전(마지막 `-` 뒤) */
function split(v: string): [string, string, string] {
  let epoch = "0";
  let rest = v;
  const colon = v.indexOf(":");
  if (colon > 0 && /^[0-9]+$/.test(v.slice(0, colon))) {
    epoch = v.slice(0, colon);
    rest = v.slice(colon + 1);
  }
  const dash = rest.lastIndexOf("-");
  return dash < 0 ? [epoch, rest, ""] : [epoch, rest.slice(0, dash), rest.slice(dash + 1)];
}

/**
 * 두 버전을 Debian 규칙으로 비교한다(`opl_types::version::compare`와 같다): `[에포크:]업스트림[-리비전]`,
 * `~`는 끝보다도 낮다. 형식에 맞지 않는 문자열도 같은 규칙으로 비교한다.
 */
export function compareVersions(a: string, b: string): number {
  const bytes = (s: string) => new TextEncoder().encode(s);
  const [ae, au, ar] = split(a);
  const [be, bu, br] = split(b);
  return (
    compareNumber(ae, be) || comparePart(bytes(au), bytes(bu)) || comparePart(bytes(ar), bytes(br))
  );
}
