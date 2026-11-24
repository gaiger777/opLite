// 서버 미리 확인(계획 71 설계 §8.4): 브라우저 오류가 없을 때만, 마지막 바뀜 0.6초 뒤 `POST /vm-checks`. 하나만 날고(앞
// 요청은 끊음), 답은 보낸 본문의 열쇠와 함께 두어 그 사이 값이 바뀌었으면 "지난 확인"(흐림)이다. 409
// `cluster_upgrading`·404 `no API at`(옛 노드 — 길이 없음)은 "업그레이드 뒤", 403은 "이 폴더에 만들 권한 없음", 그 밖(볼 수
// 없는 폴더의 404 등)은 "확인 못 함"과 서버의 까닭 — 어느 것도 만들기를 막지 않는다.
import { useCallback, useEffect, useState } from "react";

import { ApiError, api, call } from "@/api/client";
import type { VmCheck, VmCheckRequest } from "@/vmBuilder/model/requests";

/** 마지막 바뀜 뒤 기다리는 시간 */
export const CHECK_DELAY_MS = 600;

export type CheckStatus =
  "off" | "held" | "checking" | "done" | "upgrading" | "forbidden" | "failed";

export interface ServerCheck {
  status: CheckStatus;
  /** 마지막으로 받은 답(지금 값에 대한 것이 아닐 수 있음 — `stale`) */
  check: VmCheck | null;
  /** 받은 답이 지금 값에 대한 것이 아니다. */
  stale: boolean;
  /** 받은 때(ms) */
  at: number;
  /** "확인 못 함"의 까닭(서버 오류) */
  error: unknown;
  retry(): void;
}

/** 옛 노드가 모르는 길(`POST /vm-checks`가 없음)의 404 — CLI `--dry-run`과 같은 판정 */
export function noRoute(err: unknown): boolean {
  return (
    err instanceof ApiError &&
    err.status === 404 &&
    err.code === "not_found" &&
    err.message.startsWith("no API at")
  );
}

/**
 * `body`가 null이거나 `enabled`가 거짓이면 묻지 않는다(브라우저 오류·템플릿 바탕). `supported`가 거짓이면(어느 노드에
 * 기능 `vm-check`가 없음) 묻지 않고 "업그레이드 뒤"다. `held`면 묻지 않고 "기다림"이다(서버가 아직 지울 것을 들고 있음).
 */
export function useServerCheck(
  body: VmCheckRequest | null,
  enabled: boolean,
  supported = true,
  held = false,
): ServerCheck {
  const asks = body !== null && enabled;
  const key = asks && !held ? JSON.stringify(body) : null;
  const [result, setResult] = useState<{ check: VmCheck; key: string; at: number } | null>(null);
  // 묻고 받은 상태(묻지 않을 때는 아래에서 "off"·"upgrading")
  const [asked, setStatus] = useState<CheckStatus>("off");
  const [error, setError] = useState<unknown>(null);
  const [again, setAgain] = useState(0);

  useEffect(() => {
    if (key === null || !supported) {
      return;
    }
    const controller = new AbortController();
    const timer = setTimeout(() => {
      setStatus("checking");
      call(
        api.POST("/vm-checks", {
          body: JSON.parse(key) as VmCheckRequest,
          signal: controller.signal,
        }),
      )
        .then((check) => {
          setResult({ check, key, at: Date.now() });
          setStatus("done");
        })
        .catch((err: unknown) => {
          if (controller.signal.aborted) {
            return;
          }
          const status = err instanceof ApiError ? err.status : 0;
          const old =
            noRoute(err) || (status === 409 && (err as ApiError).code === "cluster_upgrading");
          setError(err);
          setStatus(old ? "upgrading" : status === 403 ? "forbidden" : "failed");
        });
    }, CHECK_DELAY_MS);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [key, supported, again]);

  const retry = useCallback(() => setAgain((n) => n + 1), []);
  return {
    status: !asks ? "off" : held ? "held" : supported ? asked : "upgrading",
    check: result?.check ?? null,
    stale: result !== null && result.key !== key,
    at: result?.at ?? 0,
    error,
    retry,
  };
}
