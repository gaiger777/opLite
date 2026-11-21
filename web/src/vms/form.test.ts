import { installVm, NODE, replicatedVm, vm } from "@/test/fixtures";
import {
  defaultMachine,
  emptyDisk,
  emptyForm,
  firmwareUnavailable,
  fromVm,
  type CdromRow,
  gibToBytes,
  haUnavailable,
  toSpec,
  validate,
  windows11,
} from "@/vms/form";

it("checks values the server would reject", () => {
  const v = {
    ...emptyForm("q35"),
    name: "Web_01",
    cores: "0",
    memory: "32",
    disks: [
      { ...emptyDisk("web", 0), pool: "" },
      { ...emptyDisk("web", 1), pool: "local", bootOrder: "1" },
    ],
    nics: [
      {
        network: "user",
        model: "virtio" as const,
        mac: "52:54:00:12:34",
        securityGroups: [],
        ips: "",
      },
    ],
  };
  expect(validate(v, true)).toEqual({
    name: "vmForm.errors.name",
    cores: "vmForm.errors.cores",
    memory: "vmForm.errors.memory",
    "disks.0.pool": "vmForm.errors.pool",
    "disks.1.bootOrder": "vmForm.errors.bootOrder",
    "nics.0.mac": "vmForm.errors.mac",
  });
  expect(validate({ ...emptyForm("q35"), name: "web-01" }, true)).toEqual({});
});

it("round-trips a VM through the form", () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      disks: [{ volume: "web-root", bus: "sata", boot_order: 1 }],
      nics: [
        {
          network: "lan",
          model: "e1000e",
          mac: "52:54:00:12:34:56",
          security_groups: ["web", "db"],
        },
        { network: "user", model: "virtio", mac: "52:54:00:12:34:57" },
      ],
      snapshot_schedule: { every: "daily", keep: 7, memory: true },
      ha: true,
    },
  });
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
  // 예약이 없으면 끔으로 보이고 그대로 없다.
  const plain = vm("db", "stopped");
  expect(fromVm(plain).snapshotSchedule.every).toBe("off");
  expect(toSpec(fromVm(plain), "stopped").snapshot_schedule).toBeNull();
  // 고정은 `placement`로 오가고, 고정하지 않은 VM에는 `placement`가 없다.
  const pinned = vm("pin", "running", {
    spec: { ...vm("pin", "running").spec, placement: { pinned: true } },
  });
  expect(fromVm(pinned).pinned).toBe(true);
  expect(toSpec(fromVm(pinned), "running").placement).toEqual({ pinned: true });
  expect(toSpec(fromVm(plain), "stopped")).not.toHaveProperty("placement");
});

it("allows up to eight security groups on an interface", () => {
  const nic = (count: number) => ({
    network: "lan",
    model: "virtio" as const,
    mac: "",
    securityGroups: Array.from({ length: count }, (_, i) => `g${i}`),
    ips: "",
  });
  const v = { ...emptyForm("q35"), name: "web", nics: [nic(8), nic(9)] };
  expect(validate(v, true)).toEqual({ "nics.1.securityGroups": "vmForm.errors.securityGroups" });
  // 그룹이 없는 NIC는 `security_groups`를 보내지 않는다.
  expect(toSpec({ ...v, nics: [nic(0), nic(1)] }, "stopped").nics).toEqual([
    { network: "lan", model: "virtio", mac: null },
    { network: "lan", model: "virtio", mac: null, security_groups: ["g0"] },
  ]);
});

it("checks the number of scheduled snapshots to keep only when scheduled", () => {
  const v = { ...emptyForm("q35"), name: "web" };
  const schedule = { every: "daily" as const, keep: "7", memory: false };
  for (const keep of ["0", "101", "1.5", ""]) {
    expect(validate({ ...v, snapshotSchedule: { ...schedule, keep } }, true)).toEqual({
      "schedule.keep": "vmForm.errors.keep",
    });
  }
  for (const keep of ["1", "100"]) {
    expect(validate({ ...v, snapshotSchedule: { ...schedule, keep } }, true)).toEqual({});
  }
  // 끄면 남은 보관 개수는 보지 않고 보내지도 않는다.
  const off = { ...v, snapshotSchedule: { ...schedule, every: "off" as const, keep: "0" } };
  expect(validate(off, true)).toEqual({});
  expect(toSpec(off, "stopped").snapshot_schedule).toBeNull();
  expect(toSpec({ ...v, snapshotSchedule: schedule }, "stopped").snapshot_schedule).toEqual({
    every: "daily",
    keep: 7,
    memory: false,
  });
});

it("picks the machine from the node architecture", () => {
  const arm = { ...NODE, id: "n2", status: { ...NODE.status, arch: "aarch64" as const } };
  const x86 = { ...NODE, id: "n3", status: { ...NODE.status, arch: "x86_64" as const } };
  const unknown = NODE;
  // 지정 노드의 아키텍처가 먼저다.
  expect(defaultMachine([arm, x86], "n2")).toBe("virt");
  expect(defaultMachine([arm, x86], "n3")).toBe("q35");
  // 자동이거나 지정 노드의 아키텍처를 모르면, 아는 노드가 모두 같을 때 그것이다.
  expect(defaultMachine([arm, unknown])).toBe("virt");
  expect(defaultMachine([arm, unknown], "n1")).toBe("virt");
  expect(defaultMachine([arm, x86])).toBe("q35");
  expect(defaultMachine([unknown])).toBe("q35");
  expect(defaultMachine([])).toBe("q35");
  // 위트니스는 VM을 돌리지 않으므로 그 아키텍처는 보지 않는다.
  const witness = { ...x86, id: "n4", spec: { ...NODE.spec, witness: true } };
  expect(defaultMachine([arm, witness])).toBe("virt");
});

it("matches the server's limits on cores, boot order and MACs", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    cores: "65536",
    disks: [
      { ...emptyDisk("web", 0), pool: "local", bootOrder: "256" },
      { ...emptyDisk("web", 1), pool: "local", bootOrder: "2" },
      { ...emptyDisk("web", 2), pool: "local", bootOrder: "02" },
    ],
    nics: [
      {
        network: "user",
        model: "virtio" as const,
        mac: "01:00:5e:00:00:01",
        securityGroups: [],
        ips: "",
      },
      {
        network: "user",
        model: "virtio" as const,
        mac: "52:54:00:12:34:56",
        securityGroups: [],
        ips: "",
      },
      {
        network: "user",
        model: "virtio" as const,
        mac: "52:54:00:12:34:56",
        securityGroups: [],
        ips: "",
      },
    ],
  };
  expect(validate(v, true)).toEqual({
    cores: "vmForm.errors.cores",
    "disks.0.bootOrder": "vmForm.errors.bootOrder",
    // "02"와 "2"는 같은 순서다.
    "disks.2.bootOrder": "vmForm.errors.bootOrder",
    "nics.0.mac": "vmForm.errors.macMulticast",
    "nics.2.mac": "vmForm.errors.macDuplicate",
  });
  expect(validate({ ...v, cores: "65535", disks: [], nics: [] }, true)).toEqual({});
});

it("checks volume names across disks and against existing volumes", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [
      { ...emptyDisk("", 0), kind: "existing" as const },
      { ...emptyDisk("web", 0), pool: "local", bootOrder: "" },
      { ...emptyDisk("web", 0), pool: "local", bootOrder: "" },
      { ...emptyDisk("web", 3), pool: "local" },
    ],
  };
  expect(validate(v, true, ["web-disk3"])).toEqual({
    "disks.0.volume": "vmForm.errors.volumeChoose",
    "disks.2.volume": "vmForm.errors.volumeDuplicate",
    "disks.3.volume": "vmForm.errors.volumeExists",
  });
});

it("rejects the reserved volume name for new disks", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [{ ...emptyDisk("web", 0), volume: "import", pool: "local" }],
  };
  expect(validate(v, true)).toEqual({ "disks.0.volume": "volumes.errors.reserved" });
});

it("turns GiB into a whole number of bytes", () => {
  expect(gibToBytes("16")).toBe(16 * 1024 ** 3);
  expect(gibToBytes("1.5")).toBe(1.5 * 1024 ** 3);
  for (const bad of [
    "",
    "0",
    "-1",
    "1e-10",
    "0.0000000001",
    "1e3",
    "0x10",
    "Infinity",
    " 1",
    "99999999",
  ]) {
    expect(gibToBytes(bad)).toBeUndefined();
  }
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [{ ...emptyDisk("web", 0), pool: "local" }],
  };
  expect(validate({ ...v, disks: [{ ...v.disks[0]!, sizeGib: "1e3" }] }, true)).toEqual({
    "disks.0.size": "vmForm.errors.size",
  });
});

it("says why HA cannot be turned on for the chosen node", () => {
  const fenced = {
    ...NODE,
    status: { ...NODE.status, ha: { fencing: "process" as const, recover_after_secs: 7 } },
  };
  const off = { ...NODE, id: "n2", name: "node-b" };
  const witness = { ...fenced, id: "n3", name: "node-w", spec: { ...NODE.spec, witness: true } };
  // 노드 목록을 모르면 서버에 맡긴다.
  expect(haUnavailable(undefined, "n2")).toBeNull();
  expect(haUnavailable([fenced, off], "n1")).toBeNull();
  expect(haUnavailable([fenced, off], "n2")).toEqual({
    key: "vmForm.haNodeOff",
    values: { node: "node-b" },
  });
  expect(haUnavailable([witness], "n3")).toEqual({
    key: "vmForm.haNodeOff",
    values: { node: "node-w" },
  });
  // 자동이면 HA 가능 노드가 하나라도 있으면 된다(위트니스는 세지 않는다).
  expect(haUnavailable([fenced, off], "")).toBeNull();
  expect(haUnavailable([off, witness], "")).toEqual({ key: "vmForm.haNoNode" });
});

it("checks the hotplug maximums like the server", () => {
  const v = { ...emptyForm("q35"), name: "web", sockets: 2, cores: "2", memory: "2048" };
  // 비우면 상한 없음이고 보내지 않는다.
  expect(validate(v, true)).toEqual({});
  expect(toSpec(v, "stopped").cpu).not.toHaveProperty("max_vcpus");
  expect(toSpec(v, "stopped")).not.toHaveProperty("max_memory_mib");
  for (const [maxVcpus, error] of [
    ["3", "vmForm.errors.maxVcpusLow"],
    ["5", "vmForm.errors.maxVcpusSockets"],
    ["258", "vmForm.errors.maxVcpusHigh"],
    ["4.5", "vmForm.errors.maxVcpusLow"],
  ] as const) {
    expect(validate({ ...v, maxVcpus }, true)).toEqual({ maxVcpus: error });
  }
  for (const [maxMemory, memory, error] of [
    ["1024", "2048", "vmForm.errors.maxMemoryLow"],
    ["524416", "2048", "vmForm.errors.maxMemoryHigh"],
    ["4100", "2048", "vmForm.errors.memoryAlign"],
    ["4096", "2000", "vmForm.errors.memoryAlign"],
  ] as const) {
    expect(validate({ ...v, memory, maxMemory }, true)).toEqual({ maxMemory: error });
  }
  const set = { ...v, maxVcpus: "4", maxMemory: "524288" };
  expect(validate(set, true)).toEqual({});
  expect(toSpec(set, "stopped").cpu).toEqual({ sockets: 2, cores: 2, model: "host", max_vcpus: 4 });
  expect(toSpec(set, "stopped").max_memory_mib).toBe(524288);
  // 상한이 있는 VM을 폼으로 읽고 다시 보내면 그대로다.
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      cpu: { sockets: 1, cores: 2, model: "host", max_vcpus: 8 },
      max_memory_mib: 16384,
      snapshot_schedule: null,
    },
  });
  expect(fromVm(web)).toMatchObject({ maxVcpus: "8", maxMemory: "16384" });
  expect(toSpec(fromVm(web), "running")).toEqual(web.spec);
});

it("round-trips replication targets and leaves them out when there are none", () => {
  const web = replicatedVm("running");
  const spec = {
    ...web.spec,
    replication: [
      { node: "n2", schedule: "*/15" },
      { node: "n3", schedule: "*/5", pool: "b" },
    ],
  };
  const values = fromVm({ ...web, spec });
  expect(values.replication).toEqual([
    { node: "n2", minutes: "15", pool: "" },
    { node: "n3", minutes: "5", pool: "b" },
  ]);
  expect(toSpec(values, "running")).toEqual({ ...spec, snapshot_schedule: null });
  // "05"는 서버처럼 5분이다.
  expect(
    toSpec({ ...values, replication: [{ node: "n2", minutes: "05", pool: "" }] }, "running")
      .replication,
  ).toEqual([{ node: "n2", schedule: "*/5" }]);
  // 대상을 모두 빼면 필드를 보내지 않는다(서버가 복제를 끈다).
  expect(toSpec({ ...values, replication: [] }, "running")).not.toHaveProperty("replication");
  expect(toSpec(emptyForm("q35"), "stopped")).not.toHaveProperty("replication");
});

it("checks replication targets like the server", () => {
  const base = { ...emptyForm("q35"), name: "web", node: "n1" };
  const row = (node: string, minutes = "15") => ({ node, minutes, pool: "" });
  expect(validate({ ...base, replication: [row("n2"), row("n3", "1440")] }, true)).toEqual({});
  expect(
    validate(
      {
        ...base,
        replication: [row(""), row("n2", "0"), row("n2", "1441"), row("n1", "*/5")],
      },
      true,
    ),
  ).toEqual({
    replication: "vmForm.errors.replicationTargets",
    "replication.0.node": "vmForm.errors.replicationNode",
    "replication.1.minutes": "vmForm.errors.replicationMinutes",
    "replication.2.node": "vmForm.errors.replicationDuplicate",
    "replication.2.minutes": "vmForm.errors.replicationMinutes",
    "replication.3.node": "vmForm.errors.replicationOwnNode",
    "replication.3.minutes": "vmForm.errors.replicationMinutes",
  });
  // 스냅샷 예약과 함께 쓰지 않는다(서버의 409).
  expect(
    validate(
      {
        ...base,
        replication: [row("n2")],
        snapshotSchedule: { every: "daily", keep: "7", memory: false },
      },
      true,
    ),
  ).toEqual({ replication: "vmForm.errors.replicationSnapshots" });
  // 노드를 스케줄러에 맡긴 VM은 어느 대상이든 받는다(서버가 고른 노드와 겹치면 400).
  expect(validate({ ...base, node: "", replication: [row("n1")] }, true)).toEqual({});
});

it("round-trips CD-ROM drives and cloud-init", () => {
  const web = installVm("web", "stopped");
  const values = fromVm(web);
  expect(values.cdroms).toEqual([
    { iso: "alpine", bootOrder: "" },
    { iso: "", bootOrder: "" },
  ]);
  expect(values.cloudInit).toMatchObject({ enabled: true, user: "orbi", passwordSet: true });
  // 저장된 비밀번호는 가림 값으로 되보낸다.
  expect(toSpec(values, "stopped")).toEqual({ ...web.spec, snapshot_schedule: null });
  // 없으면 보내지 않는다(옛 클러스터).
  const plain = toSpec(fromVm(vm("db", "stopped")), "stopped");
  expect(plain).not.toHaveProperty("cdroms");
  expect(plain).not.toHaveProperty("cloud_init");
  expect(
    toSpec({ ...values, cloudInit: { ...values.cloudInit, enabled: false } }, "stopped"),
  ).not.toHaveProperty("cloud_init");
});

it("checks CD-ROM drives and boot orders like the server", () => {
  const cd = (bootOrder = ""): CdromRow => ({ iso: "", bootOrder });
  const base = { ...emptyForm("q35"), name: "web" };
  expect(validate({ ...base, cdroms: [cd(), cd(), cd(), cd(), cd()] }, true)).toEqual({
    cdroms: "vmForm.errors.cdromCount",
  });
  // q35의 SATA 포트 6개: SATA 디스크 넷 + CD 둘 + cloud-init 드라이브는 넘친다.
  const sata = Array.from({ length: 4 }, (_, i) => ({
    ...emptyDisk("web", i),
    pool: "local",
    bus: "sata" as const,
    bootOrder: "",
  }));
  const crowded = { ...base, disks: sata, cdroms: [cd(), cd()] };
  expect(validate(crowded, true)).toEqual({});
  expect(
    validate({ ...crowded, cloudInit: { ...crowded.cloudInit, enabled: true } }, true),
  ).toEqual({ cdroms: "vmForm.errors.sataPorts" });
  // virt에는 SATA 규칙이 없다.
  expect(
    validate(
      { ...crowded, machine: "virt", cloudInit: { ...crowded.cloudInit, enabled: true } },
      true,
    ),
  ).toEqual({});
  // 디스크와 CD-ROM 사이에서도 겹치면 안 된다.
  const disk = { ...emptyDisk("web", 0), pool: "local", bootOrder: "1" };
  expect(validate({ ...base, disks: [disk], cdroms: [cd("1")] }, true)).toEqual({
    "cdroms.0.bootOrder": "vmForm.errors.bootOrder",
  });
  // 255 뒤에는 CD-ROM을 둘 자리가 없다.
  expect(
    validate({ ...base, disks: [{ ...disk, bootOrder: "255" }], cdroms: [cd()] }, true),
  ).toEqual({ cdroms: "vmForm.errors.bootOverflow" });
});

it("rejects the reserved volume names", () => {
  const v = {
    ...emptyForm("q35"),
    name: "web",
    disks: [{ ...emptyDisk("web", 0), pool: "local", volume: "download" }],
  };
  expect(validate(v, true)).toEqual({ "disks.0.volume": "volumes.errors.reserved" });
});

it("round-trips TPM, Secure Boot and the guest agent and leaves them out when off", () => {
  const plain = toSpec({ ...emptyForm("q35"), name: "web" }, "stopped");
  expect(plain).not.toHaveProperty("tpm");
  expect(plain).not.toHaveProperty("secure_boot");
  expect(plain).not.toHaveProperty("guest_agent");
  const win = vm("win", "stopped", {
    spec: {
      ...vm("win", "stopped").spec,
      tpm: true,
      secure_boot: true,
      guest_agent: { freeze_fs: false, shutdown: "acpi" },
    },
  });
  const form = fromVm(win);
  expect(form).toMatchObject({
    tpm: true,
    secureBoot: true,
    guestAgent: true,
    agentFreeze: false,
    agentShutdown: "acpi",
  });
  expect(toSpec(form, "stopped")).toMatchObject({
    tpm: true,
    secure_boot: true,
    guest_agent: { freeze_fs: false, shutdown: "acpi" },
  });
  // 기본 에이전트 설정은 끄는 방법을 보내지 않는다.
  const agent = toSpec({ ...emptyForm("q35"), guestAgent: true }, "stopped");
  expect(agent.guest_agent).toEqual({ freeze_fs: true });
  // 에이전트를 꺼도 설정은 남겨 두되 보내지 않는다.
  expect(toSpec({ ...form, guestAgent: false }, "stopped")).not.toHaveProperty("guest_agent");
});

it("needs UEFI for TPM and Secure Boot", () => {
  const v = { ...emptyForm("q35"), name: "web", firmware: "bios" as const };
  expect(validate({ ...v, tpm: true, secureBoot: true }, true)).toEqual({
    tpm: "vmForm.errors.needsUefi",
    secureBoot: "vmForm.errors.needsUefi",
  });
  expect(validate({ ...v, firmware: "uefi", tpm: true, secureBoot: true }, true)).toEqual({});
});

it("fills in Windows 11 like the CLI preset", () => {
  const v = {
    ...emptyForm("virt"),
    name: "win",
    firmware: "bios" as const,
    sockets: 2,
    disks: [emptyDisk("win", 0)],
    nics: [{ network: "lan", model: "virtio" as const, mac: "", securityGroups: [], ips: "" }],
  };
  const w = windows11(v);
  expect(w).toMatchObject({
    name: "win",
    machine: "q35",
    firmware: "uefi",
    secureBoot: true,
    tpm: true,
    guestAgent: true,
    agentFreeze: true,
    agentShutdown: "agent",
    sockets: 1,
    cores: "2",
    memory: "4096",
  });
  expect(w.disks.map((d) => d.bus)).toEqual(["sata"]);
  expect(w.nics.map((n) => [n.network, n.model])).toEqual([["lan", "e1000e"]]);
  expect(toSpec(w, "stopped")).toMatchObject({ guest_agent: { freeze_fs: true } });
});

it("says why TPM or Secure Boot cannot be turned on for the chosen node", () => {
  const tpmOnly = {
    ...NODE,
    status: { ...NODE.status, firmware_caps: { tpm: true, secure_boot: false } },
  };
  // 옛 노드는 능력을 알리지 않는다(없는 것으로 본다).
  const old = { ...NODE, id: "n2", name: "node-b" };
  const witness = {
    ...NODE,
    id: "n3",
    name: "w",
    spec: { ...NODE.spec, witness: true },
    status: { ...NODE.status, firmware_caps: { tpm: true, secure_boot: true } },
  };
  expect(firmwareUnavailable(undefined, "", "tpm")).toBeNull();
  expect(firmwareUnavailable([tpmOnly, old], "", "tpm")).toBeNull();
  expect(firmwareUnavailable([tpmOnly, old, witness], "", "secure_boot")).toEqual({
    key: "vmForm.secureBootNoNode",
  });
  expect(firmwareUnavailable([tpmOnly, old], "n2", "tpm")).toEqual({
    key: "vmForm.tpmNodeOff",
    values: { node: "node-b" },
  });
  expect(firmwareUnavailable([tpmOnly, old], "n1", "tpm")).toBeNull();
});

it("checks, round-trips and sends NIC QoS (plan 67)", () => {
  const web = vm("web", "running", {
    spec: {
      ...vm("web", "running").spec,
      nics: [
        {
          network: "t1",
          model: "virtio",
          mac: "52:54:00:12:34:56",
          qos: { out_mbps: 100, dscp: 46 },
        },
        { network: "lan", model: "virtio", mac: "52:54:00:12:34:57" },
      ],
    },
  });
  const v = fromVm(web);
  expect(v.nics[0]!.qos).toEqual({ out: "100", in: "", burst: "", dscp: "46" });
  // QoS가 없는 NIC는 칸이 없다(옛 폼 그대로)
  expect(v.nics[1]).not.toHaveProperty("qos");
  expect(toSpec(v, "running", [true, false]).nics).toEqual([
    { network: "t1", model: "virtio", mac: "52:54:00:12:34:56", qos: { out_mbps: 100, dscp: 46 } },
    { network: "lan", model: "virtio", mac: "52:54:00:12:34:57" },
  ]);
  // 모두 비우면 보내지 않는다
  const cleared = {
    ...v,
    nics: [{ ...v.nics[0]!, qos: { out: "", in: "", burst: "", dscp: "" } }],
  };
  expect(toSpec(cleared, "running", [true]).nics?.[0]).not.toHaveProperty("qos");

  const bad = {
    ...v,
    nics: [
      { ...v.nics[1]!, qos: { out: "0", in: "", burst: "", dscp: "7" } },
      { ...v.nics[1]!, mac: "", qos: { out: "", in: "", burst: "100", dscp: "" } },
      { ...v.nics[1]!, mac: "", network: "user", qos: { out: "10", in: "", burst: "", dscp: "" } },
    ],
  };
  expect(validate(bad, false, [], [false, false, false])).toEqual({
    "nics.0.qos.out": "vmForm.errors.qos.out",
    "nics.0.qos.dscp": "vmForm.errors.qos.dscpNeedsSdn",
    "nics.1.qos.burst": "vmForm.errors.qos.burstWithoutRate",
    "nics.2.qos.out": "vmForm.errors.qos.user",
  });
});
