import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { usePermissions } from "@/auth/usePermissions";
import { backupPools } from "@/backups/backups";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { remoteBackupStores, remoteStoragePools } from "@/remotes/remotes";

/** 사본을 보낼 곳. `remote`가 비면 고르지 않은 것이다. */
export interface RemoteTarget {
  remote: string;
  pool: string;
}

/**
 * 원격과 그 원격의 백업 풀을 고르는 칸. 원격 목록은 이 클러스터의 API로, 풀 목록은 중계(`GET
 * /storage-pools`)로 읽는다. 원격을 바꾸면 풀은 다시 고른다. 이미 정한 값(작업의 대상)은 목록에 없어도
 * 보인다(서버가 저장할 때 판단한다). `none`은 원격을 고르지 않은 칸의 글이다.
 *
 * `kind`가 `store`면(저장소 백업의 사본 — 계획 68 §12.4) 풀 대신 원격의 쓸 수 있는 백업 저장소(`GET
 * /backup-stores`, 준비됐고 읽기 전용이 아닌 것)를 고른다. 서버는 같은 `pool` 칸을 원격 저장소 이름으로 읽는다.
 * `fingerprint`(보내는 저장소의 키 지문, 평문이면 null)를 주면 키가 다른 원격 저장소는 고를 수 없게 보인다 — 받는 쪽은
 * 같은 키이거나 둘 다 평문이어야 받는다(409 `store_key_mismatch`, 리뷰 2 N2). 모르면(undefined) 가리지 않는다.
 */
export function RemoteTargetFields({
  id,
  label,
  none,
  help,
  value,
  errors,
  onChange,
  kind = "pool",
  fingerprint,
}: {
  id: string;
  label: string;
  none: string;
  help?: string;
  kind?: "pool" | "store";
  fingerprint?: string | null;
  value: RemoteTarget;
  errors: { remote?: string; pool?: string };
  onChange(value: RemoteTarget): void;
}) {
  const { t } = useTranslation();
  // 원격 목록은 루트 `cluster.read`다. 없으면 묻지 않고 한 줄로 알린다(정한 값은 그대로 둔다).
  const readable = usePermissions().reads("remote");
  const remotes = useQuery({
    queryKey: ["remotes"],
    queryFn: () => call(api.GET("/remotes")),
    select: (list) => list.items.map((r) => r.name).sort((a, b) => a.localeCompare(b)),
    enabled: readable,
  });
  const pools = useQuery({
    queryKey: ["remotes", value.remote, "storage-pools"],
    queryFn: ({ signal }) => remoteStoragePools(value.remote, signal),
    select: (items) => backupPools(items).map((p) => p.name),
    enabled: value.remote !== "" && kind === "pool",
  });
  const stores = useQuery({
    queryKey: ["remotes", value.remote, "backup-stores"],
    queryFn: ({ signal }) => remoteBackupStores(value.remote, signal),
    select: (items) =>
      items
        .filter((s) => s.status.phase === "ready" && !s.spec.read_only)
        .sort((a, b) => a.name.localeCompare(b.name)),
    enabled: value.remote !== "" && kind === "store",
  });
  const targets = kind === "store" ? stores : pools;
  const targetNames = (kind === "store" ? stores.data?.map((s) => s.name) : pools.data) ?? [];
  // 키가 다른 원격 저장소(보내는 저장소의 지문을 알 때만)
  const otherKey = new Set(
    kind === "store" && fingerprint !== undefined
      ? (stores.data ?? [])
          .filter((s) => (s.spec.encryption?.fingerprint ?? null) !== fingerprint)
          .map((s) => s.name)
      : [],
  );
  const noSameKey =
    targets.isSuccess && targetNames.length > 0 && targetNames.every((n) => otherKey.has(n));
  const remoteNames = [...(remotes.data ?? [])];
  if (value.remote !== "" && !remoteNames.includes(value.remote)) {
    remoteNames.unshift(value.remote);
  }
  const poolNames = [...targetNames];
  if (value.pool !== "" && !poolNames.includes(value.pool)) {
    poolNames.unshift(value.pool);
  }

  if (!readable) {
    return <p className="text-sm text-muted-foreground">{t("remotes.needsRead", { label })}</p>;
  }
  return (
    <>
      {!remotes.isSuccess && <ListPending error={remotes.error} />}
      <Field id={`${id}-remote`} label={label} error={errors.remote} help={help}>
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
          {!targets.isSuccess && <ListPending error={targets.error} />}
          <Field
            id={`${id}-pool`}
            label={t(kind === "store" ? "backups.remoteStore" : "backups.remotePool")}
            error={errors.pool}
          >
            {(a11y) => (
              <NativeSelect
                id={`${id}-pool`}
                className="w-full"
                value={value.pool}
                onChange={(e) => onChange({ ...value, pool: e.target.value })}
                {...a11y}
              >
                <NativeSelectOption value="">
                  {t(kind === "store" ? "backups.chooseStore" : "backups.choosePool")}
                </NativeSelectOption>
                {poolNames.map((name) => (
                  <NativeSelectOption key={name} value={name} disabled={otherKey.has(name)}>
                    {otherKey.has(name) ? t("backups.otherKeyStore", { name }) : name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          {targets.isSuccess && targetNames.length === 0 && (
            <p className="text-sm text-muted-foreground">
              {t(kind === "store" ? "backups.noRemoteStores" : "backups.noRemotePools")}
            </p>
          )}
          {noSameKey && (
            <p className="text-sm text-muted-foreground">{t("backups.noSameKeyStores")}</p>
          )}
        </>
      )}
    </>
  );
}
