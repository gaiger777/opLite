// 홈의 인프라 지도 칸(계획 75 §4.1): Infra › 지도와 같은 컴포넌트(`clusterMap/InfraMap`, 5단계)를 "홈 모드"로 늦게 읽는다.
// 시험은 이 모듈을 `vi.mock`으로 갈음한다.
import { lazy, Suspense } from "react";
import { useTranslation } from "react-i18next";

import type { InfraMapProps } from "@/clusterMap/InfraMap";

const InfraMap = lazy(() => import("@/clusterMap/InfraMap"));

export function HomeMap(props: Omit<InfraMapProps, "variant">) {
  const { t } = useTranslation();
  return (
    <Suspense fallback={<p className="text-sm text-muted-foreground">{t("common.loading")}</p>}>
      <InfraMap variant="home" {...props} />
    </Suspense>
  );
}
