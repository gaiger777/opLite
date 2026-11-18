import type { QueryClient } from "@tanstack/react-query";

import { applyEvent, type List, listKey } from "@/api/live";
import type { Resources, WatchKind } from "@/api/types";

/**
 * 서버 Task가 없는 쓰기(노드 레이블·유지보수 …)를 응답 전에 실시간 목록 캐시에 보인다(설계 §5.5).
 * 돌려주는 함수가 되돌린다. 그 사이 watch나 응답이 새 값(더 큰 revision)을 넣었으면 그대로 둔다.
 */
export function optimistic<K extends WatchKind>(
  queryClient: QueryClient,
  kind: K,
  id: string,
  change: (item: Resources[K]) => Resources[K],
): () => void {
  let before: Resources[K] | undefined;
  queryClient.setQueryData<List<Resources[K]>>(
    listKey(kind),
    (list) =>
      list && {
        ...list,
        items: list.items.map((item) => {
          if (item.id !== id) {
            return item;
          }
          before = item;
          return change(item);
        }),
      },
  );
  // 캐시는 구조를 나눠 쓰므로(structural sharing) 객체가 같은지가 아니라 revision으로 본다. 꾸민 값은 revision이
  // 그대로이고, 응답·watch가 넣은 값은 더 크다.
  return () =>
    queryClient.setQueryData<List<Resources[K]>>(
      listKey(kind),
      (list) =>
        list && {
          ...list,
          items: list.items.map((item) =>
            before && item.id === id && item.revision === before.revision ? before : item,
          ),
        },
    );
}

/** 쓰기 응답(새 revision)을 watch 이벤트보다 먼저 목록에 넣는다. */
export function putResource<K extends WatchKind>(
  queryClient: QueryClient,
  kind: K,
  resource: Resources[K],
) {
  queryClient.setQueryData<List<Resources[K]>>(
    listKey(kind),
    (list) => list && applyEvent(list, { type: "put", kind, resource }),
  );
}
