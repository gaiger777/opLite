import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { backupPools } from "@/backups/backups";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { remoteStoragePools } from "@/remotes/remotes";

/** 사본을 보낼 곳. `remote`가 비면 고르지 않은 것이다. */
export interface RemoteTarget {
  remote: string;
  pool: string;
}

/**
 * 원격과 그 원격의 백업 풀을 고르는 칸. 원격 목록은 이 클러스터의 API로, 풀 목록은 중계(`GET
 * /storage-pools`)로 읽는다. 원격을 바꾸면 풀은 다시 고른다. 이미 정한 값(작업의 대상)은 목록에 없어도
 * 보인다(서버가 저장할 때 판단한다). `none`은 원격을 고르지 않은 칸의 글이다.
 */
export function RemoteTargetFields({
  id,
  label,
  none,
  hint,
  value,
  errors,
  onChange,
}: {
  id: string;
  label: string;
  none: string;
  hint?: string;
  value: RemoteTarget;
  errors: { remote?: string; pool?: string };
  onChange(value: RemoteTarget): void;
}) {
  const { t } = useTranslation();
  const remotes = useQuery({
    queryKey: ["remotes"],
    queryFn: () => call(api.GET("/remotes")),
    select: (list) => list.items.map((r) => r.name).sort((a, b) => a.localeCompare(b)),
  });
  const pools = useQuery({
    queryKey: ["remotes", value.remote, "storage-pools"],
    queryFn: ({ signal }) => remoteStoragePools(value.remote, signal),
    select: (items) => backupPools(items).map((p) => p.name),
    enabled: value.remote !== "",
  });
  const remoteNames = [...(remotes.data ?? [])];
  if (value.remote !== "" && !remoteNames.includes(value.remote)) {
    remoteNames.unshift(value.remote);
  }
  const poolNames = [...(pools.data ?? [])];
  if (value.pool !== "" && !poolNames.includes(value.pool)) {
    poolNames.unshift(value.pool);
  }

  return (
    <>
      {!remotes.isSuccess && <ListPending error={remotes.error} />}
      <Field id={`${id}-remote`} label={label} error={errors.remote} hint={hint}>
        {(a11y) => (
          <NativeSelect
            id={`${id}-remote`}
            className="w-full"
            value={value.remote}
            onChange={(e) => onChange({ remote: e.target.value, pool: "" })}
            {...a11y}
          >
            <NativeSelectOption value="">{none}</NativeSelectOption>
            {remoteNames.map((name) => (
              <NativeSelectOption key={name} value={name}>
                {name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {remotes.isSuccess && remotes.data.length === 0 && (
        <p className="text-sm text-muted-foreground">{t("backups.noRemotes")}</p>
      )}
      {value.remote !== "" && (
        <>
          {!pools.isSuccess && <ListPending error={pools.error} />}
          <Field id={`${id}-pool`} label={t("backups.remotePool")} error={errors.pool}>
            {(a11y) => (
              <NativeSelect
                id={`${id}-pool`}
                className="w-full"
                value={value.pool}
                onChange={(e) => onChange({ ...value, pool: e.target.value })}
                {...a11y}
              >
                <NativeSelectOption value="">{t("backups.choosePool")}</NativeSelectOption>
                {poolNames.map((name) => (
                  <NativeSelectOption key={name} value={name}>
                    {name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          {pools.isSuccess && pools.data.length === 0 && (
            <p className="text-sm text-muted-foreground">{t("backups.noRemotePools")}</p>
          )}
        </>
      )}
    </>
  );
}
