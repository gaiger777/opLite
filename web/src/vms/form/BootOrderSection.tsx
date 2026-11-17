import { ArrowDown, ArrowUp } from "lucide-react";
import { useTranslation } from "react-i18next";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { type BootDevice, bootList, deviceName } from "@/vms/boot";
import { bootOrders, type VmFormValues } from "@/vms/form";

/**
 * 부팅 순서: 디스크와 CD-ROM을 한 목록에서 위·아래로 옮기고, 디스크는 "부팅 안 함"으로 뺀다. 값을 정하지 않은 장치는
 * 서버의 기본 규칙(`effectiveBoot` — 디스크 다음 CD-ROM)대로 보이고, 한 번이라도 바꾸면 목록 순서대로 1, 2, …를 적는다.
 * CD-ROM은 늘 부팅 장치다(서버가 값 없는 드라이브에도 순서를 준다). 디스크를 모두 빼면 서버가 디스크를 CD-ROM 뒤에
 * 차례로 둔다(매체를 뺀 뒤 디스크로 켜지게). 실행 중에는 바꾸지 못한다(디스크 순서는 서버의 409).
 */
export function BootOrderSection({
  values,
  errors,
  onChange,
  locked,
}: {
  values: VmFormValues;
  errors: Record<string, string>;
  onChange: (patch: Partial<VmFormValues>) => void;
  locked: boolean;
}) {
  const { t } = useTranslation();
  const orders = bootOrders(values);
  const { booting, off } = bootList(orders.disks, orders.cdroms);
  const label = (d: BootDevice) =>
    d.kind === "disk"
      ? t("vmForm.bootDisk", {
          name: deviceName(d),
          volume: values.disks[d.index]?.volume || "—",
        })
      : t("vmForm.bootCdrom", {
          name: deviceName(d),
          iso: values.cdroms[d.index]?.iso || t("vmForm.cdromEmpty"),
        });

  /** 목록 순서대로 1, 2, …를 적고, 목록에 없는 디스크는 부팅하지 않는다. */
  function apply(order: BootDevice[]) {
    const disks = values.disks.map((d) => ({ ...d, bootOrder: "" }));
    const cdroms = values.cdroms.map((c) => ({ ...c, bootOrder: "" }));
    order.forEach((d, i) => {
      const rows = d.kind === "disk" ? disks : cdroms;
      rows[d.index] = { ...rows[d.index]!, bootOrder: String(i + 1) };
    });
    onChange({ disks, cdroms });
  }
  const move = (i: number, by: number) => {
    const order = [...booting];
    [order[i], order[i + by]] = [order[i + by]!, order[i]!];
    apply(order);
  };
  const rowErrors = [
    ...values.disks.map((_, i) => errors[`disks.${i}.bootOrder`]),
    ...values.cdroms.map((_, i) => errors[`cdroms.${i}.bootOrder`]),
  ].filter((e): e is string => e !== undefined);

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2 id="vm-boot-order">{t("vmForm.bootOrder")}</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-sm text-muted-foreground">
          {locked ? t("vmForm.bootOrderRunning") : t("vmForm.bootOrderNote")}
        </p>
        {booting.length === 0 && (
          <p className="text-sm text-muted-foreground">{t("vmForm.bootNone")}</p>
        )}
        <ol aria-labelledby="vm-boot-order" className="flex flex-col gap-1">
          {booting.map((d, i) => (
            <li
              key={deviceName(d)}
              className="flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm"
            >
              <span className="w-6 text-muted-foreground">{i + 1}.</span>
              <span className="flex-1">{label(d)}</span>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("vmForm.bootUp", { name: deviceName(d) })}
                disabled={locked || i === 0}
                onClick={() => move(i, -1)}
              >
                <ArrowUp />
              </Button>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={t("vmForm.bootDown", { name: deviceName(d) })}
                disabled={locked || i === booting.length - 1}
                onClick={() => move(i, 1)}
              >
                <ArrowDown />
              </Button>
              {d.kind === "disk" && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  aria-label={t("vmForm.bootOffNamed", { name: deviceName(d) })}
                  disabled={locked}
                  onClick={() => apply(booting.filter((_, j) => j !== i))}
                >
                  {t("vmForm.bootOff")}
                </Button>
              )}
            </li>
          ))}
        </ol>
        {off.length > 0 && (
          <div className="flex flex-col gap-1">
            <span className="text-xs text-muted-foreground">{t("vmForm.bootOffList")}</span>
            <ul className="flex flex-col gap-1">
              {off.map((d) => (
                <li
                  key={deviceName(d)}
                  className="flex items-center gap-2 rounded-md border border-dashed px-3 py-1.5 text-sm"
                >
                  <span className="flex-1">{label(d)}</span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    aria-label={t("vmForm.bootOnNamed", { name: deviceName(d) })}
                    disabled={locked}
                    onClick={() => apply([...booting, d])}
                  >
                    {t("vmForm.bootOn")}
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}
        {[...new Set(rowErrors)].map((e) => (
          <p key={e} className="text-xs text-destructive">
            {t(e)}
          </p>
        ))}
      </CardContent>
    </Card>
  );
}
