import { cn } from "cn";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { useList } from "@/api/live";
import type { Node } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { formatMib } from "@/lib/format";
import { isTemplate } from "@/templates/templates";
import type { VmFormValues } from "@/vms/form";
import { type FormStep, type Preset, PRESETS, stepId } from "@/vms/form/steps";
import { useVmActionHost } from "@/vms/actionHost";

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
    [t("vmForm.review.firmware"), `${firmware} · ${values.machine}`],
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

/**
 * VM 만들기의 첫 단계 "프리셋 고르기"(설계 §5.6): 빈 VM·Linux 서버·Windows 11·템플릿에서. 라디오 묶음이다.
 */
export function PresetPicker({
  value,
  onChange,
  disabled,
}: {
  value: Preset;
  onChange(preset: Preset): void;
  disabled?: boolean;
}) {
  const { t } = useTranslation();
  return (
    <fieldset disabled={disabled} className="flex min-w-0 flex-col gap-2">
      <legend className="mb-2 text-sm font-medium">{t("vmForm.preset.title")}</legend>
      <div className="grid grid-cols-1 gap-2 sm:grid-cols-2 xl:grid-cols-4">
        {PRESETS.map((preset) => (
          // 카드 전체를 누를 수 있게 레이블의 ::after가 카드를 덮는다. 설명은 이름에 섞이지 않게 레이블 밖이다.
          <div
            key={preset}
            className={cn(
              "relative flex flex-col gap-1 rounded-lg border p-3 text-sm has-[:focus-visible]:ring-3 has-[:focus-visible]:ring-ring/50",
              value === preset ? "border-brand bg-surface-2" : "hover:bg-muted",
            )}
          >
            <label className="flex cursor-pointer items-center gap-2 font-medium after:absolute after:inset-0 after:content-['']">
              <input
                type="radio"
                name="vm-preset"
                value={preset}
                checked={value === preset}
                onChange={() => onChange(preset)}
                className="accent-brand"
                aria-describedby={`vm-preset-${preset}-hint`}
              />
              {t(`vmForm.preset.${preset}`)}
            </label>
            <span id={`vm-preset-${preset}-hint`} className="text-xs text-muted-foreground">
              {t(`vmForm.preset.${preset}Hint`)}
            </span>
          </div>
        ))}
      </div>
    </fieldset>
  );
}

/** "템플릿에서" 프리셋: 템플릿을 골라 배포 대화상자(복제)를 연다. 템플릿이 없으면 템플릿 화면으로 안내한다. */
export function TemplatePicker() {
  const { t } = useTranslation();
  const host = useVmActionHost();
  const vms = useList("vm");
  const templates = vms.data?.filter(isTemplate);
  if (!templates) {
    return <p className="text-sm text-muted-foreground">{t("common.loading")}</p>;
  }
  if (templates.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        {t("vmForm.preset.noTemplates")}{" "}
        <Link to="/templates" className="text-brand underline-offset-4 hover:underline">
          {t("nav.templates")}
        </Link>
      </p>
    );
  }
  return (
    <ul aria-label={t("nav.templates")} className="flex flex-col divide-y rounded-lg border">
      {templates.map((tpl) => (
        <li key={tpl.id} className="flex items-center justify-between gap-3 px-3 py-2 text-sm">
          <span className="min-w-0 truncate font-medium">{tpl.name}</span>
          <span className="text-xs text-muted-foreground tabular-nums">
            {formatMib(tpl.spec.memory_mib)}
          </span>
          <Button
            type="button"
            size="sm"
            variant="outline"
            onClick={() => host.open({ vm: tpl, which: "deploy" })}
          >
            {t("vmForm.preset.deploy", { name: tpl.name })}
          </Button>
        </li>
      ))}
    </ul>
  );
}
