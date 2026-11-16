import { useQueryClient } from "@tanstack/react-query";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { CephView, Node } from "@/api/types";
import { AddMonDialog } from "@/ceph/AddMonDialog";
import { AddOsdDialog } from "@/ceph/AddOsdDialog";
import { CEPH_KEY, INIT_STAGES, monWarning, publicNetworkFor, useCephStatus } from "@/ceph/ceph";
import { CreateCephPoolDialog } from "@/ceph/CreateCephPoolDialog";
import { InstallClientsDialog } from "@/ceph/InstallClientsDialog";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { errorMessage } from "@/i18n/errors";
import { trackTask } from "@/tasks/tasks";

/** 마법사의 단계: 노드·네트워크, 만들기 진행, mon, OSD, 풀, 클라이언트 꾸러미 */
const STEPS = ["network", "progress", "mons", "osds", "pool", "clients"] as const;
type Step = (typeof STEPS)[number];

/**
 * Ceph 만들기 마법사(admin). 첫 단계가 `POST /ceph`로 만들고, 다음 단계들은 상태 화면의 대화상자를 차례로 연다.
 * mon·OSD·풀·클라이언트 단계는 건너뛸 수 있다. `ceph`는 주기적으로 다시 읽은 관리 Ceph(만들기 전에는 null)다.
 */
export function CephWizard({
  ceph,
  onStarted,
  onDone,
}: {
  ceph: CephView | null;
  onStarted(): void;
  onDone(): void;
}) {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>("network");
  const [task, setTask] = useState<string | null>(null);
  const next = () => setStep(STEPS[STEPS.indexOf(step) + 1] ?? step);

  return (
    <Card className="max-w-3xl">
      <CardHeader>
        <CardTitle>{t("ceph.wizard.title")}</CardTitle>
        <ol className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
          {STEPS.map((s, i) => (
            <li
              key={s}
              aria-current={s === step ? "step" : undefined}
              className={s === step ? "font-medium" : "text-muted-foreground"}
            >
              {`${i + 1}. ${t(`ceph.wizard.steps.${s}`)}`}
            </li>
          ))}
        </ol>
      </CardHeader>
      <CardContent>
        {step === "network" ? (
          <NetworkStep
            onCreated={(id) => {
              setTask(id);
              onStarted();
              setStep("progress");
            }}
          />
        ) : step === "progress" ? (
          <ProgressStep task={task} ceph={ceph} onRetried={setTask} onNext={next} />
        ) : ceph ? (
          <ActionStep step={step} ceph={ceph} onNext={step === "clients" ? onDone : next} />
        ) : null}
      </CardContent>
    </Card>
  );
}

/** ① init 노드와 망. 망은 노드 주소로 미리 채우고, 비워 두면 서버가 같은 규칙으로 정한다. */
function NetworkStep({ onCreated }: { onCreated(task: string): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const choices = (nodes.data ?? []).filter((n) => !n.spec.witness);
  const [node, setNode] = useState("");
  const chosen: Node | undefined =
    choices.find((n) => n.id === node) ?? choices.find((n) => n.status.online);
  const [publicNetwork, setPublicNetwork] = useState<string | null>(null);
  const [clusterNetwork, setClusterNetwork] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const shownPublic = publicNetwork ?? (chosen ? publicNetworkFor(chosen) : "");

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!chosen) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const task = await call(
        api.POST("/ceph", {
          body: {
            node: chosen.id,
            ...(shownPublic.trim() !== "" && { public_network: shownPublic.trim() }),
            ...(clusterNetwork.trim() !== "" && { cluster_network: clusterNetwork.trim() }),
          },
        }),
      );
      trackTask(task, t("tasks.op.ceph-init", { node: chosen.name }));
      await queryClient.invalidateQueries({ queryKey: CEPH_KEY });
      onCreated(task.id);
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form noValidate onSubmit={(e) => void submit(e)} className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">{t("ceph.wizard.networkHint")}</p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <fieldset disabled={busy} className="flex min-w-0 flex-col gap-4">
        <Field id="ceph-init-node" label={t("ceph.wizard.initNode")}>
          <NativeSelect
            id="ceph-init-node"
            value={chosen?.id ?? ""}
            onChange={(e) => {
              setNode(e.target.value);
              setPublicNetwork(null);
            }}
          >
            {choices.map((n) => (
              <NativeSelectOption key={n.id} value={n.id} disabled={!n.status.online}>
                {n.status.online ? n.name : t("ceph.offlineNode", { name: n.name })}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        </Field>
        <Field
          id="ceph-public-network"
          label={t("ceph.publicNetwork")}
          hint={t("ceph.wizard.publicHint")}
        >
          {(a11y) => (
            <Input
              id="ceph-public-network"
              placeholder="10.0.0.0/24"
              value={shownPublic}
              onChange={(e) => setPublicNetwork(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
        <Field
          id="ceph-cluster-network"
          label={t("ceph.clusterNetwork")}
          hint={t("ceph.wizard.clusterHint")}
        >
          {(a11y) => (
            <Input
              id="ceph-cluster-network"
              value={clusterNetwork}
              onChange={(e) => setClusterNetwork(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      </fieldset>
      <div>
        <Button type="submit" disabled={busy || !chosen}>
          {t("ceph.wizard.create")}
        </Button>
      </div>
    </form>
  );
}

/** ② 부트스트랩 Task의 진행을 단계 목록으로. 실패하면 이유와 다시 하기(같은 노드로 `POST /ceph`) */
function ProgressStep({
  task: id,
  ceph,
  onRetried,
  onNext,
}: {
  task: string | null;
  ceph: CephView | null;
  onRetried(task: string): void;
  onNext(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const tasks = useList("task");
  const nodes = useList("node");
  const [error, setError] = useState<string | null>(null);
  const task = tasks.data?.find((x) => x.id === id);
  const done = task?.status.state === "succeeded" || ceph?.phase === "ready";
  const failed = task?.status.state === "failed";
  const stage = task?.status.progress?.stage;
  const current = done ? INIT_STAGES.length : Math.max(0, INIT_STAGES.indexOf(stage as never));

  async function retry() {
    if (!task) {
      return;
    }
    setError(null);
    try {
      const again = await call(api.POST("/ceph", { body: { node: task.spec.node } }));
      const name = nodes.data?.find((n) => n.id === task.spec.node)?.name ?? task.spec.node;
      trackTask(again, t("tasks.op.ceph-init", { node: name }));
      onRetried(again.id);
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      await queryClient.invalidateQueries({ queryKey: CEPH_KEY });
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <ol aria-label={t("ceph.wizard.stages")} className="flex flex-col gap-1 text-sm">
        {INIT_STAGES.map((s, i) => (
          <li
            key={s}
            aria-current={!done && !failed && i === current ? "step" : undefined}
            className={
              i < current
                ? "text-muted-foreground line-through"
                : i === current && !failed
                  ? "font-medium"
                  : undefined
            }
          >
            {t(`tasks.cephStage.${s}`)}
          </li>
        ))}
      </ol>
      {failed && (
        <p role="alert" className="text-sm text-destructive">
          {t("ceph.wizard.failed", { error: task.status.error ?? "" })}
        </p>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex gap-2">
        {failed && (
          <Button variant="outline" onClick={() => void retry()}>
            {t("ceph.retry")}
          </Button>
        )}
        <Button disabled={!done} onClick={onNext}>
          {t("ceph.wizard.next")}
        </Button>
      </div>
    </div>
  );
}

/** ③~⑥ 상태 화면의 대화상자를 여는 단계. 지금 있는 것을 보이고 건너뛸 수 있다. */
function ActionStep({
  step,
  ceph,
  onNext,
}: {
  step: Exclude<Step, "network" | "progress">;
  ceph: CephView;
  onNext(): void;
}) {
  const { t } = useTranslation();
  const nodes = useList("node");
  const status = useCephStatus(step === "pool");
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const names = (ids: string[]) =>
    ids.map((id) => nodes.data?.find((n) => n.id === id)?.name ?? id).join(", ") || "—";
  const busy = ceph.tasks.length > 0;
  const warning = step === "mons" ? monWarning(ceph.mons.length) : null;
  const hosts = status.data && !status.data.stale ? status.data.osd_hosts_up : undefined;
  const current = {
    mons: t("ceph.wizard.haveMons", {
      n: ceph.mons.length,
      names: ceph.mons.map((m) => m.name).join(", "),
    }),
    osds: t("ceph.wizard.haveOsds", { names: names(ceph.osd_nodes) }),
    pool: hosts === undefined ? "" : t("ceph.wizard.haveHosts", { n: hosts }),
    clients: "",
  }[step];
  const button = {
    mons: "ceph.mon.add",
    osds: "ceph.osd.add",
    pool: "ceph.pool.create",
    clients: "ceph.clients.install",
  }[step];

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">{t(`ceph.wizard.${step}Hint`)}</p>
      {current && <p className="text-sm">{current}</p>}
      {busy && <p className="text-sm text-muted-foreground">{t("ceph.wizard.busy")}</p>}
      {warning && <p className="text-sm text-amber-700 dark:text-amber-400">{t(warning)}</p>}
      <div className="flex gap-2">
        <Button
          variant="outline"
          disabled={busy && step === "mons"}
          onClick={(e) => {
            trigger.current = e.currentTarget;
            setOpen(true);
          }}
        >
          {t(button)}
        </Button>
        <Button onClick={onNext}>
          {t(step === "clients" ? "ceph.wizard.finish" : "ceph.wizard.next")}
        </Button>
      </div>
      {open &&
        (step === "mons" ? (
          <AddMonDialog
            ceph={ceph}
            role="mon"
            returnFocus={trigger}
            onClose={() => setOpen(false)}
          />
        ) : step === "osds" ? (
          <AddOsdDialog returnFocus={trigger} onClose={() => setOpen(false)} />
        ) : step === "pool" ? (
          <CreateCephPoolDialog
            hosts={hosts}
            returnFocus={trigger}
            onClose={() => setOpen(false)}
          />
        ) : (
          <InstallClientsDialog ceph={ceph} returnFocus={trigger} onClose={() => setOpen(false)} />
        ))}
    </div>
  );
}
