import type { ComponentProps, ReactNode, Ref } from "react";
import { useTranslation } from "react-i18next";

import { ACTION_ICONS } from "@/components/icons";
import { PinButton } from "@/components/PinButton";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import type { ResourceRef } from "@/lib/recent";

/** 요약 카드 하나: 작은 이름과 큰 값 */
export interface SummaryItem {
  label: string;
  value: ReactNode;
}

/**
 * 상세 화면의 머리(설계 §5.3): 이름(h1), 상태 칩, 즐겨찾기 ★, 주 동작 몇 개, 케밥(나머지), 그 아래 요약 카드 줄.
 * 이동 경로·영역 탭은 셸이 그린다. `menu`는 `DropdownMenuItem`들이고, 케밥 단추의 이름은 "{name}의 동작"이다.
 */
export function ResourceHeader({
  name,
  title = name,
  status,
  pin,
  meta,
  actions,
  menu,
  menuRef,
  menuTrigger,
  summary,
  children,
}: {
  name: string;
  /** h1 글(기본은 이름) */
  title?: ReactNode;
  status?: ReactNode;
  /** 즐겨찾기를 둘 수 있는 리소스면 ★ */
  pin?: ResourceRef;
  /** 이름 옆 작은 글(주소 …) */
  meta?: ReactNode;
  actions?: ReactNode;
  menu?: ReactNode;
  /** 케밥 단추. 메뉴에서 연 대화상자가 닫히면 여기로 포커스를 돌린다. */
  menuRef?: Ref<HTMLButtonElement>;
  /** 케밥 단추에 더할 속성(연 단추 기억하기 …) */
  menuTrigger?: ComponentProps<"button">;
  summary?: SummaryItem[];
  /** 동작 아래 줄(막힌 까닭 …) */
  children?: ReactNode;
}) {
  const { t } = useTranslation();
  const More = ACTION_ICONS.more;
  return (
    <div className="flex flex-col gap-(--gap)">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1">
          <h1 className="text-xl font-semibold break-all">{title}</h1>
          {pin && <PinButton resource={pin} />}
          {status}
          {/* 긴 폴더 경로가 휴대폰에서 가로로 넘치지 않게 줄을 바꾼다(리뷰 2). */}
          {meta && <span className="min-w-0 text-sm break-all text-muted-foreground">{meta}</span>}
        </div>
        {(actions || menu) && (
          <div className="flex flex-wrap items-center gap-2">
            {actions}
            {menu && (
              <DropdownMenu modal={false}>
                <DropdownMenuTrigger asChild>
                  <Button
                    {...menuTrigger}
                    ref={menuRef}
                    variant="outline"
                    size="icon-sm"
                    aria-label={t("vms.actionsFor", { name })}
                  >
                    <More />
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">{menu}</DropdownMenuContent>
              </DropdownMenu>
            )}
          </div>
        )}
        {children}
      </div>
      {summary && summary.length > 0 && <SummaryCards items={summary} />}
    </div>
  );
}

/** 요약 카드 줄. 휴대폰은 두 칸, 넓으면 네 칸이다. */
export function SummaryCards({ items }: { items: SummaryItem[] }) {
  return (
    <dl className="grid grid-cols-2 gap-(--gap) lg:grid-cols-4">
      {items.map((item) => (
        <div key={item.label} className="min-w-0 rounded-lg border bg-surface-2 px-4 py-3">
          <dt className="text-xs text-muted-foreground">{item.label}</dt>
          <dd className="mt-1 truncate text-base font-medium tabular-nums">{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}
