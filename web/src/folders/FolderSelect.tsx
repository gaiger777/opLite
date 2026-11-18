import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import type { FolderKind } from "@/api/types";
import type { Privilege } from "@/auth/permissions";
import { usePermissions } from "@/auth/usePermissions";
import { Field } from "@/components/Field";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { folderPath, folderTree, MEMBER_KIND } from "@/folders/folders";

/**
 * 새 객체를 둘 폴더 고르기(계획 62): 루트(루트에 권한이 있을 때)와 그 권한이 있는 폴더. 처음 값은 루트, 루트에
 * 권한이 없으면 첫 폴더다(서버 400 `folder_required`를 피한다). 폴더가 없으면 칸이 없다. 값은 폴더 id(루트는 "").
 */
export function useNewFolder(kind: FolderKind, privilege: Privilege) {
  const perms = usePermissions();
  const folders = useList("folder");
  const options = useMemo(() => {
    const tree = folderTree(folders.data, kind);
    const inside = tree.folders
      .filter((f) =>
        perms.has(privilege, { type: "object", kind: MEMBER_KIND[kind], folder: f.id }),
      )
      .map((f) => ({ id: f.id, path: folderPath(tree, f.id) }))
      .sort((a, b) => a.path.localeCompare(b.path));
    return { root: perms.has(privilege), inside, any: tree.folders.length > 0 };
  }, [folders.data, kind, perms, privilege]);
  const [chosen, setChosen] = useState<string | null>(null);
  const value = chosen ?? (options.root ? "" : (options.inside[0]?.id ?? ""));
  return { value, set: setChosen, options };
}

export function FolderSelect({
  id,
  state,
}: {
  id: string;
  state: ReturnType<typeof useNewFolder>;
}) {
  const { t } = useTranslation();
  const { value, set, options } = state;
  if (!options.any) {
    return null;
  }
  return (
    <Field id={id} label={t("folders.folder")}>
      <NativeSelect id={id} className="w-full" value={value} onChange={(e) => set(e.target.value)}>
        {options.root && <NativeSelectOption value="">{t("folders.root")}</NativeSelectOption>}
        {options.inside.map((f) => (
          <NativeSelectOption key={f.id} value={f.id}>
            {f.path}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </Field>
  );
}
