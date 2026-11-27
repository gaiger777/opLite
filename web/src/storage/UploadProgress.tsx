import { Check, Pause, Play, X } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import type { TFunction } from "i18next";

import { ApiError } from "@/api/client";
import { useList } from "@/api/live";
import type { Task } from "@/api/types";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { cn } from "@/lib/utils";
import {
  cancelChunked,
  dismissUpload,
  hasFile,
  pauseChunked,
  remainingSeconds,
  resumeChunked,
  resumeSame,
  type UploadItem,
  uploadStage,
} from "@/storage/chunked";
import { useTaskHref } from "@/tasks/taskLink";

/** 남은 시간 글: "3분 20초", "40초", 한 시간 넘으면 "1시간 5분" */
export function durationText(seconds: number, t: TFunction): string {
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = seconds % 60;
  if (h > 0) {
    return t("uploads.time.hm", { h, m });
  }
  return m > 0 ? t("uploads.time.ms", { m, s }) : t("uploads.time.s", { s });
}

/** 단계(서버 가져오기 Task 순서와 같다 — 확인 → 변환 → 마무리, 뒤로 가지 않음) */
type Step = "upload" | "check" | "convert" | "finish" | "done";

/** 이 업로드의 VM 만들기 주소(77-5 화면): ISO는 설치 CD에, 디스크 이미지는 첫 디스크로 */
export function createVmHref(item: Pick<UploadItem, "name" | "content">): string {
  return item.content === "iso"
    ? `/vms/-new?purpose=iso&iso=${encodeURIComponent(item.name)}`
    : `/vms/-new?purpose=image&disk=${encodeURIComponent(item.name)}`;
}

/**
 * 업로드 하나의 진행(계획 77-8 — 업로드 대화상자와 작업·알림 센터가 함께 그림): 단계(올리는 중 → 확인 → 변환 → 마무리 → 끝),
 * 진행 막대, 올린 양/전체·속도·남은 시간, 일시정지·이어 올리기·취소, 실패하면 이어 올리기·버리기(서버에 받은 것이 남았을 때),
 * 파일을 다시 골라 잇기(탭을 닫았다 옴), 끝나면 "이 이미지로 VM 만들기". 다 보낸 뒤의 단계는 가져오기 Task 진행이다.
 */
export function UploadProgress({
  item,
  onTask,
  compact = false,
}: {
  item: UploadItem;
  /** 이어 올린 업로드가 다 보낸 뒤의 Task(알림·지켜보기) */
  onTask(task: Task, item: UploadItem): void;
  /** 센터의 좁은 항목 */
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const tasks = useList("task");
  const nodes = useList("node");
  const taskHref = useTaskHref();
  const pickId = useId();
  // 이어 올리려 고른 파일이 이 업로드의 파일이 아니다(크기·이름이 다름)
  const [wrong, setWrong] = useState(false);
  const task = item.task ? tasks.data?.find((x) => x.id === item.task) : undefined;
  const after = item.phase === "processing" ? uploadStage(task) : null;
  const step: Step =
    after?.stage === "done"
      ? "done"
      : after?.stage === "finishing"
        ? "finish"
        : after?.stage === "converting"
          ? "convert"
          : after || item.phase === "completing"
            ? "check"
            : "upload";
  const steps: Step[] = item.direct
    ? ["upload", "check", "finish", "done"]
    : ["upload", "check", "convert", "finish", "done"];
  const at = steps.indexOf(step);
  const failed = item.phase === "failed" || after?.stage === "failed";
  const cancelled = item.phase === "cancelled";
  const discarding = item.phase === "discarding";
  const stuck = item.phase === "discardFailed";
  // 실패했지만 서버에 받은 것이 남았다 — 이어 올리거나 버린다(리뷰 I2)
  const kept = item.phase === "failed" && item.kept && item.id !== null;
  const waiting = item.phase === "paused" || item.phase === "needsFile" || kept;
  // 이 노드에 업로드가 없다 — 다른 노드에 접속했거나(VIP 이동) 시간이 지나 서버가 버렸다(리뷰 S3)
  const lost = item.error instanceof ApiError && item.error.status === 404 && !cancelled;
  const nodeName = nodes.data?.find((n) => n.id === item.node)?.name ?? item.node ?? "—";
  const live = item.phase === "uploading" || item.phase === "starting";
  const percent =
    after?.stage === "converting"
      ? after.percent
      : step === "upload"
        ? item.size > 0
          ? Math.floor((Math.min(item.sent, item.size) * 100) / item.size)
          : 0
        : null;
  const left = live ? remainingSeconds(item) : null;
  const title = t(item.content === "iso" ? "uploads.item.iso" : "uploads.item.disk", {
    name: item.name,
  });

  const status = cancelled
    ? t("uploads.state.cancelled")
    : discarding
      ? t("uploads.state.discarding")
      : stuck
        ? t("uploads.state.discardFailed")
        : failed
          ? t("uploads.state.failed")
          : item.phase === "paused"
            ? t("uploads.state.paused")
            : item.phase === "needsFile"
              ? t("uploads.state.needsFile")
              : step === "done"
                ? t("uploads.state.done")
                : t(`uploads.step.${step}`);
  const named = (key: string) => t(key, { name: item.name });

  return (
    <div className="flex flex-col gap-1.5" data-upload={item.key}>
      <div className="flex items-start gap-2">
        <span className="min-w-0 font-medium break-words">{title}</span>
        <span className="ml-auto shrink-0 text-xs text-muted-foreground" role="status">
          {status}
        </span>
      </div>
      {!compact && !cancelled && (
        <ol className="flex flex-wrap gap-x-2 gap-y-1 text-xs" aria-label={t("uploads.stepsLabel")}>
          {steps.map((s, i) => (
            <li
              key={s}
              aria-current={i === at && !failed ? "step" : undefined}
              className={cn(
                "flex items-center gap-1",
                i < at || step === "done"
                  ? "text-success-fg"
                  : i === at
                    ? "font-medium"
                    : "text-muted-foreground",
              )}
            >
              {i > 0 && (
                <span className="mr-2 text-muted-foreground" aria-hidden>
                  →
                </span>
              )}
              {(i < at || step === "done") && <Check className="size-3" aria-hidden />}
              {t(`uploads.step.${s}`)}
            </li>
          ))}
        </ol>
      )}
      {percent !== null && !cancelled && (
        <div
          role="progressbar"
          aria-label={title}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          className="h-1.5 overflow-hidden rounded-full bg-muted"
        >
          <div
            className={cn("h-full", failed ? "bg-destructive" : "bg-brand")}
            style={{ width: `${percent}%` }}
          />
        </div>
      )}
      {step === "upload" && !cancelled && (
        <span className="text-xs text-muted-foreground tabular-nums">
          {t("uploads.amount", {
            percent: percent ?? 0,
            sent: formatBytes(item.sent),
            total: formatBytes(item.size),
          })}
          {live &&
            item.speed !== null &&
            ` · ${t("uploads.speed", { speed: formatBytes(item.speed) })}`}
          {left !== null && ` · ${t("uploads.left", { time: durationText(left, t) })}`}
        </span>
      )}
      {after?.stage === "converting" && (
        <span className="text-xs text-muted-foreground">{t("uploads.converting")}</span>
      )}
      {item.error != null && !cancelled && (
        <span className={cn("text-xs", failed || stuck ? "text-destructive" : "text-warning-fg")}>
          {live
            ? t("uploads.retrying", { error: errorMessage(t, item.error) })
            : lost
              ? t("uploads.lost", { node: nodeName })
              : errorMessage(t, item.error)}
        </span>
      )}
      {after?.stage === "failed" && task?.status.error && (
        <span className="text-xs break-words text-destructive">{task.status.error}</span>
      )}
      {wrong && (
        <span role="alert" className="text-xs text-destructive">
          {t("uploads.otherFile", { size: formatBytes(item.size), name: item.fileName ?? "—" })}
        </span>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {live && (
          <Button
            variant="outline"
            size="sm"
            aria-label={named("uploads.pauseNamed")}
            onClick={() => pauseChunked(item.key)}
          >
            <Pause />
            {t("uploads.pause")}
          </Button>
        )}
        {waiting && item.phase !== "needsFile" && hasFile(item.key) && (
          <Button
            variant="outline"
            size="sm"
            aria-label={named("uploads.resumeNamed")}
            onClick={() => resumeSame(item.key, onTask)}
          >
            <Play />
            {t("uploads.resume")}
          </Button>
        )}
        {waiting && (item.phase === "needsFile" || !hasFile(item.key)) && (
          <label
            htmlFor={pickId}
            className="inline-flex h-8 cursor-pointer items-center gap-1.5 rounded-md border px-3 text-sm font-medium hover:bg-accent focus-within:ring-2 focus-within:ring-ring"
          >
            <Play className="size-4" aria-hidden />
            {t("uploads.pickToResume")}
            <input
              id={pickId}
              type="file"
              className="sr-only"
              aria-label={named("uploads.pickToResumeNamed")}
              onChange={(e) => {
                const file = e.target.files?.[0];
                e.target.value = "";
                if (file) {
                  void resumeChunked(item.key, file, onTask).then((ok) => setWrong(!ok));
                }
              }}
            />
          </label>
        )}
        {(live || item.phase === "paused" || item.phase === "needsFile") && (
          <Button
            variant="ghost"
            size="sm"
            aria-label={named("uploads.cancelNamed")}
            onClick={() => cancelChunked(item.key)}
          >
            <X />
            {t("uploads.cancel")}
          </Button>
        )}
        {(kept || stuck) && (
          <Button
            variant="ghost"
            size="sm"
            aria-label={named(stuck ? "uploads.discardAgainNamed" : "uploads.discardNamed")}
            onClick={() => cancelChunked(item.key)}
          >
            <X />
            {t(stuck ? "uploads.discardAgain" : "uploads.discard")}
          </Button>
        )}
        {step === "done" && (
          <Button asChild size="sm">
            <Link to={createVmHref(item)}>{t("uploads.createVm")}</Link>
          </Button>
        )}
        {item.task && (after?.stage === "failed" || step === "done") && (
          <Link
            to={taskHref(item.task)}
            className="text-xs text-brand underline-offset-4 hover:underline"
          >
            {t("uploads.viewTask")}
          </Link>
        )}
        {(step === "done" || (failed && !kept) || cancelled) && (
          <Button
            variant="ghost"
            size="sm"
            className="ml-auto"
            aria-label={t("uploads.dismissNamed", { name: item.name })}
            onClick={() => dismissUpload(item.key)}
          >
            <X />
          </Button>
        )}
      </div>
    </div>
  );
}
