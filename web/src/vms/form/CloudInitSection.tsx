import { useTranslation } from "react-i18next";

import { Field } from "@/components/Field";
import { InfoButton } from "@/components/InfoButton";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  type CloudInitValues,
  DEFAULT_NET,
  type NetRow,
  userDataWarning,
  validateKeys,
} from "@/vms/cloudInit";
import type { VmFormValues } from "@/vms/form";

const TEXTAREA =
  "min-h-20 w-full rounded-lg border border-input bg-transparent px-2.5 py-1.5 font-mono text-xs outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 aria-invalid:border-destructive";

/**
 * cloud-init(NoCloud 씨앗): 호스트 이름·사용자·비밀번호·SSH 키·NIC마다 주소·DNS와 "고급"의 user-data·vendor-data.
 * 저장된 비밀번호는 응답에서 가려지므로 "설정됨"으로 보이고, 바꾸지 않으면 가림 값을 되보내 그대로 둔다. 바꾼 설정은
 * 다음 QEMU 시작 때 적용된다.
 */
export function CloudInitSection({
  values,
  errors,
  onChange,
  running,
  sdn = [],
}: {
  /** VM 폼의 값. 배포 창은 이름(패턴)·NIC·cloud-init만 준다. */
  values: Pick<VmFormValues, "name" | "nics" | "cloudInit">;
  errors: Record<string, string>;
  onChange: (patch: Pick<VmFormValues, "cloudInit">) => void;
  /** 실행 중인 VM이다(바꾸면 다시 시작할 때 적용) */
  running: boolean;
  /** NIC마다 SDN 네트워크인지(계획 65 — 그 NIC의 주소는 IPAM이 정해 칸이 없다) */
  sdn?: readonly boolean[];
}) {
  const { t } = useTranslation();
  const ci = values.cloudInit;
  const set = (patch: Partial<CloudInitValues>) =>
    onChange({ cloudInit: { ...values.cloudInit, ...patch } });
  const setNet = (i: number, patch: Partial<NetRow>) => {
    const networks = Array.from(
      { length: Math.max(ci.networks.length, i + 1) },
      (_, j) => ci.networks[j] ?? DEFAULT_NET,
    );
    networks[i] = { ...networks[i]!, ...patch };
    set({ networks });
  };
  const keyError = validateKeys(ci.sshKeys);
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("vmForm.cloudInit")}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div className="flex items-center gap-2">
          <Checkbox
            id="ci-enabled"
            checked={ci.enabled}
            onCheckedChange={(v) => set({ enabled: v === true })}
          />
          <Label htmlFor="ci-enabled">{t("vmForm.ciEnabled")}</Label>
          <InfoButton topic="vmForm.cloudInit" label={t("vmForm.cloudInit")} />
        </div>
        <p className="text-sm text-muted-foreground">
          {running ? t("vmForm.ciRunning") : t("vmForm.ciNote")}
        </p>
        {ci.enabled && (
          <>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field
                id="ci-hostname"
                label={t("vmForm.ciHostname")}
                error={errors["ci.hostname"]}
                help={t("vmForm.ciHostnameHint", { name: values.name || "—" })}
              >
                {(a11y) => (
                  <Input
                    id="ci-hostname"
                    {...a11y}
                    value={ci.hostname}
                    onChange={(e) => set({ hostname: e.target.value })}
                  />
                )}
              </Field>
              <Field
                id="ci-user"
                label={t("vmForm.ciUser")}
                error={errors["ci.user"]}
                help={t("vmForm.ciUserHint")}
              >
                {(a11y) => (
                  <Input
                    id="ci-user"
                    {...a11y}
                    value={ci.user}
                    onChange={(e) => set({ user: e.target.value })}
                  />
                )}
              </Field>
            </div>
            {ci.passwordSet && ci.password === "" ? (
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <span>{t("vmForm.ciPasswordSet")}</span>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => set({ passwordSet: false })}
                >
                  {t("vmForm.ciPasswordChange")}
                </Button>
              </div>
            ) : (
              <Field
                id="ci-password"
                label={t("vmForm.ciPassword")}
                error={errors["ci.password"]}
                help={t("vmForm.ciPasswordHint")}
              >
                {(a11y) => (
                  <Input
                    id="ci-password"
                    type="password"
                    autoComplete="new-password"
                    {...a11y}
                    value={ci.password}
                    onChange={(e) => set({ password: e.target.value })}
                  />
                )}
              </Field>
            )}
            <Field
              id="ci-keys"
              label={t("vmForm.ciKeys")}
              error={errors["ci.sshKeys"]}
              errorValues={keyError?.values}
              help={t("vmForm.ciKeysHint")}
            >
              {(a11y) => (
                <textarea
                  id="ci-keys"
                  {...a11y}
                  className={TEXTAREA}
                  value={ci.sshKeys}
                  onChange={(e) => set({ sshKeys: e.target.value })}
                />
              )}
            </Field>
            {values.nics.map((_, i) => {
              const net = ci.networks[i] ?? DEFAULT_NET;
              if (sdn[i]) {
                return (
                  <fieldset
                    key={i}
                    aria-label={t("vmForm.ciNic", { n: i + 1 })}
                    className="rounded-md border p-3 text-sm text-muted-foreground"
                  >
                    {t("vmForm.ciSdn", { n: i + 1 })}
                  </fieldset>
                );
              }
              return (
                <fieldset
                  key={i}
                  aria-label={t("vmForm.ciNic", { n: i + 1 })}
                  className="grid grid-cols-1 items-end gap-2 rounded-md border p-3 sm:grid-cols-3"
                >
                  <Field
                    id={`ci-${i}-ipv4`}
                    label={t("vmForm.ciIpv4", { n: i })}
                    error={errors[`ci.networks.${i}.ipv4`]}
                    info="vmForm.ciIpMode"
                  >
                    <NativeSelect
                      id={`ci-${i}-ipv4`}
                      className="w-full"
                      value={net.ipv4}
                      onChange={(e) => setNet(i, { ipv4: e.target.value as NetRow["ipv4"] })}
                    >
                      <NativeSelectOption value="none">{t("vmForm.ciIpv4None")}</NativeSelectOption>
                      <NativeSelectOption value="dhcp">DHCP</NativeSelectOption>
                      <NativeSelectOption value="static">{t("vmForm.ciStatic")}</NativeSelectOption>
                    </NativeSelect>
                  </Field>
                  {net.ipv4 === "static" ? (
                    <>
                      <Field
                        id={`ci-${i}-ipv4-address`}
                        label={t("vmForm.ciAddress")}
                        error={errors[`ci.networks.${i}.ipv4Address`]}
                        info="vmForm.ciAddress"
                      >
                        {(a11y) => (
                          <Input
                            id={`ci-${i}-ipv4-address`}
                            {...a11y}
                            placeholder="10.0.0.5/24"
                            value={net.ipv4Address}
                            onChange={(e) => setNet(i, { ipv4Address: e.target.value })}
                          />
                        )}
                      </Field>
                      <Field
                        id={`ci-${i}-ipv4-gateway`}
                        label={t("vmForm.ciGateway")}
                        error={errors[`ci.networks.${i}.ipv4Gateway`]}
                        info="vmForm.ciGateway"
                      >
                        {(a11y) => (
                          <Input
                            id={`ci-${i}-ipv4-gateway`}
                            {...a11y}
                            value={net.ipv4Gateway}
                            onChange={(e) => setNet(i, { ipv4Gateway: e.target.value })}
                          />
                        )}
                      </Field>
                    </>
                  ) : (
                    <div className="sm:col-span-2" />
                  )}
                  <Field
                    id={`ci-${i}-ipv6`}
                    label={t("vmForm.ciIpv6", { n: i })}
                    info="vmForm.ciIpMode"
                  >
                    <NativeSelect
                      id={`ci-${i}-ipv6`}
                      className="w-full"
                      value={net.ipv6}
                      onChange={(e) => setNet(i, { ipv6: e.target.value as NetRow["ipv6"] })}
                    >
                      <NativeSelectOption value="none">{t("vmForm.ciIpv6None")}</NativeSelectOption>
                      <NativeSelectOption value="dhcp">DHCPv6</NativeSelectOption>
                      <NativeSelectOption value="slaac">SLAAC</NativeSelectOption>
                      <NativeSelectOption value="static">{t("vmForm.ciStatic")}</NativeSelectOption>
                    </NativeSelect>
                  </Field>
                  {net.ipv6 === "static" && (
                    <>
                      <Field
                        id={`ci-${i}-ipv6-address`}
                        label={t("vmForm.ciAddress6")}
                        error={errors[`ci.networks.${i}.ipv6Address`]}
                        info="vmForm.ciAddress"
                      >
                        {(a11y) => (
                          <Input
                            id={`ci-${i}-ipv6-address`}
                            {...a11y}
                            placeholder="2001:db8::5/64"
                            value={net.ipv6Address}
                            onChange={(e) => setNet(i, { ipv6Address: e.target.value })}
                          />
                        )}
                      </Field>
                      <Field
                        id={`ci-${i}-ipv6-gateway`}
                        label={t("vmForm.ciGateway6")}
                        error={errors[`ci.networks.${i}.ipv6Gateway`]}
                        info="vmForm.ciGateway"
                      >
                        {(a11y) => (
                          <Input
                            id={`ci-${i}-ipv6-gateway`}
                            {...a11y}
                            value={net.ipv6Gateway}
                            onChange={(e) => setNet(i, { ipv6Gateway: e.target.value })}
                          />
                        )}
                      </Field>
                    </>
                  )}
                </fieldset>
              );
            })}
            <div className="grid gap-4 sm:grid-cols-2">
              <Field
                id="ci-dns"
                label={t("vmForm.ciDns")}
                error={errors["ci.dns"]}
                help={t("vmForm.ciListHint")}
              >
                {(a11y) => (
                  <Input
                    id="ci-dns"
                    {...a11y}
                    value={ci.dns}
                    onChange={(e) => set({ dns: e.target.value })}
                  />
                )}
              </Field>
              <Field
                id="ci-search"
                label={t("vmForm.ciSearch")}
                error={errors["ci.search"]}
                help={t("vmForm.ciListHint")}
              >
                {(a11y) => (
                  <Input
                    id="ci-search"
                    {...a11y}
                    value={ci.search}
                    onChange={(e) => set({ search: e.target.value })}
                  />
                )}
              </Field>
            </div>
            <details
              className="flex flex-col gap-3"
              open={ci.userData !== "" || ci.vendorData !== ""}
            >
              <summary className="cursor-pointer text-sm font-medium">
                {t("vmForm.ciAdvanced")}
              </summary>
              <div className="mt-3 flex flex-col gap-3">
                <Field
                  id="ci-user-data"
                  label={t("vmForm.ciUserData")}
                  error={errors["ci.userData"]}
                  help={t("vmForm.ciUserDataHint")}
                  info="vmForm.ciUserData"
                >
                  {(a11y) => (
                    <textarea
                      id="ci-user-data"
                      {...a11y}
                      className={TEXTAREA}
                      value={ci.userData}
                      onChange={(e) => set({ userData: e.target.value })}
                    />
                  )}
                </Field>
                <Field
                  id="ci-vendor-data"
                  label={t("vmForm.ciVendorData")}
                  error={errors["ci.vendorData"]}
                  help={t("vmForm.ciVendorDataHint")}
                >
                  {(a11y) => (
                    <textarea
                      id="ci-vendor-data"
                      {...a11y}
                      className={TEXTAREA}
                      value={ci.vendorData}
                      onChange={(e) => set({ vendorData: e.target.value })}
                    />
                  )}
                </Field>
              </div>
            </details>
            {userDataWarning(ci) && (
              <p role="status" className="text-sm text-warning-fg">
                {t("vmForm.ciUserDataWarning")}
              </p>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
