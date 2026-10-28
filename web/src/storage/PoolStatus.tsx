import { useTranslation } from "react-i18next";

import type { PoolState } from "@/storage/pools";

/**
 * 한 노드에서 본 풀 상태: `이름: 준비됨`, 준비되지 않았으면 그 아래에 노드가 알린 이유. 꺼진 노드는
 * 알린 상태와 상관없이 오프라인이다.
 */
export function PoolStatus({
  name,
  state,
  offline = false,
}: {
  name: string;
  state: PoolState | undefined;
  offline?: boolean;
}) {
  const { t } = useTranslation();
  if (offline) {
    return (
      <div>
        {name}: <span className="text-muted-foreground">{t("pools.state.offline")}</span>
      </div>
    );
  }
  return (
    <div>
      {name}:{" "}
      {state === undefined ? (
        <span className="text-muted-foreground">{t("pools.state.unknown")}</span>
      ) : state.ready ? (
        t("pools.state.ready")
      ) : (
        <span className="text-destructive">{t("pools.state.notReady")}</span>
      )}
      {state?.error && <div className="text-xs text-muted-foreground">{state.error}</div>}
    </div>
  );
}
