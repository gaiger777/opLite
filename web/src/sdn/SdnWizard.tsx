import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Node, Task } from "@/api/types";
import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { errorMessage } from "@/i18n/errors";
import { ipFamily } from "@/sdn/ips";
import { ChassisTable } from "@/sdn/SdnStatusView";
import {
  callWarned,
  INIT_STAGES,
  MAX_CENTRALS,
  nodeBlocker,
  nodeName,
  SDN_KEY,
  type SdnView,
  suggestCentrals,
  useVoters,
} from "@/sdn/sdn";
import { trackTask } from "@/tasks/tasks";

/** 세우기 계획: 첫 세우기 Task·노드·준 주소(다시 하기가 같은 주소를 보낸다 — 리뷰 1 #1)와 나머지 중앙 */
interface Plan {
  task: string;
  init: string;
  address?: string;
  rest: string[];
}

/** 마법사의 단계: 중앙 노드 고르기, 설치 진행, chassis */
const STEPS = ["centrals", "progress", "chassis"] as const;
type Step = (typeof STEPS)[number];

/**
 * SDN(OVN) 세우기 마법사(admin, 계획 56 Ceph 마법사와 같은 꼴). ① 중앙 노드(투표 멤버 셋을 미리 고름) ② 첫 중앙의
 * `sdn-init`과 나머지 중앙의 `sdn-central-add`를 차례로(중앙 Task는 클러스터에서 한 번에 하나다) ③ 다른 노드의 chassis
 * 설치. `sdn`은 주기적으로 다시 읽은 상태다.
 */
export function SdnWizard({
  sdn,
  onStarted,
  onDone,
}: {
  sdn: SdnView;
  onStarted(): void;
  onDone(): void;
}) {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>("centrals");
  const [plan, setPlan] = useState<Plan | null>(null);

  return (
    <Card className="max-w-3xl">
      <CardHeader>
        <CardTitle>{t("sdn.wizard.title")}</CardTitle>
        <ol className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
          {STEPS.map((s, i) => (
            <li
              key={s}
              aria-current={s === step ? "step" : undefined}
              className={s === step ? "font-medium" : "text-muted-foreground"}
            >
              {`${i + 1}. ${t(`sdn.wizard.steps.${s}`)}`}
            </li>
          ))}
        </ol>
      </CardHeader>
      <CardContent>
        {step === "centrals" ? (
          <CentralsStep
            onCreated={(next) => {
              setPlan(next);
              onStarted();
              setStep("progress");
            }}
          />
        ) : step === "progress" && plan ? (
          <ProgressStep
            sdn={sdn}
            plan={plan}
            onRetried={(task) => setPlan({ ...plan, task })}
            onNext={() => setStep("chassis")}
          />
        ) : (
          <div className="flex flex-col gap-4">
            <p className="text-sm text-muted-foreground">{t("sdn.wizard.chassisHint")}</p>
            <ChassisTable sdn={sdn} admin />
            <div>
              <Button onClick={onDone}>{t("sdn.wizard.finish")}</Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

/** ① 중앙 노드. 막힌 노드(위트니스·macOS·underlay 없음·유지보수·오프라인)는 까닭과 함께 고를 수 없다. */
function CentralsStep({ onCreated }: { onCreated(plan: Plan): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const voters = useVoters();
  const all = [...(nodes.data ?? [])]
    .filter((n) => !n.spec.witness)
    .sort((a, b) => a.name.localeCompare(b.name));
  // 고르지 않았으면 제안(투표 멤버 셋)이다.
  const [chosen, setChosen] = useState<string[] | null>(null);
  const picked = (chosen ?? suggestCentrals(all, voters)).filter((id) =>
    all.some((n) => n.id === id && nodeBlocker(n, true) === null),
  );
  const [address, setAddress] = useState("");
  const [addressError, setAddressError] = useState<string | undefined>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function toggle(node: Node, on: boolean) {
    setChosen(on ? [...picked, node.id] : picked.filter((id) => id !== node.id));
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const [init, ...rest] = picked;
    if (!init) {
      return;
    }
    if (address.trim() !== "" && ipFamily(address.trim()) === undefined) {
      setAddressError("sdn.errors.ip");
      return;
    }
    setAddressError(undefined);
    setBusy(true);
    setError(null);
    const given = address.trim() !== "" ? address.trim() : undefined;
    try {
      const task = await call(
        api.POST("/sdn", { body: { node: init, ...(given && { address: given }) } }),
      );
      trackTask(task, t("tasks.op.sdn-init", { node: nodeName(nodes.data, init) }));
      await queryClient.invalidateQueries({ queryKey: SDN_KEY });
      onCreated({ task: task.id, init, ...(given && { address: given }), rest });
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  const odd = picked.length % 2 === 1;
  return (
    <form noValidate onSubmit={(e) => void submit(e)} className="flex flex-col gap-4">
      <p className="text-sm text-muted-foreground">{t("sdn.wizard.centralsHint")}</p>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <fieldset disabled={busy} className="flex min-w-0 flex-col gap-2">
        <legend className="mb-1 text-sm font-medium">{t("sdn.wizard.centralNodes")}</legend>
        {all.map((n) => {
          const blocked = nodeBlocker(n, true);
          const id = `sdn-central-${n.id}`;
          const on = picked.includes(n.id);
          return (
            <div key={n.id} className="flex flex-wrap items-center gap-2">
              <Checkbox
                id={id}
                checked={on}
                disabled={blocked !== null || (!on && picked.length >= MAX_CENTRALS)}
                onCheckedChange={(v) => toggle(n, v === true)}
              />
              <Label htmlFor={id}>{n.name}</Label>
              {voters?.has(n.id) && (
                <span className="text-xs text-muted-foreground">{t("sdn.wizard.voter")}</span>
              )}
              {blocked && <span className="text-xs text-muted-foreground">{t(blocked)}</span>}
              {on && picked[0] === n.id && (
                <span className="text-xs text-muted-foreground">{t("sdn.wizard.initNode")}</span>
              )}
            </div>
          );
        })}
        {picked.length > 0 && !odd && (
          <p className="text-sm text-warning-fg">{t("sdn.warn.even", { count: picked.length })}</p>
        )}
        <Field
          id="sdn-address"
          label={t("sdn.wizard.address")}
          error={addressError}
          hint={t("sdn.wizard.addressHint")}
        >
          {(a11y) => (
            <Input
              id="sdn-address"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              {...a11y}
            />
          )}
        </Field>
      </fieldset>
      <div>
        <Button type="submit" disabled={busy || picked.length === 0}>
          {t("sdn.wizard.start")}
        </Button>
      </div>
    </form>
  );
}

function finished(task: Task | undefined) {
  return task?.status.state === "succeeded" || task?.status.state === "failed";
}

/**
 * ② 첫 세우기 Task의 단계 목록, 끝나면 나머지 중앙을 하나씩 더한다(앞 Task가 끝나야 다음 — 서버 409 `sdn_busy`).
 * 실패하면 까닭과 "다시 하기"(첫 세우기는 같은 노드로 `POST /sdn`, 중앙 더하기는 그 노드로 다시).
 */
function ProgressStep({
  sdn,
  plan,
  onRetried,
  onNext,
}: {
  sdn: SdnView;
  plan: Plan;
  onRetried(task: string): void;
  onNext(): void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const tasks = useList("task");
  const nodes = useList("node");
  const [error, setError] = useState<string | null>(null);
  // 더한 중앙마다의 Task(노드 id → Task id)
  const [adds, setAdds] = useState<Record<string, string>>({});
  const posting = useRef(false);
  const task = tasks.data?.find((x) => x.id === plan.task);
  const done = task?.status.state === "succeeded" || sdn.phase === "ready";
  const failed = task?.status.state === "failed";
  const stage = task?.status.progress?.stage;
  const current = done ? INIT_STAGES.length : Math.max(0, INIT_STAGES.indexOf(stage as never));
  const addTask = (node: string) => tasks.data?.find((x) => x.id === adds[node]);
  const isCentral = (node: string) => sdn.centrals.some((c) => c.node === node);
  // 다음에 더할 중앙: 아직 중앙이 아니고 Task가 없거나(처음) 실패하지 않은 것
  const next = plan.rest.find((node) => !isCentral(node) && !finished(addTask(node)));
  const addFailed = plan.rest.find(
    (node) => !isCentral(node) && addTask(node)?.status.state === "failed",
  );
  const allDone = done && plan.rest.every(isCentral);

  async function addCentral(node: string) {
    posting.current = true;
    setError(null);
    try {
      const { data, warning } = await callWarned(api.POST("/sdn/centrals", { body: { node } }));
      trackTask(data, t("tasks.op.sdn-central-add", { node: nodeName(nodes.data, node) }));
      setAdds((a) => ({ ...a, [node]: data.id }));
      // 짝수 경고는 마지막 중앙의 것만 보인다: 셋을 고르면 둘째를 더할 때의 "2개" 경고는 곧 셋이 되므로 틀린 말이다
      // (리뷰 1 #8).
      const last = plan.rest.every((n) => n === node || isCentral(n));
      if (warning && last) {
        toast.warning(warning);
      }
    } catch (err) {
      setError(errorMessage(t, err));
    } finally {
      posting.current = false;
      await queryClient.invalidateQueries({ queryKey: SDN_KEY });
    }
  }

  // 첫 세우기가 끝나고 도는 SDN Task가 없으면 다음 중앙을 더한다. 이미 보낸 노드는 그 Task가 끝나기를 기다린다(Task
  // 목록에 아직 없을 수도 있다).
  const idle = sdn.tasks.length === 0 && (next === undefined || adds[next] === undefined);
  useEffect(() => {
    if (done && next && idle && !addFailed && !error && !posting.current) {
      void addCentral(next);
    }
    // addCentral은 렌더마다 새로 만든다. 조건이 바뀔 때만 부른다.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [done, next, idle, addFailed, error]);

  async function retry() {
    setError(null);
    if (failed) {
      try {
        // 처음 준 주소를 다시 보낸다(서버도 주소 없는 다시 하기에 지난번 주소를 쓴다 — 리뷰 1 #1).
        const again = await call(
          api.POST("/sdn", {
            body: { node: plan.init, ...(plan.address && { address: plan.address }) },
          }),
        );
        trackTask(again, t("tasks.op.sdn-init", { node: nodeName(nodes.data, plan.init) }));
        onRetried(again.id);
      } catch (err) {
        setError(errorMessage(t, err));
      } finally {
        await queryClient.invalidateQueries({ queryKey: SDN_KEY });
      }
    } else if (addFailed) {
      await addCentral(addFailed);
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <ol aria-label={t("sdn.wizard.stages")} className="flex flex-col gap-1 text-sm">
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
            {t(`tasks.sdnStage.${s}`)}
          </li>
        ))}
      </ol>
      {plan.rest.length > 0 && (
        <ul aria-label={t("sdn.wizard.moreCentrals")} className="flex flex-col gap-1 text-sm">
          {plan.rest.map((node) => {
            const state = isCentral(node)
              ? "done"
              : (addTask(node)?.status.state ?? (done ? "waiting" : "later"));
            return (
              <li key={node}>
                {t("sdn.wizard.addCentral", {
                  node: nodeName(nodes.data, node),
                  state: t(`sdn.wizard.addState.${state}`),
                })}
              </li>
            );
          })}
        </ul>
      )}
      {failed && (
        <p role="alert" className="text-sm text-destructive">
          {t("sdn.wizard.failed", { error: task.status.error ?? "" })}
        </p>
      )}
      {!failed && addFailed && (
        <p role="alert" className="text-sm text-destructive">
          {t("sdn.wizard.addFailed", {
            node: nodeName(nodes.data, addFailed),
            error: addTask(addFailed)?.status.error ?? "",
          })}
        </p>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex gap-2">
        {(failed || addFailed || error) && (
          <Button variant="outline" onClick={() => void retry()}>
            {t("sdn.retry")}
          </Button>
        )}
        <Button disabled={!allDone} onClick={onNext}>
          {t("sdn.wizard.next")}
        </Button>
      </div>
    </div>
  );
}
