import { Fragment } from "react";
import { useTranslation } from "react-i18next";

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Kbd } from "@/components/ui/kbd";
import { keyLabel, SHORTCUTS } from "@/lib/shortcuts";

/** 단축키 도움말(`?`, 사용자 메뉴). 단축키 표(`SHORTCUTS`)로 그린다. */
export function ShortcutsHelp({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange(v: boolean): void;
}) {
  const { t } = useTranslation();
  const groups = ["general", "navigation"] as const;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("shortcuts.title")}</DialogTitle>
          <DialogDescription>{t("shortcuts.description")}</DialogDescription>
        </DialogHeader>
        {groups.map((group) => (
          <section key={group} aria-labelledby={`shortcuts-${group}`}>
            <h3
              id={`shortcuts-${group}`}
              className="mb-1 text-xs font-medium text-muted-foreground"
            >
              {t(`shortcuts.group.${group}`)}
            </h3>
            <dl className="grid grid-cols-[1fr_auto] gap-x-4 gap-y-1.5">
              {SHORTCUTS.filter((s) => s.group === group).map((s) => (
                <Fragment key={s.id}>
                  <dt>{t(`shortcuts.${s.id}`)}</dt>
                  <dd className="flex items-center gap-1">
                    {s.keys.map((key, i) => (
                      <Fragment key={i}>
                        {i > 0 && (
                          <span className="text-xs text-muted-foreground">
                            {t("shortcuts.then")}
                          </span>
                        )}
                        <Kbd>{keyLabel(key)}</Kbd>
                      </Fragment>
                    ))}
                  </dd>
                </Fragment>
              ))}
            </dl>
          </section>
        ))}
      </DialogContent>
    </Dialog>
  );
}
