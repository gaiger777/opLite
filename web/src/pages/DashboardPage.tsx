import { useTranslation } from "react-i18next";

export function DashboardPage() {
  const { t } = useTranslation();
  return <h1 className="text-2xl font-semibold">{t("nav.dashboard")}</h1>;
}
