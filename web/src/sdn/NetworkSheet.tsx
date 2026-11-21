import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, ApiError, call } from "@/api/client";
import { putResource } from "@/api/optimistic";
import type { Network } from "@/api/types";
import { ListPending } from "@/components/ListPending";
import { StatusChip } from "@/components/StatusChip";
import { Button } from "@/components/ui/button";
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
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { PortOffChip } from "@/sdn/chips";
import {
  isProvider,
  refreshRouting,
  routedOf,
  routerOf,
  useRouted,
  useRouters,
} from "@/routing/routing";
import { formatIps } from "@/sdn/ips";
import type { NetworkIps } from "@/sdn/sdn";
import { SubnetsEditor } from "@/sdn/SubnetsEditor";
import { subnetErrors, subnetRow, type SubnetRow, toSubnet } from "@/sdn/subnets";

/** `GET /networks/{key}/ips`의 캐시 키 */
export function ipsKey(id: string) {
  return ["network-ips", id] as const;
}

/**
 * SDN 네트워크의 주소(서브넷 사용·NIC 주소·OVN 포트 상태). `live`는 OVN을 읽지 못했으면 null이다. `live`가 거짓이면
 * OVN을 읽지 않는 목록용 질의다(네트워크 목록의 사용률 — 행마다 NB·SB를 읽지 않게, 리뷰 1 #10).
 */
export function useNetworkIps(network: Network, enabled = true, live = true) {
  return useQuery({
    queryKey: [...ipsKey(network.id), live ? "live" : "usage"],
    queryFn: () =>
      call(
        api.GET("/networks/{key}/ips", {
          params: { path: { key: network.id }, ...(!live && { query: { live: false } }) },
        }),
      ),
    enabled,
    staleTime: 10_000,
  });
}

/** JSON 수의 정확한 끝(2^53). IPv6 서브넷의 크기는 이보다 커서 정확하지 않다. */
const EXACT = 2 ** 53;

/** 주소 수: IPv6 /64처럼 아주 크면 "≈1.8e19"(리뷰 1 #10) */
export function addressCount(n: number): string {
  return n >= EXACT ? `≈${n.toExponential(1).replace("e+", "e")}` : String(n);
}

/** 사용률 막대가 뜻이 있는지(크기가 아주 크면 늘 0%라 그리지 않는다) */
export function hasUsageBar(size: number): boolean {
  return size > 0 && size < EXACT;
}

/** 서브넷 사용률 막대 하나 */
export function UsageBar({ used, size }: { used: number; size: number }) {
  const { t } = useTranslation();
  const percent = size > 0 ? Math.min(100, (used / size) * 100) : 0;
  return (
    <div
      role="meter"
      aria-label={t("sdn.ips.usage")}
      aria-valuemin={0}
      aria-valuemax={size}
      aria-valuenow={Math.min(used, size)}
      className="h-1.5 w-24 overflow-hidden rounded-full bg-muted"
    >
      <div
        className={percent >= 90 ? "h-full bg-warning" : "h-full bg-brand"}
        style={{ width: `${percent}%` }}
      />
    </div>
  );
}

/**
 * SDN 네트워크의 오른쪽 서랍(네트워크에는 상세 화면이 없다): 서브넷 편집(`PATCH`는 목록 전체, `If-Match`)과 주소 표.
 * `writes`는 그 네트워크의 `network.config`다.
 */
export function NetworkSheet({
  network,
  writes,
  onClose,
}: {
  network: Network;
  writes: boolean;
  onClose(): void;
}) {
  const { t } = useTranslation();
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-full overflow-y-auto sm:max-w-3xl" aria-describedby={undefined}>
        <SheetTitle className="px-4 pt-4 text-lg">
          {t("sdn.sheet.title", { name: network.name })}
        </SheetTitle>
        <div className="flex flex-col gap-6 px-4 pb-4">
          {!isProvider(network) && <RouterSection network={network} writes={writes} />}
          <SubnetsSection network={network} writes={writes} />
          <IpsSection network={network} />
        </div>
      </SheetContent>
    </Sheet>
  );
}

/**
 * 이 네트워크가 인터페이스인 라우터와 "라우터에서 떼기"(계획 66 넘김 — `DELETE /networks/{key}/router`): 네트워크의
 * `network.config`면 라우터를 읽을 수 없어도 뗀다(권한을 거둔 다른 폴더의 라우터가 기본 경로를 쥐고 있을 때). 유동 IP·부하
 * 분산기 VIP가 그 길을 쓰면 서버가 409 `fip_in_use`·`lb_in_use`다.
 */
function RouterSection({ network, writes }: { network: Network; writes: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const routed = useRouted();
  const routers = useRouters();
  const [detaching, setDetaching] = useState(false);
  if (!routedOf(routed.data, network.name)) {
    return null;
  }
  const router = routerOf(routers.data, network.name);
  return (
    <section aria-labelledby="sdn-router" className="flex flex-col gap-2">
      <h2 id="sdn-router" className="font-medium">
        {t("sdn.sheet.router")}
      </h2>
      <div className="flex flex-wrap items-center gap-3 text-sm">
        {router ? (
          <Link to={`/routers?open=${encodeURIComponent(router.id)}`} className="underline">
            {router.name}
          </Link>
        ) : (
          <span className="text-muted-foreground">{t("sdn.sheet.hiddenRouter")}</span>
        )}
        {writes && (
          <Button variant="outline" size="sm" onClick={() => setDetaching(true)}>
            {t("sdn.sheet.detach")}
          </Button>
        )}
      </div>
      <ConfirmDialog
        open={detaching}
        onOpenChange={setDetaching}
        title={t("sdn.sheet.detachTitle", { name: network.name })}
        description={t("sdn.sheet.detachDescription")}
        confirmLabel={t("sdn.sheet.detach")}
        onConfirm={async () => {
          await call(
            api.DELETE("/networks/{key}/router", { params: { path: { key: network.id } } }),
          );
          await refreshRouting(queryClient);
          toast.success(t("sdn.sheet.detached", { name: network.name }));
        }}
      />
    </section>
  );
}

/** 고치는 서브넷 목록에 새로 더한 CIDR이 있는지(저장된 것과 견줌, 앞뒤 공백 무시) */
export function addsSubnets(saved: readonly SubnetRow[], rows: readonly SubnetRow[]): boolean {
  const known = new Set(saved.map((r) => r.cidr.trim()));
  return rows.some((r) => r.cidr.trim() !== "" && !known.has(r.cidr.trim()));
}

function SubnetsSection({ network, writes }: { network: Network; writes: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 고치기 시작한 리비전(If-Match). 그 사이 다른 사람이 바꿨으면 412 revision_mismatch다.
  const [editing, setEditing] = useState<{ revision: number; rows: SubnetRow[] } | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // 공급자 네트워크(계획 66): DHCP 기본 끔, IPv4 서브넷의 유동 IP 풀
  const provider = isProvider(network);
  const saved = (network.spec.subnets ?? []).map((s) => subnetRow(s, provider));
  // 서브넷을 더하면 서버가 주소 없는 인터페이스에 새 서브넷의 주소를 준다(리뷰 2 웹 C-1). 고치는 동안만 센다(OVN은 읽지
  // 않는 목록용 질의).
  const usage = useNetworkIps(network, editing !== null, false);
  const adding = editing !== null && addsSubnets(saved, editing.rows);
  const bare = usage.data?.allocations.filter((a) => a.ips.length === 0).length ?? 0;

  async function save() {
    if (!editing) {
      return;
    }
    const found = subnetErrors(editing.rows, provider);
    setErrors(found);
    setFailure(null);
    if (Object.keys(found).length > 0) {
      return;
    }
    setBusy(true);
    try {
      const updated = await call(
        api.PATCH("/networks/{key}", {
          params: { path: { key: network.id }, header: { "If-Match": editing.revision } },
          body: { subnets: editing.rows.map((row) => toSubnet(row, provider)) },
        }),
      );
      putResource(queryClient, "network", updated);
      await queryClient.invalidateQueries({ queryKey: ipsKey(network.id) });
      toast.success(t("sdn.subnets.saved", { name: network.name }));
      setEditing(null);
    } catch (err) {
      if (err instanceof ApiError && err.code === "revision_mismatch") {
        // 그새 누가 서브넷을 바꿨다(412): 지금 리비전을 다시 읽어 입력한 행은 두고 다시 저장하게 한다(리뷰 2 웹 M-1 —
        // 옛 리비전으로는 저장이 늘 412였다).
        try {
          const fresh = await call(
            api.GET("/networks/{key}", { params: { path: { key: network.id } } }),
          );
          putResource(queryClient, "network", fresh);
          setEditing({ revision: fresh.revision, rows: editing.rows });
          setFailure(
            t("sdn.subnets.conflict", {
              subnets:
                (fresh.spec.subnets ?? []).map((s) => s.cidr).join(", ") ||
                t("sdn.subnets.noneShort"),
            }),
          );
        } catch (again) {
          setFailure(errorMessage(t, again));
        }
        return;
      }
      // 409 subnet_in_use: 서버가 그 서브넷의 주소를 쓰는 VM 이름을 말한다. 409 ip_exhausted: 주소 없는 인터페이스 수와
      // 서브넷마다 남은 수.
      setFailure(errorMessage(t, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-labelledby="sdn-subnets" className="flex flex-col gap-3">
      <div className="flex items-center justify-between gap-2">
        <h2 id="sdn-subnets" className="font-medium">
          {t("sdn.subnets.title")}
        </h2>
        {writes && !editing && (
          <Button
            variant="outline"
            size="sm"
            onClick={() => setEditing({ revision: network.revision, rows: saved })}
          >
            {t("sdn.subnets.edit")}
          </Button>
        )}
      </div>
      {editing ? (
        <>
          <p className="text-xs text-muted-foreground">{t("sdn.subnets.editHint")}</p>
          {adding && bare > 0 && (
            <p role="status" className="text-sm text-warning-fg">
              {t("sdn.subnets.fillNotice", { count: bare })}
            </p>
          )}
          <SubnetsEditor
            id="sheet-subnet"
            rows={editing.rows}
            errors={errors}
            disabled={busy}
            provider={provider}
            onChange={(rows) => setEditing({ ...editing, rows })}
          />
          {failure && (
            <p role="alert" className="text-sm text-destructive">
              {failure}
            </p>
          )}
          <div className="flex gap-2">
            <Button disabled={busy} onClick={() => void save()}>
              {t("common.save")}
            </Button>
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => {
                setEditing(null);
                setErrors({});
                setFailure(null);
              }}
            >
              {t("common.cancel")}
            </Button>
          </div>
        </>
      ) : saved.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("sdn.subnets.none")}</p>
      ) : (
        <Table aria-labelledby="sdn-subnets">
          <TableHeader>
            <TableRow>
              <TableHead>{t("sdn.subnets.cidr")}</TableHead>
              <TableHead>{t("sdn.subnets.gateway")}</TableHead>
              <TableHead>{t("sdn.subnets.dhcp")}</TableHead>
              <TableHead>{t("sdn.subnets.dns")}</TableHead>
              <TableHead>{t("sdn.subnets.pools")}</TableHead>
              {provider && <TableHead>{t("sdn.subnets.floating")}</TableHead>}
              <TableHead>{t("sdn.subnets.ipv6Mode")}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {(network.spec.subnets ?? []).map((s) => (
              <TableRow key={s.cidr}>
                <TableCell className="font-mono text-xs">{s.cidr}</TableCell>
                <TableCell className="font-mono text-xs">
                  {s.gateway ?? t("sdn.subnets.firstHost")}
                </TableCell>
                <TableCell>{t(subnetRow(s, provider).dhcp ? "nodes.yes" : "nodes.no")}</TableCell>
                <TableCell className="font-mono text-xs">
                  {(s.dns_servers ?? []).join(", ") || "—"}
                </TableCell>
                <TableCell className="font-mono text-xs">
                  {(s.pools ?? []).map((p) => `${p.start}-${p.end}`).join(", ") ||
                    t("sdn.subnets.wholeRange")}
                </TableCell>
                {provider && (
                  <TableCell className="font-mono text-xs">
                    {(s.floating_pools ?? []).map((p) => `${p.start}-${p.end}`).join(", ") ||
                      (s.cidr.includes(":") ? "—" : t("sdn.subnets.floatingFromPools"))}
                  </TableCell>
                )}
                <TableCell>{s.ipv6_mode ? t(`sdn.ipv6Mode.${s.ipv6_mode}`) : "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </section>
  );
}

function IpsSection({ network }: { network: Network }) {
  // 서브넷이 있는 네트워크의 주소 없는 NIC는 포트가 꺼져 있다(리뷰 2 웹 I-1).
  const subnets = (network.spec.subnets ?? []).length > 0;
  const { t } = useTranslation();
  const ips = useNetworkIps(network);
  const data: NetworkIps | undefined = ips.data;
  return (
    <section aria-labelledby="sdn-ips" className="flex flex-col gap-3">
      <h2 id="sdn-ips" className="font-medium">
        {t("sdn.ips.title")}
      </h2>
      {!data ? (
        <ListPending error={ips.error} />
      ) : (
        <>
          {data.subnets.length > 0 && (
            <ul className="flex flex-col gap-1 text-sm">
              {data.subnets.map((s) => (
                <li key={s.cidr} className="flex flex-wrap items-center gap-2">
                  <span className="font-mono text-xs">{s.cidr}</span>
                  {hasUsageBar(s.size) && <UsageBar used={s.used} size={s.size} />}
                  <span className="text-muted-foreground">
                    {t("sdn.ips.used", {
                      used: s.used,
                      size: addressCount(s.size),
                      free: addressCount(s.free),
                    })}
                  </span>
                  {s.floating && (
                    <span className="text-muted-foreground">
                      {t("sdn.ips.floating", {
                        used: s.floating.used,
                        size: addressCount(s.floating.size),
                        free: addressCount(s.floating.free),
                      })}
                    </span>
                  )}
                </li>
              ))}
            </ul>
          )}
          {data.live_error && (
            <p role="status" className="text-sm text-warning-fg">
              {t("sdn.ips.liveError", { error: data.live_error })}
            </p>
          )}
          <Table aria-labelledby="sdn-ips">
            <TableHeader>
              <TableRow>
                <TableHead>{t("sdn.ips.vm")}</TableHead>
                <TableHead>{t("sdn.ips.nic")}</TableHead>
                <TableHead>{t("vms.mac")}</TableHead>
                <TableHead>{t("sdn.ips.addresses")}</TableHead>
                <TableHead>{t("sdn.ips.port")}</TableHead>
                <TableHead>{t("sdn.ips.node")}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.allocations.length === 0 && !data.hidden && (
                <TableRow>
                  <TableCell colSpan={6} className="text-muted-foreground">
                    {t("sdn.ips.empty")}
                  </TableCell>
                </TableRow>
              )}
              {data.allocations.map((a) => (
                <TableRow key={`${a.vm}-${a.nic}`}>
                  <TableCell>
                    {/* 라우터 외부 주소·유동 IP 행(계획 66)은 `vm`·`vm_name`이 그 리소스의 것이다. */}
                    {a.kind === "router" ? (
                      <Link to="/routers" className="underline">
                        {t("sdn.ips.router", { name: a.vm_name })}
                      </Link>
                    ) : a.kind === "floating-ip" ? (
                      <Link to="/floating-ips" className="underline">
                        {a.target
                          ? t("sdn.ips.fipTo", { name: a.vm_name, vm: a.target })
                          : t("sdn.ips.fip", { name: a.vm_name })}
                      </Link>
                    ) : a.kind === "lb-vip" || a.kind === "lb-monitor" ? (
                      // 부하 분산기의 VIP·바깥 VIP·건강 검사 주소(계획 67): `vm`·`vm_name`은 그 부하 분산기의 것이다.
                      <Link
                        to={`/load-balancers?open=${encodeURIComponent(a.vm)}`}
                        className="underline"
                      >
                        {t(a.kind === "lb-vip" ? "sdn.ips.lbVip" : "sdn.ips.lbMonitor", {
                          name: a.vm_name,
                        })}
                      </Link>
                    ) : (
                      <Link to={`/vms/${encodeURIComponent(a.vm_name)}`} className="underline">
                        {a.vm_name}
                      </Link>
                    )}
                  </TableCell>
                  <TableCell>
                    {a.kind && a.kind !== "nic" ? "—" : t("sdn.ips.nicN", { n: a.nic + 1 })}
                  </TableCell>
                  <TableCell className="font-mono text-xs">{a.mac ?? "—"}</TableCell>
                  <TableCell className="font-mono text-xs">
                    {formatIps(a.ips) || t("sdn.ips.noAddress")}
                  </TableCell>
                  <TableCell>
                    {/* OVN을 읽지 못했으면(live null) 모른다. */}
                    {subnets && a.ips.length === 0 ? (
                      <PortOffChip />
                    ) : a.live == null ? (
                      "—"
                    ) : (
                      <StatusChip tone={a.live.up ? "success" : "neutral"}>
                        {t(a.live.up ? "sdn.ips.up" : "sdn.ips.down")}
                      </StatusChip>
                    )}
                  </TableCell>
                  <TableCell>{a.live?.node ?? "—"}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
          {/* 다른 폴더 VM의 NIC는 서버가 빼고 수만 준다(리뷰 1 #3). */}
          {!!data.hidden && (
            <p className="text-sm text-muted-foreground">
              {t("sdn.ips.hidden", { count: data.hidden })}
            </p>
          )}
        </>
      )}
    </section>
  );
}
