// 살피기(계획 71 설계 §8.2): 고른 칸의 값을 틀 아래에서 고친다. 계산은 밀대 + 숫자 칸(메모리는 256 MiB 단위·로그
// 눈금), 디스크·CD·NIC는 그 줄의 칸, cloud-init은 폼의 `CloudInitSection` 그대로, 배치·보호·템플릿 바탕은 몇 칸.
// 빌더에 없는 칸(스냅샷 예약·복제·NUMA·QoS·펌웨어 세부)은 "폼에서 고치기".
import { X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";

import { usePrincipal } from "@/auth/AuthProvider";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { isSdnNetwork } from "@/sdn/ips";
import { DeployDialog } from "@/templates/DeployDialog";
import { useBuilder } from "@/vmBuilder/context";
import { putDraft, writeMode } from "@/vmBuilder/draft";
import { slotLabel } from "@/vmBuilder/labels";
import { slotOf } from "@/vmBuilder/model/slots";
import { renumber } from "@/vmBuilder/model/state";
import { CloudInitSection } from "@/vms/form/CloudInitSection";
import { MAX_VCPUS, validate } from "@/vms/form";

const MIB_STEP = 256;

/** 숫자 칸과 밀대 한 쌍(밀대만으로는 정확한 값을 넣기 어렵다). `toRange`·`fromRange`로 로그 눈금 */
function Slider({
  id,
  label,
  value,
  min,
  max,
  step = 1,
  toRange = (n) => n,
  fromRange = (n) => n,
  onChange,
}: {
  id: string;
  label: string;
  value: string;
  min: number;
  max: number;
  step?: number;
  toRange?(n: number): number;
  fromRange?(n: number): number;
  onChange(v: string): void;
}) {
  const n = Number(value) || min;
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <div className="flex items-center gap-3">
        <input
          type="range"
          aria-label={label}
          className="vb-range"
          min={toRange(min)}
          max={toRange(max)}
          step={step}
          value={toRange(Math.min(Math.max(n, min), max))}
          onChange={(e) => onChange(String(fromRange(Number(e.target.value))))}
        />
        <Input
          id={id}
          inputMode="numeric"
          className="w-24"
          value={value}
          onChange={(e) => onChange(e.target.value)}
        />
      </div>
    </div>
  );
}

function Select({
  id,
  label,
  value,
  options,
  onChange,
}: {
  id: string;
  label: string;
  value: string;
  options: readonly (readonly [string, string])[];
  onChange(v: string): void;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <NativeSelect id={id} value={value} onChange={(e) => onChange(e.target.value)}>
        {options.map(([v, text]) => (
          <NativeSelectOption key={v} value={v}>
            {text}
          </NativeSelectOption>
        ))}
      </NativeSelect>
    </div>
  );
}

function Check({
  id,
  label,
  checked,
  onChange,
}: {
  id: string;
  label: string;
  checked: boolean;
  onChange(v: boolean): void;
}) {
  return (
    <div className="flex items-center gap-2">
      <Checkbox id={id} checked={checked} onCheckedChange={(c) => onChange(c === true)} />
      <Label htmlFor={id}>{label}</Label>
    </div>
  );
}

export function Inspector() {
  const { t } = useTranslation();
  const b = useBuilder();
  const navigate = useNavigate();
  const { user } = usePrincipal();
  const id = b.selected!;
  const slot = slotOf(id);
  const s = b.s;
  const v = s.values;
  const i = (key: string) => t(`vmBuilder.inspect.${key}`);
  const template = s.base.kind === "template" ? s.base : null;
  const index = slot && "index" in slot ? slot.index : -1;
  // 템플릿 바탕의 "배포 대화상자에서 더 고치기"(여러 VM·NUMA·CPU 플래그 등 — 같은 배포 API)
  const [deploying, setDeploying] = useState(false);
  const templateVm = template && b.ctx.vms.find((x) => x.id === template.id);
  let body: React.ReactNode = null;
  switch (slot?.kind) {
    case "compute":
      body = (
        <>
          <Slider
            id="vb-cores"
            label={i("vcpus")}
            value={v.cores}
            min={1}
            max={Math.min(MAX_VCPUS, 64)}
            onChange={(cores) => b.edit({ cores, sockets: 1 })}
          />
          <Slider
            id="vb-memory"
            label={i("memory")}
            value={v.memory}
            min={MIB_STEP}
            max={256 * 1024}
            step={0.125}
            toRange={Math.log2}
            fromRange={(n) => Math.max(MIB_STEP, Math.round(2 ** n / MIB_STEP) * MIB_STEP)}
            onChange={(memory) => b.edit({ memory })}
          />
          {b.ctx.cpuModels && (
            <Select
              id="vb-model"
              label={i("model")}
              value={v.model}
              options={[
                ...(b.ctx.cpuModels.some((m) => m.name === v.model)
                  ? []
                  : [[v.model, v.model] as const]),
                ...b.ctx.cpuModels
                  .filter((m) => !m.alias_of && !m.deprecated)
                  .map((m) => [m.name, m.name] as const),
              ]}
              onChange={(model) => b.edit({ model })}
            />
          )}
        </>
      );
      break;
    case "disk": {
      const d = v.disks[index];
      if (!d) {
        break;
      }
      const setDisk = (patch: Partial<typeof d>) =>
        b.edit({ disks: v.disks.map((x, j) => (j === index ? { ...x, ...patch } : x)) });
      body = template ? (
        <Slider
          id="vb-size"
          label={i("grow")}
          value={d.sizeGib}
          min={1}
          max={4096}
          onChange={(sizeGib) => setDisk({ sizeGib })}
        />
      ) : (
        <>
          {d.kind === "new" && (
            <>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="vb-volume">{i("volume")}</Label>
                <Input
                  id="vb-volume"
                  value={d.volume}
                  onChange={(e) =>
                    // 고치면 VM 이름을 따라가지 않는다.
                    b.set(
                      renumber({
                        ...s,
                        values: {
                          ...v,
                          disks: v.disks.map((x, j) =>
                            j === index ? { ...x, volume: e.target.value } : x,
                          ),
                        },
                        autoNames: v.disks.map((_, j) =>
                          j === index ? false : (s.autoNames[j] ?? false),
                        ),
                      }),
                    )
                  }
                />
              </div>
              <Slider
                id="vb-size"
                label={i("size")}
                value={d.sizeGib}
                min={1}
                max={4096}
                onChange={(sizeGib) => setDisk({ sizeGib })}
              />
            </>
          )}
          <Select
            id="vb-bus"
            label={i("bus")}
            value={d.bus}
            options={[
              ["virtio-blk", "virtio-blk"],
              ["sata", "SATA"],
            ]}
            onChange={(bus) => setDisk({ bus: bus as typeof d.bus })}
          />
        </>
      );
      break;
    }
    case "cdrom": {
      const c = v.cdroms[index];
      if (!c) {
        break;
      }
      body = (
        <Check
          id="vb-boot"
          label={t("vmBuilder.page.bootFirst")}
          checked={c.bootOrder === "1"}
          onChange={(on) =>
            b.edit({
              cdroms: v.cdroms.map((x, j) =>
                j === index ? { ...x, bootOrder: on ? "1" : "" } : x,
              ),
            })
          }
        />
      );
      break;
    }
    case "nic": {
      const n = v.nics[index];
      if (!n) {
        break;
      }
      const setNic = (patch: Partial<typeof n>) =>
        b.edit({ nics: v.nics.map((x, j) => (j === index ? { ...x, ...patch } : x)) });
      const sdn = isSdnNetwork(b.ctx.networks.find((x) => x.name === n.network));
      body = (
        <>
          {/* 배포는 템플릿의 NIC 모델·MAC을 그대로 쓴다(배포 요청에 칸이 없음). */}
          {!template && (
            <Select
              id="vb-nic-model"
              label={i("nicModel")}
              value={n.model}
              options={[
                ["virtio", "virtio"],
                ["e1000e", "e1000e"],
              ]}
              onChange={(model) => setNic({ model: model as typeof n.model })}
            />
          )}
          {!template && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="vb-mac">{i("mac")}</Label>
              <Input
                id="vb-mac"
                value={n.mac}
                placeholder={i("auto")}
                onChange={(e) => setNic({ mac: e.target.value })}
              />
            </div>
          )}
          {sdn && (
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="vb-ips">{i("ips")}</Label>
              <Input
                id="vb-ips"
                value={n.ips}
                placeholder={i("auto")}
                onChange={(e) => setNic({ ips: e.target.value })}
              />
            </div>
          )}
          {n.securityGroups.length > 0 && (
            <ul className="flex flex-wrap gap-1" aria-label={i("groups")}>
              {n.securityGroups.map((g) => (
                <li key={g} className="flex items-center gap-1 rounded-full border pl-2 text-xs">
                  {g}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    aria-label={t("vmBuilder.inspect.removeGroup", { name: g })}
                    onClick={() =>
                      setNic({ securityGroups: n.securityGroups.filter((x) => x !== g) })
                    }
                  >
                    <X />
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </>
      );
      break;
    }
    case "cloudInit": {
      const sdn = v.nics.map((n) => isSdnNetwork(b.ctx.networks.find((x) => x.name === n.network)));
      const errors = validate(v, true, [], sdn);
      body = (
        <CloudInitSection
          values={v}
          errors={errors}
          onChange={(patch) => b.edit(patch)}
          running={false}
          sdn={sdn}
        />
      );
      break;
    }
    case "placement":
      body = s.zone ? (
        <Select
          id="vb-zone"
          label={t("vmBuilder.part.zone", { name: s.zone.value })}
          value={s.zone.hard ? "hard" : "soft"}
          options={[
            ["hard", i("zoneHard")],
            ["soft", i("zoneSoft")],
          ]}
          onChange={(h) => b.set({ ...s, zone: { ...s.zone!, hard: h === "hard" } })}
        />
      ) : v.node ? (
        // 배포는 노드 고정을 싣지 못한다.
        !template && (
          <Check
            id="vb-pinned"
            label={i("pinned")}
            checked={v.pinned}
            onChange={(pinned) => b.edit({ pinned })}
          />
        )
      ) : (
        <p className="text-sm text-muted-foreground">{i("auto")}</p>
      );
      break;
    case "protection":
      body = template ? (
        <p className="text-sm text-muted-foreground">{t("vmBuilder.reason.templateHa")}</p>
      ) : (
        <>
          <Check
            id="vb-ha"
            label={t("vmBuilder.part.ha")}
            checked={v.ha}
            onChange={(ha) => b.edit({ ha })}
          />
          {v.ha && (
            <Select
              id="vb-ha-group"
              label={i("haGroup")}
              value={v.haGroup}
              options={[["", "—"], ...b.ctx.haGroups.map((g) => [g.name, g.name] as const)]}
              onChange={(haGroup) => b.edit({ haGroup })}
            />
          )}
        </>
      );
      break;
    case "vm":
      if (template) {
        body = (
          <>
            <Select
              id="vb-linked"
              label={i("copy")}
              value={template.linked === null ? "auto" : template.linked ? "linked" : "full"}
              options={[
                ["auto", i("copyAuto")],
                ["linked", i("copyLinked")],
                ["full", i("copyFull")],
              ]}
              onChange={(c) =>
                b.set({ ...s, base: { ...template, linked: c === "auto" ? null : c === "linked" } })
              }
            />
            <Select
              id="vb-pool"
              label={i("pool")}
              value={template.pool}
              options={[["", i("samePool")], ...b.ctx.pools.map((p) => [p.name, p.name] as const)]}
              onChange={(pool) => b.set({ ...s, base: { ...template, pool } })}
            />
            <div className="flex flex-wrap gap-2">
              <Button type="button" variant="outline" size="sm" onClick={() => b.blank()}>
                {i("blank")}
              </Button>
              {templateVm && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => setDeploying(true)}
                >
                  {i("deployDialog")}
                </Button>
              )}
            </div>
            {deploying && templateVm && (
              <DeployDialog template={templateVm} onClose={() => setDeploying(false)} />
            )}
          </>
        );
      }
      break;
  }
  return (
    <section
      aria-labelledby="vb-inspect"
      className="flex flex-col gap-3 rounded-lg border bg-card p-3"
    >
      <div className="flex items-center justify-between gap-2">
        <h3 id="vb-inspect" className="text-sm font-medium">
          {t("vmBuilder.inspect.title", { slot: slotLabel(t, id, s) })}
        </h3>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          aria-label={i("close")}
          onClick={() => b.select(null)}
        >
          <X />
        </Button>
      </div>
      <fieldset disabled={b.busy} className="flex flex-col gap-3">
        {body}
      </fieldset>
      {!template && (
        <Button
          type="button"
          variant="link"
          size="sm"
          className="self-start px-0"
          onClick={() => {
            putDraft({ values: v });
            writeMode(user, "form");
            void navigate("/vms/-new");
          }}
        >
          {i("editInForm")}
        </Button>
      )}
    </section>
  );
}
