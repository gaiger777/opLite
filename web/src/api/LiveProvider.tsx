import { useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useState } from "react";

import { applyEvent, fetchList, KINDS, type List, listKey, type WatchEvent } from "@/api/live";

export type LiveStatus = "connecting" | "live" | "offline";

const LiveContext = createContext<LiveStatus>("connecting");

const FIRST_RETRY_MS = 1000;
const MAX_RETRY_MS = 30_000;

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
            queryClient.fetchQuery({ queryKey: listKey(kind), queryFn: () => fetchList(kind) }),
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
      ws.onopen = () => {
        delay = FIRST_RETRY_MS;
        setStatus("live");
      };
      ws.onmessage = (message) => {
        const event = JSON.parse(String(message.data)) as WatchEvent;
        if (event.type === "put" || event.type === "deleted") {
          queryClient.setQueryData<List<{ id: string; revision: number }>>(
            listKey(event.kind),
            (list) => list && applyEvent(list, event),
          );
        } else {
          // reset이나 오류면 이어 받을 수 없다. 닫히면 다시 읽는다.
          ws.close();
        }
      };
      ws.onclose = () => {
        if (socket === ws) {
          socket = null;
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
