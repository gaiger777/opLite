import { ShieldCheck } from "lucide-react";
import { Suspense } from "react";
import { useTranslation } from "react-i18next";

import type { Target } from "@/auth/permissions";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { PermissionsPanel } from "@/permissions/lazyPanel";

/** 행의 "권한" 단추(상세 화면이 없는 네트워크·풀) */
export function PermissionsButton({ name, onClick }: { name: string; onClick(): void }) {
  const { t } = useTranslation();
  return (
    <Button
      variant="ghost"
      size="icon-sm"
      aria-label={t("permissions.openNamed", { name })}
      onClick={onClick}
    >
      <ShieldCheck />
    </Button>
  );
}

/** 상세 화면이 없는 객체(네트워크·풀)의 권한 탭을 오른쪽 서랍으로 연다. */
export function PermissionsSheet({
  target,
  label,
  onClose,
}: {
  target: Target;
  label: string;
  onClose(): void;
}) {
  const { t } = useTranslation();
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-xl" aria-describedby={undefined}>
        <SheetTitle className="px-4 pt-4 text-lg">
          {t("permissions.title", { name: label })}
        </SheetTitle>
        <div className="px-4 pb-4">
          <Suspense fallback={null}>
            <PermissionsPanel target={target} label={label} />
          </Suspense>
        </div>
      </SheetContent>
    </Sheet>
  );
}
