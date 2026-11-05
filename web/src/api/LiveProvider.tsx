import { useQueryClient } from "@tanstack/react-query";
import { createContext, useContext, useEffect, useState } from "react";

import {
  applyEvent,
  fetchList,
  KINDS,
  type List,
  listKey,
  SETTINGS_KEY,
  SETTINGS_KIND,
  SUBSCRIPTION_KEY,
  type WatchEvent,
} from "@/api/live";
import type { Settings } from "@/api/types";

export type LiveStatus = "connecting" | "live" | "offline";

const LiveContext = createContext<LiveStatus>("connecting");

const FIRST_RETRY_MS = 1000;
const MAX_RETRY_MS = 30_000;
/** 이만큼 이어져 있다 끊기면 다시 1초부터 기다린다. 곧바로 끊기면(lagged 등) 계속 늘린다. */
const STABLE_MS = 30_000;

function watchUrl(since: number) {
  const scheme = window.location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${window.location.host}/api/v1/watch?kinds=${[...KINDS, SETTINGS_KIND].join(",")}&since=${since}`;
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
        // 설정과 구독 상태는 목록이 없으므로 watch를 열기 전에 다시 읽는다(보는 화면이 있을 때만).
        await queryClient.invalidateQueries({ queryKey: SETTINGS_KEY });
        await queryClient.invalidateQueries({ queryKey: SUBSCRIPTION_KEY });
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
            // 설정은 지우지 않는다. 읽은 것보다 새것일 때만 바꾼다.
            if (event.kind === SETTINGS_KIND) {
              const resource = event.resource as Settings;
              if (event.type === "put") {
                queryClient.setQueryData<Settings>(SETTINGS_KEY, (current) =>
                  current && current.revision >= resource.revision ? current : resource,
                );
                // 구독 키가 바뀌었을 수 있다(이벤트에는 키가 없으므로 상태를 다시 읽는다).
                void queryClient.invalidateQueries({ queryKey: SUBSCRIPTION_KEY });
              }
              break;
            }
            // 노드가 합류하거나 떠나면 구독의 노드 수가 바뀐다.
            if (
              event.kind === "node" &&
              (event.type === "deleted" ||
                !queryClient
                  .getQueryData<List<{ id: string }>>(listKey("node"))
                  ?.items.some((node) => node.id === event.resource.id))
            ) {
              void queryClient.invalidateQueries({ queryKey: SUBSCRIPTION_KEY });
            }
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
