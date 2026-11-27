// 묶음의 "파일 값 ↔ 실제 값"(계획 77-7, 진단 §3.5): 묶음이 기억하는 선언(`AppliedObject.declared` — 매니페스트 꼴의
// metadata·spec·state에서 파일에 적은 칸만)과 지금 리소스의 매니페스트(`GET /manifests?apply_set=`)를 칸마다 견준다. 서버
// 드리프트처럼 파일에 적은 칸만 보고(객체는 칸마다, 목록·값은 통째), 비밀은 견주지 않는다. 순수 함수.
import type { AppliedObject, Manifest } from "@/declarative/api";
import { secretMark } from "@/history/diff";

export interface CompareRow {
  /** 칸 경로(spec 칸은 `memory_mib`·`cpu.cores`, 레이블은 `labels.<열쇠>`, 그 밖은 `folder`·`state`) */
  path: string;
  file: unknown;
  live: unknown;
  /** 비밀 칸(선언에 값이 없다 — 견주지 않는다) */
  secret: boolean;
  differs: boolean;
}

/** 저장 꼴 종류(`storage-pool`) → 매니페스트 종류(`StoragePool`) */
export function manifestKind(kind: string): string {
  return kind
    .split("-")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join("");
}

/** 객체 칸 이름 차례를 무시한 같음(없음과 null은 같다) */
export function sameValue(a: unknown, b: unknown): boolean {
  if (a === undefined || a === null) {
    return b === undefined || b === null;
  }
  return stable(a) === stable(b);
}

function stable(v: unknown): string {
  if (Array.isArray(v)) {
    return `[${v.map(stable).join(",")}]`;
  }
  if (typeof v === "object" && v !== null) {
    const o = v as Record<string, unknown>;
    return `{${Object.keys(o)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stable(o[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(v);
}

/**
 * 파일에 적은 것이 실제에 그대로 있는지: 객체는 파일에 적은 칸만(서버가 채운 칸 — NIC의 MAC, 디스크의 `boot_order: null` —
 * 은 보지 않는다), 목록은 길이가 같고 차례대로, 값은 같음. 서버 드리프트가 서버 칸을 지키고 견주는 것과 같은 뜻이다.
 */
export function covers(file: unknown, live: unknown): boolean {
  if (Array.isArray(file)) {
    return (
      Array.isArray(live) && live.length === file.length && file.every((f, i) => covers(f, live[i]))
    );
  }
  if (isPlainObject(file)) {
    const l = isPlainObject(live) ? live : {};
    return Object.keys(file).every((k) => covers(file[k], l[k]));
  }
  return sameValue(file, live);
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function walk(path: string, file: unknown, live: unknown, out: CompareRow[]) {
  if (secretMark(file) !== null) {
    out.push({ path, file, live, secret: true, differs: false });
    return;
  }
  if (isPlainObject(file) && Object.keys(file).length > 0) {
    const sub = isPlainObject(live) ? live : {};
    for (const key of Object.keys(file).sort()) {
      walk(path ? `${path}.${key}` : key, file[key], sub[key], out);
    }
    return;
  }
  out.push({ path, file, live, secret: false, differs: !covers(file, live) });
}

/** 선언한 칸마다 파일 값·실제 값(실제 리소스가 없으면 `live`는 모두 없음) */
export function compareDeclared(declared: unknown, live: Manifest | undefined): CompareRow[] {
  const d = isPlainObject(declared) ? declared : {};
  const meta = isPlainObject(d.metadata) ? d.metadata : {};
  const liveMeta = (live?.metadata ?? {}) as { folder?: string; labels?: Record<string, string> };
  const out: CompareRow[] = [];
  if (meta.folder !== undefined) {
    // 폴더가 없는 리소스는 뿌리(`/`)에 있다
    const liveFolder = live ? (liveMeta.folder ?? "/") : undefined;
    out.push({
      path: "folder",
      file: meta.folder,
      live: liveFolder,
      secret: false,
      differs: !sameValue(meta.folder, liveFolder),
    });
  }
  if (isPlainObject(meta.labels)) {
    for (const key of Object.keys(meta.labels).sort()) {
      const file = meta.labels[key];
      const value = liveMeta.labels?.[key];
      out.push({
        path: `labels.${key}`,
        file,
        live: value,
        secret: false,
        differs: !sameValue(file, value),
      });
    }
  }
  if (isPlainObject(d.spec)) {
    const spec = d.spec;
    const liveSpec = isPlainObject(live?.spec) ? live.spec : {};
    for (const key of Object.keys(spec).sort()) {
      walk(key, spec[key], liveSpec[key], out);
    }
  }
  if (d.state !== undefined) {
    out.push({
      path: "state",
      file: d.state,
      live: live?.state,
      secret: false,
      differs: !sameValue(d.state, live?.state),
    });
  }
  return out;
}

/** 지금 리소스들: 매니페스트와 같은 차례의 id(계획 77-7 서버 — 옛 서버는 없다) */
export interface LiveSet {
  documents: readonly Manifest[];
  ids?: readonly string[];
}

/**
 * 묶음 객체의 지금 매니페스트. 서버가 id를 주면 묶음이 기록한 id로 짝짓는다(밖에서 이름을 바꾸거나 둘이 이름을 맞바꿔도
 * 같은 리소스). 옛 서버는 종류·이름(`name` — 드리프트가 알린 지금 이름이 있으면 그것)으로, 폴더는 든 폴더까지 같은 것.
 */
export function liveOf(
  obj: Pick<AppliedObject, "id" | "kind" | "name" | "declared">,
  live: LiveSet,
  name = obj.name,
): Manifest | undefined {
  const { documents, ids } = live;
  if (ids && ids.length === documents.length) {
    const at = ids.indexOf(obj.id);
    return at < 0 ? undefined : documents[at];
  }
  const kind = manifestKind(obj.kind);
  const declaredFolder = (obj.declared as { metadata?: { folder?: string } } | undefined)?.metadata
    ?.folder;
  return documents.find(
    (m) =>
      m.kind === kind &&
      m.metadata.name === name &&
      (kind !== "Folder" || (m.metadata.folder ?? "/") === (declaredFolder ?? "/")),
  );
}
