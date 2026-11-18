import { useQuery } from "@tanstack/react-query";
import { Check, ChevronsUpDown } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Kbd } from "@/components/ui/kbd";
import { createItems } from "@/layout/create";
import { keyLabel } from "@/lib/shortcuts";
import { useRemotes } from "@/remotes/remotes";

/** 머리줄의 검색 상자 모양 단추. 팔레트를 연다(⌘K). */
export function SearchButton({ onOpen }: { onOpen(): void }) {
  const { t } = useTranslation();
  const Icon = ACTION_ICONS.search;
  return (
    <Button
      variant="outline"
      onClick={onOpen}
      aria-label={t("palette.open")}
      aria-keyshortcuts="Meta+K Control+K"
      className="text-muted-foreground lg:w-64 lg:justify-start"
    >
      <Icon />
      <span className="hidden lg:inline">{t("palette.placeholder")}</span>
      <Kbd className="ml-auto hidden lg:inline-flex">{keyLabel("mod+k")}</Kbd>
    </Button>
  );
}

/**
 * "+ 만들기" 메뉴. 쓰기 권한이 있는 것만 보이고, 하나도 없으면(viewer) 단추가 없다. `c` 단축키가 연다(`open`).
 */
export function CreateMenu({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange(v: boolean): void;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const navigate = useNavigate();
  const items = createItems(perms);
  if (items.length === 0) {
    return null;
  }
  const Icon = ACTION_ICONS.create;
  return (
    <DropdownMenu open={open} onOpenChange={onOpenChange}>
      <DropdownMenuTrigger asChild>
        <Button aria-label={t("create.menu")}>
          <Icon />
          <span className="hidden sm:inline">{t("common.create")}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {items.map((item) => (
          <DropdownMenuItem key={item.id} onSelect={() => void navigate(item.to)}>
            {t(`create.${item.id}`)}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * 클러스터 전환: 이 클러스터와 원격 클러스터들. 원격을 고르면 그 클러스터의 VM 보기(`/remotes/:name`)로 간다.
 * 원격 목록은 메뉴를 열 때만 읽는다.
 */
export function ClusterSwitch() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
  });
  const readRemotes = perms.can("read", "remote");
  const remotes = useRemotes(open && readRemotes);
  const name = cluster.data?.name ?? "";
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className="hidden max-w-48 lg:inline-flex"
          aria-label={t("cluster.switch", { name })}
        >
          <span className="truncate">{name}</span>
          <ChevronsUpDown className="opacity-60" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-56">
        <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
          {t("cluster.thisCluster")}
        </DropdownMenuLabel>
        <DropdownMenuItem onSelect={() => void navigate("/")}>
          <Check />
          <span className="truncate">{name}</span>
        </DropdownMenuItem>
        {readRemotes && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuLabel className="text-xs font-normal text-muted-foreground">
              {t("nav.remotes")}
            </DropdownMenuLabel>
            {remotes.isPending && (
              <DropdownMenuItem disabled>{t("common.loading")}</DropdownMenuItem>
            )}
            {remotes.data?.length === 0 && (
              <DropdownMenuItem disabled>{t("cluster.noRemotes")}</DropdownMenuItem>
            )}
            {remotes.data?.map((remote) => (
              <DropdownMenuItem
                key={remote.name}
                onSelect={() => void navigate(`/remotes/${encodeURIComponent(remote.name)}`)}
              >
                <span className="size-4" aria-hidden />
                <span className="truncate">{remote.name}</span>
              </DropdownMenuItem>
            ))}
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
