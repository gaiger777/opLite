import { useQueryClient } from "@tanstack/react-query";
import { Plus, Route, Trash2, X } from "lucide-react";
import { useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Network } from "@/api/types";
import { objectTarget } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EmptyState } from "@/components/EmptyState";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { PageHeader } from "@/components/PageHeader";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { FolderSelect, useNewFolder } from "@/folders/FolderSelect";
import { useFolderPaths } from "@/folders/useFolderPaths";
import { errorMessage } from "@/i18n/errors";
import { PermissionsButton, PermissionsSheet } from "@/permissions/PermissionsSheet";
import {
  isOverlay,
  isProvider,
  MAX_INTERFACES,
  MAX_ROUTES,
  refreshRouting,
  routedOf,
  type Router,
  type RouterPatch,
  serverText,
  useRouted,
  useRouter,
  useRouters,
} from "@/routing/routing";
import { cidrFamily } from "@/security-groups/rules";
import { ipFamily } from "@/sdn/ips";
import { callWarned, sdnReady, useSdn } from "@/sdn/sdn";
import { validName } from "@/vms/form";

/** 서버의 `MAX_DESCRIPTION` */
const MAX_DESCRIPTION = 200;

/** SDN이 세워졌는지(읽을 수 없는 사용자에게는 막지 않고 서버에 맡긴다) */
function useSdnBlocked() {
  const readsSdn = usePermissions().reads("sdn");
  const sdn = useSdn(false, readsSdn);
  return sdn.isSuccess && !sdnReady(sdn.data);
}

/**
 * 인프라 › 네트워크 › 라우터(계획 66): 목록(이름·폴더·외부·인터페이스 수·SNAT·지금 게이트웨이)과 서랍(인터페이스,
 * 외부와 SNAT, 정적 경로, 게이트웨이 순위, IPv6 윗단 경로). 만들기는 어딘가에 `network.config`가 있으면 보인다.
 */
export function RoutersPage() {
  const { t } = useTranslation();
  const perms = usePermissions();
  const routers = useRouters();
  const folderPath = useFolderPaths();
  const blocked = useSdnBlocked();
  const [creating, setCreating] = useState(false);
  const [opened, setOpened] = useState<string | null>(null);
  const [sharing, setSharing] = useState<Router | null>(null);
  const [deleting, setDeleting] = useState<Router | null>(null);
  const queryClient = useQueryClient();
  const write = perms.anywhere("network.config");
  const writes = (r: Router) => perms.has("network.config", objectTarget("router", r));
  const delegates = (r: Router) => perms.has("permission.modify", objectTarget("router", r));
  const items = routers.data ?? [];
  // 지금 게이트웨이(NB)는 연 서랍의 라우터 하나만 읽는다(리뷰 1 웹 I1). 읽기 전에는 목록의 것을 보인다.
  const live = useRouter(opened);
  const openedRouter = live.data ?? items.find((r) => r.id === opened);
  // 리뷰 2 웹 m1: NB를 읽었을 때만 주인·대기를 가린다. NB를 읽지 못했으면(`live_error`) 모름, 요청이 실패했으면 그 까닭.
  const liveState: LiveState =
    live.data === undefined
      ? live.error
        ? "failed"
        : "pending"
      : live.data.live_error
        ? "unknown"
        : "read";

  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        id="routers-title"
        title={t("nav.routers")}
        description={t("router.pageHint")}
        actions={
          write &&
          items.length > 0 && (
            <Button disabled={blocked} onClick={() => setCreating(true)}>
              <Plus />
              {t("common.create")}
            </Button>
          )
        }
      />
      {blocked && (
        <p role="status" className="text-sm text-warning-fg">
          {t("router.sdnNotReady")}
        </p>
      )}
      {routers.data === undefined ? (
        <ListPending error={routers.error} />
      ) : items.length === 0 ? (
        <EmptyState
          icon={Route}
          title={t("router.empty.title")}
          description={t("router.empty.description")}
          action={
            write && (
              <Button disabled={blocked} onClick={() => setCreating(true)}>
                <Plus />
                {t("router.create")}
              </Button>
            )
          }
        />
      ) : (
        <Table aria-labelledby="routers-title">
          <TableHeader>
            <TableRow>
              <TableHead>{t("common.name")}</TableHead>
              <TableHead>{t("folders.folder")}</TableHead>
              <TableHead>{t("router.external")}</TableHead>
              <TableHead>{t("router.interfaces")}</TableHead>
              <TableHead>{t("router.snat")}</TableHead>
              <TableHead>{t("router.preferredGateway")}</TableHead>
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {items.map((r) => (
              <TableRow key={r.id}>
                <TableCell className="font-medium">
                  <button
                    type="button"
                    className="underline-offset-4 hover:underline"
                    aria-label={t("router.open", { name: r.name })}
                    onClick={() => setOpened(r.id)}
                  >
                    {r.name}
                  </button>
                </TableCell>
                <TableCell className="text-sm">
                  {r.spec.folder ? folderPath(r.spec.folder) : "/"}
                </TableCell>
                <TableCell>
                  {r.spec.external ? (
                    <span className="flex flex-col">
                      <span>{r.spec.external.network}</span>
                      <span className="font-mono text-xs text-muted-foreground">
                        {(r.spec.external.ips ?? []).join(", ")}
                      </span>
                    </span>
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell>{r.spec.interfaces?.length ?? 0}</TableCell>
                <TableCell>
                  {r.spec.external
                    ? t(r.spec.external.snat === false ? "router.off" : "router.on")
                    : "—"}
                </TableCell>
                <TableCell>
                  <GatewayCell router={r} />
                </TableCell>
                <TableCell className="text-right whitespace-nowrap">
                  {delegates(r) && (
                    <PermissionsButton name={r.name} onClick={() => setSharing(r)} />
                  )}
                  {writes(r) && <DeleteRouterButton router={r} onDelete={() => setDeleting(r)} />}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {creating && <CreateRouterDialog onClose={() => setCreating(false)} />}
      {openedRouter && (
        <RouterSheet
          router={openedRouter}
          live={liveState}
          liveFailure={live.error ? errorMessage(t, live.error) : null}
          writes={writes(openedRouter)}
          onClose={() => setOpened(null)}
        />
      )}
      {sharing && (
        <PermissionsSheet
          target={objectTarget("router", sharing)}
          label={sharing.name}
          onClose={() => setSharing(null)}
        />
      )}
      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={t("router.deleteTitle", { name: deleting?.name })}
        description={t("router.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const router = deleting!;
          await call(
            api.DELETE("/routers/{key}", {
              params: { path: { key: router.id }, header: { "If-Match": router.revision } },
            }),
          );
          await refreshRouting(queryClient);
          toast.success(t("router.deleted", { name: router.name }));
        }}
      />
    </div>
  );
}

/** 서랍의 지금 게이트웨이(NB) 읽기: 읽는 중, 읽음, NB를 읽지 못함(`live_error`), 요청 실패 */
type LiveState = "pending" | "read" | "unknown" | "failed";

/**
 * 목록의 "1순위 게이트웨이" 칸: 후보 없음 경고, 아니면 순위가 가장 높은 후보(Raft)와 보이는 안내 줄. 목록은 NB를 읽지
 * 않으므로(리뷰 1 웹 I1) 지금 주인은 서랍에서 본다. 그 노드가 꺼져 있으면(`online` 거짓) OVN이 다음 후보로 넘겼으므로
 * 경고 칩을 단다(리뷰 2 웹 I2 — 넘긴 뒤에도 죽은 노드를 가리키지 않게).
 */
export function GatewayCell({ router }: { router: Router }) {
  const { t } = useTranslation();
  if (!router.spec.external) {
    return <span className="text-muted-foreground">{t("router.noExternal")}</span>;
  }
  if (router.gateway_error) {
    return <StatusChip tone="warning">{t("router.noGateway")}</StatusChip>;
  }
  const first = router.gateways?.[0];
  if (first) {
    return (
      <span className="flex flex-col items-start gap-0.5">
        <span className="flex flex-wrap items-center gap-1.5">
          {/* 루트 `cluster.read`가 없으면 서버가 노드 이름을 비운다(리뷰 1 M-4). */}
          {first.node || t("router.nodeHidden")}
          {!first.online && <StatusChip tone="warning">{t("router.gatewayDown")}</StatusChip>}
        </span>
        <span className="text-xs text-muted-foreground">{t("router.firstGatewayHint")}</span>
      </span>
    );
  }
  return <span className="text-muted-foreground">—</span>;
}

/**
 * 라우터 지우기 단추. 인터페이스가 남은 라우터는 서버가 지우지 않으므로(409 `router_in_use`) 막지만, 키보드·터치에서도
 * 까닭을 알 수 있게 단추는 초점을 받고(`aria-disabled`) 누르면 까닭을 알리며 화면 읽기 프로그램에는 설명으로 잇는다
 * (리뷰 2 웹 m5).
 */
function DeleteRouterButton({ router, onDelete }: { router: Router; onDelete(): void }) {
  const { t } = useTranslation();
  const reasonId = useId();
  const blocked = (router.spec.interfaces?.length ?? 0) > 0;
  return (
    <>
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label={t("common.deleteNamed", { name: router.name })}
        aria-disabled={blocked || undefined}
        aria-describedby={blocked ? reasonId : undefined}
        title={blocked ? t("router.removeFirst") : undefined}
        className={blocked ? "opacity-50" : undefined}
        onClick={() => (blocked ? toast.info(t("router.removeFirst")) : onDelete())}
      >
        <Trash2 />
      </Button>
      {blocked && (
        <span id={reasonId} className="sr-only">
          {t("router.removeFirst")}
        </span>
      )}
    </>
  );
}

/**
 * 라우터에 붙일 수 있는 네트워크(서버 `check_router`와 권한): 인터페이스는 오버레이 SDN, 서브넷이 있음, 다른 라우터의
 * 인터페이스가 아님, 그 네트워크의 `network.config`(라우터는 그 네트워크 모든 VM의 기본 경로를 바꾼다 — 리뷰 1 #1).
 * 외부는 서브넷이 있는 공급자 네트워크와 `network.assign`(리뷰 1 웹 M3).
 */
function useAttachable(self?: Router) {
  const perms = usePermissions();
  const networks = useList("network");
  // 다른 라우터의 인터페이스인지는 서버의 `routed`로 본다 — 읽을 수 없는 라우터도(리뷰 2 웹 I1).
  const routed = useRouted();
  const all = networks.data ?? [];
  const assigns = (n: Network) => perms.has("network.assign", objectTarget("network", n));
  const configures = (n: Network) => perms.has("network.config", objectTarget("network", n));
  const hasSubnet = (n: Network) => (n.spec.subnets ?? []).length > 0;
  return {
    interfaces: all.filter((n) => {
      const mine = self?.spec.interfaces?.some((i) => i.network === n.name) ?? false;
      return (
        isOverlay(n) &&
        hasSubnet(n) &&
        configures(n) &&
        (mine || routedOf(routed.data, n.name) === undefined)
      );
    }),
    externals: all.filter((n) => isProvider(n) && hasSubnet(n) && assigns(n)),
  };
}

function CreateRouterDialog({ onClose }: { onClose(): void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const folder = useNewFolder("network", "network.config");
  const { interfaces, externals } = useAttachable();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [external, setExternal] = useState("");
  const [snat, setSnat] = useState(true);
  const [chosen, setChosen] = useState<string[]>([]);
  const [errors, setErrors] = useState<Record<string, string>>({});

  async function submit() {
    const found: Record<string, string> = {};
    if (!validName(name)) {
      found.name = "forms.name";
    }
    if (description.length > MAX_DESCRIPTION) {
      found.description = "router.errors.description";
    }
    if (chosen.length > MAX_INTERFACES) {
      found.interfaces = "router.errors.tooMany";
    }
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    const spec = {
      ...(folder.value && { folder: folder.value }),
      ...(description !== "" && { description }),
      ...(external !== "" && { external: { network: external, ...(!snat && { snat: false }) } }),
      ...(chosen.length > 0 && { interfaces: chosen.map((network) => ({ network })) }),
    };
    const { warning } = await callWarned(api.POST("/routers", { body: { name, spec } }));
    await refreshRouting(queryClient);
    toast.success(t("router.created", { name }));
    if (warning) {
      toast.warning(serverText(t, warning));
    }
  }

  return (
    <FormDialog
      title={t("router.createTitle")}
      description={t("router.createDescription")}
      submitLabel={t("common.create")}
      onClose={onClose}
      onSubmit={submit}
    >
      <FolderSelect id="router-folder" state={folder} />
      <Field id="router-name" label={t("common.name")} error={errors.name}>
        {(a11y) => (
          <Input
            id="router-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <Field
        id="router-description"
        label={t("router.descriptionOptional")}
        error={errors.description}
      >
        {(a11y) => (
          <Input
            id="router-description"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            {...a11y}
          />
        )}
      </Field>
      <ExternalFields
        id="router-external"
        externals={externals}
        network={external}
        snat={snat}
        onNetwork={setExternal}
        onSnat={setSnat}
      />
      <div
        role="group"
        aria-label={t("router.interfaces")}
        className="flex flex-col gap-1.5"
        {...(errors.interfaces && { "aria-describedby": "router-interfaces-error" })}
      >
        <span className="text-sm font-medium">{t("router.interfaces")}</span>
        {interfaces.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("router.noAttachable")}</p>
        ) : (
          <div className="flex flex-wrap gap-x-4 gap-y-2">
            {interfaces.map((n) => (
              <div key={n.id} className="flex items-center gap-2">
                <Checkbox
                  id={`router-if-${n.name}`}
                  checked={chosen.includes(n.name)}
                  onCheckedChange={(on) =>
                    setChosen(
                      on === true ? [...chosen, n.name] : chosen.filter((x) => x !== n.name),
                    )
                  }
                />
                <Label htmlFor={`router-if-${n.name}`}>{n.name}</Label>
              </div>
            ))}
          </div>
        )}
        {errors.interfaces && (
          <p id="router-interfaces-error" className="text-xs text-destructive">
            {t(errors.interfaces)}
          </p>
        )}
      </div>
    </FormDialog>
  );
}

/** 외부 네트워크 고르기(없음 = 안쪽끼리만)와 SNAT */
function ExternalFields({
  id,
  externals,
  network,
  snat,
  disabled,
  onNetwork,
  onSnat,
}: {
  id: string;
  externals: Network[];
  network: string;
  snat: boolean;
  disabled?: boolean;
  onNetwork(network: string): void;
  onSnat(snat: boolean): void;
}) {
  const { t } = useTranslation();
  // 지금 외부가 목록에 없어도(권한·지운 네트워크) 고른 값을 보인다.
  const names = [...new Set([...externals.map((n) => n.name), ...(network ? [network] : [])])];
  return (
    <>
      <Field id={id} label={t("router.external")} hint={t("router.externalHint")}>
        {(a11y) => (
          <NativeSelect
            id={id}
            className="w-full"
            disabled={disabled}
            value={network}
            onChange={(e) => onNetwork(e.target.value)}
            {...a11y}
          >
            <NativeSelectOption value="">{t("router.noExternal")}</NativeSelectOption>
            {names.map((n) => (
              <NativeSelectOption key={n} value={n}>
                {n}
              </NativeSelectOption>
            ))}
          </NativeSelect>
        )}
      </Field>
      {network !== "" && (
        <div className="flex items-center gap-2">
          <Checkbox
            id={`${id}-snat`}
            disabled={disabled}
            checked={snat}
            onCheckedChange={(on) => onSnat(on === true)}
          />
          <Label htmlFor={`${id}-snat`}>{t("router.snatLabel")}</Label>
        </div>
      )}
      {network !== "" && !snat && (
        <p className="text-xs text-muted-foreground">{t("router.snatOffHint")}</p>
      )}
    </>
  );
}

/**
 * 라우터 서랍: 경고 띠(게이트웨이 노드 없음·NB를 읽지 못함), 인터페이스, 외부와 SNAT, 정적 경로, 게이트웨이 순위, IPv6
 * 윗단 경로. 바꾸기는 `PATCH`(바꾼 칸만, 목록은 통째로, `If-Match`)다.
 */
function RouterSheet({
  router,
  live,
  liveFailure,
  writes,
  onClose,
}: {
  router: Router;
  /** 지금 주인(NB) 읽기의 상태 */
  live: LiveState;
  /** `GET /routers/{key}` 요청이 실패한 까닭 */
  liveFailure: string | null;
  writes: boolean;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const perms = usePermissions();
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  /** 바꾼 칸을 보낸다. 실패하면 서랍 위에 까닭을 보이고 거짓이다. */
  async function patch(body: RouterPatch): Promise<boolean> {
    setBusy(true);
    setFailure(null);
    try {
      const { warning } = await callWarned(
        api.PATCH("/routers/{key}", {
          params: { path: { key: router.id }, header: { "If-Match": router.revision } },
          body,
        }),
      );
      await refreshRouting(queryClient);
      toast.success(t("router.saved", { name: router.name }));
      if (warning) {
        toast.warning(serverText(t, warning));
      }
      return true;
    } catch (err) {
      // 412: 그새 누가 바꿨다 — 목록을 다시 읽어 지금 것을 보이고 다시 하게 한다.
      await refreshRouting(queryClient);
      setFailure(errorMessage(t, err));
      return false;
    } finally {
      setBusy(false);
    }
  }

  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-3xl" aria-describedby={undefined}>
        <SheetTitle className="px-4 pt-4 text-lg">
          {t("router.sheetTitle", { name: router.name })}
        </SheetTitle>
        <div className="flex flex-col gap-6 px-4 pb-4">
          {router.spec.description && (
            <p className="text-sm text-muted-foreground">{router.spec.description}</p>
          )}
          {router.gateway_error && (
            <p
              role="alert"
              className="rounded-md bg-warning-soft px-3 py-2 text-sm text-warning-fg"
            >
              {/* 까닭은 웹이 안다(외부 네트워크의 physnet) — 서버의 영어 문장 대신(리뷰 2 웹 m7). 노드 SDN은 루트
                  `cluster.admin`만 바꾸므로 다른 사용자에게는 관리자에게 부탁하라고 한다(리뷰 2 웹 m4). */}
              {t(perms.has("cluster.admin") ? "router.gatewayError" : "router.gatewayErrorAsk", {
                network: router.spec.external?.network ?? "",
              })}
            </p>
          )}
          {router.live_error && (
            <p role="status" className="text-sm text-warning-fg">
              {t("router.liveError", { error: router.live_error })}
            </p>
          )}
          {liveFailure && (
            <p role="status" className="text-sm text-warning-fg">
              {t("router.liveError", { error: liveFailure })}
            </p>
          )}
          {failure && (
            <p role="alert" className="text-sm text-destructive">
              {failure}
            </p>
          )}
          <InterfacesSection router={router} writes={writes} busy={busy} patch={patch} />
          <ExternalSection router={router} writes={writes} busy={busy} patch={patch} />
          <RoutesSection router={router} writes={writes} busy={busy} patch={patch} />
          <GatewaysSection router={router} live={live} />
        </div>
      </SheetContent>
    </Sheet>
  );
}

type Patch = (body: RouterPatch) => Promise<boolean>;

interface SectionProps {
  router: Router;
  writes: boolean;
  busy: boolean;
  patch: Patch;
}

function InterfacesSection({ router, writes, busy, patch }: SectionProps) {
  const { t } = useTranslation();
  const networks = useList("network");
  const { interfaces } = useAttachable(router);
  const current = router.spec.interfaces ?? [];
  const addable = interfaces.filter((n) => !current.some((i) => i.network === n.name));
  const [adding, setAdding] = useState("");
  const pick = adding || addable[0]?.name || "";
  const gateways = (name: string) =>
    (networks.data?.find((n) => n.name === name)?.spec.subnets ?? [])
      .map((s) => `${s.gateway ?? t("sdn.subnets.firstHost")} (${s.cidr})`)
      .join(", ");
  return (
    <section aria-labelledby="router-interfaces" className="flex flex-col gap-3">
      <h2 id="router-interfaces" className="font-medium">
        {t("router.interfaces")}
      </h2>
      <Table aria-labelledby="router-interfaces">
        <TableHeader>
          <TableRow>
            <TableHead>{t("vms.network")}</TableHead>
            <TableHead>{t("router.portAddresses")}</TableHead>
            {writes && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {current.length === 0 && (
            <TableRow>
              <TableCell colSpan={writes ? 3 : 2} className="text-muted-foreground">
                {t("router.noInterfaces")}
              </TableCell>
            </TableRow>
          )}
          {current.map((i) => (
            <TableRow key={i.network}>
              <TableCell>{i.network}</TableCell>
              <TableCell className="font-mono text-xs">{gateways(i.network) || "—"}</TableCell>
              {writes && (
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    disabled={busy}
                    aria-label={t("router.removeInterface", { network: i.network })}
                    onClick={() =>
                      void patch({ interfaces: current.filter((x) => x.network !== i.network) })
                    }
                  >
                    <X />
                  </Button>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {writes && current.length < MAX_INTERFACES && (
        <div className="flex flex-wrap items-end gap-2">
          {addable.length === 0 ? (
            <p className="text-xs text-muted-foreground">{t("router.noAttachable")}</p>
          ) : (
            <>
              <Field id="router-add-interface" label={t("router.addInterface")}>
                {(a11y) => (
                  <NativeSelect
                    id="router-add-interface"
                    value={pick}
                    disabled={busy}
                    onChange={(e) => setAdding(e.target.value)}
                    {...a11y}
                  >
                    {addable.map((n) => (
                      <NativeSelectOption key={n.id} value={n.name}>
                        {n.name}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                )}
              </Field>
              <Button
                variant="outline"
                disabled={busy || pick === ""}
                onClick={() =>
                  void patch({ interfaces: [...current, { network: pick }] }).then(
                    (ok) => ok && setAdding(""),
                  )
                }
              >
                <Plus />
                {t("router.add")}
              </Button>
            </>
          )}
        </div>
      )}
      <p className="text-xs text-muted-foreground">{t("router.interfacesHint")}</p>
    </section>
  );
}

function ExternalSection({ router, writes, busy, patch }: SectionProps) {
  const { t } = useTranslation();
  const { externals } = useAttachable(router);
  const external = router.spec.external;
  const [editing, setEditing] = useState<{ network: string; snat: boolean } | null>(null);

  async function save() {
    if (!editing) {
      return;
    }
    const ok = await patch({
      external:
        editing.network === ""
          ? null
          : { network: editing.network, ...(!editing.snat && { snat: false }) },
    });
    if (ok) {
      setEditing(null);
    }
  }

  return (
    <section aria-labelledby="router-external" className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-2">
        <h2 id="router-external" className="font-medium">
          {t("router.external")}
        </h2>
        {writes && !editing && (
          <Button
            variant="outline"
            size="sm"
            onClick={() =>
              setEditing({ network: external?.network ?? "", snat: external?.snat !== false })
            }
          >
            {t("router.editExternal")}
          </Button>
        )}
      </div>
      {editing ? (
        <div className="flex flex-col gap-3">
          <ExternalFields
            id="router-sheet-external"
            externals={externals}
            network={editing.network}
            snat={editing.snat}
            disabled={busy}
            onNetwork={(network) => setEditing({ ...editing, network })}
            onSnat={(snat) => setEditing({ ...editing, snat })}
          />
          <p className="text-xs text-muted-foreground">{t("router.externalChangeHint")}</p>
          <div className="flex gap-2">
            <Button disabled={busy} onClick={() => void save()}>
              {t("common.save")}
            </Button>
            <Button variant="outline" disabled={busy} onClick={() => setEditing(null)}>
              {t("common.cancel")}
            </Button>
          </div>
        </div>
      ) : external ? (
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">{t("vms.network")}</dt>
          <dd>{external.network}</dd>
          <dt className="text-muted-foreground">{t("router.externalIps")}</dt>
          <dd className="font-mono text-xs">{(external.ips ?? []).join(", ") || "—"}</dd>
          <dt className="text-muted-foreground">{t("router.snat")}</dt>
          <dd>{t(external.snat === false ? "router.off" : "router.on")}</dd>
        </dl>
      ) : (
        <p className="text-sm text-muted-foreground">{t("router.noExternalHint")}</p>
      )}
      {(router.upstream_routes ?? []).length > 0 && (
        <div className="flex flex-col gap-1">
          <p className="text-sm">{t("router.upstreamHint")}</p>
          <ul className="flex flex-col gap-0.5 font-mono text-xs">
            {router.upstream_routes!.map((r) => (
              <li key={r}>{r}</li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

/** 정적 경로 하나의 입력 오류(번역 키). 서버 `RouterSpec::validate`의 흔한 것만 본다. */
export function routeError(
  destination: string,
  nexthop: string,
  external: boolean,
): { field: "destination" | "nexthop"; key: string } | null {
  const family = destination.includes("/") ? cidrFamily(destination) : undefined;
  if (family === undefined) {
    return { field: "destination", key: "router.errors.destination" };
  }
  if (ipFamily(nexthop) !== family) {
    return { field: "nexthop", key: "router.errors.nexthop" };
  }
  if (external && /\/0$/.test(destination)) {
    return { field: "destination", key: "router.errors.defaultRoute" };
  }
  return null;
}

function RoutesSection({ router, writes, busy, patch }: SectionProps) {
  const { t } = useTranslation();
  const routes = router.spec.routes ?? [];
  const [destination, setDestination] = useState("");
  const [nexthop, setNexthop] = useState("");
  const [error, setError] = useState<{ field: string; key: string } | null>(null);

  async function add() {
    const found = routeError(destination.trim(), nexthop.trim(), !!router.spec.external);
    setError(found);
    if (found) {
      return;
    }
    const ok = await patch({
      routes: [...routes, { destination: destination.trim(), nexthop: nexthop.trim() }],
    });
    if (ok) {
      setDestination("");
      setNexthop("");
    }
  }

  return (
    <section aria-labelledby="router-routes" className="flex flex-col gap-3">
      <h2 id="router-routes" className="font-medium">
        {t("router.routes")}
      </h2>
      <Table aria-labelledby="router-routes">
        <TableHeader>
          <TableRow>
            <TableHead>{t("router.destination")}</TableHead>
            <TableHead>{t("router.nexthop")}</TableHead>
            {writes && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {routes.length === 0 && (
            <TableRow>
              <TableCell colSpan={writes ? 3 : 2} className="text-muted-foreground">
                {t(router.spec.external ? "router.noRoutesDefault" : "router.noRoutes")}
              </TableCell>
            </TableRow>
          )}
          {routes.map((r, i) => (
            <TableRow key={`${r.destination}-${r.nexthop}`}>
              <TableCell className="font-mono text-xs">{r.destination}</TableCell>
              <TableCell className="font-mono text-xs">{r.nexthop}</TableCell>
              {writes && (
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    disabled={busy}
                    aria-label={t("router.removeRoute", { destination: r.destination })}
                    onClick={() => void patch({ routes: routes.filter((_, j) => j !== i) })}
                  >
                    <X />
                  </Button>
                </TableCell>
              )}
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {writes && routes.length < MAX_ROUTES && (
        <div className="flex flex-wrap items-start gap-2">
          <Field
            id="router-route-destination"
            label={t("router.destination")}
            error={error?.field === "destination" ? error.key : undefined}
          >
            {(a11y) => (
              <Input
                id="router-route-destination"
                placeholder="10.20.0.0/16"
                disabled={busy}
                value={destination}
                onChange={(e) => setDestination(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="router-route-nexthop"
            label={t("router.nexthop")}
            error={error?.field === "nexthop" ? error.key : undefined}
          >
            {(a11y) => (
              <Input
                id="router-route-nexthop"
                placeholder="10.10.0.254"
                disabled={busy}
                value={nexthop}
                onChange={(e) => setNexthop(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Button variant="outline" className="mt-6" disabled={busy} onClick={() => void add()}>
            <Plus />
            {t("router.addRoute")}
          </Button>
        </div>
      )}
    </section>
  );
}

/** 게이트웨이 순위(Raft의 HRW 순위)와 지금 주인(NB `hosting-chassis`) */
function GatewaysSection({ router, live }: { router: Router; live: LiveState }) {
  const { t } = useTranslation();
  if (!router.spec.external) {
    return null;
  }
  const gateways = router.gateways ?? [];
  return (
    <section aria-labelledby="router-gateways" className="flex flex-col gap-3">
      <h2 id="router-gateways" className="font-medium">
        {t("router.gateways")}
      </h2>
      <p className="text-xs text-muted-foreground">{t("router.gatewaysHint")}</p>
      <Table aria-labelledby="router-gateways">
        <TableHeader>
          <TableRow>
            <TableHead>{t("router.node")}</TableHead>
            <TableHead>{t("router.nodePriority")}</TableHead>
            <TableHead>{t("router.haPriority")}</TableHead>
            <TableHead>{t("router.active")}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {gateways.length === 0 && (
            <TableRow>
              <TableCell colSpan={4} className="text-muted-foreground">
                {t("router.noGateway")}
              </TableCell>
            </TableRow>
          )}
          {gateways.map((g) => (
            <TableRow key={g.priority}>
              <TableCell>
                <span className="flex flex-wrap items-center gap-1.5">
                  {g.node || t("router.nodeHidden")}
                  {!g.online && <StatusChip tone="warning">{t("router.gatewayDown")}</StatusChip>}
                </span>
              </TableCell>
              <TableCell>{g.node_priority}</TableCell>
              <TableCell>{g.priority}</TableCell>
              <TableCell>
                {g.active ? (
                  <StatusChip tone="success">{t("router.activeNow")}</StatusChip>
                ) : live === "read" ? (
                  <span className="text-muted-foreground">{t("router.standby")}</span>
                ) : live === "pending" ? (
                  <span className="text-muted-foreground">…</span>
                ) : (
                  // NB를 읽지 못했다: 누가 주인인지 모른다(모두 대기로 보이지 않게 — 리뷰 2 웹 m1).
                  <span className="text-muted-foreground" title={t("router.activeUnknown")}>
                    <span aria-hidden>?</span>
                    <span className="sr-only">{t("router.activeUnknown")}</span>
                  </span>
                )}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {/* 순위 밖의 노드가 주인이면(순위를 바꾼 직후) 그대로 알린다. */}
      {router.hosting && !gateways.some((g) => g.node === router.hosting) && (
        <p className="text-sm">{t("router.hostingOutside", { node: router.hosting })}</p>
      )}
    </section>
  );
}
