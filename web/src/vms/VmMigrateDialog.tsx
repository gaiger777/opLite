import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, StoragePool, Vm, Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { formatBytes } from "@/lib/format";
import { isShared, poolsUsableOn } from "@/storage/pools";
import { trackTask } from "@/tasks/tasks";
import { availability } from "@/vms/actions";

type Schemas = components["schemas"];

/** 머신 타입의 아키텍처(서버의 `MachineType::arch`) */
const ARCH_OF: Record<Schemas["MachineType"], Schemas["Arch"]> = {
  q35: "x86_64",
  virt: "aarch64",
};

/**
 * 옮겨 갈 수 있어 보이는 노드: 위트니스가 아니고 온라인이고 유지보수 중이 아니며 VM 머신의 아키텍처인,
 * 지금 노드가 아닌 노드.
 * 풀·브리지·메모리는 서버가 본다(409로 알린다).
 */
export function migrationTargets(vm: Vm, nodes: readonly Node[]): Node[] {
  const here = vm.spec.node ?? vm.status.node;
  const arch = ARCH_OF[vm.spec.machine];
  return nodes.filter(
    (n) =>
      n.id !== here &&
      !n.spec.witness &&
      n.status.online &&
      !n.spec.maintenance &&
      n.status.arch === arch,
  );
}

/**
 * 마이그레이션이 대상 노드로 복사할 디스크: 노드 로컬 풀(공유가 아닌 풀)의 볼륨. 서버의 `disk_moves`처럼
 * 볼륨·풀 기록이 없는 디스크는 뺀다.
 */
export function localDisks(
  vm: Vm,
  volumes: readonly Volume[],
  pools: readonly StoragePool[],
): { volume: Volume; pool: StoragePool }[] {
  return (vm.spec.disks ?? []).flatMap((disk) => {
    const volume = volumes.find((v) => v.name === disk.volume);
    const pool = pools.find((p) => p.name === volume?.spec.pool);
    return volume && pool && !isShared(pool) ? [{ volume, pool }] : [];
  });
}

/** 실행 중인 VM의 라이브 마이그레이션 대화상자. 열 때만 그린다. */
export function VmMigrateDialog({
  vm,
  onClose,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const snapshots = useList("snapshot");
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const targets = migrationTargets(vm, nodes.data ?? []);
  const [chosen, setChosen] = useState<string | null>(null);
  // 고르지 않았거나 고른 노드가 후보에서 빠졌으면 첫 후보
  const target = targets.find((n) => n.id === chosen) ?? targets[0];
  // 대화상자를 연 사이 VM이 멈추거나 옮기기 시작했을 수 있다.
  const able = availability(vm, "migrate", vmTasks);
  // 복사할 디스크와 스냅샷을 알 때까지 보내지 않는다.
  const loaded = Boolean(volumes.data && pools.data && snapshots.data);
  const disks = localDisks(vm, volumes.data ?? [], pools.data ?? []);
  const copySize = disks.reduce((sum, d) => sum + d.volume.spec.size_bytes, 0);
  // 스냅샷은 복사하지 않으므로 서버가 거절한다(migration_unsupported).
  const snapshotted = disks.length > 0 && (snapshots.data ?? []).some((s) => s.spec.vm === vm.id);
  // 고른 노드에서 쓸 수 있는 풀. 비우면(같은 이름의 풀) `pool`을 보내지 않는다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], target?.id);
  // 디스크가 모두 한 풀에 있으면 그 이름의 풀은 기본(같은 이름의 풀)과 같으므로 목록에서 뺀다.
  const sources = new Set(disks.map((d) => d.pool.name));
  const poolChoices = sources.size === 1 ? usable.filter((p) => !sources.has(p.name)) : usable;
  const [chosenPool, setChosenPool] = useState("");
  const pool = poolChoices.some((p) => p.name === chosenPool) ? chosenPool : "";
  // 같은 이름의 풀을 골랐는데 고른 노드에서 쓸 수 없는 원본 풀(서버가 409로 거절한다)
  const missingPool =
    target && pool === ""
      ? disks.find((d) => !usable.some((p) => p.name === d.pool.name))?.pool.name
      : undefined;

  async function submit() {
    if (!target) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/migrate", {
        params: { path: { key: vm.id } },
        body: disks.length > 0 && pool ? { node: target.id, pool } : { node: target.id },
      }),
    );
    trackTask(task, t("migrate.op", { vm: vm.name, node: target.name }));
  }

  return (
    <FormDialog
      title={t("migrate.title", { name: vm.name })}
      description={t("migrate.description")}
      submitLabel={t("migrate.submit")}
      submitDisabled={!able.ok || !target || !loaded || snapshotted || missingPool !== undefined}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!able.ok && <p className="text-sm text-muted-foreground">{t(able.reason)}</p>}
      {!nodes.data ? (
        <ListPending error={nodes.error} />
      ) : (
        targets.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("migrate.noTarget")}</p>
        )
      )}
      <Field id="migrate-node" label={t("migrate.node")}>
        {(a11y) => (
          <NativeSelect
            id="migrate-node"
            className="w-full"
            value={target?.id ?? ""}
            onChange={(e) => setChosen(e.target.value)}
            {...a11y}
          >
            {targets.map((n) => (
              <NativeSelectOption key={n.id} value={n.id}>
                {n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {disks.length > 0 && (
        <>
          <div className="flex flex-col gap-1 text-sm">
            <p className="font-medium">{t("migrate.localDisks")}</p>
            <ul className="list-disc pl-5">
              {disks.map(({ volume, pool: from }) => (
                <li key={volume.id}>
                  {t("migrate.localDisk", {
                    volume: volume.name,
                    pool: from.name,
                    size: formatBytes(volume.spec.size_bytes),
                  })}
                </li>
              ))}
            </ul>
          </div>
          <Field id="migrate-pool" label={t("migrate.pool")}>
            {(a11y) => (
              <NativeSelect
                id="migrate-pool"
                className="w-full"
                value={pool}
                onChange={(e) => setChosenPool(e.target.value)}
                {...a11y}
              >
                <NativeSelectOption value="">{t("migrate.samePool")}</NativeSelectOption>
                {poolChoices.map((p) => (
                  <NativeSelectOption key={p.id} value={p.name}>
                    {p.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          {missingPool !== undefined && target && (
            <p
              role="alert"
              className="rounded-md border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-sm"
            >
              {t("migrate.noSamePool", { node: target.name, pool: missingPool })}
            </p>
          )}
          <p className="text-sm text-muted-foreground">
            {snapshotted
              ? t("migrate.snapshots")
              : t("migrate.copyNote", { count: disks.length, size: formatBytes(copySize) })}
          </p>
        </>
      )}
    </FormDialog>
  );
}
