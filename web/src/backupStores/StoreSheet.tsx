import { useQueryClient } from "@tanstack/react-query";
import { KeyRound, Lock } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import { usePermissions } from "@/auth/usePermissions";
import { formatUtc } from "@/backups/jobs";
import { LastGc, LastVerify, StorePhase } from "@/backupStores/chips";
import { RecoveryKeyView } from "@/backupStores/RecoveryKey";
import {
  type BackupStore,
  backendText,
  checkSettings,
  dedupRatio,
  formatRatio,
  isS3,
  MIN_PASSPHRASE,
  refreshStores,
  settingsFormOf,
  settingsPatchOf,
  type StoreCheck,
  type StoreSettingsForm,
} from "@/backupStores/stores";
import { DaysField, VerifyFields } from "@/backupStores/StoreWizard";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { trackTask } from "@/tasks/tasks";

/** 이름과 값 한 줄(`dl` 안) */
function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[10rem_1fr] gap-2 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

/** `{at, days}` 일정을 "매일 05:00 UTC"처럼 */
function useScheduleText() {
  const { t } = useTranslation();
  return (schedule: { at: string; days?: string[] }) =>
    `${
      (schedule.days ?? []).length === 0
        ? t("backupJobs.everyDay")
        : (schedule.days ?? []).map((d) => t(`backupJobs.day.${d}`)).join(", ")
    } ${schedule.at} UTC`;
}

/**
 * 저장소 서랍: 요약(백엔드·주인·사용량·깨진 청크), 지금 하기(GC·검증·연결 확인·설정, 읽기 전용이면 scan·넘겨받기), 일정과
 * 마지막 결과, 키 카드, 노드별 닿음(노드 보고, "연결 확인"이면 그 결과).
 */
export function StoreSheet({ store, onClose }: { store: BackupStore; onClose(): void }) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const nodes = useList("node");
  const schedule = useScheduleText();
  const write = perms.has("storage.config");
  const admin = perms.has("cluster.admin");
  const [check, setCheck] = useState<StoreCheck | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const [takingOver, setTakingOver] = useState(false);
  const [force, setForce] = useState(false);
  const readOnly = store.spec.read_only === true;
  const ready = store.status.phase === "ready";
  const usage = store.status.usage;
  const ratio = dedupRatio(usage);
  const nodeName = (id: string) => nodes.data?.find((n) => n.id === id)?.name ?? id.slice(-8);
  // 노드가 60초마다 알린 닿음(이 노드가 그 저장소를 쓸 수 있을 때만 — 설계 §6)
  const reported = (nodes.data ?? []).flatMap((n) =>
    (n.status.backup_stores ?? [])
      .filter((s) => s.store === store.id)
      .map((s) => ({
        node: n.id,
        name: n.name,
        ok: s.ok,
        error: s.error,
        checked_at: s.checked_at,
      })),
  );
  const reach = check?.nodes ?? reported;

  async function act(what: "gc" | "verify" | "scan") {
    setBusy(what);
    try {
      const path = { params: { path: { key: store.id } } };
      const task =
        what === "gc"
          ? await call(api.POST("/backup-stores/{key}/actions/gc", path))
          : what === "verify"
            ? await call(api.POST("/backup-stores/{key}/actions/verify", { ...path, body: {} }))
            : await call(api.POST("/backup-stores/{key}/actions/scan", path));
      trackTask(task, t(`tasks.op.backup-store-${what}`, { target: store.name }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    } finally {
      setBusy(null);
    }
  }

  async function runCheck() {
    setBusy("check");
    try {
      setCheck(
        await call(
          api.POST("/backup-stores/{key}/actions/check", { params: { path: { key: store.id } } }),
        ),
      );
    } catch (err) {
      toast.error(errorMessage(t, err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-3xl" aria-describedby={undefined}>
        <SheetTitle className="px-4 pt-4 text-lg">
          {t("backupStores.sheetTitle", { name: store.name })}
        </SheetTitle>
        <div className="flex flex-col gap-6 px-4 pb-4">
          <dl className="flex flex-col gap-2">
            <Row label={t("common.state")}>
              <StorePhase store={store} />
            </Row>
            <Row label={t("backupStores.backend")}>
              {t(`backupStores.backendKind.${isS3(store) ? "s3" : "pool"}`)}{" "}
              <span className="font-mono text-xs">{backendText(store)}</span>
            </Row>
            {readOnly && (
              <Row label={t("backupStores.access")}>{t("backupStores.readOnlyBadge")}</Row>
            )}
            {store.status.owner && (
              <Row label={t("backupStores.owner")}>{store.status.owner.cluster}</Row>
            )}
            {store.status.node && <Row label={t("common.node")}>{nodeName(store.status.node)}</Row>}
            <Row label={t("backupStores.usage")}>
              {usage
                ? t("backupStores.usageLine", {
                    snapshots: usage.snapshots,
                    stored: formatBytes(usage.stored_bytes),
                    logical: formatBytes(usage.logical_bytes),
                    ratio: ratio !== undefined ? formatRatio(ratio) : "—",
                  })
                : t("backupStores.notCounted")}
            </Row>
            <Row label={t("backupStores.badChunks")}>
              {(store.status.bad_chunks ?? 0) > 0 ? (
                <span className="text-destructive">{store.status.bad_chunks}</span>
              ) : (
                "0"
              )}
            </Row>
          </dl>

          {(write || (readOnly && admin)) && (
            <div className="flex flex-wrap gap-2">
              {write && !readOnly && (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!ready || busy !== null || !!store.status.gc}
                  onClick={() => void act("gc")}
                >
                  {t("backupStores.gcNow")}
                </Button>
              )}
              {write && (
                <>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={!ready || busy !== null}
                    onClick={() => void act("verify")}
                  >
                    {t("backupStores.verifyNow")}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy !== null}
                    onClick={() => void runCheck()}
                  >
                    {t("backupStores.check")}
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
                    {t("backupStores.editSettings")}
                  </Button>
                </>
              )}
              {write && readOnly && (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!ready || busy !== null}
                  onClick={() => void act("scan")}
                >
                  {t("backupStores.scan")}
                </Button>
              )}
              {/* 넘겨받기는 `cluster.admin`만 든다(`storage.config` 없이도). 준비되지 않은 저장소는 서버가 409다. */}
              {readOnly && admin && (
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!ready}
                  onClick={() => setTakingOver(true)}
                >
                  {t("backupStores.takeOver")}
                </Button>
              )}
            </div>
          )}

          <section className="flex flex-col gap-2" aria-labelledby="store-schedule">
            <h3 id="store-schedule" className="font-medium">
              {t("backupStores.schedules")}
            </h3>
            <dl className="flex flex-col gap-2">
              {!readOnly && (
                <Row label={t("backupStores.gc")}>
                  {schedule(store.spec.gc)}
                  {store.status.next_gc !== undefined && (
                    <div className="text-xs text-muted-foreground">
                      {t("backupStores.nextAt", { at: formatUtc(store.status.next_gc) })}
                    </div>
                  )}
                </Row>
              )}
              <Row label={t("backupStores.verify")}>
                {store.spec.verify ? (
                  <>
                    {schedule(store.spec.verify.schedule)}
                    <div className="text-xs text-muted-foreground">
                      {t("backupStores.verifyPolicy", {
                        days: store.spec.verify.reverify_days,
                        mode: t(`backupStores.mode.${store.spec.verify.mode ?? "full"}`),
                      })}
                      {store.spec.verify.verify_new && ` ${t("backupStores.verifyNewShort")}`}
                    </div>
                    {store.status.next_verify !== undefined && (
                      <div className="text-xs text-muted-foreground">
                        {t("backupStores.nextAt", { at: formatUtc(store.status.next_verify) })}
                      </div>
                    )}
                  </>
                ) : (
                  t("backupStores.noVerify")
                )}
              </Row>
              <Row label={t("backupStores.lastGc")}>
                <LastGc store={store} />
              </Row>
              <Row label={t("backupStores.lastVerify")}>
                <LastVerify store={store} />
                {store.status.last_verify && (
                  <div className="text-xs text-muted-foreground">
                    {t("backupStores.verifyCounts", {
                      snapshots: store.status.last_verify.snapshots,
                      chunks: store.status.last_verify.chunks,
                    })}
                  </div>
                )}
                {store.status.last_verify?.error && (
                  <div className="text-xs text-destructive">{store.status.last_verify.error}</div>
                )}
              </Row>
            </dl>
          </section>

          <KeyCard store={store} admin={admin} />

          <section className="flex flex-col gap-2" aria-labelledby="store-reach">
            <h3 id="store-reach" className="font-medium">
              {t("backupStores.reach")}
            </h3>
            {check && (
              <p role="status" className="text-sm">
                {check.checked_here
                  ? check.ok
                    ? t("backupStores.checkOk")
                    : t("backupStores.checkFailed", { error: check.error ?? "" })
                  : t("backupStores.checkNotHere")}
              </p>
            )}
            {reach.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t("backupStores.noReports")}</p>
            ) : (
              <Table aria-labelledby="store-reach">
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("common.node")}</TableHead>
                    <TableHead>{t("common.state")}</TableHead>
                    <TableHead>{t("backupStores.checkedAt")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {reach.map((r) => (
                    <TableRow key={r.node}>
                      <TableCell>{r.name}</TableCell>
                      <TableCell>
                        <StatusChip tone={r.ok ? "success" : "danger"}>
                          {t(r.ok ? "backupStores.reachable" : "backupStores.unreachable")}
                        </StatusChip>
                        {r.error && (
                          <div className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                            {r.error}
                          </div>
                        )}
                      </TableCell>
                      <TableCell>{formatUtc(r.checked_at)}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </section>
        </div>
        {editing && <SettingsDialog store={store} onClose={() => setEditing(false)} />}
        <ConfirmDialog
          open={takingOver}
          onOpenChange={(open) => {
            setTakingOver(open);
            setForce(false);
          }}
          title={t("backupStores.takeOverTitle", { name: store.name })}
          description={t("backupStores.takeOverDescription", {
            owner: store.status.owner?.cluster ?? "—",
          })}
          confirmLabel={t("backupStores.takeOver")}
          typeToConfirm={store.name}
          onConfirm={async () => {
            await call(
              api.POST("/backup-stores/{key}/actions/take-over", {
                params: { path: { key: store.id } },
                body: force ? { force: true } : {},
              }),
            );
            await refreshStores(queryClient);
            toast.success(t("backupStores.tookOver", { name: store.name }));
          }}
        >
          <div className="flex items-center gap-2">
            <Checkbox
              id="take-over-force"
              checked={force}
              onCheckedChange={(on) => setForce(on === true)}
            />
            <Label htmlFor="take-over-force">{t("backupStores.takeOverForce")}</Label>
          </div>
        </ConfirmDialog>
      </SheetContent>
    </Sheet>
  );
}

/**
 * 키 카드(설계 §9.4): 지문, escrow 쓰기(`cluster.admin`, 읽기 전용이 아닐 때), 복구 키 다시 보기(`cluster.admin` — 확인 뒤,
 * 감사에 남음). 평문 저장소는 그렇다고만 말한다.
 */
function KeyCard({ store, admin }: { store: BackupStore; admin: boolean }) {
  const { t } = useTranslation();
  const fingerprint = store.spec.encryption?.fingerprint;
  const [exporting, setExporting] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [escrowing, setEscrowing] = useState(false);
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2 text-base">
          <KeyRound className="size-4" aria-hidden />
          {t("backupStores.key.title")}
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3 text-sm">
        {fingerprint ? (
          <>
            <p className="flex items-center gap-2">
              <StatusChip tone="info" icon={Lock}>
                {t("backupStores.encrypted")}
              </StatusChip>
              <span className="text-muted-foreground">{t("backupStores.key.aes")}</span>
            </p>
            <p>
              {t("backupStores.key.fingerprint")}{" "}
              <span className="font-mono text-xs break-all">{fingerprint}</span>
            </p>
            {admin ? (
              <div className="flex flex-wrap gap-2">
                {!store.spec.read_only && (
                  <Button
                    variant="outline"
                    size="sm"
                    // 서버가 저장소를 열어 쓴다 — 준비되지 않았으면 409 `store_not_ready`
                    disabled={store.status.phase !== "ready"}
                    onClick={() => setEscrowing(true)}
                  >
                    {t("backupStores.key.escrow")}
                  </Button>
                )}
                <Button variant="outline" size="sm" onClick={() => setExporting(true)}>
                  {t("backupStores.key.export")}
                </Button>
              </div>
            ) : (
              <p className="text-xs text-muted-foreground">{t("backupStores.key.adminOnly")}</p>
            )}
          </>
        ) : (
          <p className="text-muted-foreground">{t("backupStores.key.plain")}</p>
        )}
      </CardContent>
      <ConfirmDialog
        open={exporting}
        onOpenChange={setExporting}
        title={t("backupStores.key.exportTitle", { name: store.name })}
        description={t("backupStores.key.exportDescription")}
        confirmLabel={t("backupStores.key.exportConfirm")}
        onConfirm={async () => {
          const result = await call(
            api.POST("/backup-stores/{key}/key/export", { params: { path: { key: store.id } } }),
          );
          setShown(result.recovery_key);
        }}
      />
      {shown && (
        <Dialog open onOpenChange={(open) => !open && setShown(null)}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{t("backupStores.key.exportTitle", { name: store.name })}</DialogTitle>
              <DialogDescription>{t("backupStores.key.exportShown")}</DialogDescription>
            </DialogHeader>
            <RecoveryKeyView store={store.name} fingerprint={fingerprint} recoveryKey={shown} />
            <DialogFooter>
              <Button onClick={() => setShown(null)}>{t("backupStores.done")}</Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
      {escrowing && <EscrowDialog store={store} onClose={() => setEscrowing(false)} />}
    </Card>
  );
}

/** 저장소 안 escrow 쓰기: 암호 문구 두 번(같아야), 8자 이상. 암호 문구는 저장하지 않는다. */
function EscrowDialog({ store, onClose }: { store: BackupStore; onClose(): void }) {
  const { t } = useTranslation();
  const [passphrase, setPassphrase] = useState("");
  const [again, setAgain] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (passphrase.length < MIN_PASSPHRASE) {
      found.passphrase = "backupStores.errors.passphrase";
    } else if (passphrase !== again) {
      found.again = "backupStores.errors.passphraseAgain";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(
      api.POST("/backup-stores/{key}/key/escrow", {
        params: { path: { key: store.id } },
        body: { passphrase },
      }),
    );
    toast.success(t("backupStores.key.escrowed", { name: store.name }));
  }

  return (
    <FormDialog
      title={t("backupStores.key.escrowTitle", { name: store.name })}
      description={t("backupStores.key.escrowDescription")}
      submitLabel={t("backupStores.key.escrow")}
      onClose={onClose}
      onSubmit={submit}
    >
      <Field id="escrow-passphrase" label={t("backupStores.passphrase")} error={errors.passphrase}>
        {(a11y) => (
          <Input
            id="escrow-passphrase"
            type="password"
            autoComplete="new-password"
            value={passphrase}
            onChange={(e) => setPassphrase(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field id="escrow-again" label={t("backupStores.passphraseAgain")} error={errors.again}>
        {(a11y) => (
          <Input
            id="escrow-again"
            type="password"
            autoComplete="new-password"
            value={again}
            onChange={(e) => setAgain(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
    </FormDialog>
  );
}

/** 설정 바꾸기: GC·검증 일정, S3 동시 요청 수와 자격 증명(백엔드·암호화는 바꾸지 못한다) */
function SettingsDialog({ store, onClose }: { store: BackupStore; onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const s3 = isS3(store);
  const [form, setForm] = useState<StoreSettingsForm>(() => settingsFormOf(store));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<StoreSettingsForm>) => setForm((f) => ({ ...f, ...part }));
  const unchanged =
    Object.keys(checkSettings(form, s3)).length === 0 &&
    Object.keys(settingsPatchOf(store, form)).length === 0;

  async function submit() {
    const found = checkSettings(form, s3);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    await call(
      api.PATCH("/backup-stores/{key}", {
        params: { path: { key: store.id } },
        body: settingsPatchOf(store, form),
      }),
    );
    await refreshStores(queryClient);
    toast.success(t("backupStores.saved", { name: store.name }));
  }

  return (
    <FormDialog
      title={t("backupStores.settingsTitle", { name: store.name })}
      description={t("backupStores.settingsDescription")}
      submitLabel={t("common.save")}
      submitDisabled={unchanged}
      onClose={onClose}
      onSubmit={submit}
    >
      {!store.spec.read_only && (
        <>
          <Field
            id="settings-gc-at"
            label={t("backupStores.gcAt")}
            error={errors.gcAt}
            hint={t("backupJobs.atHint")}
          >
            {(a11y) => (
              <Input
                id="settings-gc-at"
                value={form.gcAt}
                onChange={(e) => change({ gcAt: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <DaysField
            id="settings-gc-day"
            legend={t("backupStores.gcDays")}
            days={form.gcDays}
            onChange={(gcDays) => change({ gcDays })}
          />
        </>
      )}
      <VerifyFields id="settings-verify" form={form} errors={errors} onChange={change} />
      {s3 && (
        <>
          <Field
            id="settings-streams"
            label={t("backupStores.maxStreams")}
            error={errors.maxStreams}
            hint={t("backupStores.maxStreamsHint")}
          >
            {(a11y) => (
              <Input
                id="settings-streams"
                inputMode="numeric"
                value={form.maxStreams}
                onChange={(e) => change({ maxStreams: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="settings-access-key"
            label={t("backupStores.newAccessKeyId")}
            hint={t("backupStores.credentialsHint")}
            error={errors.accessKeyId}
          >
            {(a11y) => (
              <Input
                id="settings-access-key"
                autoComplete="off"
                data-1p-ignore=""
                data-lpignore="true"
                value={form.accessKeyId}
                onChange={(e) => change({ accessKeyId: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="settings-secret-key"
            label={t("backupStores.newSecretKey")}
            error={errors.secretKey}
          >
            {(a11y) => (
              <Input
                id="settings-secret-key"
                type="password"
                autoComplete="new-password"
                value={form.secretKey}
                onChange={(e) => change({ secretKey: e.target.value })}
                {...a11y}
              />
            )}
          </Field>
        </>
      )}
    </FormDialog>
  );
}
