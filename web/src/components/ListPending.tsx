import { useTranslation } from "react-i18next";

import { errorMessage } from "@/i18n/errors";

/** 목록을 읽는 중이거나 읽지 못했을 때 */
export function ListPending({ error }: { error: unknown }) {
  const { t } = useTranslation();
  return error ? (
    <p role="alert" className="text-destructive">
      {errorMessage(t, error)}
    </p>
  ) : (
    <p role="status" className="text-muted-foreground">
      {t("common.loading")}
    </p>
  );
}
