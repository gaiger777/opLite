import { useQuery } from "@tanstack/react-query";
import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, ApiError, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm, Volume } from "@/api/types";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { poolsUsableOn } from "@/storage/pools";
import { trackTask } from "@/tasks/tasks";
import { isBusy, isStopped } from "@/vms/actions";
import { validName } from "@/vms/form";

/** 원본 칸에서 "지금 상태"를 뜻하는 값(스냅샷은 id를 쓴다) */
const CURRENT = "current";

/**
 * 서버의 `vm_node`처럼 복제 Task가 돌 노드: 고정한 노드, 놓인 노드, 첫 디스크 볼륨의 노드, 요청을 받는
 * 노드(접속한 노드) 순. 볼륨 목록이나 접속한 노드를 아직 몰라 정할 수 없으면 undefined.
 */
export function cloneNode(
  vm: Vm,
  volumes: readonly Volume[] | undefined,
  here: string | undefined,
): string | undefined {
  const placed = vm.spec.node ?? vm.status.node;
  if (placed) {
    return placed;
  }
  const first = vm.spec.disks?.[0]?.volume;
  if (first !== undefined) {
    if (volumes === undefined) {
      return undefined;
    }
    const node = volumes.find((v) => v.name === first)?.status.node;
    if (node) {
      return node;
    }
  }
  return here;
}

/** VM 복제 대화상자. 열 때만 그린다. 원본은 꺼진 VM의 지금 상태나 `ready` 스냅샷이다. */
export function VmCloneDialog({
  vm,
  onClose,
  returnFocus,
}: {
  vm: Vm;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const snapshots = useList("snapshot");
  const pools = useList("storage-pool");
  const volumes = useList("volume");
  const nodes = useList("node");
  // 만들어진 차례(UUIDv7)의 역순: 최근 스냅샷이 위에 온다.
  const ready = (snapshots.data ?? [])
    .filter((s) => s.spec.vm === vm.id && s.status.phase === "ready")
    .sort((a, b) => (a.id < b.id ? 1 : -1));
  const stopped = isStopped(vm);
  const [name, setName] = useState("");
  // 고르지 않았으면 꺼진 VM은 지금 상태, 아니면 최근 스냅샷이다.
  const [chosen, setChosen] = useState<string | null>(null);
  const source = chosen ?? (stopped ? CURRENT : (ready[0]?.id ?? CURRENT));
  // 고른 스냅샷이 사라졌거나 ready가 아니게 되었다. 다른 것으로 바꾸지 않고 다시 고르게 한다.
  const gone = source !== CURRENT && !ready.some((s) => s.id === source);
  const [pool, setPool] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 놓인 노드가 없을 때만 접속한 노드가 필요하다(UploadDialog와 같은 질의).
  const cluster = useQuery({
    queryKey: ["cluster"],
    queryFn: () => call(api.GET("/cluster")),
    enabled: !(vm.spec.node ?? vm.status.node),
  });
  // 노드를 정할 때까지는(목록을 읽는 중) 풀을 보이지 않는다.
  const node = cloneNode(vm, volumes.data, cluster.data?.node);
  // 복제 Task는 그 노드에서 돈다. 그 노드에서 준비된 풀만 고른다.
  const usable = poolsUsableOn(pools.data ?? [], nodes.data ?? [], node);
  // 서버는 시작·정지 중인 VM을 복제하지 않는다(vm_busy).
  const busy = isBusy(vm);
  // 지금 상태는 꺼진 VM에서만 복사한다(vm_running). 대화상자를 연 사이 켜질 수 있다.
  const running = source === CURRENT && !stopped;

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    } else if ((vms.data ?? []).some((v) => v.name === name)) {
      found.name = "clone.errors.exists";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    let task;
    try {
      task = await call(
        api.POST("/vms/{key}/clone", {
          params: { path: { key: vm.id } },
          body: {
            name,
            snapshot: source === CURRENT ? null : source,
            pool: pool === "" ? null : pool,
          },
        }),
      );
    } catch (err) {
      // 일반 번역은 업로드의 "접속한 노드"를 말한다. 복제는 VM이 있는 노드에서 돈다.
      if (err instanceof ApiError && err.code === "pool_not_on_node") {
        setErrors({ pool: "clone.errors.poolNotOnNode" });
        return false;
      }
      // 대화상자를 연 사이 그 노드에서 풀이 준비되지 않게 됐다.
      if (err instanceof ApiError && err.code === "pool_unavailable") {
        setErrors({ pool: "clone.errors.poolUnavailable" });
        return false;
      }
      throw err;
    }
    trackTask(task, t("clone.op", { vm: vm.name, name }));
  }

  return (
    <FormDialog
      title={t("clone.title", { name: vm.name })}
      description={t("clone.description")}
      submitLabel={t("clone.submit")}
      // 스냅샷 목록을 읽어야 원본을 고를 수 있다.
      submitDisabled={busy || running || gone || !snapshots.isSuccess}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {busy && <p className="text-sm text-muted-foreground">{t("clone.busy")}</p>}
      <Field id="clone-name" label={t("clone.name")} error={errors.name}>
        {(a11y) => (
          <Input id="clone-name" value={name} onChange={(e) => setName(e.target.value)} {...a11y} />
        )}
      </Field>
      {!snapshots.isSuccess && <ListPending error={snapshots.error} />}
      <Field
        id="clone-source"
        label={t("clone.source")}
        hint={running ? t("clone.running") : gone ? t("clone.gone") : undefined}
      >
        {(a11y) => (
          <NativeSelect
            id="clone-source"
            className="w-full"
            value={source}
            onChange={(e) => setChosen(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value={CURRENT}>{t("clone.current")}</NativeSelectOption>
            {gone && (
              <NativeSelectOption value={source}>{t("clone.goneOption")}</NativeSelectOption>
            )}
            {ready.map((s) => (
              <NativeSelectOption key={s.id} value={s.id}>
                {t("clone.snapshot", { name: s.spec.name })}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {!pools.isSuccess ? (
        <ListPending error={pools.error} />
      ) : (
        node === undefined && <ListPending error={volumes.error ?? cluster.error} />
      )}
      <Field id="clone-pool" label={t("clone.pool")} error={errors.pool}>
        {(a11y) => (
          <NativeSelect
            id="clone-pool"
            className="w-full"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("clone.samePool")}</NativeSelectOption>
            {usable.map((p) => (
              <NativeSelectOption key={p.id} value={p.name}>
                {p.name}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
    </FormDialog>
  );
}
