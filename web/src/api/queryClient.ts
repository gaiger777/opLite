import { QueryClient } from "@tanstack/react-query";

import { ApiError } from "@/api/client";

/** 읽기는 서버·네트워크 오류일 때만 두 번 더 시도한다. 쓰기는 다시 시도하지 않는다. */
export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        retry: (failures, error) =>
          failures < 2 && !(error instanceof ApiError && error.status < 500),
        refetchOnWindowFocus: false,
      },
      mutations: { retry: false },
    },
  });
}
