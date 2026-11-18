import { cn } from "cn";
import { useTranslation } from "react-i18next";

import { useAlerts } from "@/alerts/alerts";
import { useList } from "@/api/live";
import { ACTION_ICONS } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { isActive, isRinging, setCenterOpen, useCenterOpen } from "@/tasks/center";
import { NotificationCenter } from "@/tasks/NotificationCenter";

/**
 * 머리줄의 종(설계 §5.4): 도는 Task와 울리는 경보(critical·warning) 수를 보이고, 누르거나 `Shift+N`이면 오른쪽에
 * 작업·알림 센터를 연다. 숫자는 이미 받는 Task 목록과 `useAlerts()`(30초)에서 센다.
 */
export function NotificationBell() {
  const { t } = useTranslation();
  const open = useCenterOpen();
  const tasks = useList("task");
  const alerts = useAlerts();
  const running = tasks.data?.filter(isActive).length ?? 0;
  const ringing = alerts.data?.alerts.filter(isRinging).length ?? 0;
  const count = running + ringing;
  const Icon = ACTION_ICONS.notifications;
  return (
    <Sheet open={open} onOpenChange={setCenterOpen}>
      <SheetTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className="relative"
          aria-label={count > 0 ? t("center.openCount", { n: count }) : t("center.title")}
          aria-keyshortcuts="Shift+N"
        >
          <Icon />
          {count > 0 && (
            <span
              aria-hidden
              className={cn(
                "absolute -top-0.5 -right-0.5 flex h-4 min-w-4 items-center justify-center rounded-full px-1 text-[10px] leading-none font-semibold tabular-nums",
                ringing > 0 ? "bg-danger-fg text-background" : "bg-brand text-brand-foreground",
              )}
            >
              {count > 99 ? "99+" : count}
            </span>
          )}
        </Button>
      </SheetTrigger>
      <SheetContent side="right" className="w-full gap-0 sm:max-w-md">
        <SheetHeader>
          <SheetTitle>{t("center.title")}</SheetTitle>
          <SheetDescription>{t("center.description")}</SheetDescription>
        </SheetHeader>
        <NotificationCenter />
      </SheetContent>
    </Sheet>
  );
}
