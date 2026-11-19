import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { components } from "@/api/schema";
import { useTaskIndex } from "@/api/taskIndex";
import type { Node, StoragePool, Vm, Volume } from "@/api/types";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { useVmCpu } from "@/cpu/cpu";
import { formatBytes } from "@/lib/format";
import { numaInUse, useVmNuma } from "@/numa/numa";
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
  initialNode,
  onClose,
  returnFocus,
}: {
  vm: Vm;
  /** 미리 고를 노드 id(팔레트의 `migrate web to node2`). 후보가 아니면 첫 후보다. */
  initialNode?: string;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  // 배치 규칙을 넘는 것은 규칙을 쓸 수 있는 사용자(admin)만이다(서버도 403으로 막는다).
  const mayForce = perms.can("write", "placement-rule");
  const [force, setForce] = useState(false);
  const nodes = useList("node");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const snapshots = useList("snapshot");
  const vmTasks = useTaskIndex().get(vm.id) ?? [];
  const targets = migrationTargets(vm, nodes.data ?? []);
  const [chosen, setChosen] = useState<string | null>(initialNode ?? null);
  // 팔레트가 고른 노드가 그 사이 후보에서 빠졌으면 다른 노드로 바꾸지 않고, 사용자가 다시 고를 때까지 보내지 않는다.
  const dropped =
    chosen !== null && nodes.data !== undefined && !targets.some((n) => n.id === chosen)
      ? (nodes.data.find((n) => n.id === chosen)?.name ?? chosen)
      : undefined;
  // VM의 CPU를 받을 수 있는지(계획 63 `GET /vms/{key}/cpu`): 못 받는 노드는 고를 수 없고 까닭을 보인다. 읽지 못하면
  // 서버가 409로 알린다.
  const cpu = useVmCpu(vm.id);
  const fitOf = (n: Node) => cpu.data?.nodes.find((f) => f.node === n.name);
  // 전용 CPU·hugepages·vNUMA를 받을 수 있는지(계획 64 `GET /vms/{key}/numa`): 그 칸을 쓰는 VM만 묻는다. 못 받는 노드는
  // CPU처럼 고를 수 없고 까닭을 보인다.
  const usesNuma = numaInUse(vm.spec) || vm.status.hardware?.numa != null;
  const numa = useVmNuma(vm.id, usesNuma);
  const numaFitOf = (n: Node) => numa.data?.nodes.find((f) => f.node === n.name);
  const cpuOk = (n: Node) => fitOf(n)?.can_run !== false && numaFitOf(n)?.fits !== false;
  // 고르지 않았으면 CPU·NUMA를 받을 수 있는 첫 후보
  const target =
    dropped !== undefined
      ? undefined
      : (targets.find((n) => n.id === chosen) ?? targets.find(cpuOk) ?? targets[0]);
  const fit = target && fitOf(target);
  const numaFit = target && numaFitOf(target);
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
  // 고른 노드가 VM의 복제 대상이면 그 복제본 위에 마지막 복제 뒤 바뀐 것만 보낸다. 풀은 복제본의 풀이라 고르지 않는다
  // (다른 풀을 보내면 서버가 409).
  const toReplica =
    target !== undefined && (vm.spec.replication ?? []).some((r) => r.node === target.id);
  const pool = !toReplica && poolChoices.some((p) => p.name === chosenPool) ? chosenPool : "";
  // 같은 이름의 풀을 골랐는데 고른 노드에서 쓸 수 없는 원본 풀(서버가 409로 거절한다)
  const missingPool =
    target && !toReplica && pool === ""
      ? disks.find((d) => !usable.some((p) => p.name === d.pool.name))?.pool.name
      : undefined;

  async function submit() {
    if (!target) {
      return false;
    }
    const task = await call(
      api.POST("/vms/{key}/migrate", {
        params: { path: { key: vm.id } },
        body: {
          node: target.id,
          ...(disks.length > 0 && pool !== "" && { pool }),
          ...(force && { force: true }),
        },
      }),
    );
    trackTask(task, t("migrate.op", { vm: vm.name, node: target.name }));
  }

  return (
    <FormDialog
      title={t("migrate.title", { name: vm.name })}
      description={t("migrate.description")}
      submitLabel={t("migrate.submit")}
      submitDisabled={
        !able.ok ||
        !target ||
        !loaded ||
        snapshotted ||
        missingPool !== undefined ||
        // CPU 확인을 읽는 동안은 보내지 않는다(읽지 못하면 서버가 정한다 — 리뷰 1 웹 #9).
        cpu.isPending ||
        fit?.can_run === false ||
        (usesNuma && numa.isPending) ||
        numaFit?.fits === false
      }
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!able.ok && <p className="text-sm text-muted-foreground">{t(able.reason)}</p>}
      {!perms.reads("node") ? (
        // 노드 목록은 루트 `cluster.read`다. 없으면 목록이 오지 않으니 기다리지 않고 그렇다고 말한다.
        <p className="text-sm text-muted-foreground">{t("migrate.needsClusterRead")}</p>
      ) : !nodes.data ? (
        <ListPending error={nodes.error} />
      ) : (
        targets.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("migrate.noTarget")}</p>
        )
      )}
      {dropped && (
        <p className="text-sm text-warning-fg">{t("migrate.nodeDropped", { node: dropped })}</p>
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
            {dropped !== undefined && (
              <NativeSelectOption value="" disabled>
                {t("migrate.chooseNode")}
              </NativeSelectOption>
            )}
            {targets.map((n) => (
              <NativeSelectOption key={n.id} value={n.id} disabled={!cpuOk(n)}>
                {fitOf(n)?.can_run === false
                  ? t("migrate.cpuCannot", { node: n.name })
                  : numaFitOf(n)?.fits === false
                    ? t("migrate.numaCannot", { node: n.name })
                    : n.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {fit?.can_run === false && (
        <p role="alert" className="text-sm text-destructive">
          {fit.reason ?? t("migrate.cpuCannot", { node: fit.node })}
        </p>
      )}
      {fit?.can_run !== false && numaFit?.fits === false && (
        <p role="alert" className="text-sm text-destructive">
          {numaFit.reason ?? t("migrate.numaCannot", { node: numaFit.node })}
        </p>
      )}
      {/* `host`·`max` VM: 대상에 없는 플래그를 게스트가 쓰고 있을 수 있다(막지 않는다, 설계 §10). */}
      {(fit?.warnings ?? []).length > 0 && (
        <div
          role="status"
          className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
        >
          <p className="font-medium">{t("migrate.cpuWarning")}</p>
          <ul className="list-disc pl-5">
            {(fit?.warnings ?? []).map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        </div>
      )}
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
          {!toReplica && (
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
          )}
          {missingPool !== undefined && target && (
            <p
              role="alert"
              className="rounded-md border border-warning/50 bg-warning-soft px-3 py-2 text-sm"
            >
              {t("migrate.noSamePool", { node: target.name, pool: missingPool })}
            </p>
          )}
          <p className="text-sm text-muted-foreground">
            {snapshotted
              ? t("migrate.snapshots")
              : toReplica
                ? t("migrate.replicaNote")
                : t("migrate.copyNote", { count: disks.length, size: formatBytes(copySize) })}
          </p>
        </>
      )}
      {mayForce && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <Checkbox
              id="migrate-force"
              checked={force}
              onCheckedChange={(on) => setForce(on === true)}
            />
            <Label htmlFor="migrate-force">{t("migrate.force")}</Label>
          </div>
          <p className="text-xs text-muted-foreground">{t("migrate.forceHint")}</p>
        </div>
      )}
    </FormDialog>
  );
}
