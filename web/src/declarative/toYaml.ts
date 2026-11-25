// 매니페스트를 YAML로 쓰는 작은 쓰개(계획 73 §13 웹 "YAML로 내보내기"). 웹은 YAML을 읽지 않으므로 읽개는 없다. 쓴 글은
// CLI(`orbivisorctl apply -f`, serde-saphyr)가 같은 값으로 읽어야 한다 — 고정 파일 `__fixtures__/yaml.json`을 웹 시험이
// 쓰고 CLI 교차 시험(`crates/oplctl/src/web_cross.rs`)이 읽어 맞춰 본다. 꼴은 CLI `export`와 같게 문서마다 `---`로 시작한다.

/** 따옴표 없이 써도 문자열로 읽히는 글: 낱말·경로·이름(`web01`·`/prod/web`·`orbivisor.io/apply-set`·`x86-64-v2`) */
const PLAIN = /^[A-Za-z_/][A-Za-z0-9_./@+-]*(?: [A-Za-z0-9_./@+()-]+)*$/;
/** YAML 1.1·1.2가 문자열이 아닌 값으로 읽을 수 있는 낱말 */
const RESERVED = /^(?:true|false|yes|no|on|off|y|n|null|nan|inf)$/i;

function text(s: string): string {
  // JSON 문자열은 YAML 큰따옴표 문자열이기도 하다(같은 탈출 문자).
  return PLAIN.test(s) && !RESERVED.test(s) ? s : JSON.stringify(s);
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** 한 줄에 쓰는 값(스칼라·빈 목록·빈 객체). 줄을 나눠 쓸 값이면 null */
function inline(v: unknown): string | null {
  if (v === null || v === undefined) {
    return "null";
  }
  if (typeof v === "string") {
    return text(v);
  }
  if (typeof v === "number") {
    // JSON에 없는 값(NaN·무한)은 쓰지 않는다 — JSON.stringify처럼 null
    return Number.isFinite(v) ? String(v) : "null";
  }
  if (typeof v === "boolean") {
    return v ? "true" : "false";
  }
  if (Array.isArray(v)) {
    return v.length === 0 ? "[]" : null;
  }
  if (isObject(v)) {
    return Object.keys(v).length === 0 ? "{}" : null;
  }
  return JSON.stringify(String(v));
}

/** 값 하나를 줄들로(들여쓰기 없이 — 부르는 쪽이 붙인다) */
function block(v: unknown): string[] {
  const one = inline(v);
  if (one !== null) {
    return [one];
  }
  const out: string[] = [];
  if (Array.isArray(v)) {
    for (const item of v) {
      const lines = block(item);
      const nested = inline(item) === null;
      if (nested && Array.isArray(item)) {
        // 목록 안 목록: `-` 다음 줄에 들여 쓴다
        out.push("-", ...lines.map((l) => `  ${l}`));
      } else {
        out.push(`- ${lines[0]}`, ...lines.slice(1).map((l) => `  ${l}`));
      }
    }
    return out;
  }
  for (const [key, value] of Object.entries(v as Record<string, unknown>)) {
    // JSON처럼 undefined 칸은 쓰지 않는다
    if (value === undefined) {
      continue;
    }
    const one = inline(value);
    if (one !== null) {
      out.push(`${text(key)}: ${one}`);
    } else {
      out.push(`${text(key)}:`, ...block(value).map((l) => `  ${l}`));
    }
  }
  return out;
}

/** 문서들을 YAML로: 문서마다 `---` 줄로 시작한다. */
export function toYaml(documents: readonly unknown[]): string {
  return documents.map((doc) => `---\n${block(doc).join("\n")}\n`).join("");
}
