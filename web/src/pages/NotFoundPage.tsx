import { useTranslation } from "react-i18next";
import { Link } from "react-router";

import { PageHeader } from "@/components/PageHeader";

export function NotFoundPage() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col items-start gap-2">
      <PageHeader title={t("notFound.title")} />
      <Link to="/" className="text-sm underline">
        {t("notFound.home")}
      </Link>
    </div>
  );
}
