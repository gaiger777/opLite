import { useId, useState } from "react";
import { useTranslation } from "react-i18next";

import { useList } from "@/api/live";
import { FormDialog } from "@/components/FormDialog";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { drErrorText, useRemoteVms } from "@/dr/api";
import { candidates } from "@/dr/model";
import { formatTime } from "@/lib/format";
import type { RemoteView } from "@/remotes/remotes";

/**
 * 그룹에 VM을 더하는 고르기(설계 §4): 운영 클러스터(원격) VM 목록(원격 중계)과 이 클러스터에 있는 그 원격의 사본(`origin`)
 * 에서 고른다. 원격에 닿지 않아도 사본으로 고를 수 있다. 이미 계획에 있는 VM은 뺀다.
 */
export function VmPickerDialog({
  source,
  taken,
  group,
  onClose,
  onPick,
}: {
  source: RemoteView | undefined;
  taken: ReadonlySet<string>;
  group: string;
  onClose(): void;
  onPick(names: string[]): void;
}) {
  const { t, i18n } = useTranslation();
  const id = useId();
  const remoteVms = useRemoteVms(source?.name);
  const backups = useList("backup");
  const [filter, setFilter] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const list = candidates(remoteVms.data, backups.data, source?.ca_fingerprint).filter(
    (c) => !taken.has(c.vm) && c.vm.includes(filter.trim()),
  );
  const toggle = (vm: string, on: boolean) =>
    setChosen((c) => (on ? [...c, vm] : c.filter((v) => v !== vm)));

  return (
    <FormDialog
      title={t("dr.pick.title", { group })}
      description={
        source ? t("dr.pick.description", { source: source.name }) : t("dr.pick.noSource")
      }
      submitLabel={t("dr.pick.add", { count: chosen.length })}
      submitDisabled={chosen.length === 0}
      onClose={onClose}
      onSubmit={async () => {
        onPick(chosen);
        return false;
      }}
    >
      <div className="flex flex-col gap-1.5">
        <Label htmlFor={`${id}-filter`}>{t("dr.pick.filter")}</Label>
        <Input
          id={`${id}-filter`}
          autoComplete="off"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>
      {remoteVms.error && (
        <p role="status" className="text-xs text-warning-fg">
          {t("dr.pick.remoteFailed", { error: drErrorText(t, remoteVms.error) })}
        </p>
      )}
      {list.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {remoteVms.isLoading ? t("common.loading") : t("dr.pick.none")}
        </p>
      ) : (
        <ul aria-label={t("dr.pick.list")} className="flex max-h-72 flex-col gap-1 overflow-y-auto">
          {list.map((c) => (
            <li key={c.vm} className="flex items-center gap-2">
              <Checkbox
                id={`${id}-${c.vm}`}
                checked={chosen.includes(c.vm)}
                onCheckedChange={(v) => toggle(c.vm, v === true)}
              />
              <Label htmlFor={`${id}-${c.vm}`} className="flex flex-wrap gap-x-2 font-normal">
                <span className="font-medium">{c.vm}</span>
                <span className="text-xs text-muted-foreground">
                  {[
                    c.remote ? t("dr.pick.onSource") : t("dr.pick.notOnSource"),
                    c.copyAt !== undefined
                      ? t("dr.pick.copyAt", {
                          time: formatTime(new Date(c.copyAt * 1000), i18n.language),
                        })
                      : t("dr.pick.noCopy"),
                  ].join(" · ")}
                </span>
              </Label>
            </li>
          ))}
        </ul>
      )}
    </FormDialog>
  );
}
