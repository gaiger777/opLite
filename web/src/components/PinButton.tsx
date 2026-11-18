import { cn } from "cn";
import { useEffect } from "react";
import { useTranslation } from "react-i18next";

import { usePrincipal } from "@/auth/AuthProvider";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { pushRecent, type ResourceRef, useRefs } from "@/lib/recent";

/**
 * 상세 머리의 즐겨찾기 ★(설계 §5.7). `f` 단축키가 `data-pin-toggle`인 단추를 누른다. 브라우저에만 둔다. 목록 행·미리
 * 보기의 ★는 `shortcut={false}`다(`f`가 첫 행을 누르지 않게).
 */
export function PinButton({
  resource,
  shortcut = true,
}: {
  resource: ResourceRef;
  shortcut?: boolean;
}) {
  const { t } = useTranslation();
  const me = usePrincipal();
  const { pins, toggle } = useRefs(me.user);
  const pinned = pins.some((p) => p.kind === resource.kind && p.id === resource.id);
  const Icon = ACTION_ICONS.favorite;
  return (
    <Button
      variant="ghost"
      size="icon-sm"
      data-pin-toggle={shortcut ? "" : undefined}
      aria-pressed={pinned}
      aria-label={t("favorites.toggle", { name: resource.name })}
      onClick={() => toggle({ kind: resource.kind, id: resource.id, name: resource.name })}
    >
      <Icon className={cn(pinned && "fill-current text-warning")} />
    </Button>
  );
}

/** 상세 화면을 열면 최근 항목에 넣는다. */
export function useVisit(resource: ResourceRef | null) {
  const me = usePrincipal();
  const kind = resource?.kind;
  const id = resource?.id;
  const name = resource?.name;
  useEffect(() => {
    if (kind && id && name) {
      pushRecent(me.user, { kind, id, name });
    }
  }, [me.user, kind, id, name]);
}
