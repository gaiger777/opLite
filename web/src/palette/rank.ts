// 팔레트 순위(설계 §5.2). 정확 > 접두사 > 단어 접두사 > 부분 문자열이고, 같은 점수면 즐겨찾기·최근이 앞이다.
// 종류마다 8개까지 보인다. IP는 접두사로, CIDR(`10.0.0.0/24`)는 포함 검사로 찾는다.

export const EXACT = 4;
export const PREFIX = 3;
export const WORD = 2;
export const SUBSTRING = 1;

/** 그룹마다 보이는 수 */
export const GROUP_LIMIT = 8;

/** 낱말 경계: 공백·`-`·`_`·`.`·`/`·`:`·`›` 뒤 */
const WORD_START = /[\s\-_./:›]/;

/**
 * `query`가 `text`에 얼마나 맞는지(0은 맞지 않음). 대소문자를 가리지 않는다. 통째로 맞지 않는 여러 낱말 질의(`create
 * pool`)는 낱말마다 맞아야 하고, 가장 낮은 낱말 점수(많아야 단어 접두사)다.
 */
export function matchScore(query: string, text: string): number {
  const q = query.trim().toLowerCase();
  const s = text.toLowerCase();
  if (!q) {
    return 0;
  }
  const whole = wholeScore(q, s);
  const words = q.split(/\s+/);
  if (whole > 0 || words.length < 2) {
    return whole;
  }
  return Math.min(WORD, ...words.map((word) => wholeScore(word, s)));
}

function wholeScore(q: string, s: string): number {
  if (s === q) {
    return EXACT;
  }
  if (s.startsWith(q)) {
    return PREFIX;
  }
  let at = s.indexOf(q);
  if (at < 0) {
    return 0;
  }
  while (at >= 0) {
    if (WORD_START.test(s[at - 1] ?? "")) {
      return WORD;
    }
    at = s.indexOf(q, at + 1);
  }
  return SUBSTRING;
}

/** IPv4 점 주소 → 32비트 수. 아니면 null */
export function parseIpv4(text: string): number | null {
  const parts = text.split(".");
  if (parts.length !== 4) {
    return null;
  }
  let value = 0;
  for (const part of parts) {
    if (!/^\d{1,3}$/.test(part) || Number(part) > 255) {
      return null;
    }
    value = value * 256 + Number(part);
  }
  return value;
}

/** `10.0.0.0/24` 꼴의 IPv4 CIDR. 아니면 null */
export function parseCidr(text: string): { base: number; bits: number } | null {
  const m = /^([\d.]+)\/(\d{1,2})$/.exec(text.trim());
  if (!m) {
    return null;
  }
  const base = parseIpv4(m[1]!);
  const bits = Number(m[2]);
  return base === null || bits > 32 ? null : { base, bits };
}

/** IPv4 주소(뒤 `/접두 길이`는 무시)가 CIDR 안에 있다. */
export function inCidr(ip: string, cidr: string): boolean {
  const range = parseCidr(cidr);
  const addr = parseIpv4(ip.split("/")[0] ?? "");
  if (!range || addr === null) {
    return false;
  }
  const size = 2 ** (32 - range.bits);
  return Math.floor(addr / size) === Math.floor(range.base / size);
}

/** 순위를 매길 항목. `group`마다 따로 자른다. */
export interface Candidate {
  id: string;
  group: string;
  label: string;
  /** 이름 말고 찾을 글(노드 이름 따위) */
  keywords?: readonly string[];
  /** 찾을 IP·MAC(`10.0.0.5`, `10.0.0.5/24`, `52:54:00:…`) */
  addresses?: readonly string[];
  pinned?: boolean;
  /** 최근 항목에서의 자리(작을수록 최근) */
  recent?: number;
}

/** 한 항목의 점수. 이름·낱말·주소 가운데 가장 높은 것이고, CIDR 질의는 주소가 그 안에 있으면 부분 문자열 점수다. */
export function candidateScore(query: string, item: Candidate): number {
  const q = query.trim();
  let best = matchScore(q, item.label);
  for (const word of item.keywords ?? []) {
    best = Math.max(best, Math.min(matchScore(q, word), WORD));
  }
  const cidr = parseCidr(q) !== null;
  for (const address of item.addresses ?? []) {
    const score = cidr ? (inCidr(address, q) ? SUBSTRING : 0) : matchScore(q, address);
    // 주소의 부분 문자열(`0.0.1`)은 우연히 맞기 쉬우므로 접두사부터 받는다.
    best = Math.max(best, score >= PREFIX || cidr ? score : 0);
  }
  return best;
}

/**
 * 맞는 항목을 점수 → 즐겨찾기 → 최근 → 원래 순서로 늘어놓고 그룹마다 `limit`개로 자른다. 빈 질의면 거르지 않고
 * 즐겨찾기·최근을 앞에 둔다.
 */
export function rank<T extends Candidate>(items: readonly T[], query: string, limit = GROUP_LIMIT) {
  const q = query.trim();
  const scored = items
    .map((item, index) => ({ item, index, score: q ? candidateScore(q, item) : 0 }))
    .filter((s) => !q || s.score > 0);
  scored.sort(
    (a, b) =>
      b.score - a.score ||
      Number(b.item.pinned ?? false) - Number(a.item.pinned ?? false) ||
      (a.item.recent ?? Infinity) - (b.item.recent ?? Infinity) ||
      a.index - b.index,
  );
  const counts = new Map<string, number>();
  const out: T[] = [];
  for (const { item } of scored) {
    const n = counts.get(item.group) ?? 0;
    if (n < limit) {
      counts.set(item.group, n + 1);
      out.push(item);
    }
  }
  return out;
}
