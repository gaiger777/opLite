import { Printer } from "lucide-react";
import { useId, useRef } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { QrCode } from "@/accounts/TwoFactorSetup";
import { CopyButton } from "@/components/CopyButton";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";

/**
 * 인쇄용 창: 저장소 이름·지문·복구 키 글과 QR(화면에서 그린 SVG 노드를 복사). 글은 `textContent`로 넣는다. 창을 열 수 없으면
 * (팝업 막힘) 알린다.
 */
function printKey(lines: string[], svg: SVGSVGElement | null | undefined, blocked: string) {
  const w = window.open("", "_blank");
  if (!w) {
    toast.error(blocked);
    return;
  }
  const doc = w.document;
  doc.title = lines[0] ?? "";
  for (const [i, line] of lines.entries()) {
    const p = doc.createElement(i === 0 ? "h1" : "p");
    p.textContent = line;
    p.style.fontFamily = i === 2 ? "monospace" : "sans-serif";
    doc.body.appendChild(p);
  }
  if (svg) {
    const box = doc.createElement("div");
    box.style.width = "60mm";
    box.appendChild(doc.importNode(svg, true));
    doc.body.appendChild(box);
  }
  w.focus();
  w.print();
}

/**
 * 복구 키(종이 키, 설계 §9.4): 글·QR·복사·인쇄. `onSavedChange`가 있으면 "저장했습니다" 확인 칸을 보인다(만들기
 * 마법사 — 확인 없이는 닫히지 않는다).
 */
export function RecoveryKeyView({
  store,
  fingerprint,
  recoveryKey,
  saved,
  onSavedChange,
}: {
  store: string;
  fingerprint?: string;
  recoveryKey: string;
  /** 주면 "저장했습니다" 확인 칸을 보인다. */
  saved?: boolean;
  onSavedChange?(saved: boolean): void;
}) {
  const { t } = useTranslation();
  const qr = useRef<HTMLDivElement>(null);
  const titleId = useId();
  return (
    <section aria-labelledby={titleId} className="flex flex-col gap-3">
      <h3 id={titleId} className="font-medium">
        {t("backupStores.key.recoveryTitle")}
      </h3>
      <p role="note" className="rounded-md bg-warning-soft p-3 text-sm text-warning-fg">
        {t("backupStores.key.recoveryWarning")}
      </p>
      <p
        aria-label={t("backupStores.key.recoveryKey")}
        className="rounded-md bg-muted p-3 font-mono text-sm break-all"
      >
        {recoveryKey}
      </p>
      {fingerprint && (
        <p className="text-xs text-muted-foreground">
          {t("backupStores.key.fingerprintLine", { fingerprint })}
        </p>
      )}
      <div className="flex flex-wrap items-start gap-4">
        <div ref={qr}>
          <QrCode text={recoveryKey} label={t("backupStores.key.qr", { name: store })} />
        </div>
        <div className="flex items-center gap-2">
          <CopyButton text={recoveryKey} label={t("backupStores.key.copy")} />
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() =>
              printKey(
                [
                  t("backupStores.key.printTitle", { name: store }),
                  fingerprint ? t("backupStores.key.fingerprintLine", { fingerprint }) : "",
                  recoveryKey,
                  t("backupStores.key.recoveryWarning"),
                ],
                qr.current?.querySelector("svg"),
                t("backupStores.key.printBlocked"),
              )
            }
          >
            <Printer />
            {t("backupStores.key.print")}
          </Button>
        </div>
      </div>
      {onSavedChange && (
        <div className="flex items-center gap-2">
          <Checkbox
            id="recovery-saved"
            checked={saved === true}
            onCheckedChange={(on) => onSavedChange(on === true)}
          />
          <Label htmlFor="recovery-saved">{t("backupStores.key.saved")}</Label>
        </div>
      )}
    </section>
  );
}
