import { Plus, X } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Field } from "@/components/Field";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { emptySubnetRow, type Ipv6ModeValue, rowFamily, type SubnetRow } from "@/sdn/subnets";

/** 서버 `Ipv6Mode`(계획 66) */
const IPV6_MODES = ["slaac", "dhcpv6-stateful", "dhcpv6-stateless"] as const;

/** 서버는 주소 체계마다 서브넷 하나다. */
const MAX_SUBNETS = 2;

/**
 * SDN 네트워크의 서브넷 표(만들기 대화상자와 서랍). 저장된 서브넷은 CIDR·게이트웨이를 바꿀 수 없다. `id`는 칸 id의
 * 앞이다. `provider`면 공급자(외부) 네트워크다: IPv4 서브넷에 유동 IP 풀 칸이 있고 DHCP는 기본 끔이다(계획 66).
 */
export function SubnetsEditor({
  id,
  rows,
  errors,
  disabled,
  provider = false,
  onChange,
}: {
  id: string;
  rows: SubnetRow[];
  errors: Record<string, string>;
  disabled?: boolean;
  provider?: boolean;
  onChange(rows: SubnetRow[]): void;
}) {
  const { t } = useTranslation();
  const set = (i: number, patch: Partial<SubnetRow>) =>
    onChange(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div role="group" aria-label={t("sdn.subnets.title")} className="flex flex-col gap-3">
      <span className="text-sm font-medium">{t("sdn.subnets.title")}</span>
      {rows.length === 0 && (
        <p className="text-xs text-muted-foreground">{t("sdn.subnets.none")}</p>
      )}
      {rows.map((row, i) => {
        const at = (field: string) => `${id}-${i}-${field}`;
        const err = (field: string) => errors[`subnets.${i}.${field}`];
        const v4 = rowFamily(row) !== 6;
        return (
          <fieldset
            key={i}
            disabled={disabled}
            aria-label={t("sdn.subnets.n", { n: i + 1 })}
            className="grid grid-cols-1 items-end gap-2 rounded-md border p-3 sm:grid-cols-2"
          >
            <Field id={at("cidr")} label={t("sdn.subnets.cidr")} error={err("cidr")}>
              {(a11y) => (
                <Input
                  id={at("cidr")}
                  placeholder="10.10.0.0/24"
                  disabled={row.saved}
                  value={row.cidr}
                  onChange={(e) => set(i, { cidr: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id={at("gateway")}
              label={t("sdn.subnets.gateway")}
              error={err("gateway")}
              hint={row.saved ? undefined : t("sdn.subnets.gatewayHint")}
            >
              {(a11y) => (
                <Input
                  id={at("gateway")}
                  disabled={row.saved}
                  value={row.gateway}
                  onChange={(e) => set(i, { gateway: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id={at("dns")}
              label={t("sdn.subnets.dns")}
              error={err("dns")}
              hint={t("sdn.subnets.dnsHint")}
            >
              {(a11y) => (
                <Input
                  id={at("dns")}
                  value={row.dns}
                  onChange={(e) => set(i, { dns: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <Field
              id={at("pools")}
              label={t("sdn.subnets.pools")}
              error={err("pools")}
              hint={t("sdn.subnets.poolsHint")}
            >
              {(a11y) => (
                <Input
                  id={at("pools")}
                  placeholder="10.10.0.100-10.10.0.199"
                  value={row.pools}
                  onChange={(e) => set(i, { pools: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            {provider && v4 && (
              <Field
                id={at("floating")}
                label={t("sdn.subnets.floating")}
                error={err("floating")}
                hint={t("sdn.subnets.floatingHint")}
              >
                {(a11y) => (
                  <Input
                    id={at("floating")}
                    placeholder="203.0.113.100-203.0.113.199"
                    value={row.floating}
                    onChange={(e) => set(i, { floating: e.target.value })}
                    {...a11y}
                  />
                )}
              </Field>
            )}
            {!v4 && (
              <Field
                id={at("ipv6Mode")}
                label={t("sdn.subnets.ipv6Mode")}
                error={err("ipv6Mode")}
                hint={t("sdn.subnets.ipv6ModeHint")}
              >
                {(a11y) => (
                  <NativeSelect
                    id={at("ipv6Mode")}
                    className="w-full"
                    value={row.ipv6Mode}
                    onChange={(e) => set(i, { ipv6Mode: e.target.value as Ipv6ModeValue })}
                    {...a11y}
                  >
                    <NativeSelectOption value="">{t("sdn.ipv6Mode.none")}</NativeSelectOption>
                    {IPV6_MODES.map((m) => (
                      <NativeSelectOption key={m} value={m}>
                        {t(`sdn.ipv6Mode.${m}`)}
                      </NativeSelectOption>
                    ))}
                  </NativeSelect>
                )}
              </Field>
            )}
            <Field
              id={at("description")}
              label={t("sdn.subnets.description")}
              error={err("description")}
            >
              {(a11y) => (
                <Input
                  id={at("description")}
                  value={row.description}
                  onChange={(e) => set(i, { description: e.target.value })}
                  {...a11y}
                />
              )}
            </Field>
            <div className="flex items-center justify-between gap-2">
              {v4 ? (
                <div className="flex items-center gap-2">
                  <Checkbox
                    id={at("dhcp")}
                    checked={row.dhcp}
                    onCheckedChange={(on) => set(i, { dhcp: on === true })}
                  />
                  <Label htmlFor={at("dhcp")}>{t("sdn.subnets.dhcp")}</Label>
                </div>
              ) : (
                <span className="text-xs text-muted-foreground">{t("sdn.subnets.noDhcp6")}</span>
              )}
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={t("sdn.subnets.remove", { n: i + 1 })}
                onClick={() => onChange(rows.filter((_, j) => j !== i))}
              >
                <X />
              </Button>
            </div>
          </fieldset>
        );
      })}
      {rows.length < MAX_SUBNETS && !disabled && (
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => onChange([...rows, emptySubnetRow(provider)])}
          >
            <Plus />
            {t("sdn.subnets.add")}
          </Button>
        </div>
      )}
    </div>
  );
}
