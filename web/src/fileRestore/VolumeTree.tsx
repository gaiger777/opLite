import { cn } from "cn";
import { HardDrive, TriangleAlert } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";

import { Badge } from "@/components/ui/badge";
import {
  groupVolumes,
  volumeBlocked,
  volumeLabel,
  type VolumeView,
  visibleName,
  volumeWarning,
} from "@/fileRestore/fileRestore";
import { SafeName } from "@/fileRestore/SafeName";
import { formatBytes } from "@/lib/format";

/**
 * 왼쪽 볼륨 나무(설계 §14): 디스크 → 파티션·LV, 파일시스템 배지·레이블·크기. 찾아볼 수 없는 볼륨은 흐리게 막고 까닭을 글로
 * 보인다(툴팁에만 두지 않는다 — 휴대폰·화면 읽기). "저널 재생 안 됨" 같은 덧붙임은 경고 배지다.
 */
export function VolumeTree({
  volumes,
  chosen,
  onChoose,
}: {
  volumes: readonly VolumeView[];
  chosen: string | undefined;
  onChoose(id: string): void;
}) {
  const { t } = useTranslation();
  const groups = groupVolumes(volumes);
  return (
    <nav aria-label={t("fileRestore.volumes")} className="flex min-w-0 flex-col gap-3">
      {groups.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("fileRestore.noVolumes")}</p>
      )}
      {groups.map((g) => (
        <section key={g.disk} aria-labelledby={`fr-disk-${g.disk}`} className="flex flex-col gap-1">
          <h2
            id={`fr-disk-${g.disk}`}
            className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground uppercase"
          >
            <HardDrive aria-hidden className="size-3.5" />
            {t("fileRestore.disk", { n: g.disk })}
            {g.self && (
              <span className="font-normal normal-case">· {formatBytes(g.self.size)}</span>
            )}
          </h2>
          <ul className="flex flex-col gap-0.5">
            {/* 디스크 자체는 파일시스템이 있거나(파티션 표 없음) 아래에 아무것도 없을 때(까닭을 보이려고) 행이다. */}
            {g.self && (!volumeBlocked(g.self) || g.children.length === 0) && (
              <VolumeRow volume={g.self} chosen={chosen === g.self.id} onChoose={onChoose} />
            )}
            {g.children.map((v) => (
              <VolumeRow key={v.id} volume={v} chosen={chosen === v.id} onChoose={onChoose} />
            ))}
          </ul>
        </section>
      ))}
    </nav>
  );
}

function VolumeRow({
  volume,
  chosen,
  onChoose,
}: {
  volume: VolumeView;
  chosen: boolean;
  onChoose(id: string): void;
}) {
  const { t } = useTranslation();
  const blocked = volumeBlocked(volume);
  const warning = volumeWarning(volume);
  const label = volumeLabel(volume);
  const reason = blocked ? t(blocked.key, blocked.values) : undefined;
  // 볼륨 id(LV 이름은 백업에서 온다)로 만들면 `vg/a.b`와 `vg/a_b`가 같아진다 — 행마다 고유한 id
  const reasonId = useId();
  return (
    <li>
      <button
        type="button"
        aria-current={chosen ? "true" : undefined}
        aria-disabled={blocked ? true : undefined}
        aria-describedby={reason || warning ? reasonId : undefined}
        title={reason ?? (warning ? visibleName(warning) : undefined)}
        onClick={() => !blocked && onChoose(volume.id)}
        className={cn(
          "flex w-full min-w-0 flex-col items-start gap-0.5 rounded-md px-2 py-1.5 text-left text-sm",
          blocked
            ? "cursor-not-allowed text-muted-foreground opacity-70"
            : "hover:bg-muted focus-visible:bg-muted",
          chosen && "bg-muted font-medium",
        )}
      >
        <span className="flex w-full min-w-0 items-center gap-1.5">
          <span className="truncate">{t(label.key, label.values)}</span>
          {volume.fs && (
            <Badge variant="outline" className="font-mono">
              <SafeName name={volume.fs} />
            </Badge>
          )}
          {warning && <TriangleAlert aria-hidden className="size-3.5 shrink-0 text-warning-fg" />}
          <span className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums">
            {formatBytes(volume.size)}
          </span>
        </span>
        {(volume.label || volume.partition?.type_name) && (
          <span className="truncate text-xs text-muted-foreground">
            {volume.label && <SafeName name={volume.label} />}
            {volume.label && volume.partition?.type_name && " · "}
            {volume.partition?.type_name}
          </span>
        )}
        {(reason || warning) && (
          <span
            id={reasonId}
            className={cn(
              "text-xs whitespace-normal",
              blocked ? "text-muted-foreground" : "text-warning-fg",
            )}
          >
            {reason ? <bdi>{reason}</bdi> : warning && <SafeName name={warning} />}
          </span>
        )}
      </button>
    </li>
  );
}
