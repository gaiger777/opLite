import { cn } from "cn";
import { useId } from "react";
import { useTranslation } from "react-i18next";

import type { Node } from "@/api/types";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { formatMib } from "@/lib/format";
import type { VmFormValues } from "@/vms/form";
import { type FormStep, stepId } from "@/vms/form/steps";

/**
 * 왼쪽 단계 표시(설계 §5.6): 단계마다 그 섹션으로 가는 링크이고, 입력 오류가 있는 단계는 표시와 수를 읽는다.
 * 넓은 화면(1024px 이상)에서만 보인다.
 */
export function FormSteps({
  steps,
  errors,
}: {
  steps: readonly FormStep[];
  errors: Partial<Record<FormStep, number>>;
}) {
  const { t } = useTranslation();
  return (
    <nav aria-label={t("vmForm.steps")} className="hidden lg:block">
      <ol className="sticky top-4 flex flex-col gap-0.5 text-sm">
        {steps.map((step, i) => {
          const count = errors[step] ?? 0;
          return (
            <li key={step}>
              <a
                href={`#${stepId(step)}`}
                onClick={(e) => {
                  // 해시 대신 섹션으로 옮겨 라우터 주소를 건드리지 않는다.
                  e.preventDefault();
                  const target = document.getElementById(stepId(step));
                  target?.scrollIntoView?.({ block: "start" });
                  target?.querySelector<HTMLElement>("input, select, textarea, button")?.focus({
                    preventScroll: true,
                  });
                }}
                className={cn(
                  "flex items-center gap-2 rounded-md px-2 py-1 hover:bg-muted",
                  count > 0 ? "font-medium text-danger-fg" : "text-muted-foreground",
                )}
              >
                <span
                  aria-hidden
                  className={cn(
                    "flex size-5 shrink-0 items-center justify-center rounded-full text-xs tabular-nums",
                    count > 0 ? "bg-danger-soft" : "bg-muted",
                  )}
                >
                  {count > 0 ? "!" : i + 1}
                </span>
                <span>
                  {t(`vmForm.step.${step}`)}
                  {count > 0 && ` ${t("vmForm.stepErrors", { count })}`}
                </span>
              </a>
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

/** 단계 하나의 자리: 왼쪽 단계 링크가 가리키는 섹션 */
export function StepSection({ step, children }: { step: FormStep; children: React.ReactNode }) {
  return (
    <div id={stepId(step)} className="scroll-mt-4">
      {children}
    </div>
  );
}

/** 마지막 단계: 보낼 값의 요약 */
export function ReviewSection({
  values,
  nodes,
  creating,
}: {
  values: VmFormValues;
  nodes: Node[] | undefined;
  creating: boolean;
}) {
  const { t } = useTranslation();
  const id = useId();
  const node = values.node ? (nodes?.find((n) => n.id === values.node)?.name ?? values.node) : null;
  const on = (v: boolean) => t(v ? "vmForm.review.on" : "vmForm.review.off");
  const firmware = [
    values.firmware === "uefi" ? "UEFI" : "BIOS",
    ...(values.tpm ? ["TPM 2.0"] : []),
    ...(values.secureBoot ? [t("vmForm.secureBoot")] : []),
  ].join(" · ");
  const rows: [string, string][] = [
    [t("common.name"), values.name || "—"],
    [t("common.node"), node ?? t("vmForm.nodeAuto")],
    [
      t("vmForm.review.cpu"),
      t("vmForm.review.cpuValue", { sockets: values.sockets, cores: values.cores }),
    ],
    [t("vmForm.memory"), formatMib(Number(values.memory) || 0)],
    [t("vms.disks"), values.disks.map((d) => d.volume).join(", ") || "—"],
    [
      t("vms.nics"),
      values.nics
        .map((n) => (n.network === "user" ? t("vmForm.userNetwork") : n.network))
        .join(", ") || "—",
    ],
    [t("vmForm.cdroms"), String(values.cdroms.length)],
    [t("vmForm.review.firmware"), `${firmware} · ${values.machine || t("vmForm.machineAuto")}`],
    [t("vmForm.guestAgent"), on(values.guestAgent)],
    [t("vmForm.cloudInit"), on(values.cloudInit.enabled)],
    ...(creating ? [[t("vmForm.startAfter"), on(values.start)] as [string, string]] : []),
  ];
  return (
    <Card role="region" aria-labelledby={id}>
      <CardHeader>
        <CardTitle>
          <h2 id={id}>{t("vmForm.step.review")}</h2>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <dl className="grid grid-cols-1 gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
          {rows.map(([label, value]) => (
            <div key={label} className="flex min-w-0 justify-between gap-3 border-b py-1">
              <dt className="text-muted-foreground">{label}</dt>
              <dd className="truncate text-right font-medium">{value}</dd>
            </div>
          ))}
        </dl>
      </CardContent>
    </Card>
  );
}
