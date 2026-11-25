import { useQuery } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { useTranslation } from "react-i18next";

import { CopyButton } from "@/components/CopyButton";
import { ListPending } from "@/components/ListPending";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { type Manifest, saveText } from "@/declarative/api";
import { toYaml } from "@/declarative/toYaml";

/**
 * "YAML로 내보내기"(계획 73 §13 웹): 서버의 매니페스트(`GET /manifests…` — 비밀은 `secretRef`)를 웹의 작은 쓰개(`toYaml`)로
 * 바꿔 보이고 복사·내려받기를 준다. `orbivisorctl apply -f`가 그대로 읽는다. 웹은 YAML을 적용하지 않는다(편집기는 §16).
 */
export function ExportYamlDialog({
  title,
  fileName,
  queryKey,
  load,
  onClose,
}: {
  title: string;
  fileName: string;
  queryKey: readonly unknown[];
  load(): Promise<Manifest[]>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const docs = useQuery({
    queryKey: ["manifests", ...queryKey],
    queryFn: load,
    retry: false,
    gcTime: 0,
  });
  const yaml = docs.data ? toYaml(docs.data) : "";
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle className="break-all">{title}</DialogTitle>
          <DialogDescription>{t("declarative.export.hint")}</DialogDescription>
        </DialogHeader>
        {docs.data === undefined ? (
          <ListPending error={docs.error} />
        ) : (
          <pre
            aria-label={t("declarative.export.yaml")}
            tabIndex={0}
            className="max-h-[55vh] overflow-auto rounded-md bg-muted p-3 font-mono text-xs"
          >
            {yaml}
          </pre>
        )}
        <DialogFooter className="gap-2">
          {docs.data && (
            <>
              <CopyButton text={yaml} label={t("declarative.export.copy")} />
              <Button variant="outline" onClick={() => saveText(fileName, yaml)}>
                <Download />
                {t("declarative.export.download")}
              </Button>
            </>
          )}
          <Button onClick={onClose}>{t("common.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
