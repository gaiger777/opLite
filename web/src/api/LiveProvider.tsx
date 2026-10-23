import { useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useState } from "react";

import { applyEvent, fetchList, KINDS, type List, listKey, type WatchEvent } from "@/api/live";

export type LiveStatus = "connecting" | "live" | "offline";

const LiveContext = createContext<LiveStatus>("connecting");

const FIRST_RETRY_MS = 1000;
const MAX_RETRY_MS = 30_000;
/** 이만큼 이어져 있다 끊기면 다시 1초부터 기다린다. 곧바로 끊기면(lagged 등) 계속 늘린다. */
const STABLE_MS = 30_000;

function watchUrl(since: number) {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/v1/watch?kinds=${KINDS.join(",")}&since=${since}`;
}

/**
 * 로그인한 동안 목록을 모두 읽고, 그중 가장 작은 revision부터 watch를 하나 열어 변경을 캐시에 반영한다.
 * 이어 받을 수 없으면(오류, reset, 연결 끊김) 목록을 다시 읽고 새로 연다.
 */
export function LiveProvider({ children }: { children: React.ReactNode }) {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState<LiveStatus>("connecting");

  useEffect(() => {
    let socket: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let delay = FIRST_RETRY_MS;
    let stopped = false;

    const retry = () => {
      if (stopped) {
        return;
      }
      setStatus("offline");
      timer = setTimeout(connect, delay);
      delay = Math.min(delay * 2, MAX_RETRY_MS);
    };

    async function connect() {
      let since: number;
      try {
        const lists = await Promise.all(
          KINDS.map((kind) =>
            // 다시 시도는 여기의 backoff만 한다.
            queryClient.fetchQuery({
              queryKey: listKey(kind),
              queryFn: () => fetchList(kind),
              retry: false,
            }),
          ),
        );
        since = Math.min(...lists.map((list) => list.revision));
      } catch {
        retry();
        return;
      }
      if (stopped) {
        return;
      }
      const ws = new WebSocket(watchUrl(since));
      socket = ws;
      let openedAt: number | undefined;
      ws.onopen = () => {
        openedAt = Date.now();
        setStatus("live");
      };
      ws.onmessage = (message) => {
        let event: WatchEvent | null;
        try {
          event = JSON.parse(String(message.data)) as WatchEvent | null;
        } catch {
          // 읽을 수 없는 프레임이면 무엇을 놓쳤는지 모른다. 닫히면 다시 읽는다.
          ws.close();
          return;
        }
        switch (event?.type) {
          case "put":
          case "deleted":
            queryClient.setQueryData<List<{ id: string; revision: number }>>(
              listKey(event.kind),
              (list) => list && applyEvent(list, event),
            );
            break;
          case "reset":
          case "error":
            // 이어 받을 수 없다. 닫히면 다시 읽는다.
            ws.close();
            break;
          // 모르는 종류는 무시한다(서버가 새 종류를 더해도 끊지 않는다).
          default:
            break;
        }
      };
      ws.onclose = () => {
        if (socket === ws) {
          socket = null;
          if (openedAt !== undefined && Date.now() - openedAt > STABLE_MS) {
            delay = FIRST_RETRY_MS;
          }
          retry();
        }
      };
    }

    void connect();
    return () => {
      stopped = true;
      clearTimeout(timer);
      socket?.close();
      socket = null;
    };
  }, [queryClient]);

  return <LiveContext value={status}>{children}</LiveContext>;
}

/** 실시간 연결 상태 */
export function useLiveStatus(): LiveStatus {
  return useContext(LiveContext);
}
