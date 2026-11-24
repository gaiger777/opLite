// CLI 미리 보기(계획 71 설계 §7.6): 보낼 요청과 같은 것을 만드는 `orbivisorctl` 명령. 명령은 인자 배열(`argv`)로 짓고
// (교차 시험이 셸을 거치지 않고 그대로 CLI 파서에 넣는다) 글로 낼 때만 셸 따옴표를 친다. 비밀번호는 `--ci-password-prompt`,
// user-data는 파일(`cat > … <<'EOF'`). CLI 플래그가 없는 값은 주석 줄로 알리고 JSON을 권한다.
import { folderPath, folderTree } from "@/folders/folders";
import { numaSpec } from "@/numa/numa";
import { splitList } from "@/sdn/ips";
import { volumeRequests, sdnOf, zoneRule } from "@/vmBuilder/model/requests";
import type { BuilderCtx, BuilderState } from "@/vmBuilder/model/state";
import { bootList, deviceName } from "@/vms/boot";
import { DEFAULT_NET, keyLines, type NetRow } from "@/vms/cloudInit";
import { bootOrders, gibToBytes, hasQos, type VmFormValues } from "@/vms/form";
import { DEFAULT_HA } from "@/vms/ha";

/** CLI 플래그가 없는 칸(주석 줄 `vmBuilder.cli.what.<칸>`) */
export type Missing =
  "diskBus" | "nicModel" | "mac" | "numa" | "schedule" | "folder" | "templateSg";

export interface CliPlan {
  /** 앞에 쓸 파일(user-data 등) */
  files: { name: string; content: string }[];
  /** 차례대로 낼 명령 */
  commands: string[][];
  missing: Missing[];
}

const SAFE = /^[A-Za-z0-9_@%+=:,./-]+$/;

/** 셸 작은따옴표(안의 `'`는 `'\''`). 따옴표가 필요 없는 글은 그대로 */
export function shq(s: string): string {
  return SAFE.test(s) ? s : `'${s.replaceAll("'", `'\\''`)}'`;
}

/** 바이트를 CLI 크기(`20G`·`1536M`·`512K`, 아니면 바이트)로 */
export function sizeArg(bytes: number): string {
  for (const [unit, n] of [
    ["T", 1024 ** 4],
    ["G", 1024 ** 3],
    ["M", 1024 ** 2],
    ["K", 1024],
  ] as const) {
    if (bytes % n === 0) {
      return `${bytes / n}${unit}`;
    }
  }
  return String(bytes);
}

/** cloud-init 플래그(`vm create`와 `template deploy`가 같다) */
function cloudInitArgs(
  v: VmFormValues,
  sdn: readonly boolean[],
  files: CliPlan["files"],
): string[] {
  const ci = v.cloudInit;
  if (!ci.enabled) {
    return [];
  }
  const out: string[] = [];
  if (ci.hostname.trim()) {
    out.push("--ci-hostname", ci.hostname.trim());
  }
  if (ci.user.trim()) {
    out.push("--ci-user", ci.user.trim());
  }
  if (ci.password !== "") {
    out.push("--ci-password-prompt");
  }
  for (const key of keyLines(ci.sshKeys)) {
    out.push("--ci-ssh-key", key);
  }
  // 폼의 `toCloudInit`과 같은 조건: 기본(DHCP)이 아닌 줄이 있거나 저장된 설정에 있었을 때만
  const rows: NetRow[] = v.nics.map((_, i) => ci.networks[i] ?? DEFAULT_NET);
  const same = (a: NetRow) =>
    (Object.keys(a) as (keyof NetRow)[]).every((k) => a[k] === DEFAULT_NET[k]);
  if (ci.explicitNetworks || rows.some((r, i) => !sdn[i] && !same(r))) {
    rows.forEach((r, i) => {
      if (sdn[i]) {
        return;
      }
      const addr = (address: string, gateway: string) =>
        `${address.trim()}${gateway.trim() ? `,gw=${gateway.trim()}` : ""}`;
      if (r.ipv4 !== "none") {
        out.push(
          "--ci-ip",
          `nic${i}=${r.ipv4 === "static" ? addr(r.ipv4Address, r.ipv4Gateway) : "dhcp"}`,
        );
      }
      if (r.ipv6 !== "none") {
        out.push(
          "--ci-ip6",
          `nic${i}=${r.ipv6 === "static" ? addr(r.ipv6Address, r.ipv6Gateway) : r.ipv6}`,
        );
      }
    });
  }
  for (const dns of ci.dns.split(/[\s,]+/).filter(Boolean)) {
    out.push("--ci-dns", dns);
  }
  for (const search of ci.search.split(/[\s,]+/).filter(Boolean)) {
    out.push("--ci-search", search);
  }
  for (const [data, name, flag] of [
    [ci.userData, "user-data.yaml", "--ci-user-data"],
    [ci.vendorData, "vendor-data.yaml", "--ci-vendor-data"],
  ] as const) {
    if (data !== "") {
      files.push({ name, content: data });
      out.push(flag, name);
    }
  }
  return out.length > 0 ? out : ["--cloud-init"];
}

/** 노드 id → 이름(모르면 id — CLI는 둘 다 받는다) */
function nodeName(ctx: BuilderCtx, id: string): string {
  return ctx.nodes?.find((n) => n.id === id)?.name ?? id;
}

/** `vm create` 인자 */
function vmCreate(
  s: BuilderState,
  ctx: BuilderCtx,
  files: CliPlan["files"],
  missing: Set<Missing>,
): string[] {
  const v = s.values;
  const a = ["orbivisorctl", "vm", "create", v.name];
  if (v.sockets !== 1) {
    a.push("--sockets", String(v.sockets));
  }
  a.push("--cores", v.cores, "--memory", v.memory);
  if (v.maxVcpus !== "") {
    a.push("--max-vcpus", v.maxVcpus);
  }
  if (v.maxMemory !== "") {
    a.push("--max-memory", v.maxMemory);
  }
  if (v.machine !== "") {
    a.push("--machine", v.machine);
  }
  if (v.firmware !== "uefi") {
    a.push("--firmware", v.firmware);
  }
  // `--preset windows11`만 디스크를 SATA로, NIC를 e1000e로 둔다(CLI에 장치마다의 플래그가 없다).
  const sata = v.disks.length > 0 && v.disks.every((d) => d.bus === "sata");
  const e1000e = v.nics.length > 0 && v.nics.every((n) => n.model === "e1000e");
  const preset =
    (sata || e1000e) && (sata || v.disks.length === 0) && (e1000e || v.nics.length === 0);
  if (preset) {
    a.push("--preset", "windows11");
  }
  if (v.disks.some((d) => d.bus !== (preset ? "sata" : "virtio-blk"))) {
    missing.add("diskBus");
  }
  if (v.nics.some((n) => n.model !== (preset ? "e1000e" : "virtio"))) {
    missing.add("nicModel");
  }
  // 프리셋이 켜는 것은 끈 값을, 아니면 켠 값을 적는다.
  for (const [flag, on] of [
    ["--tpm", v.tpm],
    ["--secure-boot", v.secureBoot],
    ["--guest-agent", v.guestAgent],
  ] as const) {
    if (on !== preset) {
      a.push(flag, on ? "on" : "off");
    }
  }
  if (v.guestAgent && !v.agentFreeze) {
    a.push("--agent-freeze", "off");
  }
  if (v.guestAgent && v.agentShutdown !== "agent") {
    a.push("--agent-shutdown", v.agentShutdown);
  }
  if (v.model !== "host") {
    a.push("--cpu-model", v.model);
  }
  if (v.cpuFlags.length > 0) {
    a.push(`--cpu-flags=${v.cpuFlags.join(",")}`);
  }
  const numa = numaSpec(v.numa);
  if (numa.numa || numa.hugepages || Object.keys(numa.cpu).length > 0) {
    missing.add("numa");
  }
  for (const d of v.disks) {
    a.push("--disk", d.volume);
  }
  // 디스크가 여럿이거나 순서를 정했으면 실제 부팅 순서를 적는다(CLI 기본은 첫 디스크만 부팅).
  const orders = bootOrders(v);
  if (v.disks.length > 1 || [...orders.disks, ...orders.cdroms].some((o) => o !== null)) {
    const { booting } = bootList(orders.disks, orders.cdroms);
    if (booting.length > 0) {
      a.push("--boot", booting.map(deviceName).join(","));
    }
  }
  for (const c of v.cdroms) {
    a.push("--cdrom", c.iso || "none");
  }
  const sdn = sdnOf(s, ctx);
  v.nics.forEach((n, i) => {
    const parts = [n.network];
    if (n.securityGroups.length > 0) {
      parts.push(`sg=${n.securityGroups.join("+")}`);
    }
    const ips = sdn[i] ? splitList(n.ips) : [];
    if (ips.length > 0) {
      parts.push(`ip=${ips.join("+")}`);
    }
    if (hasQos(n.qos)) {
      for (const [key, value] of [
        ["qos-out", n.qos.out],
        ["qos-in", n.qos.in],
        ["qos-burst", n.qos.burst],
        ["dscp", n.qos.dscp],
      ] as const) {
        if (value.trim() !== "") {
          parts.push(`${key}=${value.trim()}`);
        }
      }
    }
    if (n.mac !== "") {
      missing.add("mac");
    }
    a.push("--nic", parts.join(","));
  });
  if (v.restart !== "never") {
    a.push("--restart", v.restart);
  }
  if (v.folder !== "") {
    a.push("--folder", folderPath(folderTree(ctx.folders, "vm"), v.folder));
  }
  if (v.node !== "") {
    a.push("--node", nodeName(ctx, v.node));
  }
  if (v.ha) {
    a.push("--ha");
    if (v.haState !== DEFAULT_HA.state) {
      a.push("--ha-state", v.haState);
    }
    if (v.haGroup !== "") {
      a.push("--ha-group", v.haGroup);
    }
    for (const [flag, value, base] of [
      ["--max-restart", v.maxRestart, DEFAULT_HA.max_restart],
      ["--max-relocate", v.maxRelocate, DEFAULT_HA.max_relocate],
      ["--ha-priority", v.haPriority, DEFAULT_HA.priority],
    ] as const) {
      if (Number(value) !== base) {
        a.push(flag, value);
      }
    }
    if (v.guestWatch) {
      a.push("--guest-watch", v.guestWatchSecs);
    }
  }
  if (v.pinned) {
    a.push("--pinned");
  }
  for (const r of v.replication) {
    a.push(
      "--replicate",
      [nodeName(ctx, r.node), `*/${Number(r.minutes)}`, ...(r.pool ? [`pool=${r.pool}`] : [])].join(
        ",",
      ),
    );
  }
  if (v.snapshotSchedule.every !== "off") {
    missing.add("schedule");
  }
  a.push(...cloudInitArgs(v, sdn, files));
  if (v.start) {
    a.push("--start");
  }
  return a;
}

/** `template deploy` 인자(배포 본문과 같은 덮어쓰기) */
function templateDeploy(
  s: BuilderState,
  ctx: BuilderCtx,
  files: CliPlan["files"],
  missing: Set<Missing>,
): string[] {
  const v = s.values;
  const base = s.base.kind === "template" ? s.base : null;
  const t = ctx.vms.find((x) => x.id === base?.id);
  const spec = t?.spec;
  const a = ["orbivisorctl", "template", "deploy", t?.name ?? base?.id ?? "", "--name", v.name];
  if (base?.linked === true) {
    a.push("--linked");
  } else if (base?.linked === false) {
    a.push("--full");
  }
  if (v.node !== "") {
    a.push("--node", nodeName(ctx, v.node));
  }
  if (base?.pool) {
    a.push("--pool", base.pool);
  }
  const vcpus = v.sockets * Number(v.cores);
  if (spec && vcpus !== (spec.cpu.sockets ?? 1) * spec.cpu.cores) {
    a.push("--cpus", String(vcpus));
  }
  if (spec && Number(v.memory) !== spec.memory_mib) {
    a.push("--memory", v.memory);
  }
  if (v.model !== (spec?.cpu.model ?? "host")) {
    a.push("--cpu-model", v.model);
  }
  if (v.cpuFlags.join(" ") !== (spec?.cpu.flags ?? []).join(" ")) {
    a.push(v.cpuFlags.length ? `--cpu-flags=${v.cpuFlags.join(",")}` : "--no-cpu-flags");
  }
  const sdn = sdnOf(s, ctx);
  v.nics.forEach((n, i) => {
    if (n.network !== spec?.nics?.[i]?.network) {
      a.push("--network", `nic${i}=${n.network}`);
    }
    const ips = sdn[i] ? splitList(n.ips) : [];
    if (ips.length > 0) {
      a.push("--ip", `nic${i}=${ips.join("+")}`);
    }
    if (n.securityGroups.length > 0) {
      missing.add("templateSg");
    }
  });
  v.disks.forEach((d, i) => {
    const bytes = d.sizeGib.trim() === "" ? undefined : gibToBytes(d.sizeGib.trim());
    if (bytes !== undefined) {
      a.push("--disk-size", `disk${i}=${sizeArg(Math.ceil(bytes / 1024 ** 2) * 1024 ** 2)}`);
    }
  });
  if (v.folder !== "") {
    missing.add("folder");
  }
  a.push(...cloudInitArgs(v, sdn, files));
  if (v.start) {
    a.push("--start");
  }
  return a;
}

/** CLI 계획: 파일, 명령(볼륨 → 영역 규칙 → VM), 플래그 없는 칸 */
export function cliPlan(s: BuilderState, ctx: BuilderCtx): CliPlan {
  const files: CliPlan["files"] = [];
  const missing = new Set<Missing>();
  const commands: string[][] = [];
  if (s.base.kind === "blank") {
    for (const r of volumeRequests(s, ctx)) {
      commands.push([
        "orbivisorctl",
        "volume",
        "create",
        r.name,
        "--pool",
        r.spec.pool,
        "--size",
        sizeArg(r.spec.size_bytes),
      ]);
    }
  }
  const rule = zoneRule(s);
  if (rule) {
    commands.push([
      "orbivisorctl",
      "placement-rule",
      "create",
      rule.name,
      "--kind",
      rule.spec.kind,
      "--vm",
      s.values.name,
      "--node-tag",
      rule.spec.node_tag!,
    ]);
  }
  commands.push(
    s.base.kind === "template"
      ? templateDeploy(s, ctx, files, missing)
      : vmCreate(s, ctx, files, missing),
  );
  return { files, commands, missing: [...missing] };
}

/** 한 명령을 셸 글로(인자마다 따옴표, 긴 명령은 ` \` 줄 바꿈 — 플래그 앞에서) */
export function commandText(argv: readonly string[]): string {
  const lines: string[] = [];
  let line = "";
  for (const arg of argv.map(shq)) {
    if (arg.startsWith("--") && line.length > 60) {
      lines.push(`${line} \\`);
      line = `  ${arg}`;
    } else {
      line = line ? `${line} ${arg}` : arg;
    }
  }
  return [...lines, line].join("\n");
}

/** 파일을 만드는 heredoc(내용에 끝 표시가 있으면 다른 표시) */
function heredoc(name: string, content: string): string {
  let end = "EOF";
  while (content.split("\n").includes(end)) {
    end += "_";
  }
  return `cat > ${shq(name)} <<'${end}'\n${content.endsWith("\n") ? content : `${content}\n`}${end}`;
}

/**
 * CLI 미리 보기 글. `t`는 주석 줄 문구(`vmBuilder.cli.*`)를 옮긴다.
 */
export function cliText(
  s: BuilderState,
  ctx: BuilderCtx,
  t: (key: string, values?: Record<string, unknown>) => string,
): string {
  const plan = cliPlan(s, ctx);
  const out: string[] = [];
  for (const what of plan.missing) {
    out.push(`# ${t("vmBuilder.cli.noFlag", { what: t(`vmBuilder.cli.what.${what}`) })}`);
  }
  if (s.values.cloudInit.enabled && s.values.cloudInit.password !== "") {
    out.push(`# ${t("vmBuilder.cli.password")}`);
  }
  for (const f of plan.files) {
    out.push(heredoc(f.name, f.content));
  }
  for (const c of plan.commands) {
    out.push(commandText(c));
  }
  return out.join("\n");
}
