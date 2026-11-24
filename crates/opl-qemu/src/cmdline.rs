use std::path::Path;

use opl_types::cpu::{self, CpuError, ResolvedCpu};
use opl_types::numa::{self, HostPlacement, MemoryPolicy, PageKind, ResolvedNuma};
use opl_types::runtime::{
    DiskKind, HOTPLUG_PORTS, HotplugLayout, ImageFormat, NicBackend, RbdTarget, VmRuntimeSpec,
};
pub use opl_types::runtime::{LiveRestoreDisk, LiveRestoreSource};
use opl_types::vm::{Arch, DiskBus, Firmware, MAX_CDROMS, MachineType, NicModel, Q35_SATA_PORTS};
use thiserror::Error;

use crate::host::HostProfile;
use crate::paths::RuntimePaths;

/// 핫플러그 루트 포트를 모아 두는 `pcie.0` 슬롯. 멀티펑션 8개(`0x1e.0`~`0x1e.7`)라 자동 주소를 밀지 않는다.
pub const HOTPLUG_SLOT: u8 = 0x1e;

/// virtio-mem 블록 크기. 마이그레이션 양쪽이 같게 박아 둔다.
pub const VIRTIO_MEM_BLOCK: &str = "2M";

/// 메모리를 미리 잡는 스레드 수의 상한(`prealloc-threads`, 계획 64)
pub const PREALLOC_THREADS: u32 = 16;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CmdlineError {
    #[error("machine type {machine:?} is not supported on {arch:?} hosts")]
    MachineArchMismatch { machine: MachineType, arch: Arch },
    #[error("BIOS firmware is not supported on aarch64; use UEFI")]
    BiosOnAarch64,
    #[error("SATA disks require the q35 machine type")]
    SataRequiresQ35,
    #[error(
        "at most {Q35_SATA_PORTS} SATA devices (SATA disks, CD-ROM drives and the cloud-init drive) are supported"
    )]
    TooManySataDevices,
    #[error("at most {MAX_CDROMS} CD-ROM drives are supported")]
    TooManyCdroms,
    #[error("disk {0} is an rbd disk without an rbd image")]
    MissingRbdImage(usize),
    #[error("vCPU hotplug is not supported on aarch64")]
    CpuHotplugUnsupported,
    #[error("at most {HOTPLUG_PORTS} hotplug ports are supported")]
    TooManyPorts,
    #[error("{0} {1} was added while running but has no hotplug port")]
    MissingPort(&'static str, usize),
    #[error("Secure Boot needs UEFI firmware")]
    SecureBootNeedsUefi,
    #[error(
        "this node has no Secure Boot firmware (a QEMU firmware descriptor with secure-boot and enrolled-keys)"
    )]
    NoSecureBootFirmware,
    #[error("live restore wiring has {wired} disks but the VM has {disks}")]
    LiveRestoreDisks { disks: usize, wired: usize },
    #[error("a file restore helper takes at most {MAX_HELPER_DISKS} disks (the backup has {0})")]
    TooManyHelperDisks(usize),
    #[error(transparent)]
    Cpu(#[from] CpuError),
}

/// 라이브 복원 NBD 백업이 끊겼을 때 QEMU가 요청을 붙잡고 다시 붙는 시간(초, §11.4). 넘기면 읽기 오류 → VM 멈춤.
pub const LIVE_RESTORE_RECONNECT_SECS: u32 = 300;

/// QEMU 인자 목록(바이너리 제외)을 만든다. 부수 효과가 없는 순수 함수다.
///
/// VM은 항상 `-S`(일시정지) 상태로 시작한다. 호출자는 QMP 연결 뒤 `cont`로 실행을 재개한다.
pub fn build_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    build(spec, host, paths, &[])
}

/// 라이브 복원(계획 68 §11.2) 중인 VM의 인자. [`build_cmdline`]의 디스크마다 백업(`disk{i}-lrsrc` — NBD는
/// `reconnect-delay=300`, 옛 qcow2 백업은 체인을 명시한 json)과 그 위 오버레이(`disk{i}-lr`)를 더하고 장치를 오버레이에
/// 붙인다(`rerror=stop,werror=stop` — 백업을 읽지 못하면 게스트가 오류를 보지 않고 VM이 멈춘다). 대상 볼륨(`disk{i}`)은
/// 지금 모양 그대로 두되 장치에 붙이지 않는다(mirror의 대상). 나머지 인자는 [`build_cmdline`]과 같다. `disks`는 VM
/// 디스크마다 하나다.
pub fn build_live_restore_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
    disks: &[LiveRestoreDisk],
) -> Result<Vec<String>, CmdlineError> {
    if disks.len() != spec.disks.len() {
        return Err(CmdlineError::LiveRestoreDisks {
            disks: spec.disks.len(),
            wired: disks.len(),
        });
    }
    build(spec, host, paths, disks)
}

/// 라이브 복원 백업 노드(`disk{i}-lrsrc`)의 `-blockdev` 값
fn live_restore_source(i: usize, source: &LiveRestoreSource) -> String {
    let node = format!("disk{i}-lrsrc");
    match source {
        LiveRestoreSource::Nbd { socket, export } => format!(
            "driver=nbd,server.type=unix,server.path={},export={},reconnect-delay={LIVE_RESTORE_RECONNECT_SECS},read-only=on,node-name={node}",
            esc_path(socket),
            esc(export)
        ),
        LiveRestoreSource::Qcow2 { chain } => {
            // 자식부터 전체 백업까지 backing을 명시한다(헤더의 backing 이름은 따르지 않음 — 계획 40 `import_chain`과 같은 뜻).
            let mut node_json = serde_json::Value::Null;
            for file in chain.iter().rev() {
                node_json = serde_json::json!({
                    "driver": "qcow2",
                    "read-only": true,
                    "file": { "driver": "file", "filename": file, "read-only": true },
                    "backing": node_json,
                });
            }
            node_json["node-name"] = serde_json::json!(node);
            node_json.to_string()
        }
    }
}

/// [`build_cmdline`]·[`build_live_restore_cmdline`]의 몸통. `live`가 비면 라이브 복원이 아니다.
fn build(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
    live: &[LiveRestoreDisk],
) -> Result<Vec<String>, CmdlineError> {
    check_compat(spec, host)?;

    let mut a = Args::default();
    a.opt(
        "-name",
        format!("guest={},debug-threads=on", esc(&spec.name)),
    );
    a.opt("-uuid", spec.id.to_string());
    a.flag("-no-user-config");
    a.flag("-nodefaults");
    a.flag("-S");
    let machine = match spec.machine {
        MachineType::Q35 => "q35",
        MachineType::Virt => "virt",
    };
    // Secure Boot는 q35에서 SMM이 있어야 펌웨어의 변수 저장소를 게스트 OS가 바꾸지 못한다.
    let smm = spec.secure_boot && spec.machine == MachineType::Q35;
    let smm_opt = if smm { ",smm=on" } else { "" };
    a.opt(
        "-machine",
        format!("{machine}{smm_opt},accel={}", host.accel.as_str()),
    );
    // 켤 때 해석한 CPU(계획 63). 없으면(옛 기록) 모델만 이 노드의 가속기로 편다(`host`는 TCG면 `max`).
    let resolved = match &spec.cpu_resolved {
        Some(r) => r.clone(),
        None => cpu::resolve(&spec.cpu, host.arch, host.accel.into(), None, &[], None)?,
    };
    let cpu = resolved.model.as_str();
    a.opt("-cpu", cpu_option(&resolved));
    let layout = spec.hotplug.as_ref();
    let boot_vcpus = spec.cpu.vcpus();
    // vCPU 자리: 상한이 켤 때 수보다 크면 소켓마다 `max / sockets`개 코어 자리를 두고 앞에서부터 켠다.
    let max_vcpus = match (layout, spec.cpu.max_vcpus) {
        (Some(_), Some(max)) if u32::from(max) > boot_vcpus => Some(u32::from(max)),
        _ => None,
    };
    match max_vcpus {
        Some(max) => a.opt(
            "-smp",
            format!(
                "{boot_vcpus},sockets={},cores={},threads=1,maxcpus={max}",
                spec.cpu.sockets,
                max / u32::from(spec.cpu.sockets)
            ),
        ),
        None => a.opt(
            "-smp",
            format!(
                "sockets={},cores={},threads=1",
                spec.cpu.sockets, spec.cpu.cores
            ),
        ),
    }
    // virtio-mem: 켤 때 메모리 위로 상한까지의 자리
    let virtio_mem = layout.filter(|l| l.max_memory_mib > spec.memory_mib);
    match virtio_mem {
        Some(l) => a.opt(
            "-m",
            format!("{}M,maxmem={}M", spec.memory_mib, l.max_memory_mib),
        ),
        None => a.opt("-m", format!("{}M", spec.memory_mib)),
    }
    // vNUMA(계획 64): 게스트 노드마다 메모리 백엔드와 `-numa`. 없으면 지금 그대로다.
    if let Some(numa) = &spec.numa {
        numa_args(&mut a, spec, numa);
    }
    // 실행 중 붙인 vCPU(원본과 같은 id·자리)
    if let (Some(l), Some(max)) = (layout, max_vcpus) {
        let per_socket = max / u32::from(spec.cpu.sockets);
        for i in boot_vcpus..u32::from(l.vcpus) {
            a.opt(
                "-device",
                format!(
                    "{cpu}-x86_64-cpu,id=cpu{i},socket-id={},core-id={},thread-id=0",
                    i / per_socket,
                    i % per_socket
                ),
            );
        }
    }

    if spec.firmware == Firmware::Uefi {
        // Secure Boot면 MS 키를 등록한 펌웨어의 코드다([`check_compat`]이 있는지 먼저 본다).
        let code = match &host.secure_boot {
            Some(pair) if spec.secure_boot => &pair.code,
            _ => &host.uefi_code,
        };
        if smm {
            a.opt(
                "-global",
                "driver=cfi.pflash01,property=secure,value=on".to_string(),
            );
        }
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=0,format=raw,readonly=on,file={}",
                esc_path(code)
            ),
        );
        a.opt(
            "-drive",
            format!(
                "if=pflash,unit=1,format=raw,file={}",
                esc_path(&paths.uefi_vars)
            ),
        );
    }

    let video = match host.arch {
        Arch::X86_64 => "VGA,id=video0",
        Arch::Aarch64 => "virtio-gpu-pci,id=video0",
    };
    a.opt("-device", video.to_string());
    a.opt("-device", "qemu-xhci,id=usb".to_string());
    a.opt("-device", "usb-tablet,bus=usb.0".to_string());
    a.opt("-device", "usb-kbd,bus=usb.0".to_string());

    // 빈 루트 포트: 주소를 박아 다른 장치의 자동 주소가 그대로다. 실행 중 붙인 장치가 쓰므로 디스크·NIC보다 먼저 둔다.
    if let Some(l) = layout {
        for k in 0..l.ports {
            let mut port = format!(
                "pcie-root-port,id=hp{k},bus=pcie.0,chassis={},addr={HOTPLUG_SLOT:#x}.{k},io-reserve=0",
                u16::from(k) + 1
            );
            if k == 0 {
                port.push_str(",multifunction=on");
            }
            a.opt("-device", port);
        }
    }

    let mut sata_port = 0;
    for (i, disk) in spec.disks.iter().enumerate() {
        let fmt = match disk.format {
            ImageFormat::Qcow2 => "qcow2",
            ImageFormat::Raw => "raw",
        };
        let source = match disk.kind {
            DiskKind::File => format!("driver=file,filename={}", esc_path(&disk.path)),
            DiskKind::Block => format!("driver=host_device,filename={}", esc_path(&disk.path)),
            DiskKind::Rbd => {
                let rbd = disk.rbd.as_ref().ok_or(CmdlineError::MissingRbdImage(i))?;
                rbd_source(rbd)
            }
        };
        // 라이브 복원: 백업 → 오버레이를 앞에 두고 장치는 오버레이에 붙인다(대상 `disk{i}`는 mirror가 채운다).
        let drive = match live.get(i).filter(|lr| !lr.pivoted) {
            Some(lr) => {
                a.opt("-blockdev", live_restore_source(i, &lr.source));
                a.opt(
                    "-blockdev",
                    format!(
                        "driver=qcow2,file.driver=file,file.filename={},backing=disk{i}-lrsrc,node-name=disk{i}-lr",
                        esc_path(&lr.overlay)
                    ),
                );
                format!("disk{i}-lr")
            }
            None => format!("disk{i}"),
        };
        a.opt("-blockdev", format!("{source},node-name=disk{i}-file"));
        a.opt(
            "-blockdev",
            format!("driver={fmt},file=disk{i}-file,node-name=disk{i}"),
        );
        let mut dev = match disk.bus {
            DiskBus::VirtioBlk => format!(
                "virtio-blk-pci,drive={drive},id=disk{i}-dev{}",
                hot_bus(layout.map(|l| (l.boot_disks, &l.disk_ports[..])), i)
            ),
            DiskBus::Sata => {
                let d = format!("ide-hd,drive={drive},bus=ide.{sata_port},id=disk{i}-dev");
                sata_port += 1;
                d
            }
        };
        if let Some(order) = disk.boot_order {
            dev.push_str(&format!(",bootindex={order}"));
        }
        if !live.is_empty() {
            dev.push_str(",rerror=stop,werror=stop");
        }
        a.opt("-device", dev);
    }

    for (i, nic) in spec.nics.iter().enumerate() {
        let netdev = match &nic.backend {
            NicBackend::User => format!("user,id=net{i}"),
            NicBackend::Tap { ifname, .. } => format!(
                "tap,id=net{i},ifname={},script=no,downscript=no",
                esc(ifname)
            ),
        };
        a.opt("-netdev", netdev);
        let model = match nic.model {
            NicModel::Virtio => "virtio-net-pci",
            NicModel::E1000e => "e1000e",
        };
        let mut dev = format!(
            "{model},netdev=net{i},mac={},id=nic{i}{}",
            nic.mac,
            hot_bus(layout.map(|l| (l.boot_nics, &l.nic_ports[..])), i)
        );
        // VXLAN 네트워크의 MTU를 게스트에 알린다(virtio만 받는다).
        if let (NicModel::Virtio, NicBackend::Tap { mtu: Some(mtu), .. }) =
            (nic.model, &nic.backend)
        {
            dev.push_str(&format!(",host_mtu={mtu}"));
        }
        a.opt("-device", dev);
    }

    // 게스트 에이전트 채널은 NIC 뒤에 단다(앞 장치들의 PCI 주소가 그대로다). 감시하는 VM만 단다.
    if spec.guest_agent {
        a.opt(
            "-chardev",
            format!(
                "socket,id=qga0,path={},server=on,wait=off",
                esc_path(&paths.qga_socket)
            ),
        );
        a.opt("-device", "virtio-serial-pci,id=qgaser".to_string());
        a.opt(
            "-device",
            "virtserialport,bus=qgaser.0,chardev=qga0,name=org.qemu.guest_agent.0".to_string(),
        );
    }

    // virtio-mem도 앞 장치들의 PCI 주소가 그대로이게 뒤에 단다. NUMA 노드는 `maxmem`이 있으면 QEMU가 만든다. vNUMA
    // VM(계획 64)은 게스트 노드마다 장치 하나다.
    if let (Some(l), Some(numa)) = (virtio_mem, &spec.numa) {
        let sizes = numa::split_blocks(l.max_memory_mib - spec.memory_mib, numa.nodes.len());
        let plugged = numa::split_plugged(l.plugged_mib, &sizes);
        for (i, (size, plugged)) in sizes.iter().zip(plugged).enumerate() {
            // 2M 블록이라 1G hugepages는 쓰지 못한다(1G + virtio-mem은 검증이 막고, `any`는 할당기가 2M으로 고른다).
            let page = numa.page.filter(|p| *p == PageKind::Size2M);
            let id = format!("vmem{i}");
            let place = spec.host_placement.as_ref();
            let mut object = backend(numa, place, i, &id, *size, page);
            if page.is_some() {
                // hugetlb mmap은 MAP_NORESERVE 없이는 상한 전체의 페이지를 띄울 때 예약한다(`HugePages_Rsvd`): 꽂을 때
                // 장치의 `prealloc=on`이 잡게 하고 띄울 때는 잡지 않는다(리뷰 1, QEMU 6.1+ `reserve`, 7.1+ 장치 `prealloc`).
                object.push_str(",reserve=off");
            }
            a.opt("-object", object);
            let prealloc = if numa.page == Some(PageKind::Size2M) {
                ",prealloc=on"
            } else {
                ""
            };
            a.opt(
                "-device",
                format!(
                    "virtio-mem-pci,id=vmem{i}-dev,memdev=vmem{i},node={i},block-size={VIRTIO_MEM_BLOCK},requested-size={plugged}M{prealloc}"
                ),
            );
        }
    } else if let Some(l) = virtio_mem {
        a.opt(
            "-object",
            format!(
                "memory-backend-ram,id=vmem0,size={}M",
                l.max_memory_mib - spec.memory_mib
            ),
        );
        a.opt(
            "-device",
            format!(
                "virtio-mem-pci,id=vmem0-dev,memdev=vmem0,block-size={VIRTIO_MEM_BLOCK},requested-size={}M",
                l.plugged_mib
            ),
        );
    }

    // CD-ROM 드라이브와 cloud-init 씨앗도 앞 장치들의 PCI 주소가 그대로이게 맨 뒤에 단다. q35는 내장 AHCI의 SATA
    // 디스크 다음 포트, virt는 새 virtio-scsi 컨트롤러다. 매체는 늘 읽기 전용이고 씨앗은 부팅 장치가 아니다.
    let seed = spec.cloud_init.as_ref().map(|_| paths.cidata.as_path());
    if !spec.cdroms.is_empty() || seed.is_some() {
        if spec.machine == MachineType::Virt {
            a.opt("-device", "virtio-scsi-pci,id=scsi0".to_string());
        }
        let drives = spec
            .cdroms
            .iter()
            .enumerate()
            .map(|(i, cd)| {
                let medium = cd.medium.as_ref().map(|m| m.path.as_path());
                (format!("cd{i}"), medium, cd.boot_order)
            })
            .chain(seed.map(|p| ("cidata".to_string(), Some(p), None)));
        for (name, medium, boot_order) in drives {
            let mut dev = match spec.machine {
                MachineType::Q35 => {
                    let d = format!("ide-cd,bus=ide.{sata_port}");
                    sata_port += 1;
                    d
                }
                MachineType::Virt => "scsi-cd,bus=scsi0.0".to_string(),
            };
            if let Some(path) = medium {
                a.opt(
                    "-blockdev",
                    format!(
                        "driver=file,filename={},node-name={name}-file,read-only=on",
                        esc_path(path)
                    ),
                );
                a.opt(
                    "-blockdev",
                    format!("driver=raw,file={name}-file,node-name={name},read-only=on"),
                );
                dev.push_str(&format!(",drive={name}"));
            }
            dev.push_str(&format!(",id={name}-dev"));
            if let Some(order) = boot_order {
                dev.push_str(&format!(",bootindex={order}"));
            }
            a.opt("-device", dev);
        }
    }

    // TPM도 앞 장치들의 PCI 주소가 그대로이게 맨 뒤에 단다. swtpm은 드라이버가 먼저 띄워 둔다.
    if spec.tpm.is_some() {
        a.opt(
            "-chardev",
            format!("socket,id=chrtpm,path={}", esc_path(&paths.tpm_socket)),
        );
        a.opt("-tpmdev", "emulator,id=tpm0,chardev=chrtpm".to_string());
        let dev = match spec.machine {
            MachineType::Q35 => "tpm-crb,tpmdev=tpm0",
            MachineType::Virt => "tpm-tis-device,tpmdev=tpm0",
        };
        a.opt("-device", dev.to_string());
    }

    a.opt("-vnc", format!("unix:{}", esc_path(&paths.vnc_socket)));
    a.opt(
        "-qmp",
        format!("unix:{},server=on,wait=off", esc_path(&paths.qmp_socket)),
    );
    a.opt("-pidfile", paths.pidfile.display().to_string());
    Ok(a.0)
}

/// vNUMA(계획 64 §10): 게스트 노드마다 `-object memory-backend-…,id=ram-node<i>` + `-numa node`, 모든 소켓의 `-numa cpu`,
/// 거리가 있으면 `-numa dist`. 백엔드 id가 RAMBlock 이름이라 마이그레이션 양쪽이 같은 모양이어야 한다(기록).
fn numa_args(a: &mut Args, spec: &VmRuntimeSpec, numa: &ResolvedNuma) {
    for (i, node) in numa.nodes.iter().enumerate() {
        let mut backend = backend(
            numa,
            spec.host_placement.as_ref(),
            i,
            &format!("ram-node{i}"),
            node.memory_mib,
            numa.page,
        );
        // 전용·hugepages VM은 켤 때 모두 잡는다(마이그레이션 받는 쪽도 받기 전에 — 옮기는 중 페이지가 모자라지 않게).
        if numa.page.is_some() || !numa.policy.is_shared() {
            let threads = spec.cpu.vcpus().clamp(1, PREALLOC_THREADS);
            backend.push_str(&format!(",prealloc=on,prealloc-threads={threads}"));
        }
        a.opt("-object", backend);
        a.opt("-numa", format!("node,nodeid={i},memdev=ram-node{i}"));
    }
    for (i, node) in numa.nodes.iter().enumerate() {
        for socket in &node.sockets {
            a.opt("-numa", format!("cpu,node-id={i},socket-id={socket}"));
        }
    }
    for (i, row) in numa.distances.iter().enumerate() {
        for (j, d) in row.iter().enumerate().skip(i + 1) {
            a.opt("-numa", format!("dist,src={i},dst={j},val={d}"));
        }
    }
}

/// 게스트 노드 `i`의 메모리 백엔드 `id`(크기 `size_mib`): hugepages(`page`)면 memfd hugetlb, 아니면 ram. 묶는 VM은 그
/// 노드의 호스트 노드에 `policy=bind|preferred`.
fn backend(
    numa: &ResolvedNuma,
    placement: Option<&HostPlacement>,
    i: usize,
    id: &str,
    size_mib: u32,
    page: Option<PageKind>,
) -> String {
    let mut o = match page {
        Some(p) => {
            format!("memory-backend-memfd,id={id},size={size_mib}M,hugetlb=on,hugetlbsize={p}")
        }
        None => format!("memory-backend-ram,id={id},size={size_mib}M"),
    };
    if let (Some(policy), Some(host)) = (numa.memory, placement.and_then(|p| p.host_nodes.get(i))) {
        let policy = match policy {
            MemoryPolicy::Strict => "bind",
            MemoryPolicy::Preferred => "preferred",
        };
        o.push_str(&format!(",host-nodes={host},policy={policy}"));
    }
    o
}

/// `-cpu`의 값: `<model>[,enforce][,<flag>=on|off]…`(플래그는 이름순). 플래그는 그 CPU 형식의 전역 속성이라 실행 중
/// 붙이는 vCPU(`<model>-x86_64-cpu`)에도 똑같이 걸린다.
pub fn cpu_option(cpu: &ResolvedCpu) -> String {
    let mut s = cpu.model.clone();
    if cpu.enforce {
        s.push_str(",enforce");
    }
    for (name, on) in &cpu.flags {
        s.push_str(&format!(",{name}={}", if *on { "on" } else { "off" }));
    }
    s
}

/// 라이브 마이그레이션을 받을 QEMU의 인자. [`build_cmdline`]과 같은 장치에 `-incoming defer`를 더한다.
/// QEMU는 QMP `migrate-incoming`을 받을 때까지 기다리고, 받은 뒤에도 `-S`라 `cont` 전까지 멈춰 있다.
pub fn build_incoming_cmdline(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    paths: &RuntimePaths,
) -> Result<Vec<String>, CmdlineError> {
    let mut args = build_cmdline(spec, host, paths)?;
    args.push("-incoming".into());
    args.push("defer".into());
    Ok(args)
}

/// 복제본을 받는 QEMU의 인자. 게스트 장치 없이(`-machine none`) 멈춘 채(`-S`) QMP만 연다. 복제본 블록 노드와
/// NBD 서버는 QMP로 만든다.
pub fn build_replica_receiver_cmdline(qmp_socket: &Path, pidfile: &Path) -> Vec<String> {
    [
        "-machine",
        "none",
        "-nodefaults",
        "-display",
        "none",
        "-S",
        "-qmp",
    ]
    .map(String::from)
    .into_iter()
    .chain([
        format!("unix:{},server=on,wait=off", esc_path(qmp_socket)),
        "-pidfile".into(),
        pidfile.display().to_string(),
    ])
    .collect()
}

/// 실행 중 붙인 장치(순번이 켤 때 수 이상)의 루트 포트 버스 옵션. 켤 때 단 장치는 `pcie.0`이라 빈 문자열이다.
/// 포트가 있는지는 [`check_compat`]이 먼저 본다.
fn hot_bus(ports: Option<(u8, &[u8])>, i: usize) -> String {
    match ports {
        Some((boot, ports)) if i >= usize::from(boot) => ports
            .get(i - usize::from(boot))
            .map_or_else(String::new, |p| format!(",bus=hp{p}")),
        _ => String::new(),
    }
}

/// RBD 이미지를 여는 `rbd` 블록 드라이버 옵션. 네임스페이스와 conf는 있을 때만 넣는다.
fn rbd_source(rbd: &RbdTarget) -> String {
    let mut s = format!("driver=rbd,pool={}", esc(&rbd.pool));
    if let Some(ns) = &rbd.namespace {
        s.push_str(&format!(",namespace={}", esc(ns)));
    }
    s.push_str(&format!(
        ",image={},user={}",
        esc(&rbd.image),
        esc(&rbd.user)
    ));
    if let Some(conf) = &rbd.conf {
        s.push_str(&format!(",conf={}", esc(conf)));
    }
    s
}

fn check_compat(spec: &VmRuntimeSpec, host: &HostProfile) -> Result<(), CmdlineError> {
    match (spec.machine, host.arch) {
        (MachineType::Q35, Arch::X86_64) | (MachineType::Virt, Arch::Aarch64) => {}
        (machine, arch) => return Err(CmdlineError::MachineArchMismatch { machine, arch }),
    }
    if host.arch == Arch::Aarch64 && spec.firmware == Firmware::Bios {
        return Err(CmdlineError::BiosOnAarch64);
    }
    if spec.secure_boot {
        if spec.firmware != Firmware::Uefi {
            return Err(CmdlineError::SecureBootNeedsUefi);
        }
        if host.secure_boot.is_none() {
            return Err(CmdlineError::NoSecureBootFirmware);
        }
    }
    let sata = spec.disks.iter().filter(|d| d.bus == DiskBus::Sata).count();
    if sata > 0 && spec.machine != MachineType::Q35 {
        return Err(CmdlineError::SataRequiresQ35);
    }
    if spec.cdroms.len() > MAX_CDROMS {
        return Err(CmdlineError::TooManyCdroms);
    }
    // q35는 SATA 디스크·CD-ROM·씨앗이 내장 AHCI 포트를 나눠 쓴다. virt의 CD는 virtio-scsi에 단다.
    let drives = spec.cdroms.len() + usize::from(spec.cloud_init.is_some());
    let sata_devices = match spec.machine {
        MachineType::Q35 => sata + drives,
        MachineType::Virt => sata,
    };
    if sata_devices > Q35_SATA_PORTS {
        return Err(CmdlineError::TooManySataDevices);
    }
    if let Some(layout) = &spec.hotplug {
        check_layout(spec, host, layout)?;
    }
    Ok(())
}

fn check_layout(
    spec: &VmRuntimeSpec,
    host: &HostProfile,
    layout: &HotplugLayout,
) -> Result<(), CmdlineError> {
    // aarch64 `virt`는 CPU 핫플러그가 없다(QEMU 11.1까지).
    let boot = spec.cpu.vcpus();
    if host.arch == Arch::Aarch64 && (spec.cpu.max() > boot || u32::from(layout.vcpus) > boot) {
        return Err(CmdlineError::CpuHotplugUnsupported);
    }
    if layout.ports > HOTPLUG_PORTS {
        return Err(CmdlineError::TooManyPorts);
    }
    let missing = |kind, count: usize, boot: u8, ports: &[u8]| {
        (usize::from(boot)..count)
            .find(|&i| ports.get(i - usize::from(boot)).is_none())
            .map(|i| CmdlineError::MissingPort(kind, i))
    };
    if let Some(e) = missing(
        "disk",
        spec.disks.len(),
        layout.boot_disks,
        &layout.disk_ports,
    )
    .or_else(|| missing("nic", spec.nics.len(), layout.boot_nics, &layout.nic_ports))
    {
        return Err(e);
    }
    Ok(())
}

/// 파일 복원 도우미 VM의 사용자 내리기(계획 69 §7.4). QEMU가 블록 장치·NBD 연결·소켓·로그를 다 연 뒤 바꾼다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperDrop {
    /// `-run-with user=<이름>`(QEMU 9.1+)
    RunWith(String),
    /// `-runas <이름>`(QEMU 9.0까지 — 10.x는 받되 경고)
    Runas(String),
}

/// 파일 복원 도우미 VM의 디스크 하나: 읽기 전용 백업 원본과 그 위 버리는 덮개(qcow2, backing은 명령줄에서만)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperDisk<'a> {
    pub source: &'a LiveRestoreSource,
    pub overlay: &'a Path,
}

/// 파일 복원 도우미 VM(계획 69 §7.4)의 인자 재료
#[derive(Debug, Clone)]
pub struct RestoreHelperArgs<'a> {
    /// 세션 id(`-name opl-fr-<id>` — 남은 것 치우기가 명령줄로 알아본다)
    pub id: uuid::Uuid,
    pub arch: Arch,
    pub accel: crate::host::Accel,
    pub kernel: &'a Path,
    pub initrd: &'a Path,
    pub memory_mib: u32,
    pub vcpus: u32,
    pub disks: &'a [HelperDisk<'a>],
    /// 에이전트 채널(virtio-serial)의 호스트 쪽 유닉스 소켓(QEMU가 서버)
    pub ctl_socket: &'a Path,
    /// 직렬 콘솔 로그(덧붙여 씀 — opld 지킴이가 크기를 자른다)
    pub console_log: &'a Path,
    pub drop: Option<HelperDrop>,
    /// seccomp `-sandbox`(seccomp로 빌드한 QEMU만)
    pub sandbox: bool,
}

/// 도우미 VM 디스크 수 상한(virtio-blk-pci는 PCI 자리 하나씩 — q35·virt 모두 넉넉히 남는 수)
pub const MAX_HELPER_DISKS: usize = 24;

/// 도우미 이미지의 통신 규약 판(`opl.proto=` — 에이전트가 다르면 끈다). `opl_restore::proto::PROTO_VERSION`과 같아야
/// 한다(opl-driver 시험이 본다).
pub const HELPER_PROTO: u32 = 2;

/// 파일 복원 도우미 VM의 QEMU 인자(바이너리 제외, 계획 69 §7.4). 펌웨어 없이 커널을 바로 올리고(`-kernel`·`-initrd`),
/// 장치는 디스크마다 virtio-blk(읽기 전용 원본 → 덮개), virtio-serial 하나, virtio-rng, 직렬 콘솔(파일)뿐이다 —
/// 네트워크·화면·USB가 없다. 원본은 `read-only=on`(NBD는 다시 붙지 않음 — 끊기면 세션이 실패한다), 옛 풀 백업은 체인을
/// 명시한 json. 디스크마다 `serial=opl-disk{i}`로 에이전트가 번호를 맞춘다. 순수 함수다.
pub fn build_restore_helper_cmdline(
    a: &RestoreHelperArgs<'_>,
) -> Result<Vec<String>, CmdlineError> {
    use crate::host::Accel;
    if a.disks.len() > MAX_HELPER_DISKS {
        return Err(CmdlineError::TooManyHelperDisks(a.disks.len()));
    }
    let mut v = Args::default();
    v.opt("-name", format!("opl-fr-{}", a.id));
    v.flag("-nodefaults");
    v.flag("-no-user-config");
    v.opt("-display", "none".into());
    v.opt("-net", "none".into());
    let accel = a.accel.as_str();
    let (machine, cpu, console) = match a.arch {
        Arch::X86_64 => (
            format!("q35,accel={accel}"),
            match a.accel {
                Accel::Tcg => "max",
                Accel::Kvm | Accel::Hvf => "host",
            },
            "ttyS0",
        ),
        Arch::Aarch64 => (
            match a.accel {
                // HVF는 GICv3를 QEMU 판에 따라 받지 못한다 — 기본(GICv2)으로 둔다.
                Accel::Hvf => format!("virt,accel={accel}"),
                Accel::Kvm | Accel::Tcg => format!("virt,accel={accel},gic-version=max"),
            },
            match a.accel {
                // TCG의 `max`는 포인터 인증(QARMA)을 흉내 내 느리다 — 구현 정의 알고리즘이 훨씬 빠르다(§4).
                Accel::Tcg => "max,pauth-impdef=on",
                Accel::Kvm | Accel::Hvf => "host",
            },
            "ttyAMA0",
        ),
    };
    v.opt("-machine", machine);
    v.opt("-cpu", cpu.into());
    v.opt("-smp", a.vcpus.to_string());
    v.opt("-m", format!("{}M", a.memory_mib));
    v.opt("-kernel", a.kernel.display().to_string());
    v.opt("-initrd", a.initrd.display().to_string());
    v.opt(
        "-append",
        format!(
            "console={console} panic=-1 quiet opl.session={} opl.proto={HELPER_PROTO}",
            a.id
        ),
    );
    for (i, d) in a.disks.iter().enumerate() {
        v.opt("-blockdev", helper_source(i, d.source));
        v.opt(
            "-blockdev",
            format!(
                "driver=qcow2,file.driver=file,file.filename={},backing=src{i},node-name=ov{i}",
                esc_path(d.overlay)
            ),
        );
        v.opt(
            "-device",
            format!("virtio-blk-pci,drive=ov{i},serial=opl-disk{i}"),
        );
    }
    v.opt("-device", "virtio-serial-pci".into());
    v.opt(
        "-chardev",
        format!(
            "socket,id=ctl,path={},server=on,wait=off",
            esc_path(a.ctl_socket)
        ),
    );
    v.opt(
        "-device",
        "virtserialport,chardev=ctl,name=org.orbivisor.restore.0".into(),
    );
    v.opt("-device", "virtio-rng-pci".into());
    v.opt(
        "-chardev",
        format!("file,id=con,path={},append=on", esc_path(a.console_log)),
    );
    v.opt("-serial", "chardev:con".into());
    v.flag("-no-reboot");
    match &a.drop {
        Some(HelperDrop::RunWith(user)) => v.opt("-run-with", format!("user={}", esc(user))),
        Some(HelperDrop::Runas(user)) => v.opt("-runas", user.clone()),
        None => {}
    }
    if a.sandbox {
        // seccomp는 사용자를 바꾸기 전에 걸린다: `elevateprivileges=deny`면 QEMU 자신의 setuid가 SIGSYS로 죽는다(Rocky
        // qemu-kvm 10.1에서 확인). 내린 뒤에는 root로 돌아갈 수 없으므로 그때만 뺀다.
        let sandbox = if a.drop.is_some() {
            HELPER_SANDBOX_DROPPED
        } else {
            HELPER_SANDBOX
        };
        v.opt("-sandbox", sandbox.into());
    }
    Ok(v.0)
}

/// 도우미 VM의 seccomp 설정(§7.2): 옛 시스템 호출·권한 올리기·새 프로세스·자원 바꾸기를 막는다.
pub const HELPER_SANDBOX: &str =
    "on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny";
/// 사용자를 내리는 도우미의 seccomp 설정([`HELPER_SANDBOX`]에서 `elevateprivileges=deny`를 뺌)
pub const HELPER_SANDBOX_DROPPED: &str = "on,obsolete=deny,spawn=deny,resourcecontrol=deny";

/// 도우미 디스크 원본(`src{i}`)의 `-blockdev` 값 — 늘 읽기 전용, NBD는 다시 붙지 않는다.
fn helper_source(i: usize, source: &LiveRestoreSource) -> String {
    let node = format!("src{i}");
    match source {
        LiveRestoreSource::Nbd { socket, export } => format!(
            "driver=nbd,server.type=unix,server.path={},export={},read-only=on,node-name={node}",
            esc_path(socket),
            esc(export)
        ),
        LiveRestoreSource::Qcow2 { chain } => {
            let mut node_json = serde_json::Value::Null;
            for file in chain.iter().rev() {
                node_json = serde_json::json!({
                    "driver": "qcow2",
                    "read-only": true,
                    "file": { "driver": "file", "filename": file, "read-only": true },
                    "backing": node_json,
                });
            }
            node_json["node-name"] = serde_json::json!(node);
            node_json.to_string()
        }
    }
}

#[derive(Default)]
struct Args(Vec<String>);

impl Args {
    fn flag(&mut self, flag: &str) {
        self.0.push(flag.to_string());
    }

    fn opt(&mut self, flag: &str, value: String) {
        self.0.push(flag.to_string());
        self.0.push(value);
    }
}

/// QEMU 옵션 값 안의 쉼표는 두 번 써서 이스케이프한다.
fn esc(s: &str) -> String {
    s.replace(',', ",,")
}

fn esc_path(p: &Path) -> String {
    esc(&p.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use opl_types::cpu::CpuSource;
    use opl_types::runtime::{
        DiskKind, RbdTarget, RuntimeCdrom, RuntimeDisk, RuntimeMedium, RuntimeNic,
    };
    use opl_types::vm::{CpuModel, CpuSpec};
    use uuid::Uuid;

    use super::*;
    use crate::host::Accel;

    /// 이미 갈아탄 디스크(`pivoted`, 리뷰 1 I3)는 다시 켜도 대상(`disk{i}`)에 붙는다 — 다른 디스크는 그대로 오버레이에.
    #[test]
    fn pivoted_live_restore_disks_attach_their_target() {
        let spec = x86_spec();
        let disk = |i: usize, pivoted| LiveRestoreDisk {
            source: LiveRestoreSource::Nbd {
                socket: "/run/opl/bk.sock".into(),
                export: format!("disk{i}"),
            },
            overlay: format!("/var/lib/opl/vm/x/lr-disk{i}.qcow2").into(),
            pivoted,
        };
        let args = build_live_restore_cmdline(
            &spec,
            &x86_host(),
            &paths(),
            &[disk(0, false), disk(1, true)],
        )
        .unwrap();
        assert!(args.iter().any(|a| a.ends_with("node-name=disk0-lr")));
        assert!(!args.iter().any(|a| a.contains("disk1-lr")), "{args:?}");
        let devices: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-device" && w[1].contains("id=disk"))
            .map(|w| w[1].as_str())
            .collect();
        assert!(devices[0].contains("drive=disk0-lr,"), "{devices:?}");
        assert!(devices[1].contains("drive=disk1,"), "{devices:?}");
    }

    /// 계획 68 §11.2: 라이브 복원 VM은 디스크마다 백업 → 오버레이를 두고 장치를 오버레이에 붙인다(`rerror`·`werror`=stop).
    /// 대상 볼륨 노드(`disk{i}`)는 그대로 있되 장치에 붙지 않는다. 디스크 아닌 인자는 보통 명령줄과 같다.
    #[test]
    fn live_restore_wires_backup_overlay_and_target() {
        let spec = x86_spec();
        let live = [
            LiveRestoreDisk {
                source: LiveRestoreSource::Nbd {
                    socket: "/run/opl/bk,1.sock".into(),
                    export: "disk0".into(),
                },
                overlay: "/var/lib/opl/vm/x/lr-disk0.qcow2".into(),
                pivoted: false,
            },
            LiveRestoreDisk {
                source: LiveRestoreSource::Qcow2 {
                    chain: vec!["/bk/b2/disk1.qcow2".into(), "/bk/b1/disk1.qcow2".into()],
                },
                overlay: "/var/lib/opl/vm/x/lr-disk1.qcow2".into(),
                pivoted: false,
            },
        ];
        let args = build_live_restore_cmdline(&spec, &x86_host(), &paths(), &live).unwrap();
        let plain = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        let blockdevs: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-blockdev")
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(
            blockdevs[0],
            "driver=nbd,server.type=unix,server.path=/run/opl/bk,,1.sock,export=disk0,reconnect-delay=300,read-only=on,node-name=disk0-lrsrc"
        );
        assert_eq!(
            blockdevs[1],
            "driver=qcow2,file.driver=file,file.filename=/var/lib/opl/vm/x/lr-disk0.qcow2,backing=disk0-lrsrc,node-name=disk0-lr"
        );
        assert_eq!(
            blockdevs[2],
            "driver=file,filename=/var/lib/opl/pools/local/web-01-root.qcow2,node-name=disk0-file"
        );
        assert_eq!(blockdevs[3], "driver=qcow2,file=disk0-file,node-name=disk0");
        // 옛 백업 체인은 backing을 명시한 json(자식 → 부모 → 끝은 null)
        let chain: serde_json::Value = serde_json::from_str(blockdevs[4]).unwrap();
        assert_eq!(chain["node-name"], "disk1-lrsrc");
        assert_eq!(chain["read-only"], true);
        assert_eq!(chain["file"]["filename"], "/bk/b2/disk1.qcow2");
        assert_eq!(chain["backing"]["file"]["filename"], "/bk/b1/disk1.qcow2");
        assert_eq!(chain["backing"]["backing"], serde_json::Value::Null);
        assert!(blockdevs[5].ends_with("backing=disk1-lrsrc,node-name=disk1-lr"));
        let devices: Vec<&str> = args
            .windows(2)
            .filter(|w| w[0] == "-device" && w[1].contains("id=disk"))
            .map(|w| w[1].as_str())
            .collect();
        assert_eq!(
            devices,
            [
                "virtio-blk-pci,drive=disk0-lr,id=disk0-dev,bootindex=1,rerror=stop,werror=stop",
                "ide-hd,drive=disk1-lr,bus=ide.0,id=disk1-dev,rerror=stop,werror=stop",
            ]
        );
        // 디스크 인자를 빼면 보통 명령줄과 같다.
        let others = |a: &[String]| -> Vec<String> {
            let mut out = Vec::new();
            let mut it = a.iter().peekable();
            while let Some(x) = it.next() {
                if (x == "-blockdev" || x == "-device")
                    && it.peek().is_some_and(|v| v.contains("disk"))
                {
                    it.next();
                    continue;
                }
                out.push(x.clone());
            }
            out
        };
        assert_eq!(others(&args), others(&plain));
        // 보통 명령줄에는 오류 정책을 더하지 않는다(라이브 복원이 아닌 VM은 지금과 같음 — §13).
        assert!(!plain.iter().any(|a| a.contains("rerror")));
        // 디스크 수가 다르면 거절
        assert_eq!(
            build_live_restore_cmdline(&spec, &x86_host(), &paths(), &live[..1]),
            Err(CmdlineError::LiveRestoreDisks { disks: 2, wired: 1 })
        );
    }

    #[test]
    fn replica_receiver_has_no_guest_devices() {
        let args = build_replica_receiver_cmdline(
            Path::new("/run/opl/replica-x,y/qmp.sock"),
            Path::new("/run/opl/replica-x,y/qemu.pid"),
        );
        assert_eq!(
            args,
            [
                "-machine",
                "none",
                "-nodefaults",
                "-display",
                "none",
                "-S",
                "-qmp",
                "unix:/run/opl/replica-x,,y/qmp.sock,server=on,wait=off",
                "-pidfile",
                "/run/opl/replica-x,y/qemu.pid",
            ]
        );
    }

    fn x86_host() -> HostProfile {
        HostProfile {
            arch: Arch::X86_64,
            accel: Accel::Kvm,
            qemu_binary: PathBuf::from("/usr/bin/qemu-system-x86_64"),
            uefi_code: PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            uefi_vars_template: PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
            secure_boot: None,
        }
    }

    fn mac_host() -> HostProfile {
        HostProfile::homebrew_hvf(Path::new("/opt/homebrew/share/qemu"))
    }

    fn paths() -> RuntimePaths {
        RuntimePaths::new(Path::new("/run/opl"), Uuid::nil()).unwrap()
    }

    fn x86_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            id: Uuid::nil(),
            name: "web-01".into(),
            cpu: CpuSpec {
                emulator: None,
                policy: Default::default(),
                thread_policy: None,
                flags: Vec::new(),
                sockets: 1,
                cores: 4,
                model: CpuModel::Host,
                max_vcpus: None,
            },
            memory_mib: 8192,
            firmware: Firmware::Uefi,
            machine: MachineType::Q35,
            disks: vec![
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-root.qcow2".into(),
                    kind: DiskKind::File,
                    rbd: None,
                    format: ImageFormat::Qcow2,
                    bus: DiskBus::VirtioBlk,
                    boot_order: Some(1),
                },
                RuntimeDisk {
                    path: "/var/lib/opl/pools/local/web-01-data.raw".into(),
                    kind: DiskKind::File,
                    rbd: None,
                    format: ImageFormat::Raw,
                    bus: DiskBus::Sata,
                    boot_order: None,
                },
            ],
            nics: vec![
                RuntimeNic {
                    mac: "52:54:00:12:34:56".parse().unwrap(),
                    model: NicModel::Virtio,
                    backend: NicBackend::Tap {
                        ifname: "tap0i0".into(),
                        bridge: "vmbr0".into(),
                        vlan: Some(10),
                        mtu: None,
                        ovn_port: None,
                        qos: None,
                    },
                },
                RuntimeNic {
                    mac: "52:54:00:12:34:57".parse().unwrap(),
                    model: NicModel::E1000e,
                    backend: NicBackend::User,
                },
            ],
            guest_agent: false,
            hotplug: None,
            cdroms: vec![],
            cloud_init: None,
            tpm: None,
            secure_boot: false,
            freeze_fs: false,
            firmware_epoch: 0,
            keep_tpm: false,
            cpu_resolved: None,
            numa: None,
            host_placement: None,
            pinning: None,
        }
    }

    fn mac_spec() -> VmRuntimeSpec {
        VmRuntimeSpec {
            machine: MachineType::Virt,
            disks: vec![RuntimeDisk {
                path: "/tmp/opl/root.qcow2".into(),
                kind: DiskKind::File,
                rbd: None,
                format: ImageFormat::Qcow2,
                bus: DiskBus::VirtioBlk,
                boot_order: Some(1),
            }],
            nics: vec![RuntimeNic {
                mac: "52:54:00:12:34:56".parse().unwrap(),
                model: NicModel::Virtio,
                backend: NicBackend::User,
            }],
            ..x86_spec()
        }
    }

    #[test]
    fn x86_kvm_uefi_full() {
        let args = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    #[test]
    fn aarch64_hvf_minimal() {
        let args = build_cmdline(&mac_spec(), &mac_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    /// 받는 쪽은 원본과 같은 장치(블록 노드 이름·장치 id)에 `-incoming defer`만 더한다.
    #[test]
    fn incoming_adds_defer_to_the_same_devices() {
        let normal = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        let incoming = build_incoming_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        assert_eq!(incoming[..normal.len()], normal[..]);
        assert_eq!(incoming[normal.len()..], ["-incoming", "defer"]);
        assert!(incoming.contains(&"-S".to_string()));
        assert!(!normal.contains(&"-incoming".to_string()));
        // 맞지 않는 조합은 똑같이 거절한다.
        assert_eq!(
            build_incoming_cmdline(&x86_spec(), &mac_host(), &paths()).unwrap_err(),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
    }

    /// VXLAN 네트워크의 MTU는 virtio NIC에만 `host_mtu`로 알린다.
    #[test]
    fn tap_mtu_sets_virtio_host_mtu() {
        let mut spec = x86_spec();
        let tap = NicBackend::Tap {
            ifname: "tap0i0".into(),
            bridge: "oplv1000".into(),
            vlan: None,
            mtu: Some(1450),
            ovn_port: None,
            qos: None,
        };
        spec.nics[0].backend = tap.clone();
        spec.nics[1].backend = tap;
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"virtio-net-pci,netdev=net0,mac=52:54:00:12:34:56,id=nic0,host_mtu=1450".to_string()
        ));
        assert!(args.contains(&"e1000e,netdev=net1,mac=52:54:00:12:34:57,id=nic1".to_string()));
    }

    /// SDN(OVN) NIC의 명령줄은 같은 MTU의 tap NIC와 바이트까지 같다(계획 65 §10): 논리 포트는 드라이버가 tap을 OVS에
    /// 붙일 때만 쓴다. 그래서 SDN NIC가 없는 VM의 명령줄도 그대로다.
    #[test]
    fn sdn_tap_has_the_command_line_of_a_tap() {
        let tap = |ovn_port: Option<&str>| NicBackend::Tap {
            ifname: "tap0i0".into(),
            bridge: "br-int".into(),
            vlan: None,
            mtu: Some(1442),
            ovn_port: ovn_port.map(Into::into),
            qos: None,
        };
        let mut plain = x86_spec();
        plain.nics[0].backend = tap(None);
        let mut sdn = plain.clone();
        sdn.nics[0].backend = tap(Some("opl-x-525400123456"));
        for build in [build_cmdline, build_incoming_cmdline] {
            let args = build(&sdn, &x86_host(), &paths()).unwrap();
            assert_eq!(args, build(&plain, &x86_host(), &paths()).unwrap());
            assert!(
                args.contains(&"tap,id=net0,ifname=tap0i0,script=no,downscript=no".to_string())
            );
            assert!(!args.iter().any(|a| a.contains("opl-x")), "{args:?}");
        }
    }

    /// 게스트 감시를 켠 VM만 QGA 채널(소켓 서버, virtio-serial 포트)을 단다. 받는 쪽도 같다. aarch64(virt)도
    /// PCI 장치를 쓴다.
    #[test]
    fn guest_agent_adds_a_qga_channel() {
        let qga = [
            "-chardev",
            "socket,id=qga0,path=/run/opl/vm/00000000-0000-0000-0000-000000000000/qga.sock,server=on,wait=off",
            "-device",
            "virtio-serial-pci,id=qgaser",
            "-device",
            "virtserialport,bus=qgaser.0,chardev=qga0,name=org.qemu.guest_agent.0",
        ];
        for (spec, host) in [(x86_spec(), x86_host()), (mac_spec(), mac_host())] {
            let without = build_cmdline(&spec, &host, &paths()).unwrap();
            assert!(!without.iter().any(|a| a.contains("qga")), "{without:?}");
            let watched = VmRuntimeSpec {
                guest_agent: true,
                ..spec
            };
            let with = build_cmdline(&watched, &host, &paths()).unwrap();
            let at = with.iter().position(|a| a == "-chardev").unwrap();
            assert_eq!(with[at..at + qga.len()], qga);
            // 앞 장치(디스크·NIC)의 순서는 그대로이고 채널만 끼어든다.
            assert_eq!(with[..at], without[..at]);
            assert_eq!(with[at + qga.len()..], without[at..]);
            let incoming = build_incoming_cmdline(&watched, &host, &paths()).unwrap();
            assert_eq!(incoming[..with.len()], with[..]);
        }
    }

    #[test]
    fn bios_omits_pflash() {
        let spec = VmRuntimeSpec {
            firmware: Firmware::Bios,
            ..x86_spec()
        };
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(!args.iter().any(|a| a.contains("pflash")));
    }

    #[test]
    fn host_cpu_falls_back_to_max_under_tcg() {
        let host = HostProfile {
            accel: Accel::Tcg,
            ..x86_host()
        };
        let args = build_cmdline(&x86_spec(), &host, &paths()).unwrap();
        let i = args.iter().position(|a| a == "-cpu").unwrap();
        assert_eq!(args[i + 1], "max");
        assert!(args.contains(&"q35,accel=tcg".to_string()));
    }

    fn cpu_arg(args: &[String]) -> &str {
        let i = args.iter().position(|a| a == "-cpu").unwrap();
        &args[i + 1]
    }

    fn resolved(model: &str, flags: &[(&str, bool)], enforce: bool) -> Option<ResolvedCpu> {
        Some(ResolvedCpu {
            model: model.into(),
            flags: flags.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            enforce,
            source: CpuSource::Spec,
            dropped_mask: Vec::new(),
        })
    }

    /// 켤 때 해석한 CPU(계획 63)가 `-cpu`가 된다: 명명 모델·수준에는 `enforce`, 플래그는 이름순 `=on|off`.
    #[test]
    fn the_resolved_cpu_is_the_cpu_option() {
        let mut spec = x86_spec();
        spec.cpu_resolved = resolved("Icelake-Server-v7", &[("avx512vbmi", false)], true);
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(cpu_arg(&args), "Icelake-Server-v7,enforce,avx512vbmi=off");
        // `host`에 숨김(KVM)
        spec.cpu_resolved = resolved("host", &[("avx512vbmi", false)], false);
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(cpu_arg(&args), "host,avx512vbmi=off");
        // TCG의 `host`는 `max`로 해석해 둔다(명령줄은 기록을 그대로 쓴다).
        spec.cpu_resolved = resolved("max", &[("avx512vbmi", false)], false);
        let tcg = HostProfile {
            accel: Accel::Tcg,
            ..x86_host()
        };
        let args = build_cmdline(&spec, &tcg, &paths()).unwrap();
        assert_eq!(cpu_arg(&args), "max,avx512vbmi=off");
        // 수준
        let level: CpuModel = "x86-64-v2-aes".parse().unwrap();
        let r = cpu::resolve(
            &CpuSpec {
                model: level,
                ..spec.cpu.clone()
            },
            Arch::X86_64,
            cpu::Accel::Kvm,
            None,
            &[],
            None,
        )
        .unwrap();
        spec.cpu_resolved = Some(r);
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(
            cpu_arg(&args),
            "qemu64-v1,enforce,aes=on,pni=on,popcnt=on,sse4.1=on,sse4.2=on,ssse3=on"
        );
        // aarch64
        let mut mac = mac_spec();
        mac.cpu_resolved = resolved("max", &[("sve", false)], false);
        let args = build_cmdline(&mac, &mac_host(), &paths()).unwrap();
        assert_eq!(cpu_arg(&args), "max,sve=off");
    }

    /// 해석이 없으면(계획 63 전 기록) 모델만 이 노드의 가속기로 편다: `host`·`max`는 지금과 같고 명명 모델은 `enforce`다.
    #[test]
    fn without_a_resolution_the_model_is_expanded_here() {
        let mut spec = x86_spec();
        assert_eq!(
            cpu_arg(&build_cmdline(&spec, &x86_host(), &paths()).unwrap()),
            "host"
        );
        spec.cpu.model = CpuModel::Max;
        assert_eq!(
            cpu_arg(&build_cmdline(&spec, &x86_host(), &paths()).unwrap()),
            "max"
        );
        spec.cpu.model = "Skylake-Server-v5".parse().unwrap();
        spec.cpu.flags = vec!["-hle".parse().unwrap()];
        assert_eq!(
            cpu_arg(&build_cmdline(&spec, &x86_host(), &paths()).unwrap()),
            "Skylake-Server-v5,enforce,hle=off"
        );
        // 수준은 aarch64에서 켤 수 없다.
        let mut mac = mac_spec();
        mac.cpu.model = "x86-64-v3".parse().unwrap();
        assert!(matches!(
            build_cmdline(&mac, &mac_host(), &paths()).unwrap_err(),
            CmdlineError::Cpu(_)
        ));
    }

    /// 실행 중 붙인 vCPU(받는 쪽 명령줄)는 해석한 모델의 CPU 형식이다.
    #[test]
    fn hotplugged_vcpus_use_the_resolved_model() {
        let mut spec = hotplug_spec();
        spec.cpu_resolved = resolved("Icelake-Server-v7", &[("avx512vbmi", false)], true);
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"Icelake-Server-v7-x86_64-cpu,id=cpu2,socket-id=0,core-id=2,thread-id=0".to_string()
        ));
        assert!(!args.iter().any(|a| a.starts_with("host-x86_64-cpu")));
    }

    #[test]
    fn escapes_commas_in_names_and_paths() {
        let mut spec = x86_spec();
        spec.name = "a,b".into();
        spec.disks[0].path = "/pool/x,y.qcow2".into();
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"guest=a,,b,debug-threads=on".to_string()));
        assert!(
            args.contains(
                &"driver=file,filename=/pool/x,,y.qcow2,node-name=disk0-file".to_string()
            )
        );
    }

    /// 블록 장치 디스크는 `host_device`로 연다. 포맷 노드는 파일 디스크와 같다.
    #[test]
    fn block_disks_use_host_device() {
        let mut spec = x86_spec();
        spec.disks[1].path = "/dev/vg0/opl-a".into();
        spec.disks[1].kind = DiskKind::Block;
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"driver=host_device,filename=/dev/vg0/opl-a,node-name=disk1-file".to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk1-file,node-name=disk1".to_string()));
        assert!(args.contains(
            &"driver=file,filename=/var/lib/opl/pools/local/web-01-root.qcow2,node-name=disk0-file"
                .to_string()
        ));
    }

    /// RBD 디스크는 QEMU의 `rbd` 드라이버로 연다. 네임스페이스와 conf는 있을 때만 넘긴다.
    #[test]
    fn rbd_disks_use_rbd_driver() {
        let mut spec = x86_spec();
        spec.disks[0] = RuntimeDisk {
            path: PathBuf::new(),
            kind: DiskKind::Rbd,
            rbd: Some(RbdTarget {
                pool: "rbd".into(),
                namespace: Some("opl".into()),
                image: "opl-a".into(),
                user: "opl".into(),
                conf: Some("/etc/ceph/opl.conf".into()),
            }),
            format: ImageFormat::Raw,
            ..spec.disks[0].clone()
        };
        spec.disks[1].path = PathBuf::new();
        spec.disks[1].kind = DiskKind::Rbd;
        spec.disks[1].rbd = Some(RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-b".into(),
            user: "admin".into(),
            conf: None,
        });
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(
            &"driver=rbd,pool=rbd,namespace=opl,image=opl-a,user=opl,conf=/etc/ceph/opl.conf,node-name=disk0-file"
                .to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk0-file,node-name=disk0".to_string()));
        assert!(args.contains(
            &"driver=rbd,pool=rbd,image=opl-b,user=admin,node-name=disk1-file".to_string()
        ));
        assert!(args.contains(&"driver=raw,file=disk1-file,node-name=disk1".to_string()));
        // 이미지가 없는 RBD 디스크는 열 수 없다.
        spec.disks[1].rbd = None;
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::MissingRbdImage(1)
        );
    }

    /// 핫플러그 배치: 켤 때 vCPU 2·상한 8·지금 4, virtio-mem 4096+12288(꽂은 2048), 포트 8, 실행 중 붙인 디스크(hp0)와
    /// NIC(hp1) 하나씩.
    fn hotplug_spec() -> VmRuntimeSpec {
        let mut spec = x86_spec();
        spec.cpu.cores = 2;
        spec.cpu.max_vcpus = Some(8);
        spec.memory_mib = 4096;
        spec.disks.push(RuntimeDisk {
            path: "/var/lib/opl/pools/local/web-01-hot.qcow2".into(),
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: None,
        });
        spec.nics.push(RuntimeNic {
            mac: "52:54:00:12:34:58".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::User,
        });
        spec.hotplug = Some(HotplugLayout {
            vcpus: 4,
            max_memory_mib: 16384,
            plugged_mib: 2048,
            ports: HOTPLUG_PORTS,
            boot_disks: 2,
            boot_nics: 2,
            disk_ports: vec![0],
            nic_ports: vec![1],
        });
        spec
    }

    #[test]
    fn x86_kvm_hotplug_layout() {
        let args = build_cmdline(&hotplug_spec(), &x86_host(), &paths()).unwrap();
        insta::assert_snapshot!(args.join("\n"));
    }

    /// 배치가 없으면 상한이 있어도 옛 명령줄과 바이트까지 같다.
    #[test]
    fn without_a_layout_the_maximums_change_nothing() {
        let mut spec = x86_spec();
        let before = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        spec.cpu.max_vcpus = Some(16);
        assert_eq!(build_cmdline(&spec, &x86_host(), &paths()).unwrap(), before);
    }

    /// 상한이 없는 배치는 `-smp`·`-m`이 그대로이고 빈 루트 포트만 더한다.
    #[test]
    fn a_layout_without_maximums_adds_only_root_ports() {
        let mut spec = x86_spec();
        let before = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        spec.hotplug = Some(HotplugLayout {
            vcpus: 4,
            max_memory_mib: 8192,
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: 2,
            boot_nics: 2,
            disk_ports: vec![],
            nic_ports: vec![],
        });
        let after = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        let ports: Vec<String> = (0..8)
            .flat_map(|k| {
                let mut port = format!(
                    "pcie-root-port,id=hp{k},bus=pcie.0,chassis={},addr=0x1e.{k},io-reserve=0",
                    k + 1
                );
                if k == 0 {
                    port.push_str(",multifunction=on");
                }
                ["-device".to_string(), port]
            })
            .collect();
        let at = after
            .iter()
            .position(|a| a.starts_with("pcie-root-port"))
            .unwrap()
            - 1;
        assert_eq!(after[at..at + ports.len()], ports[..]);
        assert_eq!(after[..at], before[..at]);
        assert_eq!(after[at + ports.len()..], before[at..]);
        assert!(after.contains(&"sockets=1,cores=4,threads=1".to_string()));
        assert!(after.contains(&"8192M".to_string()));
    }

    /// 실행 중 붙인 디스크·NIC는 루트 포트에 놓여 `pcie.0` 슬롯을 먹지 않는다: 그것을 빼면 나머지 인자가 배치만 있는
    /// 명령줄과 같다. 받는 쪽도 같은 배치다.
    #[test]
    fn hot_added_devices_go_on_root_ports() {
        let spec = hotplug_spec();
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"virtio-blk-pci,drive=disk2,id=disk2-dev,bus=hp0".to_string()));
        assert!(args.contains(
            &"virtio-net-pci,netdev=net2,mac=52:54:00:12:34:58,id=nic2,bus=hp1".to_string()
        ));
        // 켤 때 단 장치에는 버스가 없다.
        assert!(args.contains(&"virtio-blk-pci,drive=disk0,id=disk0-dev,bootindex=1".to_string()));
        let mut fewer = spec.clone();
        fewer.disks.pop();
        fewer.nics.pop();
        let layout = fewer.hotplug.as_mut().unwrap();
        layout.disk_ports.clear();
        layout.nic_ports.clear();
        let without = build_cmdline(&fewer, &x86_host(), &paths()).unwrap();
        let hot = |a: &String| a.contains("disk2") || a.contains("net2") || a.contains("nic2");
        let kept: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, a)| !hot(a) && !args.get(i + 1).is_some_and(hot))
            .map(|(_, a)| a)
            .collect();
        assert_eq!(kept, without.iter().collect::<Vec<_>>());
        let incoming = build_incoming_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(incoming[..args.len()], args[..]);
    }

    /// aarch64(`virt`)는 CPU 핫플러그가 없으므로 vCPU 상한이나 붙인 vCPU가 있는 배치를 거절한다. 디스크·NIC 포트는 된다.
    #[test]
    fn aarch64_rejects_cpu_hotplug() {
        let layout = HotplugLayout {
            vcpus: 4,
            max_memory_mib: 8192,
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: 1,
            boot_nics: 1,
            disk_ports: vec![],
            nic_ports: vec![],
        };
        let spec = VmRuntimeSpec {
            hotplug: Some(layout.clone()),
            ..mac_spec()
        };
        let args = build_cmdline(&spec, &mac_host(), &paths()).unwrap();
        assert!(args.contains(&"sockets=1,cores=4,threads=1".to_string()));
        assert!(args.iter().any(|a| a.starts_with("pcie-root-port,id=hp7")));
        let mut max = spec.clone();
        max.cpu.max_vcpus = Some(8);
        assert_eq!(
            build_cmdline(&max, &mac_host(), &paths()).unwrap_err(),
            CmdlineError::CpuHotplugUnsupported
        );
        let mut added = spec;
        added.hotplug = Some(HotplugLayout { vcpus: 5, ..layout });
        assert_eq!(
            build_incoming_cmdline(&added, &mac_host(), &paths()).unwrap_err(),
            CmdlineError::CpuHotplugUnsupported
        );
    }

    #[test]
    fn rejects_bad_layouts() {
        let err = |spec: &VmRuntimeSpec| build_cmdline(spec, &x86_host(), &paths()).unwrap_err();
        let mut many = hotplug_spec();
        many.hotplug.as_mut().unwrap().ports = HOTPLUG_PORTS + 1;
        assert_eq!(err(&many), CmdlineError::TooManyPorts);
        let mut no_disk_port = hotplug_spec();
        no_disk_port.hotplug.as_mut().unwrap().disk_ports.clear();
        assert_eq!(err(&no_disk_port), CmdlineError::MissingPort("disk", 2));
        let mut no_nic_port = hotplug_spec();
        no_nic_port.hotplug.as_mut().unwrap().nic_ports.clear();
        assert_eq!(err(&no_nic_port), CmdlineError::MissingPort("nic", 2));
    }

    #[test]
    fn rejects_incompatible_combinations() {
        let err = |spec: &VmRuntimeSpec, host: &HostProfile| {
            build_cmdline(spec, host, &paths()).unwrap_err()
        };
        assert_eq!(
            err(&x86_spec(), &mac_host()),
            CmdlineError::MachineArchMismatch {
                machine: MachineType::Q35,
                arch: Arch::Aarch64
            }
        );
        assert_eq!(
            err(
                &VmRuntimeSpec {
                    firmware: Firmware::Bios,
                    ..mac_spec()
                },
                &mac_host()
            ),
            CmdlineError::BiosOnAarch64
        );
        let mut sata_on_virt = mac_spec();
        sata_on_virt.disks[0].bus = DiskBus::Sata;
        assert_eq!(
            err(&sata_on_virt, &mac_host()),
            CmdlineError::SataRequiresQ35
        );
        let mut many = x86_spec();
        many.disks = (0..7)
            .map(|i| RuntimeDisk {
                path: format!("/d{i}.raw").into(),
                kind: DiskKind::File,
                rbd: None,
                format: ImageFormat::Raw,
                bus: DiskBus::Sata,
                boot_order: None,
            })
            .collect();
        assert_eq!(err(&many, &x86_host()), CmdlineError::TooManySataDevices);
    }

    fn seed() -> opl_types::cloud_init::CloudInitSeed {
        opl_types::cloud_init::CloudInitSeed {
            instance_id: "iid-0123456789abcdef".into(),
            meta_data: "{}".into(),
            user_data: "#cloud-config\n{}".into(),
            vendor_data: None,
            network_config: None,
        }
    }

    fn cdrom(path: Option<&str>, boot_order: Option<u8>) -> RuntimeCdrom {
        RuntimeCdrom {
            medium: path.map(|p| RuntimeMedium { path: p.into() }),
            boot_order,
        }
    }

    /// `without`에 없던 인자 `added`가 `-vnc` 바로 앞에 끼어들고 나머지는 그대로인지
    fn assert_inserted_before_vnc(with: &[String], without: &[String], added: &[&str]) {
        let at = without.iter().position(|a| a == "-vnc").unwrap();
        assert_eq!(with[..at], without[..at]);
        assert_eq!(with[at..at + added.len()], *added, "{with:#?}");
        assert_eq!(with[at + added.len()..], without[at..]);
    }

    /// q35: SATA 디스크 하나(포트 0) 다음 AHCI 포트에 CD 둘(하나는 빈 드라이브)과 씨앗. bootindex는 기본 규칙
    /// (디스크 1 다음 CD 2·3)이고 씨앗에는 없다. 앞 장치의 인자는 CD 없는 명령줄과 같다.
    #[test]
    fn q35_cdroms_and_seed_follow_the_sata_disks() {
        let without = build_cmdline(&x86_spec(), &x86_host(), &paths()).unwrap();
        let (disks, cds) = opl_types::vm::effective_boot(&[Some(1), None], &[None, None]);
        assert_eq!(disks, [Some(1), None]);
        let mut spec = x86_spec();
        spec.cdroms = vec![
            cdrom(Some("/pools/iso/ubuntu.raw"), cds[0]),
            cdrom(None, cds[1]),
        ];
        spec.cloud_init = Some(seed());
        let with = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        let dir = "/run/opl/vm/00000000-0000-0000-0000-000000000000";
        assert_inserted_before_vnc(
            &with,
            &without,
            &[
                "-blockdev",
                "driver=file,filename=/pools/iso/ubuntu.raw,node-name=cd0-file,read-only=on",
                "-blockdev",
                "driver=raw,file=cd0-file,node-name=cd0,read-only=on",
                "-device",
                "ide-cd,bus=ide.1,drive=cd0,id=cd0-dev,bootindex=2",
                "-device",
                "ide-cd,bus=ide.2,id=cd1-dev,bootindex=3",
                "-blockdev",
                &format!(
                    "driver=file,filename={dir}/cidata.iso,node-name=cidata-file,read-only=on"
                ),
                "-blockdev",
                "driver=raw,file=cidata-file,node-name=cidata,read-only=on",
                "-device",
                "ide-cd,bus=ide.3,drive=cidata,id=cidata-dev",
            ],
        );
        // 받는 쪽도 같은 장치다.
        let incoming = build_incoming_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(incoming[..with.len()], with[..]);
    }

    /// virt: virtio-scsi 컨트롤러와 CD·씨앗을 게스트 에이전트·virtio-mem 뒤에 단다. NIC·디스크의 인자(PCI 주소)는 CD
    /// 없는 명령줄과 같다.
    #[test]
    fn virt_cdroms_go_on_virtio_scsi_after_everything() {
        let mut base = mac_spec();
        base.guest_agent = true;
        base.hotplug = Some(HotplugLayout {
            vcpus: 4,
            max_memory_mib: 16384,
            plugged_mib: 0,
            ports: HOTPLUG_PORTS,
            boot_disks: 1,
            boot_nics: 1,
            disk_ports: vec![],
            nic_ports: vec![],
        });
        let without = build_cmdline(&base, &mac_host(), &paths()).unwrap();
        assert!(without.iter().any(|a| a.starts_with("virtio-mem-pci")));
        let mut spec = base;
        spec.cdroms = vec![cdrom(Some("/pools/iso/a,b.raw"), Some(2))];
        spec.cloud_init = Some(seed());
        let with = build_cmdline(&spec, &mac_host(), &paths()).unwrap();
        let dir = "/run/opl/vm/00000000-0000-0000-0000-000000000000";
        assert_inserted_before_vnc(
            &with,
            &without,
            &[
                "-device",
                "virtio-scsi-pci,id=scsi0",
                "-blockdev",
                "driver=file,filename=/pools/iso/a,,b.raw,node-name=cd0-file,read-only=on",
                "-blockdev",
                "driver=raw,file=cd0-file,node-name=cd0,read-only=on",
                "-device",
                "scsi-cd,bus=scsi0.0,drive=cd0,id=cd0-dev,bootindex=2",
                "-blockdev",
                &format!(
                    "driver=file,filename={dir}/cidata.iso,node-name=cidata-file,read-only=on"
                ),
                "-blockdev",
                "driver=raw,file=cidata-file,node-name=cidata,read-only=on",
                "-device",
                "scsi-cd,bus=scsi0.0,drive=cidata,id=cidata-dev",
            ],
        );
    }

    /// 씨앗만 있어도 드라이브를 단다. 실행 디렉터리 경로의 쉼표도 이스케이프한다.
    #[test]
    fn a_seed_without_cdroms_gets_its_own_drive() {
        let paths = RuntimePaths::new(Path::new("/run/a,b"), Uuid::nil()).unwrap();
        let spec = VmRuntimeSpec {
            cloud_init: Some(seed()),
            ..mac_spec()
        };
        let args = build_cmdline(&spec, &mac_host(), &paths).unwrap();
        assert!(args.contains(&"virtio-scsi-pci,id=scsi0".to_string()));
        assert!(args.contains(
            &"driver=file,filename=/run/a,,b/vm/00000000-0000-0000-0000-000000000000/cidata.iso,node-name=cidata-file,read-only=on"
                .to_string()
        ));
        assert!(args.contains(&"scsi-cd,bus=scsi0.0,drive=cidata,id=cidata-dev".to_string()));
        assert!(!args.iter().any(|a| a.contains("cd0")));
    }

    /// q35의 AHCI 포트 6개를 SATA 디스크·CD·씨앗이 나눠 쓴다. virt의 CD는 SATA 포트를 쓰지 않는다. CD는 4개까지.
    #[test]
    fn sata_ports_and_cdrom_limits() {
        let err = |spec: &VmRuntimeSpec, host: &HostProfile| {
            build_cmdline(spec, host, &paths()).unwrap_err()
        };
        let mut q35 = x86_spec();
        q35.disks[0].bus = DiskBus::Sata;
        q35.cdroms = vec![cdrom(None, None); 3];
        q35.cloud_init = Some(seed());
        // SATA 디스크 2 + CD 3 + 씨앗 1 = 6
        let args = build_cmdline(&q35, &x86_host(), &paths()).unwrap();
        assert!(args.contains(&"ide-cd,bus=ide.5,drive=cidata,id=cidata-dev".to_string()));
        q35.cdroms.push(cdrom(None, None));
        assert_eq!(err(&q35, &x86_host()), CmdlineError::TooManySataDevices);
        // 씨앗을 빼거나 SATA 디스크를 virtio로 바꾸면 6이다.
        q35.cloud_init = None;
        build_cmdline(&q35, &x86_host(), &paths()).unwrap();
        q35.cloud_init = Some(seed());
        q35.disks[0].bus = DiskBus::VirtioBlk;
        build_cmdline(&q35, &x86_host(), &paths()).unwrap();

        let mut virt = mac_spec();
        virt.cdroms = vec![cdrom(None, None); MAX_CDROMS];
        virt.cloud_init = Some(seed());
        build_cmdline(&virt, &mac_host(), &paths()).unwrap();
        virt.cdroms.push(cdrom(None, None));
        assert_eq!(err(&virt, &mac_host()), CmdlineError::TooManyCdroms);
    }

    fn sb_host(mut host: HostProfile, code: &str, vars: &str) -> HostProfile {
        host.secure_boot = Some(crate::firmware::FirmwarePair {
            code: code.into(),
            vars_template: vars.into(),
        });
        host
    }

    /// TPM은 CD·씨앗 뒤, `-vnc` 바로 앞에 붙고 앞 장치의 인자는 그대로다. q35는 `tpm-crb`, virt는 `tpm-tis-device`다.
    /// 받는 쪽도 같다.
    #[test]
    fn tpm_goes_after_everything_else() {
        let tpm = Some(opl_types::runtime::RuntimeTpm {
            fresh: false,
            epoch: 0,
        });
        let sock =
            "socket,id=chrtpm,path=/run/opl/vm/00000000-0000-0000-0000-000000000000/tpm.sock";
        for (spec, host, dev) in [
            (x86_spec(), x86_host(), "tpm-crb,tpmdev=tpm0"),
            (mac_spec(), mac_host(), "tpm-tis-device,tpmdev=tpm0"),
        ] {
            let mut spec = spec;
            spec.cdroms = vec![cdrom(Some("/pools/iso/x.raw"), None)];
            let without = build_cmdline(&spec, &host, &paths()).unwrap();
            let with_tpm = VmRuntimeSpec { tpm, ..spec };
            let with = build_cmdline(&with_tpm, &host, &paths()).unwrap();
            assert_inserted_before_vnc(
                &with,
                &without,
                &[
                    "-chardev",
                    sock,
                    "-tpmdev",
                    "emulator,id=tpm0,chardev=chrtpm",
                    "-device",
                    dev,
                ],
            );
            let incoming = build_incoming_cmdline(&with_tpm, &host, &paths()).unwrap();
            assert_eq!(incoming[..with.len()], with[..]);
        }
    }

    /// Secure Boot(q35): SMM, secure pflash, MS 키 펌웨어의 코드. 변수 파일 경로는 그대로다. virt는 코드만 바뀐다.
    #[test]
    fn secure_boot_uses_smm_and_the_enrolled_firmware() {
        let host = sb_host(x86_host(), "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd", "/v");
        let plain = build_cmdline(&x86_spec(), &host, &paths()).unwrap();
        let spec = VmRuntimeSpec {
            secure_boot: true,
            ..x86_spec()
        };
        let args = build_cmdline(&spec, &host, &paths()).unwrap();
        assert!(args.contains(&"q35,smm=on,accel=kvm".to_string()));
        let g = args.iter().position(|a| a == "-global").unwrap();
        assert_eq!(args[g + 1], "driver=cfi.pflash01,property=secure,value=on");
        assert_eq!(args[g + 2], "-drive");
        assert_eq!(
            args[g + 3],
            "if=pflash,unit=0,format=raw,readonly=on,file=/usr/share/OVMF/OVMF_CODE_4M.secboot.fd"
        );
        assert!(args.contains(
            &"if=pflash,unit=1,format=raw,file=/run/opl/vm/00000000-0000-0000-0000-000000000000/uefi-vars.fd"
                .to_string()
        ));
        // 끄면 Secure Boot 펌웨어가 있어도 계획 58과 같다.
        assert!(plain.contains(&"q35,accel=kvm".to_string()));
        assert!(
            !plain
                .iter()
                .any(|a| a.contains("secboot") || a == "-global")
        );

        let host = sb_host(mac_host(), "/sb/code.fd", "/sb/vars.fd");
        let spec = VmRuntimeSpec {
            secure_boot: true,
            ..mac_spec()
        };
        let args = build_cmdline(&spec, &host, &paths()).unwrap();
        assert!(args.contains(&"virt,accel=hvf".to_string()));
        assert!(!args.contains(&"-global".to_string()));
        assert!(
            args.contains(&"if=pflash,unit=0,format=raw,readonly=on,file=/sb/code.fd".to_string())
        );
    }

    #[test]
    fn secure_boot_needs_uefi_and_the_firmware() {
        let spec = VmRuntimeSpec {
            secure_boot: true,
            ..x86_spec()
        };
        assert_eq!(
            build_cmdline(&spec, &x86_host(), &paths()).unwrap_err(),
            CmdlineError::NoSecureBootFirmware
        );
        let bios = VmRuntimeSpec {
            firmware: Firmware::Bios,
            ..spec
        };
        let host = sb_host(x86_host(), "/c", "/v");
        assert_eq!(
            build_cmdline(&bios, &host, &paths()).unwrap_err(),
            CmdlineError::SecureBootNeedsUefi
        );
    }
    // -------------------------------------------------------------------------------------------------------------
    // vNUMA·메모리 백엔드·hugepages(계획 64)
    // -------------------------------------------------------------------------------------------------------------

    fn guest(sockets: &[u16], memory_mib: u32) -> opl_types::numa::GuestNode {
        opl_types::numa::GuestNode {
            sockets: sockets.to_vec(),
            memory_mib,
        }
    }

    fn shared_numa(nodes: Vec<opl_types::numa::GuestNode>) -> ResolvedNuma {
        ResolvedNuma {
            nodes,
            distances: Vec::new(),
            page: None,
            policy: Default::default(),
            thread_policy: None,
            emulator: None,
            memory: None,
        }
    }

    /// `-m` 다음부터 장치 앞까지의 인자
    fn after_memory(args: &[String]) -> Vec<String> {
        let at = args.iter().position(|a| a == "-m").unwrap() + 2;
        let end = args[at..]
            .iter()
            .position(|a| a == "-device" || a == "-drive")
            .unwrap()
            + at;
        args[at..end].to_vec()
    }

    /// 기능을 쓰지 않는 VM(`host`·`max`, NUMA 없음)은 배치·고정이 있어도 명령줄이 바이트까지 같다.
    #[test]
    fn vms_without_numa_keep_the_old_command_line() {
        for model in [CpuModel::Host, CpuModel::Max] {
            let mut spec = x86_spec();
            spec.cpu.model = model;
            let before = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
            spec.pinning = Some(opl_types::runtime::RuntimePinning {
                emulator: "1".parse().unwrap(),
                vcpus: Vec::new(),
                slots_per_socket: 4,
                scope: None,
            });
            assert_eq!(build_cmdline(&spec, &x86_host(), &paths()).unwrap(), before);
            assert!(
                !before
                    .iter()
                    .any(|a| a == "-numa" || a.contains("memory-backend"))
            );
        }
    }

    #[test]
    fn shared_vnuma_has_a_ram_backend_and_the_sockets_of_each_node() {
        let mut spec = x86_spec();
        spec.cpu.sockets = 2;
        spec.cpu.cores = 2;
        let before = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        spec.numa = Some(shared_numa(vec![guest(&[0], 4096), guest(&[1], 4096)]));
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        assert_eq!(
            after_memory(&args),
            [
                "-object",
                "memory-backend-ram,id=ram-node0,size=4096M",
                "-numa",
                "node,nodeid=0,memdev=ram-node0",
                "-object",
                "memory-backend-ram,id=ram-node1,size=4096M",
                "-numa",
                "node,nodeid=1,memdev=ram-node1",
                "-numa",
                "cpu,node-id=0,socket-id=0",
                "-numa",
                "cpu,node-id=1,socket-id=1",
            ]
        );
        // 그 밖은 그대로다(장치 순서·PCI 주소, `-smp`·`-cpu`).
        let rest: Vec<&String> = args
            .iter()
            .filter(|a| !after_memory(&args).contains(a))
            .collect();
        let old: Vec<&String> = before.iter().collect();
        assert_eq!(rest, old);
    }

    #[test]
    fn dedicated_1g_vms_bind_preallocated_hugetlb_memory_with_distances() {
        let mut spec = x86_spec();
        spec.cpu.sockets = 2;
        spec.cpu.cores = 2;
        let mut numa = shared_numa(vec![guest(&[0], 4096), guest(&[1], 4096)]);
        numa.policy = opl_types::numa::CpuPolicy::Dedicated;
        numa.page = Some(PageKind::Size1G);
        numa.memory = Some(MemoryPolicy::Strict);
        numa.distances = vec![vec![10, 21], vec![21, 10]];
        spec.numa = Some(numa);
        spec.host_placement = Some(HostPlacement {
            node: Uuid::nil(),
            vcpus: vec![4, 5, 12, 13],
            siblings: Default::default(),
            emulator: Default::default(),
            host_nodes: vec![1, 0],
        });
        let args = after_memory(&build_cmdline(&spec, &x86_host(), &paths()).unwrap());
        assert_eq!(
            args[1],
            "memory-backend-memfd,id=ram-node0,size=4096M,hugetlb=on,hugetlbsize=1G,host-nodes=1,policy=bind,prealloc=on,prealloc-threads=4"
        );
        assert_eq!(
            args[5],
            "memory-backend-memfd,id=ram-node1,size=4096M,hugetlb=on,hugetlbsize=1G,host-nodes=0,policy=bind,prealloc=on,prealloc-threads=4"
        );
        assert_eq!(args[args.len() - 2..], ["-numa", "dist,src=0,dst=1,val=21"]);
        // 공유 VM의 `preferred`는 미리 잡지 않는다.
        let numa = spec.numa.as_mut().unwrap();
        numa.policy = Default::default();
        numa.page = None;
        numa.memory = Some(MemoryPolicy::Preferred);
        let args = after_memory(&build_cmdline(&spec, &x86_host(), &paths()).unwrap());
        assert_eq!(
            args[1],
            "memory-backend-ram,id=ram-node0,size=4096M,host-nodes=1,policy=preferred"
        );
    }

    /// 핫플러그 배치의 virtio-mem은 게스트 노드마다 장치이고(`node=`), 2M hugepages VM은 장치가 미리 잡는다.
    #[test]
    fn virtio_mem_is_one_device_per_guest_node() {
        let mut spec = hotplug_spec();
        spec.cpu.sockets = 2;
        spec.cpu.cores = 1;
        let mut numa = shared_numa(vec![guest(&[0], 2048), guest(&[1], 2048)]);
        numa.page = Some(PageKind::Size2M);
        numa.memory = Some(MemoryPolicy::Strict);
        spec.numa = Some(numa);
        spec.host_placement = Some(HostPlacement {
            node: Uuid::nil(),
            vcpus: Vec::new(),
            siblings: Default::default(),
            emulator: Default::default(),
            host_nodes: vec![0, 1],
        });
        let args = build_cmdline(&spec, &x86_host(), &paths()).unwrap();
        let has = |s: &str| args.iter().any(|a| a == s);
        // 상한 16384 − 4096 = 12288을 둘로, 꽂은 2048도 둘로. 리뷰 1: 상한의 페이지를 띄울 때 예약하지 않는다(`reserve=off`).
        assert!(has(
            "memory-backend-memfd,id=vmem0,size=6144M,hugetlb=on,hugetlbsize=2M,host-nodes=0,policy=bind,reserve=off"
        ));
        assert!(has(
            "memory-backend-memfd,id=vmem1,size=6144M,hugetlb=on,hugetlbsize=2M,host-nodes=1,policy=bind,reserve=off"
        ));
        // 켤 때 메모리는 그대로 예약하고 미리 잡는다.
        assert!(
            args.iter()
                .filter(|a| a.contains("id=ram-node"))
                .all(|a| !a.contains("reserve=off") && a.contains("prealloc=on")),
            "{args:?}"
        );
        assert!(has(
            "virtio-mem-pci,id=vmem0-dev,memdev=vmem0,node=0,block-size=2M,requested-size=1024M,prealloc=on"
        ));
        assert!(has(
            "virtio-mem-pci,id=vmem1-dev,memdev=vmem1,node=1,block-size=2M,requested-size=1024M,prealloc=on"
        ));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("id=vmem0-dev,memdev=vmem0,block-size"))
        );
        // 실행 중 붙인 vCPU는 그대로다(노드는 QEMU가 매핑에서 채운다).
        assert!(has(
            "host-x86_64-cpu,id=cpu2,socket-id=0,core-id=2,thread-id=0"
        ));
    }

    #[test]
    fn aarch64_virt_gets_the_same_numa_options() {
        let mut spec = x86_spec();
        spec.machine = MachineType::Virt;
        spec.disks.retain(|d| d.bus != DiskBus::Sata);
        spec.cpu.sockets = 2;
        spec.cpu.cores = 1;
        spec.numa = Some(shared_numa(vec![guest(&[0], 4096), guest(&[1], 4096)]));
        let args = build_cmdline(&spec, &mac_host(), &paths()).unwrap();
        assert!(args.contains(&"cpu,node-id=1,socket-id=1".to_string()));
        assert!(args.contains(&"memory-backend-ram,id=ram-node1,size=4096M".to_string()));
    }

    fn helper_args<'a>(
        arch: Arch,
        accel: Accel,
        disks: &'a [HelperDisk<'a>],
        drop: Option<HelperDrop>,
        sandbox: bool,
    ) -> RestoreHelperArgs<'a> {
        RestoreHelperArgs {
            id: Uuid::nil(),
            arch,
            accel,
            kernel: Path::new("/usr/share/orbivisor/restore/aarch64/Image"),
            initrd: Path::new("/usr/share/orbivisor/restore/aarch64/initrd.img"),
            memory_mib: 512,
            vcpus: 2,
            disks,
            ctl_socket: Path::new("/run/opl/file-restore/x/ctl.sock"),
            console_log: Path::new("/var/lib/opl/file-restore/x/console.log"),
            drop,
            sandbox,
        }
    }

    /// 계획 69 §7.4: aarch64 TCG 도우미 — 펌웨어 없는 직접 부팅, 원본(NBD·qcow2 체인)은 읽기 전용, 덮개에 장치, 네트워크·화면
    /// 없음, `pauth-impdef`, 권한 내림·sandbox는 맨 끝.
    #[test]
    fn restore_helper_cmdline_aarch64_tcg() {
        let nbd = LiveRestoreSource::Nbd {
            socket: "/run/opl/file-restore/x/nbd,1.sock".into(),
            export: "disk0".into(),
        };
        let chain = LiveRestoreSource::Qcow2 {
            chain: vec!["/bk/b2/disk1.qcow2".into(), "/bk/b1/disk1.qcow2".into()],
        };
        let disks = [
            HelperDisk {
                source: &nbd,
                overlay: Path::new("/var/lib/opl/file-restore/x/ov0.qcow2"),
            },
            HelperDisk {
                source: &chain,
                overlay: Path::new("/var/lib/opl/file-restore/x/ov1.qcow2"),
            },
        ];
        let args = build_restore_helper_cmdline(&helper_args(
            Arch::Aarch64,
            Accel::Tcg,
            &disks,
            Some(HelperDrop::RunWith("nobody".into())),
            true,
        ))
        .unwrap();
        let line = args.join(" ");
        let expected = [
            "-name opl-fr-00000000-0000-0000-0000-000000000000 -nodefaults -no-user-config -display none -net none",
            " -machine virt,accel=tcg,gic-version=max -cpu max,pauth-impdef=on -smp 2 -m 512M",
            " -kernel /usr/share/orbivisor/restore/aarch64/Image -initrd /usr/share/orbivisor/restore/aarch64/initrd.img",
            " -append console=ttyAMA0 panic=-1 quiet opl.session=00000000-0000-0000-0000-000000000000 opl.proto=2",
            " -blockdev driver=nbd,server.type=unix,server.path=/run/opl/file-restore/x/nbd,,1.sock,export=disk0,read-only=on,node-name=src0",
            " -blockdev driver=qcow2,file.driver=file,file.filename=/var/lib/opl/file-restore/x/ov0.qcow2,backing=src0,node-name=ov0",
            " -device virtio-blk-pci,drive=ov0,serial=opl-disk0",
        ]
        .concat();
        assert!(line.starts_with(&expected), "{line}");
        let src1: serde_json::Value = serde_json::from_str(&args[args.len() - 22]).unwrap();
        assert_eq!(src1["node-name"], "src1");
        assert_eq!(src1["read-only"], true);
        assert_eq!(src1["file"]["filename"], "/bk/b2/disk1.qcow2");
        assert_eq!(src1["backing"]["file"]["filename"], "/bk/b1/disk1.qcow2");
        assert_eq!(src1["backing"]["backing"], serde_json::Value::Null);
        assert!(line.ends_with(concat!(
            " -device virtio-blk-pci,drive=ov1,serial=opl-disk1 -device virtio-serial-pci",
            " -chardev socket,id=ctl,path=/run/opl/file-restore/x/ctl.sock,server=on,wait=off",
            " -device virtserialport,chardev=ctl,name=org.orbivisor.restore.0 -device virtio-rng-pci",
            " -chardev file,id=con,path=/var/lib/opl/file-restore/x/console.log,append=on -serial chardev:con",
            " -no-reboot -run-with user=nobody",
            " -sandbox on,obsolete=deny,spawn=deny,resourcecontrol=deny"
        )), "{line}");
        assert!(!line.contains("reconnect-delay"), "{line}");
        for absent in ["-netdev", "-vnc", "usb", "virtio-net", "-drive", "pflash"] {
            assert!(!line.contains(absent), "{absent}: {line}");
        }
    }

    /// x86 KVM은 `q35`·`-cpu host`·`ttyS0`, HVF는 GIC 판을 정하지 않는다, 옛 QEMU는 `-runas`, 디스크 수 상한.
    #[test]
    fn restore_helper_cmdline_variants() {
        let x86 = build_restore_helper_cmdline(&helper_args(
            Arch::X86_64,
            Accel::Kvm,
            &[],
            Some(HelperDrop::Runas("nobody".into())),
            false,
        ))
        .unwrap()
        .join(" ");
        assert!(x86.contains("-machine q35,accel=kvm -cpu host "), "{x86}");
        assert!(x86.contains("console=ttyS0 "), "{x86}");
        assert!(x86.ends_with("-no-reboot -runas nobody"), "{x86}");
        let x86_tcg =
            build_restore_helper_cmdline(&helper_args(Arch::X86_64, Accel::Tcg, &[], None, true))
                .unwrap()
                .join(" ");
        assert!(
            x86_tcg.contains("-machine q35,accel=tcg -cpu max "),
            "{x86_tcg}"
        );
        // 내리지 않으면 권한 올리기도 막는다.
        assert!(
            x86_tcg.ends_with(
                "-no-reboot -sandbox on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny"
            ),
            "{x86_tcg}"
        );
        let hvf =
            build_restore_helper_cmdline(&helper_args(Arch::Aarch64, Accel::Hvf, &[], None, false))
                .unwrap()
                .join(" ");
        assert!(hvf.contains("-machine virt,accel=hvf -cpu host "), "{hvf}");
        let src = LiveRestoreSource::Qcow2 {
            chain: vec!["/bk/d.qcow2".into()],
        };
        let disks: Vec<HelperDisk> = (0..=MAX_HELPER_DISKS)
            .map(|_| HelperDisk {
                source: &src,
                overlay: Path::new("/o.qcow2"),
            })
            .collect();
        assert_eq!(
            build_restore_helper_cmdline(&helper_args(
                Arch::X86_64,
                Accel::Kvm,
                &disks,
                None,
                false
            )),
            Err(CmdlineError::TooManyHelperDisks(MAX_HELPER_DISKS + 1))
        );
    }
}
