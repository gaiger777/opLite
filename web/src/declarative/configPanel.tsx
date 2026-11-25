// ⋯ 메뉴의 "설정 기록…"·"YAML로 내보내기"가 여는 것(늦게 읽는 조각 — `ConfigMenu.tsx`가 받는다)
import { useTranslation } from "react-i18next";

import type { ConfigPanelKind, ConfigTarget } from "@/declarative/ConfigMenu";
import { fetchManifest } from "@/declarative/api";
import { ExportYamlDialog } from "@/declarative/ExportYamlDialog";
import { ResourceHistoryDrawer } from "@/history/ChangeDrawer";

export { loadDeclarativeStrings } from "@/declarative/strings";

export function ConfigPanel({
  what,
  target,
  onClose,
}: {
  what: ConfigPanelKind;
  target: ConfigTarget;
  onClose(): void;
}) {
  const { t } = useTranslation();
  if (what === "history") {
    return (
      <ResourceHistoryDrawer
        kind={target.kind}
        id={target.id}
        name={target.name}
        onClose={onClose}
      />
    );
  }
  return (
    <ExportYamlDialog
      title={t("declarative.export.title", { name: target.name })}
      fileName={`${target.kind}-${target.name}.yaml`}
      queryKey={[target.kind, target.id]}
      load={async () => [await fetchManifest(target.kind, target.id)]}
      onClose={onClose}
    />
  );
}
