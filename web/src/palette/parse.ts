// 팔레트 동작 문법(설계 §5.2): `{동사} {대상} [to {노드}]`와 한국어 어순 `{대상}[을] [{노드}로] {동사}`.
// 대상과 노드는 이름 접두사이고 찾기는 부르는 쪽이 한다. 자연어 명령은 하지 않는다(§2).

export type Verb =
  "start" | "stop" | "reboot" | "force-stop" | "migrate" | "snapshot" | "console" | "open";

/** 동사의 별칭(소문자). 두 언어를 모두 받는다. */
export const VERB_ALIASES: Record<Verb, readonly string[]> = {
  start: ["start", "boot", "power-on", "시작", "켜기", "켜", "켜줘", "기동"],
  stop: ["stop", "shutdown", "정지", "끄기", "꺼", "꺼줘", "중지", "종료"],
  reboot: ["reboot", "restart", "재시작", "다시시작", "재부팅"],
  "force-stop": ["force-stop", "forcestop", "kill", "강제정지", "강제종료", "강제끄기"],
  migrate: ["migrate", "move", "옮기기", "옮겨", "옮겨줘", "이동", "마이그레이션"],
  snapshot: ["snapshot", "snap", "스냅샷", "스냅숏"],
  console: ["console", "콘솔"],
  open: ["open", "go", "열기", "열어", "보기"],
};

export interface Command {
  verb: Verb;
  /** 대상 VM 이름(접두사) */
  target: string;
  /** 옮겨 갈 노드 이름(접두사) */
  node?: string;
}

/** 두 낱말로 쓰는 별칭을 한 낱말로 */
const PHRASES: [RegExp, string][] = [
  [/\bforce\s+stop\b/g, "force-stop"],
  [/\bpower\s+on\b/g, "power-on"],
  [/강제\s+(정지|종료|끄기)/g, "강제$1"],
  [/다시\s+시작/g, "다시시작"],
];

function verbOf(word: string): Verb | null {
  for (const [verb, aliases] of Object.entries(VERB_ALIASES) as [Verb, readonly string[]][]) {
    if (aliases.includes(word)) {
      return verb;
    }
  }
  return null;
}

/** 한국어 조사를 뗀다: 대상의 `을`·`를`, 노드의 `으로`·`로`·`에`. 떼고 남는 것이 없으면 그대로 둔다. */
function strip(word: string, particles: readonly string[]): string {
  for (const p of particles) {
    if (word.length > p.length && word.endsWith(p)) {
      return word.slice(0, -p.length);
    }
  }
  return word;
}

const OBJECT = ["을", "를"];
const TOWARD = ["으로", "로", "에"];
const TO = new Set(["to", "->", "→", "onto"]);

/** 동작 명령이면 풀고, 아니면(동사가 앞이나 끝에 없거나 대상이 없으면) null */
export function parseCommand(input: string): Command | null {
  let text = input.trim().toLowerCase();
  for (const [pattern, word] of PHRASES) {
    text = text.replace(pattern, word);
  }
  const words = text.split(/\s+/).filter(Boolean);
  if (words.length < 2) {
    return null;
  }
  const first = verbOf(words[0]!);
  const last = verbOf(words[words.length - 1]!);
  let verb: Verb;
  let rest: string[];
  if (first) {
    verb = first;
    rest = words.slice(1);
  } else if (last) {
    verb = last;
    rest = words.slice(0, -1);
  } else {
    return null;
  }
  const target = strip(rest[0]!, OBJECT);
  let node: string | undefined;
  const tail = rest.slice(1);
  if (tail.length > 0) {
    // `to node2`, `node2로`, `node2`
    const word = TO.has(tail[0]!) ? tail[1] : tail[0];
    if (word !== undefined) {
      node = strip(word, TOWARD);
    }
  }
  return node && verb === "migrate" ? { verb, target, node } : { verb, target };
}
