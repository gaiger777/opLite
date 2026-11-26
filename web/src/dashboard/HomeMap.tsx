// 홈의 인프라 지도 칸(계획 75 §4.1): Infra › 지도와 같은 컴포넌트(`clusterMap/InfraMap`, 5단계)를 "홈 모드"로 늦게 읽는다.
// 이 가지에는 아직 그 파일이 없어 glob으로 찾고, 없으면 지도 화면으로 가는 한 줄을 보인다 — 5단계와 합친 뒤에는
// `lazy(() => import("@/clusterMap/InfraMap"))` 한 줄로 바꾼다. 시험은 이 모듈을 `vi.mock`으로 갈음한다.
import { type ComponentType, lazy, Suspense } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import type { MapSelection } from "@/dashboard/selection";

export interface InfraMapProps {
  variant: "home" | "page";
  selected?: MapSelection;
  onSelect?: (s: MapSelection | null) => void;
  focus?: string;
}

const found = import.meta.glob<{ default: ComponentType<InfraMapProps> }>(
  "../clusterMap/InfraMap.tsx",
);
const load = found["../clusterMap/InfraMap.tsx"];
const InfraMap = load ? lazy(load) : null;

export function HomeMap(props: Omit<InfraMapProps, "variant">) {
  const { t } = useTranslation();
  if (!InfraMap) {
    return (
      <p className="text-sm text-muted-foreground">
        <Link to="/cluster/map" className="underline underline-offset-4">
          {t("physical.map.open")}
        </Link>
      </p>
    );
  }
  return (
    <Suspense fallback={<p className="text-sm text-muted-foreground">{t("common.loading")}</p>}>
      <InfraMap variant="home" {...props} />
    </Suspense>
  );
}
