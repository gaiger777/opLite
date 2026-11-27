import { type RefObject, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { Vm } from "@/api/types";
import { useOptionalPrincipal } from "@/auth/AuthProvider";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import { archOf } from "@/cpu/cpu";
import { CpuChoice, type CpuPick, cpuOverride } from "@/cpu/CpuChoice";
import {
  memoryHotplug,
  numaFrom,
  numaOverride,
  type NumaValues,
  overrideErrors,
} from "@/numa/numa";
import { NumaChoice } from "@/numa/NumaSection";
import { errorMessage } from "@/i18n/errors";
import { formatBytes } from "@/lib/format";
import { ipsOverride, sdnNetworkNamed } from "@/sdn/ips";
import { choosablePoolsOn } from "@/storage/pools";
import { usePoolNodes } from "@/storage/usePoolNodes";
import { trackTask } from "@/tasks/tasks";
import {
  cloneBody,
  type DeployMode as Mode,
  deployNames,
  diskVolumes,
  linkableDisks,
  templateNode,
} from "@/templates/templates";
import { cloudInitErrors, emptyCloudInit, toCloudInit } from "@/vms/cloudInit";
import { gibToBytes, MAX_VCPUS, MIN_MEMORY_MIB, validName } from "@/vms/form";
import { CloudInitSection } from "@/vms/form/CloudInitSection";

const MIB = 1024 ** 2;

/** `min`부터 `max`까지의 정수인지 */
function intIn(value: string, min: number, max: number): boolean {
  const n = Number(value);
  return value.trim() !== "" && Number.isInteger(n) && n >= min && n <= max;
}

/**
 * 템플릿에서 VM을 배포한다(`POST /vms/{key}/clone`을 VM마다 차례로). 개수가 2 이상이면 이름의 `{n}`이 번호로 바뀌고,
 * 같은 값이 겹치는 cloud-init 호스트 이름·정적 IP는 받지 않는다(호스트 이름을 비우면 VM 이름이 호스트 이름이다). 비운
 * 칸은 템플릿 값 그대로다. Task마다 알림으로 진행을 보인다(성공했지만 경고가 있으면 경고 알림).
 */
export function DeployDialog({
  template,
  onClose,
  returnFocus,
}: {
  template: Vm;
  onClose(): void;
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const { t } = useTranslation();
  const vms = useList("vm");
  const volumes = useList("volume");
  const pools = useList("storage-pool");
  const nodes = useList("node");
  const networks = useList("network");
  const templateNics = template.spec.nics ?? [];
  const bases = diskVolumes(template, volumes.data ?? []);
  const [name, setName] = useState(`${template.name}-{n}`);
  const [count, setCount] = useState("1");
  const [first, setFirst] = useState("1");
  const [mode, setMode] = useState<Mode>("auto");
  const [node, setNode] = useState("");
  const [pool, setPool] = useState("");
  const [cpus, setCpus] = useState("");
  const [memory, setMemory] = useState("");
  const [nics, setNics] = useState<string[]>(() => templateNics.map(() => ""));
  // SDN NIC마다의 주소(계획 65, 비면 서브넷에서 새로 받음)
  const [ips, setIps] = useState<string[]>(() => templateNics.map(() => ""));
  // 배포한 VM의 NIC가 붙을 네트워크(바꾸지 않았으면 템플릿 그대로)가 SDN인지
  const sdnNics = templateNics.map((nic, i) =>
    sdnNetworkNamed(networks.data, nics[i] || nic.network),
  );
  const [sizes, setSizes] = useState<string[]>(() => (template.spec.disks ?? []).map(() => ""));
  const [cloudInit, setCloudInit] = useState(emptyCloudInit);
  const [start, setStart] = useState(false);
  // 템플릿의 CPU를 바꿔 배포한다(바꾸지 않았으면 null — 템플릿 그대로, 리뷰 2 웹 #3).
  const templateCpu: CpuPick = {
    model: template.spec.cpu.model ?? "host",
    flags: template.spec.cpu.flags ?? [],
  };
  const [cpu, setCpu] = useState<CpuPick | null>(null);
  // CPU 배치와 NUMA(계획 64): 고치지 않았으면 null — 템플릿 그대로
  const templateNuma = numaFrom(template.spec);
  const [numa, setNuma] = useState<NumaValues | null>(null);
  const [errors, setErrors] = useState<Record<string, string>>({});
  // 덮어쓴 vCPU·메모리로 NUMA 칸을 다시 본다(리뷰 2 웹 — vCPU를 덮어쓰면 소켓 1이라 노드 수가 틀릴 수 있다).
  const numaErrors = overrideErrors(numa ?? templateNuma, template.spec, cpus, memory);
  // 배포 Task는 서버가 정한 템플릿의 노드에서 돈다(`templateNode`). 그 노드에서 쓸 수 있는 풀만 고른다. 지난 노드가
  // 빠졌거나 꺼졌으면 접속한 노드(whoami)가 필요할 수 있다. 노드를 읽지 못하면 거르지 않는다.
  const serving = useOptionalPrincipal()?.node?.id;
  const poolNodes = usePoolNodes();
  const taskNode = templateNode(template, volumes.data, pools.data, nodes.data, serving);
  const usablePools = choosablePoolsOn(pools.data ?? [], poolNodes, taskNode);
  const linkable = linkableDisks(bases, pools.data ?? [], pool);
  const parsed = deployNames(name, Number(count), Number(first));
  const preview = "names" in parsed && Number(count) > 1 ? parsed.names : [];
  const nicCount = templateNics.length;

  async function submit() {
    const found: Record<string, string> = {};
    let names: string[] = [];
    if ("error" in parsed) {
      const field = { "templates.errors.pattern": "name", "templates.errors.first": "first" }[
        parsed.error
      ];
      found[field ?? "count"] = parsed.error;
    } else {
      names = parsed.names;
      const taken = new Set((vms.data ?? []).map((v) => v.name));
      if (names.some((n) => !validName(n))) {
        found.name = "forms.name";
      } else if (names.some((n) => taken.has(n))) {
        found.name = "clone.errors.exists";
      }
    }
    if (mode === "linked" && linkable.some((ok) => !ok)) {
      found.mode = "templates.errors.notLinkable";
    }
    if (cpus !== "" && !intIn(cpus, 1, MAX_VCPUS)) {
      found.cpus = "templates.errors.vcpus";
    }
    if (memory !== "" && !intIn(memory, MIN_MEMORY_MIB, 16 * 1024 * 1024)) {
      found.memory = "templates.errors.memory";
    }
    // 크기는 1 MiB 단위로 올려 보낸다(볼륨 크기 늘리기와 같다). 늘리기만 된다.
    const diskBytes = sizes.map((text, i) => {
      if (text.trim() === "") {
        return null;
      }
      const bytes = gibToBytes(text.trim());
      const size = bytes === undefined ? undefined : Math.ceil(bytes / MIB) * MIB;
      if (size === undefined) {
        found[`disk${i}`] = "vmForm.errors.size";
      } else if (size <= (bases[i]?.spec.size_bytes ?? 0)) {
        found[`disk${i}`] = "volumes.errors.grow";
      }
      return size ?? null;
    });
    Object.assign(found, cloudInitErrors(cloudInit, nicCount, sdnNics));
    const ipTexts = ips.map((text, i) => (sdnNics[i] ? text : ""));
    const wantIps = ipsOverride(ipTexts, found, "ips");
    // 같은 주소를 여러 VM에 줄 수 없다(CLI의 `--ip`도 `--count` 2 이상과 함께 못 쓴다).
    if (names.length > 1 && wantIps) {
      ipTexts.forEach((text, i) => {
        if (text.trim() !== "") {
          found[`ips${i}`] = "sdn.errors.ipCount";
        }
      });
    }
    // 여러 VM에 같은 값을 줄 수 없다(CLI와 같은 거절).
    if (names.length > 1 && cloudInit.enabled) {
      if (cloudInit.hostname.trim() !== "") {
        found["ci.hostname"] = "templates.errors.hostname";
      }
      cloudInit.networks.slice(0, nicCount).forEach((net, i) => {
        if (net.ipv4 === "static") {
          found[`ci.networks.${i}.ipv4Address`] = "templates.errors.staticIp";
        }
        if (net.ipv6 === "static") {
          found[`ci.networks.${i}.ipv6Address`] = "templates.errors.staticIp";
        }
      });
    }
    setErrors(found);
    if (Object.keys(found).length > 0 || Object.keys(numaErrors).length > 0) {
      return false;
    }
    const ci = toCloudInit(cloudInit, nicCount, sdnNics);
    const overrides = {
      mode,
      pool,
      node,
      ...(cpus !== "" && { vcpus: Number(cpus) }),
      ...(memory !== "" && { memoryMib: Number(memory) }),
      nics,
      ips: wantIps,
      disks: diskBytes,
      cloudInit: ci,
      start,
      cpu: cpuOverride(cpu, templateCpu),
      numa: numaOverride(numa, templateNuma),
    };
    const body = (vmName: string) => cloneBody(vmName, overrides);
    let made = 0;
    for (const vmName of names) {
      try {
        const task = await call(
          api.POST("/vms/{key}/clone", {
            params: { path: { key: template.id } },
            body: body(vmName),
          }),
        );
        trackTask(task, t("templates.deployOp", { template: template.name, name: vmName }));
        made += 1;
      } catch (err) {
        // 같은 요청이라 다음 VM도 거의 늘 같은 이유로 거절된다 — 더 만들지 않는다(CLI와 같다).
        if (made === 0) {
          throw err;
        }
        toast.error(
          t("templates.deployStopped", {
            name: vmName,
            error: errorMessage(t, err),
            rest: names.length - made - 1,
          }),
          { duration: Infinity, closeButton: true },
        );
        return;
      }
    }
  }

  return (
    <FormDialog
      title={t("templates.deployTitle", { name: template.name })}
      description={t("templates.deployDescription")}
      submitLabel={t("templates.deploy")}
      className="sm:max-w-2xl"
      // 볼륨 목록을 읽어야 디스크와 연결 가능 여부를 안다.
      submitDisabled={!volumes.isSuccess}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      <div className="-mx-1 flex max-h-[65svh] flex-col gap-4 overflow-y-auto px-1">
        <div className="grid gap-4 sm:grid-cols-3">
          <div className="sm:col-span-3">
            <Field
              id="deploy-name"
              label={t("templates.name")}
              error={errors.name}
              help={
                preview.length > 0
                  ? t("templates.preview", {
                      names:
                        preview.length > 3
                          ? `${preview.slice(0, 3).join(", ")}, … ${preview.at(-1)}`
                          : preview.join(", "),
                    })
                  : t("templates.nameHint")
              }
            >
              {(a11y) => (
                <Input
                  id="deploy-name"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  {...a11y}
                />
              )}
            </Field>
          </div>
          <Field
            id="deploy-count"
            label={t("templates.count")}
            error={errors.count}
            help={t("help.deploy.count")}
          >
            {(a11y) => (
              <Input
                id="deploy-count"
                inputMode="numeric"
                value={count}
                onChange={(e) => setCount(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="deploy-first"
            label={t("templates.first")}
            error={errors.first}
            help={t("help.deploy.first")}
          >
            {(a11y) => (
              <Input
                id="deploy-first"
                inputMode="numeric"
                value={first}
                onChange={(e) => setFirst(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </div>
        {!volumes.isSuccess && <ListPending error={volumes.error} />}
        <Field
          id="deploy-mode"
          label={t("templates.mode")}
          error={errors.mode}
          help={t(`templates.modeHint.${mode}`)}
          info="templates.mode"
        >
          {(a11y) => (
            <NativeSelect
              id="deploy-mode"
              className="w-full"
              value={mode}
              onChange={(e) => setMode(e.target.value as Mode)}
              {...a11y}
            >
              <NativeSelectOption value="auto">{t("templates.modes.auto")}</NativeSelectOption>
              <NativeSelectOption value="linked">{t("templates.modes.linked")}</NativeSelectOption>
              <NativeSelectOption value="full">{t("templates.modes.full")}</NativeSelectOption>
            </NativeSelect>
          )}
        </Field>
        <div className="grid gap-4 sm:grid-cols-2">
          <Field id="deploy-pool" label={t("clone.pool")} help={t("help.deploy.pool")}>
            {(a11y) => (
              <NativeSelect
                {...a11y}
                id="deploy-pool"
                className="w-full"
                value={pool}
                onChange={(e) => setPool(e.target.value)}
              >
                <NativeSelectOption value="">{t("clone.samePool")}</NativeSelectOption>
                {usablePools.map((p) => (
                  <NativeSelectOption key={p.id} value={p.name}>
                    {p.name}
                  </NativeSelectOption>
                ))}
              </NativeSelect>
            )}
          </Field>
          <Field id="deploy-node" label={t("templates.node")} help={t("templates.nodeHint")}>
            {(a11y) => (
              <NativeSelect
                id="deploy-node"
                className="w-full"
                value={node}
                onChange={(e) => setNode(e.target.value)}
                {...a11y}
              >
                <NativeSelectOption value="">{t("templates.anyNode")}</NativeSelectOption>
                {(nodes.data ?? [])
                  .filter((n) => !n.spec.witness)
                  .map((n) => (
                    <NativeSelectOption key={n.id} value={n.name}>
                      {n.name}
                    </NativeSelectOption>
                  ))}
              </NativeSelect>
            )}
          </Field>
          <Field
            id="deploy-cpus"
            label={t("vms.vcpus")}
            error={errors.cpus}
            help={t("help.deploy.cpus")}
          >
            {(a11y) => (
              <Input
                id="deploy-cpus"
                inputMode="numeric"
                placeholder={String((template.spec.cpu.sockets ?? 1) * template.spec.cpu.cores)}
                value={cpus}
                onChange={(e) => setCpus(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
          <Field
            id="deploy-memory"
            label={t("templates.memoryMib")}
            error={errors.memory}
            help={t("help.deploy.memory")}
          >
            {(a11y) => (
              <Input
                id="deploy-memory"
                inputMode="numeric"
                placeholder={String(template.spec.memory_mib)}
                value={memory}
                onChange={(e) => setMemory(e.target.value)}
                {...a11y}
              />
            )}
          </Field>
        </div>
        <CpuChoice
          id="deploy-cpu"
          arch={archOf(template.spec.machine)}
          value={cpu ?? templateCpu}
          onChange={setCpu}
          collapsed
        />
        <NumaChoice
          id="deploy-numa"
          sockets={cpus !== "" ? 1 : (template.spec.cpu.sockets ?? 1)}
          value={numa ?? templateNuma}
          onChange={setNuma}
          errors={numaErrors}
          hotplug={memoryHotplug({
            memory_mib: memory !== "" ? Number(memory) : template.spec.memory_mib,
            max_memory_mib: template.spec.max_memory_mib,
          })}
        />
        {bases.length > 0 && (
          <fieldset className="flex flex-col gap-3">
            <legend className="mb-2 text-sm font-medium">{t("templates.disks")}</legend>
            {bases.map((base, i) => (
              <div key={i} className="grid items-end gap-2 sm:grid-cols-2">
                <p className="text-sm">
                  {t("templates.diskLine", {
                    n: i,
                    volume: template.spec.disks?.[i]?.volume ?? "—",
                    size: base ? formatBytes(base.spec.size_bytes) : "—",
                  })}
                  <span className="block text-xs text-muted-foreground">
                    {mode === "full"
                      ? t("templates.diskFull")
                      : linkable[i]
                        ? t("templates.diskLinked")
                        : t("templates.diskNotLinkable")}
                  </span>
                </p>
                <Field
                  id={`deploy-disk-${i}`}
                  label={t("templates.diskSize", { n: i })}
                  error={errors[`disk${i}`]}
                  help={t("help.deploy.diskSize")}
                  info="templates.diskSize"
                >
                  {(a11y) => (
                    <Input
                      id={`deploy-disk-${i}`}
                      inputMode="decimal"
                      placeholder={base ? String(base.spec.size_bytes / 1024 ** 3) : ""}
                      value={sizes[i] ?? ""}
                      onChange={(e) =>
                        setSizes((s) => s.map((v, j) => (j === i ? e.target.value : v)))
                      }
                      {...a11y}
                    />
                  )}
                </Field>
              </div>
            ))}
          </fieldset>
        )}
        {nicCount > 0 && (
          <fieldset className="grid gap-4 sm:grid-cols-2">
            <legend className="mb-2 text-sm font-medium">{t("templates.nics")}</legend>
            {templateNics.map((nic, i) => (
              <Field
                key={i}
                id={`deploy-nic-${i}`}
                label={t("templates.nicNetwork", { n: i })}
                help={t("help.deploy.nic")}
              >
                {(a11y) => (
                  <NativeSelect
                    {...a11y}
                    id={`deploy-nic-${i}`}
                    className="w-full"
                    value={nics[i] ?? ""}
                    onChange={(e) => {
                      setNics((list) => list.map((v, j) => (j === i ? e.target.value : v)));
                      setIps((list) => list.map((v, j) => (j === i ? "" : v)));
                    }}
                  >
                    <NativeSelectOption value="">
                      {t("templates.keepNetwork", { network: nic.network })}
                    </NativeSelectOption>
                    {(networks.data ?? [])
                      .filter((n) => n.name !== nic.network)
                      .map((n) => (
                        <NativeSelectOption key={n.id} value={n.name}>
                          {n.name}
                        </NativeSelectOption>
                      ))}
                  </NativeSelect>
                )}
              </Field>
            ))}
            {templateNics.map(
              (_, i) =>
                sdnNics[i] && (
                  <Field
                    key={`ips-${i}`}
                    id={`deploy-ips-${i}`}
                    label={t("templates.nicIps", { n: i + 1 })}
                    error={errors[`ips${i}`]}
                    help={t("vmForm.nicIpsHint")}
                  >
                    {(a11y) => (
                      <Input
                        id={`deploy-ips-${i}`}
                        placeholder={t("vmForm.nicIpsAuto")}
                        value={ips[i] ?? ""}
                        onChange={(e) =>
                          setIps((list) => list.map((v, j) => (j === i ? e.target.value : v)))
                        }
                        {...a11y}
                      />
                    )}
                  </Field>
                ),
            )}
          </fieldset>
        )}
        <p className="text-sm text-muted-foreground">{t("templates.ciNote")}</p>
        <CloudInitSection
          values={{
            name: name.replaceAll("{n}", first),
            nics: templateNics.map((n) => ({
              network: n.network,
              model: n.model,
              mac: "",
              securityGroups: [],
              ips: "",
            })),
            cloudInit,
          }}
          errors={errors}
          onChange={(patch) => setCloudInit(patch.cloudInit)}
          running={false}
          sdn={sdnNics}
        />
        <div className="flex items-center gap-2">
          <Checkbox
            id="deploy-start"
            checked={start}
            onCheckedChange={(v) => setStart(v === true)}
          />
          <Label htmlFor="deploy-start">{t("templates.start")}</Label>
        </div>
      </div>
    </FormDialog>
  );
}
