// 스테이지 5(전환·되돌리기·마무리)의 끝점. 이 화면을 쓴 때(스테이지 7) `schema.d.ts`에 아직 없어 설계(§10·§13)대로 손으로
// 적은 타입이다. 같은 클라이언트(`api` — 401·연결 오류 처리 그대로)를 타입만 넓혀 부른다. 스테이지 5가 합쳐진 뒤
// `gen:api`로 끝점이 생기면 이 파일을 지우고 `api.POST`를 바로 쓴다.
import type { Client } from "openapi-fetch";

import { api, call } from "@/api/client";
import type { components } from "@/api/schema";

type ItemAction = components["schemas"]["ItemAction"];
type ErrorBody = components["schemas"]["ErrorBody"];

type Responses = {
  200: { headers: { [name: string]: unknown }; content: { "application/json": ItemAction } };
  default: { headers: { [name: string]: unknown }; content: { "application/json": ErrorBody } };
};

type PathKey = { query?: never; header?: never; path: { key: string }; cookie?: never };

/** 본문 없는 동작(`cutover`·`finalize`) */
interface Bare {
  parameters: PathKey;
  requestBody?: never;
  responses: Responses;
}

/** 본문이 있는 동작(`revert`) */
interface WithBody<Body> {
  parameters: PathKey;
  requestBody: { content: { "application/json": Body } };
  responses: Responses;
}

type Post<Op> = {
  parameters: { query?: never; header?: never; path?: never; cookie?: never };
  get?: never;
  put?: never;
  post: Op;
  delete?: never;
  options?: never;
  head?: never;
  patch?: never;
  trace?: never;
};

/** 되돌리기 본문: 실수로 누르지 않게 대상 VM 이름을 그대로 적는다(설계 §10.3). */
export interface RevertRequest {
  confirm: string;
}

interface Stage5Paths {
  "/vmware-vms/{key}/cutover": Post<Bare>;
  "/vmware-vms/{key}/revert": Post<WithBody<RevertRequest>>;
  "/vmware-vms/{key}/finalize": Post<Bare>;
}

const api5 = api as unknown as Client<Stage5Paths>;

/** 전환(`ready`·`syncing`만 — 아니면 409 `not_ready`): 원본을 끄고 마지막 델타를 옮기고 VM을 만들어 켠다. */
export function cutoverItem(key: string): Promise<ItemAction> {
  return call(api5.POST("/vmware-vms/{key}/cutover", { params: { path: { key } } }));
}

/** 켠 뒤 되돌리기(`done`만): 대상 VM을 끄고 원본을 다시 켠다. 대상에서 쓴 데이터는 원본으로 가지 않는다. */
export function revertItem(key: string, confirm: string): Promise<ItemAction> {
  return call(
    api5.POST("/vmware-vms/{key}/revert", { params: { path: { key } }, body: { confirm } }),
  );
}

/** 마무리(`done`만): 원본의 우리 스냅샷·CBT를 정리한다. 그 뒤에는 되돌릴 수 없다. */
export function finalizeItem(key: string): Promise<ItemAction> {
  return call(api5.POST("/vmware-vms/{key}/finalize", { params: { path: { key } } }));
}
