import { useEffect, useRef } from "react";
import { useSearchParams } from "react-router";

/**
 * `?create=1`로 들어오면 그 화면의 만들기 대화상자를 연다(설계 §5.1 "+ 만들기"·첫 걸음). 쿼리는 지워서 뒤로 가기나
 * 새로 고침이 다시 열지 않게 한다. 권한이 없으면 열지 않는다. `ready`가 거짓이면(대화상자에 필요한 목록을 읽는 중) 기다린다.
 */
export function useCreateParam(open: () => void, allowed: boolean, ready = true) {
  const [params, setParams] = useSearchParams();
  const wanted = params.get("create") === "1";
  const latest = useRef(open);
  useEffect(() => {
    latest.current = open;
  });
  useEffect(() => {
    if (!wanted || !ready) {
      return;
    }
    setParams(
      (p) => {
        p.delete("create");
        return p;
      },
      { replace: true },
    );
    if (allowed) {
      latest.current();
    }
  }, [wanted, allowed, ready, setParams]);
}
