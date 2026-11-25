//! 도우미 VM 안(Linux, PID 1)의 에이전트(설계 §9.1). `/proc`·`/sys`·`/dev`·`/run`·`/mnt`를 붙이고, 이미지의
//! `modules.order` 차례로 커널 모듈을 올리고(`finit_module`), virtio-serial 포트를 찾아 [`serve`]한다. 끝나거나 치명
//! 오류면 전원을 끈다(QEMU `-no-reboot` — opld가 끝을 본다).
//!
//! 볼륨 찾기는 `blkid -p -o export`(저수준 탐지)·`lvm`(메타데이터를 쓰지 않는 읽기 전용 활성화), 마운트는
//! `ro,nodev,nosuid,noexec` + 종류별 옵션([`super::mount`]). 프로그램은 argv로만 부른다(셸 없음).
//!
//! udev가 없으므로 이미지가 지켜야 할 것: `/dev/console` 노드, `/bin/blkid`·`/bin/lvm`, lvm.conf(또는 아래
//! `--config`)의 `udev_sync=0`·`udev_rules=0`·`verify_udev_operations=1`(LVM이 `/dev/mapper` 노드를 스스로 만든다),
//! 포트 이름 링크 `/dev/virtio-ports/…` 대신 `/sys/class/virtio-ports/*/name`을 읽는다.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::mount::MountFlags;

use super::mount::{FsClass, RETRY_NOTE, classify};
use super::probe::{self, BlockProbe, DiskProbe, Found, LvProbe, PvReport};
use super::{DirFs, Mounted, ServeOptions, Volumes, serve};
use crate::proto::{AgentError, ErrorCode, Hello, PORT_NAME, PROTO_VERSION, Volume};

const BLKID: &str = "/bin/blkid";
const LVM: &str = "/bin/lvm";
const MOUNT_ROOT: &str = "/mnt";
/// 이미지 빌더가 쓰는 이미지 판(없으면 crate 판)
const IMAGE_VERSION_FILE: &str = "/etc/orbivisor/image-version";
/// 메타데이터를 쓰지 않고 활성화, 씬 풀 검사 건너뜀, udev 없이 장치 노드를 스스로, 백업·보관 파일 쓰지 않음
const LVM_CONFIG: &str = "global{metadata_read_only=1} \
    activation{thin_check_executable=\"\" udev_sync=0 udev_rules=0 verify_udev_operations=1 monitoring=0} \
    devices{obtain_device_list_from_udev=0} backup{backup=0 archive=0}";
/// `finit_module`: 압축된 모듈을 커널이 푼다(6.4+)
const MODULE_INIT_COMPRESSED_FILE: i32 = 4;

/// 실제 볼륨(blkid·lvm·mount)
#[derive(Default)]
pub struct SystemVolumes {
    /// 탐지 결과: 볼륨 id → (장치, blkid TYPE)
    found: Mutex<HashMap<String, (String, Option<String>)>>,
    /// 붙인 볼륨(잠금을 쥔 채 붙인다 — 같은 볼륨을 두 흐름이 함께 붙이지 않게)
    mounts: Mutex<HashMap<String, Mounted>>,
}

impl SystemVolumes {
    pub fn new() -> Self {
        Self::default()
    }
}

fn read_trim(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// sysfs `size`(512바이트 섹터) → 바이트
fn sysfs_size(dir: &Path) -> u64 {
    read_trim(dir.join("size"))
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(0, |s| s.saturating_mul(512))
}

fn blkid(dev: &str) -> std::collections::BTreeMap<String, String> {
    // 아무것도 찾지 못하면 blkid는 2로 끝난다 — 빈 보고.
    match Command::new(BLKID)
        .args(["-p", "-o", "export", dev])
        .output()
    {
        Ok(out) => probe::parse_blkid_export(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            eprintln!("orbivisor-restore-agent: {BLKID} {dev}: {e}");
            Default::default()
        }
    }
}

fn lvm(args: &[&str]) -> Option<String> {
    let out = Command::new(LVM)
        .args(args)
        .args(["--config", LVM_CONFIG])
        .output()
        .map_err(|e| eprintln!("orbivisor-restore-agent: {LVM}: {e}"))
        .ok()?;
    if !out.status.success() {
        eprintln!(
            "orbivisor-restore-agent: lvm {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `/sys/block/vd*` 가운데 serial이 `opl-disk{i}`인 디스크와 그 파티션
fn probe_disks() -> Vec<DiskProbe> {
    let mut disks = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return disks;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("vd") {
            continue;
        }
        let dir = e.path();
        let Some(index) = read_trim(dir.join("serial")).and_then(|s| probe::disk_index(&s)) else {
            continue;
        };
        let dev = format!("/dev/{name}");
        let mut partitions = Vec::new();
        if let Ok(parts) = fs::read_dir(&dir) {
            for p in parts.flatten() {
                let pdir = p.path();
                let Some(number) = read_trim(pdir.join("partition")).and_then(|n| n.parse().ok())
                else {
                    continue;
                };
                let pdev = format!("/dev/{}", p.file_name().to_string_lossy());
                partitions.push((
                    number,
                    BlockProbe {
                        size: sysfs_size(&pdir),
                        blkid: blkid(&pdev),
                        dev: pdev,
                    },
                ));
            }
        }
        disks.push(DiskProbe {
            index,
            disk: BlockProbe {
                size: sysfs_size(&dir),
                blkid: blkid(&dev),
                dev,
            },
            partitions,
        });
    }
    disks
}

/// PV가 하나라도 있으면 VG를 읽기 전용으로 활성화하고 PV·LV 보고를 읽는다.
fn probe_lvm(disks: &[DiskProbe]) -> (Vec<PvReport>, Vec<LvProbe>) {
    let has_pv = disks.iter().any(|d| {
        std::iter::once(&d.disk)
            .chain(d.partitions.iter().map(|(_, p)| p))
            .any(|b| b.blkid.get("TYPE").map(String::as_str) == Some("LVM2_member"))
    });
    if !has_pv {
        return (Vec::new(), Vec::new());
    }
    lvm(&["vgchange", "-ay"]);
    let pvs = lvm(&["pvs", "--reportformat", "json", "-o", "pv_name,vg_name"])
        .and_then(|s| probe::parse_pvs_json(&s).map_err(|e| eprintln!("{e}")).ok())
        .unwrap_or_default();
    let lvs = lvm(&[
        "lvs",
        "--reportformat",
        "json",
        "--units",
        "b",
        "--nosuffix",
        "-o",
        "vg_name,lv_name,vg_uuid,lv_uuid,lv_size,lv_attr,lv_dm_path",
    ])
    .and_then(|s| probe::parse_lvs_json(&s).map_err(|e| eprintln!("{e}")).ok())
    .unwrap_or_default();
    let lvs = lvs
        .into_iter()
        .map(|report| LvProbe {
            blkid: if report.lv_dm_path.is_empty() {
                Default::default()
            } else {
                blkid(&report.lv_dm_path)
            },
            report,
        })
        .collect();
    (pvs, lvs)
}

fn mount_once(dev: &str, dir: &Path, fstype: &str, data: &str) -> io::Result<()> {
    let data = CString::new(data).map_err(|_| io::Error::from(Errno::INVAL))?;
    rustix::mount::mount(
        dev,
        dir,
        fstype,
        MountFlags::RDONLY | MountFlags::NODEV | MountFlags::NOSUID | MountFlags::NOEXEC,
        Some(data.as_c_str()),
    )?;
    Ok(())
}

impl Volumes for SystemVolumes {
    fn probe(&self) -> Result<Vec<Volume>, AgentError> {
        let disks = probe_disks();
        let (pvs, lvs) = probe_lvm(&disks);
        let found: Vec<Found> = probe::build_volumes(&disks, &pvs, &lvs);
        let mut map = self.found.lock().unwrap_or_else(|e| e.into_inner());
        map.clear();
        for f in &found {
            map.insert(f.volume.id.clone(), (f.dev.clone(), f.volume.fs.clone()));
        }
        Ok(found.into_iter().map(|f| f.volume).collect())
    }

    fn mount(&self, volume: &str) -> Result<Mounted, AgentError> {
        let mut mounts = self.mounts.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = mounts.get(volume) {
            return Ok(m.clone());
        }
        let (dev, fs_type) = self
            .found
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(volume)
            .cloned()
            .ok_or_else(|| AgentError::new(ErrorCode::NotFound, "no such volume"))?;
        let plan = match fs_type.as_deref().map(classify) {
            Some(FsClass::Mount(plan)) => plan,
            _ => {
                return Err(AgentError::new(
                    ErrorCode::NotMountable,
                    "this volume has no filesystem that can be browsed",
                ));
            }
        };
        // 볼륨 id에는 `/`가 있어 디렉터리 이름은 차례 번호로 한다.
        let dir = PathBuf::from(MOUNT_ROOT).join(format!("v{}", mounts.len()));
        fs::create_dir_all(&dir)
            .map_err(|e| AgentError::new(ErrorCode::MountFailed, format!("mkdir: {e}")))?;
        let note = match mount_once(&dev, &dir, plan.fstype, plan.data) {
            Ok(()) => None,
            Err(first) => match plan.retry {
                Some(retry) if mount_once(&dev, &dir, plan.fstype, retry).is_ok() => {
                    Some(RETRY_NOTE.to_string())
                }
                _ => {
                    return Err(AgentError::new(
                        ErrorCode::MountFailed,
                        format!("mounting the {} filesystem failed: {first}", plan.fstype),
                    ));
                }
            },
        };
        let dirfs = DirFs::open(&dir)
            .map_err(|e| AgentError::new(ErrorCode::MountFailed, format!("open: {e}")))?;
        let m = Mounted {
            fs: Arc::new(dirfs),
            note,
        };
        mounts.insert(volume.to_string(), m.clone());
        Ok(m)
    }
}

/// `/proc`·`/sys`·`/dev`·`/run`·`/mnt`
fn mount_basics() -> io::Result<()> {
    for (src, target, fstype) in [
        ("proc", "/proc", "proc"),
        ("sysfs", "/sys", "sysfs"),
        ("devtmpfs", "/dev", "devtmpfs"),
        ("tmpfs", "/run", "tmpfs"),
        ("tmpfs", MOUNT_ROOT, "tmpfs"),
    ] {
        fs::create_dir_all(target)?;
        match rustix::mount::mount(
            src,
            target,
            fstype,
            MountFlags::NOSUID,
            None::<&std::ffi::CStr>,
        ) {
            Ok(()) | Err(Errno::BUSY) => {}
            Err(e) => return Err(io::Error::other(format!("mount {target}: {e}"))),
        }
    }
    Ok(())
}

/// 이미지의 `modules.order`(우리 빌더가 만든 차례) 그대로 올린다. 실패한 모듈은 콘솔에 적고 넘어간다.
fn load_modules() {
    let uname = rustix::system::uname();
    let release = uname.release().to_string_lossy().into_owned();
    let dir = PathBuf::from("/lib/modules").join(&release);
    let Ok(order) = fs::read_to_string(dir.join("modules.order")) else {
        eprintln!("orbivisor-restore-agent: no modules.order for kernel {release}");
        return;
    };
    for line in order.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = dir.join(line);
        let flags = if [".xz", ".zst", ".gz"].iter().any(|s| line.ends_with(s)) {
            MODULE_INIT_COMPRESSED_FILE
        } else {
            0
        };
        let r =
            fs::File::open(&path).and_then(|f| Ok(rustix::system::finit_module(&f, c"", flags)?));
        match r {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(Errno::EXIST.raw_os_error()) => {}
            Err(e) => eprintln!("orbivisor-restore-agent: module {line}: {e}"),
        }
    }
}

/// `/sys/class/virtio-ports/*/name`이 [`PORT_NAME`]인 포트의 장치
fn wait_port(timeout: Duration) -> io::Result<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(entries) = fs::read_dir("/sys/class/virtio-ports") {
            for e in entries.flatten() {
                if read_trim(e.path().join("name")).as_deref() == Some(PORT_NAME) {
                    return Ok(PathBuf::from("/dev").join(e.file_name()));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("virtio-serial port {PORT_NAME} did not appear"),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn boot() -> io::Result<()> {
    mount_basics()?;
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let proto = super::cmdline_value(&cmdline, "opl.proto").and_then(|v| v.parse::<u32>().ok());
    if proto != Some(PROTO_VERSION) {
        return Err(io::Error::other(format!(
            "opld asks for protocol {proto:?}, this agent speaks {PROTO_VERSION}"
        )));
    }
    load_modules();
    let port = wait_port(Duration::from_secs(30))?;
    let reader = OpenOptions::new().read(true).write(true).open(&port)?;
    let writer = reader.try_clone()?;
    let image_version = read_trim(IMAGE_VERSION_FILE)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| concat!("orbivisor ", env!("CARGO_PKG_VERSION")).to_string());
    let hello = Hello {
        proto: PROTO_VERSION,
        image_version: probe::clip(&image_version),
        kernel: probe::clip(&rustix::system::uname().release().to_string_lossy()),
    };
    serve(
        Arc::new(SystemVolumes::new()),
        hello,
        reader,
        writer,
        ServeOptions::default(),
    )
}

fn power_off() -> ! {
    rustix::fs::sync();
    let _ = rustix::system::reboot(rustix::system::RebootCommand::PowerOff);
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// PID 1 진입점: 끝나거나 실패하면 전원을 끈다(돌아오지 않음).
pub fn run_pid1() -> ! {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("orbivisor-restore-agent: {info}");
        power_off();
    }));
    match boot() {
        Ok(()) => eprintln!("orbivisor-restore-agent: shutting down"),
        Err(e) => eprintln!("orbivisor-restore-agent: {e}"),
    }
    power_off()
}
