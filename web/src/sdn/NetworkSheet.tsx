import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import { toast } from "sonner";

import { api, call } from "@/api/client";
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
import { formatIps } from "@/sdn/ips";
import type { NetworkIps } from "@/sdn/sdn";
import { SubnetsEditor } from "@/sdn/SubnetsEditor";
import { subnetErrors, subnetRow, type SubnetRow, toSubnet } from "@/sdn/subnets";

/** `GET /networks/{key}/ips`의 캐시 키 */
export function ipsKey(id: string) {
  return ["network-ips", id] as const;
}

/** SDN 네트워크의 주소(서브넷 사용·NIC 주소·OVN 포트 상태). `live`는 OVN을 읽지 못했으면 null이다. */
export function useNetworkIps(network: Network, enabled = true) {
  return useQuery({
    queryKey: ipsKey(network.id),
    queryFn: () => call(api.GET("/networks/{key}/ips", { params: { path: { key: network.id } } })),
    enabled,
    staleTime: 10_000,
  });
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
          <SubnetsSection network={network} writes={writes} />
          <IpsSection network={network} />
        </div>
      </SheetContent>
    </Sheet>
  );
}

function SubnetsSection({ network, writes }: { network: Network; writes: boolean }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 고치기 시작한 리비전(If-Match). 그 사이 다른 사람이 바꿨으면 409 revision_mismatch다.
  const [editing, setEditing] = useState<{ revision: number; rows: SubnetRow[] } | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const saved = (network.spec.subnets ?? []).map(subnetRow);

  async function save() {
    if (!editing) {
      return;
    }
    const found = subnetErrors(editing.rows);
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
          body: { subnets: editing.rows.map(toSubnet) },
        }),
      );
      putResource(queryClient, "network", updated);
      await queryClient.invalidateQueries({ queryKey: ipsKey(network.id) });
      toast.success(t("sdn.subnets.saved", { name: network.name }));
      setEditing(null);
    } catch (err) {
      // 409 subnet_in_use: 서버가 그 서브넷의 주소를 쓰는 VM 이름을 말한다.
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
          <SubnetsEditor
            id="sheet-subnet"
            rows={editing.rows}
            errors={errors}
            disabled={busy}
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
            </TableRow>
          </TableHeader>
          <TableBody>
            {(network.spec.subnets ?? []).map((s) => (
              <TableRow key={s.cidr}>
                <TableCell className="font-mono text-xs">{s.cidr}</TableCell>
                <TableCell className="font-mono text-xs">
                  {s.gateway ?? t("sdn.subnets.firstHost")}
                </TableCell>
                <TableCell>{t(subnetRow(s).dhcp ? "nodes.yes" : "nodes.no")}</TableCell>
                <TableCell className="font-mono text-xs">
                  {(s.dns_servers ?? []).join(", ") || "—"}
                </TableCell>
                <TableCell className="font-mono text-xs">
                  {(s.pools ?? []).map((p) => `${p.start}-${p.end}`).join(", ") ||
                    t("sdn.subnets.wholeRange")}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </section>
  );
}

function IpsSection({ network }: { network: Network }) {
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
                  <UsageBar used={s.used} size={s.size} />
                  <span className="text-muted-foreground">
                    {t("sdn.ips.used", { used: s.used, size: s.size, free: s.free })}
                  </span>
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
              {data.allocations.length === 0 && (
                <TableRow>
                  <TableCell colSpan={6} className="text-muted-foreground">
                    {t("sdn.ips.empty")}
                  </TableCell>
                </TableRow>
              )}
              {data.allocations.map((a) => (
                <TableRow key={`${a.vm}-${a.nic}`}>
                  <TableCell>
                    <Link to={`/vms/${encodeURIComponent(a.vm_name)}`} className="underline">
                      {a.vm_name}
                    </Link>
                  </TableCell>
                  <TableCell>{t("sdn.ips.nicN", { n: a.nic })}</TableCell>
                  <TableCell className="font-mono text-xs">{a.mac ?? "—"}</TableCell>
                  <TableCell className="font-mono text-xs">
                    {formatIps(a.ips) || t("sdn.ips.noAddress")}
                  </TableCell>
                  <TableCell>
                    {/* OVN을 읽지 못했으면(live null) 모른다. */}
                    {a.live == null ? (
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
        </>
      )}
    </section>
  );
}
