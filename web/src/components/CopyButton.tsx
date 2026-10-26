import { Copy } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";

/** 글을 클립보드에 복사한다. `label`은 무엇을 복사하는지(버튼의 이름) */
export function CopyButton({ text, label }: { text: string; label: string }) {
  const { t } = useTranslation();
  async function copy() {
    try {
      await navigator.clipboard.writeText(text);
      toast.success(t("common.copied"));
    } catch {
      toast.error(t("common.copyFailed"));
    }
  }
  return (
    <Button
      type="button"
      variant="outline"
      size="icon-sm"
      aria-label={label}
      onClick={() => void copy()}
    >
      <Copy />
    </Button>
  );
}
