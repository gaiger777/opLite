import { useTranslation } from "react-i18next";
import { isRouteErrorResponse, Link, useNavigation, useRouteError } from "react-router";
import { toast } from "sonner";

import { EmptyState } from "@/components/EmptyState";
import { ACTION_ICONS } from "@/components/icons";
import { PageHeader } from "@/components/PageHeader";
import { Button } from "@/components/ui/button";
import i18n from "@/i18n";

/**
 * 늦게 받는 화면 조각(`import()`)을 받지 못한 오류인지. 새로 배포해 옛 해시의 파일이 없어졌을 때 브라우저마다 다른
 * 글로 난다(Chrome·Firefox·Safari, Vite의 CSS 미리 받기).
 */
export function isChunkError(error: unknown): boolean {
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "";
  return /dynamically imported module|Importing a module script failed|Unable to preload CSS|error loading dynamically imported module/i.test(
    message,
  );
}

const reload = () => window.location.reload();

/**
 * 경로의 오류 화면(설계 §9). 화면 조각을 받지 못했으면 "새 버전이 있습니다"와 새로고침을, 다른 오류면 셸 안에서 짧은
 * 설명과 새로고침·홈 링크를 보인다(React Router의 기본 화면과 스택을 보이지 않는다).
 */
export function RouteError() {
  const { t } = useTranslation();
  const error = useRouteError();
  const Reload = ACTION_ICONS.refresh;
  const button = (
    <Button onClick={reload}>
      <Reload />
      {t("routeError.reload")}
    </Button>
  );
  if (isChunkError(error)) {
    return (
      <div className="flex flex-col gap-4">
        <PageHeader title={t("routeError.newVersion")} />
        <EmptyState
          icon={Reload}
          title={t("routeError.newVersion")}
          description={t("routeError.newVersionDescription")}
          action={button}
        />
      </div>
    );
  }
  const detail = isRouteErrorResponse(error)
    ? `${error.status} ${error.statusText}`
    : error instanceof Error
      ? error.message
      : undefined;
  return (
    <div className="flex flex-col gap-4">
      <PageHeader title={t("routeError.title")} description={detail} />
      <div className="flex flex-wrap items-center gap-3">
        {button}
        <Link to="/" className="text-sm underline">
          {t("notFound.home")}
        </Link>
      </div>
    </div>
  );
}

/**
 * Vite가 화면 조각을 미리 받지 못했다(`vite:preloadError`). 경로 밖의 늦은 조각도 있으므로 알림 하나로 새로고침을
 * 권한다(경로라면 `RouteError`도 같은 말을 한다). 같은 알림을 여러 번 띄우지 않는다.
 */
export function onPreloadError() {
  toast.warning(i18n.t("routeError.newVersion"), {
    id: "new-version",
    duration: Infinity,
    action: { label: i18n.t("routeError.reload"), onClick: reload },
  });
}

/** 늦게 받는 화면을 받는 동안 위쪽에 보이는 얇은 막대(사이드바를 눌러도 반응이 없어 보이지 않게) */
export function NavProgress() {
  const { t } = useTranslation();
  const navigation = useNavigation();
  if (navigation.state === "idle") {
    return null;
  }
  return (
    <div
      role="progressbar"
      aria-label={t("routeError.loading")}
      className="nav-progress pointer-events-none fixed inset-x-0 top-0 z-50 h-0.5 overflow-hidden"
    >
      <div className="h-full w-1/3 bg-brand" />
    </div>
  );
}
