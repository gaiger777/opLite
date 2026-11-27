// 만들기 화면 조각들이 함께 보는 것(계획 71 설계 §8, 계획 77-5): 지금 상태·세상·문제, 상태 바꾸기, 고른 칸(그림 위 카드).
import { createContext, useContext } from "react";

import type { Thing } from "@/vmBuilder/labels";
import type { Problem } from "@/vmBuilder/model/problems";
import type { VmCheck } from "@/vmBuilder/model/requests";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import type { VmFormValues } from "@/vms/form";

export interface BuilderApi {
  s: BuilderState;
  ctx: BuilderCtx;
  /** 보이는 브라우저 + (지금 값에 대한) 서버 문제(이름을 건드리기 전에는 이름 문제를 뺀다) */
  list: Problem[];
  /** 이름 칸을 떠났다(그 뒤로 이름 문제를 보인다). */
  touch(): void;
  /** 이름 문제를 보인다(이름 칸을 떠났거나 만들기를 눌렀다). */
  showName: boolean;
  /** 지금 값에 대한 서버 미리 확인의 답(없거나 지난 것이면 null) */
  check: VmCheck | null;
  /** 고급 설정 서랍을 연다. */
  openAdvanced(): void;
  /** 놓기·빼기·순서 바꾸기(되돌리기에 쌓음). 알림 줄 끝에 붙일 글을 돌려준다. */
  put(thing: Thing, slot: string): string | undefined;
  /** 살피기의 칸 고치기(쌓지 않음) */
  edit(patch: Partial<VmFormValues>): void;
  /** 상태 전체 바꾸기(쌓지 않음 — 이름, 바탕의 연결 복제·풀, 영역 hard 등) */
  set(next: BuilderState): void;
  /** 상태 전체 바꾸기(되돌리기에 쌓음 — "보통 크기 Linux 서버로 시작") */
  record(next: BuilderState): void;
  /** 템플릿 바탕에서 빈 VM으로(템플릿을 놓기 전 값, 되돌리기에 쌓음) */
  blank(): void;
  selected: string | null;
  select(id: string | null): void;
  /** 보내는 중(고치지 못함) */
  busy: boolean;
}

export const BuilderContext = createContext<BuilderApi | null>(null);

export function useBuilder(): BuilderApi {
  const b = useContext(BuilderContext);
  if (!b) {
    throw new Error("useBuilder outside BuilderPage");
  }
  return b;
}
