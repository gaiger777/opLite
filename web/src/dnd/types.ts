// 끌어 놓기 틀의 타입(계획 71 설계 §6). 빌더(부품·블록)와 클러스터 지도(VM 타일)가 함께 쓴다.

/** 화면 좌표(`clientX`·`clientY`) */
export interface Point {
  x: number;
  y: number;
}

/** 번역 키와 값. 틀은 문구를 짜지 않고 쓰는 쪽의 키를 알림 줄에서 옮긴다. */
export interface Reason {
  key: string;
  values?: Record<string, string | number>;
}

/** 놓을 수 있는지: 놓을 수 있되 경고(용량 등)가 있거나, 놓을 수 없고 까닭이 있다(종류가 맞지 않으면 까닭 없음). */
export type Verdict = { ok: true; warn?: Reason } | { ok: false; reason?: Reason };

/** 끌 수 있는 것. `label`은 알림 줄과 그림자에 쓰는 이름이다. */
export interface Item {
  id: string;
  label: string;
}

/**
 * 틀의 상태: 누르는 중(문턱·길게 누르기를 기다림), 포인터로 끄는 중, 누르기·키보드로 든 채(`key`면 키보드). `again`은
 * 든 것을 다시 누른 것(떼면 취소), `armed`는 터치 길게 누르기가 찼음이다.
 */
export type DndState<T extends Item = Item> =
  | { phase: "idle" }
  | { phase: "pressing"; item: T; start: Point; touch: boolean; armed: boolean; again: boolean }
  | { phase: "dragging"; item: T; over: string | null }
  | { phase: "picked"; item: T; over: string | null; key: boolean };

export type DndEvent<T extends Item = Item> =
  | { type: "down"; item: T; point: Point; touch: boolean }
  | { type: "longpress" }
  | { type: "move"; point: Point; slot: string | null }
  | { type: "up"; slot: string | null }
  | { type: "pick"; item: T; key: boolean }
  | { type: "over"; slot: string | null }
  | { type: "drop"; slot: string }
  | { type: "cancel" };

/** 알림 줄 한 마디(`dnd.*` 키, 값의 `reason`·`warn`은 다시 옮길 키) */
export interface Say {
  key: string;
  values: Record<string, string | number | Reason | undefined>;
}

/** 상태 기계가 묻는 것: 놓기 규칙, 칸 이름, 칸 목록(차례대로) */
export interface Ctx<T extends Item = Item> {
  accepts(item: T, slot: string): Verdict;
  label(slot: string): string;
  slots(): string[];
}

/** 한 걸음의 결과: 다음 상태와 일어난 일(놓음·취소)과 읽을 말 */
export interface Step<T extends Item = Item> {
  state: DndState<T>;
  drop?: { item: T; slot: string };
  cancelled?: T;
  say?: Say;
}

/** 지도처럼 칸이 원소가 아닌 그림: 점 → 칸 id, 칸 목록과 이름 */
export interface Surface {
  find(p: Point): string | null;
  ids(): string[];
  label(id: string): string;
}
