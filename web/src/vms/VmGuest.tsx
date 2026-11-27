import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import type { Vm } from "@/api/types";
import { useVmCan } from "@/auth/usePermissions";
import { Field as FormField } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { InfoButton } from "@/components/InfoButton";
import { ReasonButton, ReasonList } from "@/components/Reason";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";
import { formatBytes, formatTime } from "@/lib/format";
import {
  agentChannel,
  fetchGuest,
  GUEST_REFRESH_MS,
  guestAddresses,
  guestKey,
  type GuestInfo,
  type GuestInfoResponse,
  guestOs,
  guestPasswordErrors,
  useGuest,
} from "@/vms/guest";
import { PendingNote } from "@/vms/VmFirmware";

/** 한 칸. `help`(한 줄)를 주면 이름 옆 ⓘ가 도움말 패널(주제 `info`)을 연다(계획 77-4). */
function Field({
  label,
  help,
  info,
  children,
}: {
  label: string;
  help?: string;
  info?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-1">
      <dt className="flex items-center gap-1 text-xs text-muted-foreground">
        {label}
        {help && info && <InfoButton topic={info} label={label} line={help} />}
      </dt>
      <dd className="text-sm">{children}</dd>
    </div>
  );
}

/** 비밀번호 바꾸기가 막힌 이유: 도는 VM에서만(서버의 409 `vm_not_running`) */
function passwordReason(vm: Vm): string | null {
  return vm.status.phase === "running" ? null : "vms.guest.notRunning";
}

/** 에이전트가 답하지 않은 상태의 문구. 상태마다 다르다. */
function StateMessage({ state }: { state: GuestInfoResponse["state"] }) {
  const { t } = useTranslation();
  return <p className="text-sm text-muted-foreground">{t(`vms.guest.state.${state}`)}</p>;
}

/** 게스트 에이전트 설정 한 줄("켜짐 (파일 시스템 얼리기, 에이전트로 끄기)") */
function settingLine(vm: Vm, t: ReturnType<typeof useTranslation>["t"]): string {
  const agent = vm.spec.guest_agent;
  if (!agent) {
    return t("vms.haOff");
  }
  return t("vms.guest.setting", {
    freeze: t(agent.freeze_fs ? "vms.guest.freezeOn" : "vms.guest.freezeOff"),
    shutdown: t(`vms.guest.shutdown.${agent.shutdown ?? "agent"}`),
  });
}

/**
 * 요약의 게스트 에이전트 카드: 설정(채널·얼리기·끄는 방법), 다음 시작 때 적용할 채널 변경, 채널이 있으면 에이전트가 알린
 * OS·호스트 이름·주소(또는 답하지 않은 까닭). 자세한 것은 "게스트" 탭에 있다.
 */
export function VmGuestCard({ vm }: { vm: Vm }) {
  const { t } = useTranslation();
  const channel = agentChannel(vm);
  const guest = useGuest(vm, channel);
  const info = guest.data?.state === "answered" ? guest.data.info : undefined;
  return (
    <Card className="mt-6">
      <CardHeader>
        <CardTitle>
          <h2 id="vm-guest-agent">{t("vms.guest.agent")}</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <dl aria-labelledby="vm-guest-agent" className="grid grid-cols-1 gap-4 sm:grid-cols-3">
          <Field
            label={t("vms.guest.settings")}
            help={t("help.vms.guest.settings")}
            info="vmForm.guestAgent"
          >
            {settingLine(vm, t)}
          </Field>
          {info && (
            <>
              <Field label={t("vms.guest.os")}>{guestOs(info) ?? "—"}</Field>
              <Field label={t("vms.guest.hostname")}>{info.hostname ?? "—"}</Field>
              <Field
                label={t("vms.guest.addresses")}
                help={t("help.vms.guest.addresses")}
                info="vms.guestAddresses"
              >
                {guestAddresses(info).join(", ") || "—"}
              </Field>
            </>
          )}
        </dl>
        <PendingNote vm={vm} fields={["guest_agent"]} />
        {channel && guest.data && guest.data.state !== "answered" && (
          <StateMessage state={guest.data.state} />
        )}
        {guest.error && (
          <p role="alert" className="text-sm text-destructive">
            {errorMessage(t, guest.error)}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

/**
 * "게스트" 탭: 에이전트가 알린 OS·호스트 이름·에이전트 버전·수집 시각, NIC와 주소, 파일 시스템 사용량. "새로 고치기"는
 * 노드가 에이전트에 새로 묻게 하고 5초에 한 번만 된다. 쓰기 권한이 있으면 게스트 비밀번호를 바꿀 수 있다.
 */
export function VmGuestTab({ vm }: { vm: Vm }) {
  const { t, i18n } = useTranslation();
  const can = useVmCan(vm);
  const hint = useId();
  const queryClient = useQueryClient();
  const guest = useGuest(vm);
  const [refreshing, setRefreshing] = useState(false);
  const [cooling, setCooling] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [password, setPassword] = useState(false);
  // 새로 고친 뒤 5초 동안 다시 누르지 못한다.
  useEffect(() => {
    if (!cooling) {
      return;
    }
    const timer = setTimeout(() => setCooling(false), GUEST_REFRESH_MS);
    return () => clearTimeout(timer);
  }, [cooling]);
  const write = can("vm.config");
  const reason = passwordReason(vm);

  async function refresh() {
    setRefreshing(true);
    setCooling(true);
    setRefreshError(null);
    try {
      const data = await fetchGuest(vm, true);
      queryClient.setQueryData(guestKey(vm), data);
    } catch (err) {
      setRefreshError(errorMessage(t, err));
    } finally {
      setRefreshing(false);
    }
  }

  const data = guest.data;
  const info = data?.state === "answered" ? data.info : undefined;
  return (
    <section className="flex flex-col gap-4 pt-2">
      <h2 id="vm-guest" className="sr-only">
        {t("vms.tab.guest")}
      </h2>
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={refreshing || cooling}
          onClick={() => void refresh()}
        >
          {t("vms.guest.refresh")}
        </Button>
        {write && (
          <ReasonButton
            variant="outline"
            size="sm"
            reason={reason}
            reasons={hint}
            onClick={() => setPassword(true)}
          >
            {t("vms.guest.password")}
          </ReasonButton>
        )}
        {info && (
          <span className="text-sm text-muted-foreground">
            {t("vms.guest.collected", {
              time: formatTime(new Date(info.collected_at * 1000), i18n.language),
            })}
          </span>
        )}
      </div>
      {write && <ReasonList base={hint} reasons={[reason]} />}
      {(refreshError ?? guest.error) && (
        <p role="alert" className="text-sm text-destructive">
          {refreshError ?? errorMessage(t, guest.error)}
        </p>
      )}
      {data && data.state !== "answered" && <StateMessage state={data.state} />}
      {info && <GuestDetails info={info} />}
      {password && <GuestPasswordDialog vm={vm} info={info} onClose={() => setPassword(false)} />}
    </section>
  );
}

function GuestDetails({ info }: { info: GuestInfo }) {
  const { t } = useTranslation();
  const nics = info.interfaces ?? [];
  const filesystems = info.filesystems ?? [];
  return (
    <>
      <dl className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <Field label={t("vms.guest.os")}>{guestOs(info) ?? "—"}</Field>
        <Field label={t("vms.guest.kernel")}>{info.os?.kernel ?? "—"}</Field>
        <Field label={t("vms.guest.hostname")}>{info.hostname ?? "—"}</Field>
        <Field label={t("vms.guest.agentVersion")}>{info.agent_version ?? "—"}</Field>
      </dl>
      <section className="flex flex-col gap-2">
        <h3 id="vm-guest-nics" className="font-medium">
          {t("vms.nics")}
        </h3>
        <Table aria-labelledby="vm-guest-nics">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("vms.mac")}</TableHead>
              <TableHead>{t("vms.guest.addresses")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {nics.length === 0 && (
              <TableRow>
                <TableCell colSpan={3} className="text-center text-muted-foreground">
                  {t("vms.noNics")}
                </TableCell>
              </TableRow>
            )}
            {nics.map((nic) => (
              <TableRow key={nic.name}>
                <TableCell>{nic.name}</TableCell>
                <TableCell className="font-mono text-xs">{nic.mac ?? "—"}</TableCell>
                <TableCell className="font-mono text-xs">
                  {(nic.addresses ?? []).map((a) => `${a.ip}/${a.prefix}`).join(", ") || "—"}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
      <section className="flex flex-col gap-2">
        <h3 id="vm-guest-fs" className="font-medium">
          {t("vms.guest.filesystems")}
        </h3>
        <Table aria-labelledby="vm-guest-fs">
          <TableHeader>
            <TableRow>
              <TableHead>{t("vms.guest.mountpoint")}</TableHead>
              <TableHead>{t("vms.guest.fstype")}</TableHead>
              <TableHead>{t("vms.guest.usage")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {filesystems.length === 0 && (
              <TableRow>
                <TableCell colSpan={3} className="text-center text-muted-foreground">
                  {t("vms.guest.noFilesystems")}
                </TableCell>
              </TableRow>
            )}
            {filesystems.map((fs) => (
              <TableRow key={fs.mountpoint}>
                <TableCell className="font-mono text-xs">{fs.mountpoint}</TableCell>
                <TableCell>{fs.fstype}</TableCell>
                <TableCell>
                  {fs.used_bytes != null && fs.total_bytes != null && fs.total_bytes > 0
                    ? t("vms.guest.used", {
                        used: formatBytes(fs.used_bytes),
                        total: formatBytes(fs.total_bytes),
                        percent: Math.round((fs.used_bytes * 100) / fs.total_bytes),
                      })
                    : "—"}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </section>
    </>
  );
}

/**
 * 게스트 비밀번호 설정(`POST /vms/{key}/guest/password`). 열 때만 그리므로 닫으면 입력이 사라지고, 비밀번호는 알림·오류에
 * 넣지 않는다. 사용자 이름은 Windows 게스트면 Administrator, 아니면 root로 채워 둔다.
 */
export function GuestPasswordDialog({
  vm,
  info,
  onClose,
}: {
  vm: Vm;
  info: GuestInfo | undefined;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const [user, setUser] = useState(info?.os?.id === "mswindows" ? "Administrator" : "root");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found = guestPasswordErrors(user, password, again);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const done = await call(
      api.POST("/vms/{key}/guest/password", {
        params: { path: { key: vm.id } },
        body: { user, password },
      }),
    );
    toast.success(t("vms.guest.passwordSet", { user: done.user, name: vm.name }));
  }

  return (
    <FormDialog
      title={t("vms.guest.passwordTitle", { name: vm.name })}
      description={t("vms.guest.passwordDescription")}
      submitLabel={t("common.save")}
      onClose={onClose}
      onSubmit={submit}
    >
      <FormField
        id="guest-user"
        label={t("vms.guest.user")}
        error={errors.user}
        help={t("help.vms.guest.user")}
      >
        {(a11y) => (
          <Input
            id="guest-user"
            {...a11y}
            autoComplete="off"
            value={user}
            onChange={(e) => setUser(e.target.value)}
          />
        )}
      </FormField>
      <FormField
        id="guest-password"
        label={t("vms.guest.newPassword")}
        error={errors.password}
        help={t("help.vms.guest.newPassword")}
      >
        {(a11y) => (
          <Input
            id="guest-password"
            {...a11y}
            type="password"
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        )}
      </FormField>
      <FormField
        id="guest-password-again"
        label={t("vms.guest.passwordAgain")}
        error={errors.again}
        help={t("help.vms.guest.passwordAgain")}
      >
        {(a11y) => (
          <Input
            id="guest-password-again"
            {...a11y}
            type="password"
            autoComplete="new-password"
            value={again}
            onChange={(e) => setAgain(e.target.value)}
          />
        )}
      </FormField>
    </FormDialog>
  );
}
