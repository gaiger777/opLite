import { useTranslation } from "react-i18next";
import { Link } from "react-router";

export function NotFoundPage() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-col items-start gap-2">
      <h1 className="text-2xl font-semibold">{t("notFound.title")}</h1>
      <Link to="/" className="text-sm underline">
        {t("notFound.home")}
      </Link>
    </div>
  );
}
