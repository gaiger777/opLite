import { lazy, Suspense, useState } from "react";
import { useTranslation } from "react-i18next";

import type { Kind } from "@/auth/permissions";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

/** ⋯ 메뉴의 설정 기록·YAML 내보내기 대상 */
export interface ConfigTarget {
  kind: Kind;
  id: string;
  name: string;
}

export type ConfigPanelKind = "history" | "yaml";

// 서랍·대화상자와 그 문구는 늦게 받는다 — 이 파일은 메뉴 두 줄만 그린다(설계 §13 웹 "⋯ 메뉴 두 줄뿐").
const Panel = lazy(async () => {
  const m = await import("@/declarative/configPanel");
  await m.loadDeclarativeStrings();
  return { default: m.ConfigPanel };
});

/**
 * 리소스 ⋯ 메뉴의 두 줄(계획 73 §13 웹): "설정 기록…"(그 리소스의 이력 서랍)과 "YAML로 내보내기"(`GET /manifests/{kind}/{key}`).
 * 메뉴가 닫혀도 서랍이 남도록 열린 것은 메뉴 밖(`host`)에 그린다. 읽을 수 있는 리소스라 권한을 따로 보지 않는다(서버가
 * 이력·매니페스트를 읽기 권한으로 거른다).
 */
export function useConfigMenu(target: ConfigTarget | null | undefined) {
  const { t } = useTranslation();
  const [open, setOpen] = useState<ConfigPanelKind | null>(null);
  const items = target ? (
    <>
      <DropdownMenuSeparator />
      <DropdownMenuItem onSelect={() => setOpen("history")}>
        {t("configMenu.history")}
      </DropdownMenuItem>
      <DropdownMenuItem onSelect={() => setOpen("yaml")}>{t("configMenu.yaml")}</DropdownMenuItem>
    </>
  ) : null;
  // 서랍·대화상자는 포털로 그리지만 React 사건은 React 트리를 따라 오른다 — 그 안 폼(되돌리기 대화상자)의 제출이 이 메뉴를
  // 둔 화면의 폼(보안 그룹 규칙 편집)까지 올라가지 않게 여기서 멈춘다.
  const host =
    open && target ? (
      <div className="contents" onSubmit={(e) => e.stopPropagation()}>
        <Suspense fallback={null}>
          <Panel what={open} target={target} onClose={() => setOpen(null)} />
        </Suspense>
      </div>
    ) : null;
  return { items, host };
}

/** 상세 화면이 없는 종류(볼륨·네트워크·라우터·…)의 목록 줄 ⋯ 메뉴 */
export function RowConfigMenu({ target }: { target: ConfigTarget }) {
  const { t } = useTranslation();
  const { items, host } = useConfigMenu(target);
  const More = ACTION_ICONS.more;
  return (
    <>
      <DropdownMenu modal={false}>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={t("configMenu.more", { name: target.name })}
          >
            <More />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">{items}</DropdownMenuContent>
      </DropdownMenu>
      {host}
    </>
  );
}

/** Terraform이 관리하는 객체의 머리 칩(레이블 `orbivisor.io/managed-by=terraform`) */
export function TerraformChip({ labels }: { labels: Record<string, string> | undefined }) {
  const { t } = useTranslation();
  if (labels?.["orbivisor.io/managed-by"] !== "terraform") {
    return null;
  }
  return (
    <span className="rounded-md bg-info-soft px-2 py-0.5 text-xs font-medium text-info-fg">
      {t("configMenu.terraform")}
    </span>
  );
}
