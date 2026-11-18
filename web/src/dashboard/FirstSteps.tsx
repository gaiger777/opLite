import { Check } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Button } from "@/components/ui/button";

/**
 * 첫 걸음 카드(설계 §5.6): 1 스토리지 풀 만들기 → 2 ISO 올리기·이미지 받기 → 3 VM 만들기. 단계는 이미 받는 목록으로
 * 판정하고 저장하지 않는다. VM이 하나라도 생기면(마지막 단계를 마치면) 사라진다. viewer에게는 보이지 않는다.
 */
export function FirstSteps() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const vms = useList("vm");
  const id = useId();
  const write = can(me.role, "write", "vm");
  // 목록을 다 읽기 전에는 보이지 않는다(있는 클러스터에서 깜박이지 않게).
  if (!write || !pools.data || !volumes.data || !vms.data || vms.data.length > 0) {
    return null;
  }
  const steps = [
    {
      id: "pool",
      done: pools.data.length > 0,
      to: "/storage-pools?create=1",
      allowed: can(me.role, "write", "storage-pool"),
    },
    {
      id: "media",
      done: volumes.data.length > 0,
      to: "/isos?create=1",
      allowed: can(me.role, "write", "volume"),
    },
    { id: "vm", done: false, to: "/vms/-new", allowed: true },
  ];
  // 지금 할 단계: 끝나지 않은 첫 단계
  const current = steps.findIndex((s) => !s.done);
  return (
    <section
      aria-labelledby={id}
      className="flex flex-col gap-3 rounded-lg border bg-surface-2 p-4"
      data-first-steps
    >
      <div className="flex flex-col gap-1">
        <h2 id={id} className="font-medium">
          {t("firstSteps.title")}
        </h2>
        <p className="text-sm text-muted-foreground">{t("firstSteps.description")}</p>
      </div>
      <ol className="grid grid-cols-1 gap-3 md:grid-cols-3">
        {steps.map((step, i) => (
          <li
            key={step.id}
            aria-current={i === current ? "step" : undefined}
            className="flex flex-col gap-2 rounded-md border bg-background p-3"
          >
            <div className="flex items-center gap-2 text-sm font-medium">
              <span
                aria-hidden
                className={
                  step.done
                    ? "flex size-6 items-center justify-center rounded-full bg-success-soft text-success-fg"
                    : "flex size-6 items-center justify-center rounded-full bg-muted text-muted-foreground"
                }
              >
                {step.done ? <Check className="size-3.5" /> : i + 1}
              </span>
              <span>{t(`firstSteps.${step.id}.title`)}</span>
              {step.done && <span className="sr-only">{t("firstSteps.done")}</span>}
            </div>
            <p className="text-xs text-muted-foreground">{t(`firstSteps.${step.id}.hint`)}</p>
            {!step.done && step.allowed && (
              <div className="mt-auto flex flex-wrap items-center gap-2">
                <Button size="sm" variant={i === current ? "default" : "outline"} asChild>
                  <Link to={step.to}>{t(`firstSteps.${step.id}.action`)}</Link>
                </Button>
                {step.id === "media" && (
                  <Link
                    to="/images"
                    className="text-xs text-brand underline-offset-4 hover:underline"
                  >
                    {t("firstSteps.media.images")}
                  </Link>
                )}
              </div>
            )}
          </li>
        ))}
      </ol>
    </section>
  );
}
