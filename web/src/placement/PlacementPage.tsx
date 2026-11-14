import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useSettings } from "@/api/live";
import type { NodeLoadView, PlacementMode, PlacementMove, PlacementPolicy } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { Field } from "@/components/Field";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatMib, formatTime } from "@/lib/format";
import {
  barWidth,
  formFromSettings,
  MAX_THRESHOLD,
  MODES,
  moveReasonText,
  pausedText,
  pct,
  type PlacementForm,
  placementPatch,
  POLICIES,
  thresholdError,
} from "@/placement/balance";
import { patchSettings, settingsError } from "@/settings/save";
import { trackTask } from "@/tasks/tasks";

/** 상태의 캐시 키. 리더의 메모리라 watch가 없어 이 화면에서 주기적으로 다시 읽는다. */
const PLACEMENT_KEY = ["placement"] as const;
const REFRESH_MS = 15_000;

/** 최적 배치: 균형 조정기의 상태, 노드 부하, 추천과 적용, admin의 설정 */
export function PlacementPage() {
  const { t, i18n } = useTranslation();
  const me = usePrincipal();
  const queryClient = useQueryClient();
  const status = useQuery({
    queryKey: PLACEMENT_KEY,
    queryFn: () => call(api.GET("/placement")),
    refetchInterval: REFRESH_MS,
    // 15초마다 다시 읽으므로 리더에 묻지 못한 503(`placement_unavailable`)을 곧바로 보인다.
    retry: false,
  });
  const [applying, setApplying] = useState<string | null>(null);
  const operate = can(me.role, "operate", "vm");
  const reload = () => queryClient.invalidateQueries({ queryKey: PLACEMENT_KEY });
  const s = status.data;
  const time = (secs: number | undefined) =>
    secs == null ? "—" : formatTime(new Date(secs * 1000), i18n.language);
  const settings = formFromSettings(s?.settings);
  // auto는 추천 대신 시작한 옮기기(그 Task)를 보인다.
  const auto = settings.mode === "auto";
  const moves = s?.recommendations ?? [];

  async function apply(move: PlacementMove) {
    setApplying(move.id);
    try {
      const task = await call(
        api.POST("/placement/recommendations/{id}/apply", { params: { path: { id: move.id } } }),
      );
      trackTask(task, t("placement.applyOp", { vm: move.vm_name, node: move.to_name }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    } finally {
      setApplying(null);
      await reload();
    }
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-col gap-2">
        <h1 className="text-2xl font-semibold">{t("nav.placement")}</h1>
        <p className="text-sm text-muted-foreground">{t("placement.pageHint")}</p>
      </div>
      {!s ? (
        // 리더에 묻지 못하면(503 `placement_unavailable`) 그 문구를 보인다.
        <ListPending error={status.error} />
      ) : (
        <>
          <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <Item label={t("placement.mode")}>{t(`placement.modes.${settings.mode}`)}</Item>
            <Item label={t("placement.policy")}>{t(`placement.policies.${settings.policy}`)}</Item>
            <Item label={t("placement.threshold")}>{`${settings.threshold}%`}</Item>
            <Item label={t("placement.imbalance")}>
              {s.imbalance
                ? t("placement.imbalanceValue", {
                    total: pct(s.imbalance.total_pct),
                    cpu: pct(s.imbalance.cpu_pct),
                    mem: pct(s.imbalance.mem_pct),
                  })
                : "—"}
            </Item>
            <Item label={t("placement.lastRun")}>{time(s.last_run)}</Item>
            <Item label={t("placement.nextRun")}>{time(s.next_run)}</Item>
            {s.paused && (
              <Item label={t("placement.paused")}>
                <span className="text-amber-700 dark:text-amber-400">
                  {pausedText(t, s.paused)}
                </span>
              </Item>
            )}
          </dl>
          <section className="flex flex-col gap-2">
            <h2 id="placement-nodes" className="font-medium">
              {t("placement.nodes")}
            </h2>
            <Table aria-labelledby="placement-nodes">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("common.node")}</TableHead>
                  <TableHead>{t("placement.cpu")}</TableHead>
                  <TableHead>{t("placement.cpu1")}</TableHead>
                  <TableHead>{t("placement.cpu5")}</TableHead>
                  <TableHead>{t("placement.memory")}</TableHead>
                  <TableHead>{t("placement.vms")}</TableHead>
                  <TableHead>{t("placement.age")}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {s.nodes.length === 0 && (
                  <TableMessage colSpan={7} query={status} empty={t("placement.noNodes")} />
                )}
                {s.nodes.map((n) => (
                  <NodeRow key={n.node} node={n} />
                ))}
              </TableBody>
            </Table>
          </section>
          <section className="flex flex-col gap-2">
            <h2 id="placement-moves" className="font-medium">
              {t(auto ? "placement.applied" : "placement.recommendations")}
            </h2>
            <Table aria-labelledby="placement-moves">
              <TableHeader>
                <TableRow>
                  <TableHead>{t("placement.vm")}</TableHead>
                  <TableHead>{t("placement.from")}</TableHead>
                  <TableHead>{t("placement.to")}</TableHead>
                  <TableHead>{t("placement.reason")}</TableHead>
                  <TableHead>{t("placement.cost")}</TableHead>
                  <TableHead>
                    {auto ? (
                      t("placement.task")
                    ) : (
                      <span className="sr-only">{t("vms.actions")}</span>
                    )}
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {moves.length === 0 && (
                  <TableMessage colSpan={6} query={status} empty={t("placement.empty")} />
                )}
                {moves.map((move) => (
                  <TableRow key={move.id}>
                    <TableCell className="font-medium">{move.vm_name}</TableCell>
                    <TableCell>{move.from_name}</TableCell>
                    <TableCell>{move.to_name}</TableCell>
                    <TableCell>
                      <Badge variant="outline">{t(`placement.reasons.${move.reason}`)}</Badge>
                      <span className="ml-2 text-xs text-muted-foreground">
                        {moveReasonText(t, move)}
                      </span>
                    </TableCell>
                    <TableCell>{formatMib(move.cost_mib)}</TableCell>
                    <TableCell className="text-right whitespace-nowrap">
                      {move.task ? (
                        // UUIDv7은 앞부분이 만든 시각이라 한 회차의 Task끼리 같으므로 모두 보인다.
                        <span className="font-mono text-xs">{move.task}</span>
                      ) : (
                        operate && (
                          <Button
                            variant="outline"
                            size="sm"
                            aria-label={t("placement.applyNamed", {
                              vm: move.vm_name,
                              node: move.to_name,
                            })}
                            disabled={applying !== null}
                            onClick={() => void apply(move)}
                          >
                            {t("placement.apply")}
                          </Button>
                        )
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </section>
        </>
      )}
      {can(me.role, "write", "settings") && <SettingsCard onSaved={reload} />}
    </div>
  );
}

function Item({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

/** 사용률 막대와 값(0.1% 단위) */
function UseBar({ label, permille }: { label: string; permille: number }) {
  return (
    <div className="flex min-w-32 items-center gap-2">
      <div
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={permille / 10}
        className="h-2 w-20 overflow-hidden rounded-full bg-muted"
      >
        <div
          className={permille > 900 ? "h-full bg-destructive" : "h-full bg-primary"}
          style={{ width: `${barWidth(permille)}%` }}
        />
      </div>
      <span className="text-xs tabular-nums">{pct(permille)}</span>
    </div>
  );
}

function NodeRow({ node: n }: { node: NodeLoadView }) {
  const { t } = useTranslation();
  const summary = n.age_secs != null;
  return (
    <TableRow>
      <TableCell className="font-medium">{n.name}</TableCell>
      <TableCell>
        <UseBar label={t("placement.cpu")} permille={n.cpu_use_pct} />
      </TableCell>
      <TableCell className="tabular-nums">{pct(n.cpu1_pct)}</TableCell>
      <TableCell className="tabular-nums">{pct(n.cpu5_pct)}</TableCell>
      <TableCell>
        <UseBar label={t("placement.memory")} permille={n.mem_use_pct} />
        <span className="block text-xs text-muted-foreground">
          {`${t("placement.memUsed")} ${n.mem_used_mib == null ? "—" : formatMib(n.mem_used_mib)} · ${t("placement.memAllocated")} ${formatMib(n.mem_allocated_mib)} / ${formatMib(n.mem_total_mib)}`}
        </span>
      </TableCell>
      <TableCell>{n.vms}</TableCell>
      <TableCell>
        {summary ? (
          t("placement.ageSecs", { n: n.age_secs })
        ) : (
          <Badge variant="outline" title={t("placement.noSummaryHint")}>
            {t("placement.noSummary")}
          </Badge>
        )}
      </TableCell>
    </TableRow>
  );
}

/** 균형 조정기 설정(admin). 설정은 watch로 따라가고, 저장하면 상태도 다시 읽는다. */
function SettingsCard({ onSaved }: { onSaved(): Promise<void> }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useSettings();
  // 고치기 전에는 서버의 값을 보인다.
  const [edited, setEdited] = useState<PlacementForm | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState<string | null>(null);

  if (!settings.data) {
    return null;
  }
  const current = formFromSettings(settings.data.spec.placement);
  const values = edited ?? current;
  const revision = settings.data.revision;
  const change = (part: Partial<PlacementForm>) => setEdited({ ...values, ...part });
  const patch = placementPatch(values, current);
  const unchanged = Object.keys(patch).length === 0;

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const invalid = thresholdError(values.threshold);
    setShown(invalid);
    if (invalid) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await patchSettings(queryClient, revision, { placement: patch });
      setEdited(null);
      toast.success(t("placement.saved"));
      await onSaved();
    } catch (err) {
      setError(settingsError(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="max-w-xl">
      <CardHeader>
        <CardTitle>{t("placement.settings")}</CardTitle>
      </CardHeader>
      <CardContent>
        <form noValidate onSubmit={(e) => void save(e)} className="flex flex-col gap-4">
          {error && (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          )}
          <Field
            id="placement-mode"
            label={t("placement.mode")}
            hint={t(`placement.modeHint.${values.mode}`)}
          >
            {(a11y) => (
              <NativeSelect
                id="placement-mode"
                value={values.mode}
                disabled={busy}
                onChange={(e) => change({ mode: e.target.value as PlacementMode })}
                {...a11y}
              >
                {MODES.map((m) => (
                  <NativeSelectOption key={m} value={m}>
                    {t(`placement.modes.${m}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Field
            id="placement-policy"
            label={t("placement.policy")}
            hint={t(`placement.policyHint.${values.policy}`)}
          >
            {(a11y) => (
              <NativeSelect
                id="placement-policy"
                value={values.policy}
                disabled={busy}
                onChange={(e) => change({ policy: e.target.value as PlacementPolicy })}
                {...a11y}
              >
                {POLICIES.map((p) => (
                  <NativeSelectOption key={p} value={p}>
                    {t(`placement.policies.${p}`)}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Field
            id="placement-threshold"
            label={t("placement.threshold")}
            error={shown ?? undefined}
            hint={t("placement.thresholdHint")}
          >
            {(a11y) => (
              <Input
                id="placement-threshold"
                type="number"
                min={1}
                max={MAX_THRESHOLD}
                value={values.threshold}
                disabled={busy}
                onChange={(e) => change({ threshold: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <div>
            <Button type="submit" variant="outline" disabled={busy || unchanged}>
              {t("placement.save")}
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}
