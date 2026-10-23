import { QueryClient } from "@tanstack/react-query";

import { ApiError } from "@/api/client";

/** 읽기는 서버·네트워크 오류일 때만 두 번 더 시도한다. 쓰기는 다시 시도하지 않는다. */
export function createQueryClient() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: {
        retry: (failures, error) =>
          failures < 2 && !(error instanceof ApiError && error.status < 500),
        refetchOnWindowFocus: false,
      },
      mutations: { retry: false },
    },
  });
  // 목록(`["list", kind]`)은 watch만 바꾼다(`LiveProvider`). 보는 화면이 없어도 캐시에서 지우지 않는다.
  // 지우면 그동안의 이벤트를 놓치고, 나중에 읽은 오래된 목록이 더 새 이벤트를 덮을 수 있다.
  // 같은 까닭으로 목록 키는 무효화(invalidate)하지 않는다. 다시 읽기는 LiveProvider만 한다.
  queryClient.setQueryDefaults(["list"], { gcTime: Infinity });
  return queryClient;
}
