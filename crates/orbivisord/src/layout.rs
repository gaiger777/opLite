//! 배치 맞추기(계획 74 설계 §3.4): 이름 바꾸기 앞의 디렉터리(`/etc/opl`, `/var/lib/opl`, `/run/opl`, `/var/log/opl`)를
//! 새 이름(`orbivisor`)으로 옮기고 옛 이름을 새 이름으로의 상대 링크로 남긴다.
//!
//! 옮기기는 `mv` + `ln`이 아니라 맞바꿈이다. 먼저 새 이름에 자기 자신을 가리키는 링크(`orbivisor → orbivisor`)를 두고 두
//! 이름을 원자적으로 바꾼다(Linux `renameat2(RENAME_EXCHANGE)`, macOS `renamex_np(RENAME_SWAP)`). 바꾼 뒤 옛 이름은
//! `orbivisor`를 가리키는 링크이므로 어느 순간에도 옛 경로가 풀린다 — 업그레이드 중에도 옛 데몬·QEMU·swtpm·ovsdb가 옛 경로를
//! 이름으로 연다. 같은 파일 시스템 안의 이름 바꾸기라 열린 fd·inode는 그대로다.
//!
//! 맞바꾸지 못하면(옛 것이 마운트 지점 — `EBUSY`, 맞바꿈을 받지 않는 파일 시스템 — `EINVAL`, 옛 것이 다른 곳으로의 링크)
//! 거꾸로 새 이름을 옛 이름으로의 링크(`orbivisor → opl`)로 만든다(리뷰 1 S1). 그러면 새 기본 경로가 옛 자리로 풀린다.
//! 그것도 못 하면 데이터 디렉터리 짝은 데몬이 시작하지 않는다([`fatal`] — 빈 새 디렉터리로 새 노드가 되지 않게).
//! 맞추기를 건너뛴 노드(root가 아님, 기본이 아닌 `--config`)는 기본 경로를 [`existing`]이 옛 자리로 돌린다.
//!
//! 몇 번 불러도 같다: 이미 맞춘 짝은 건드리지 않는다. 지난번 맞추기가 맞바꿈 직전에 멈춰 남은 자기 링크는 지우고 다시 한다.

use std::ffi::CString;
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// 새 설정 파일
pub const CONFIG: &str = "/etc/orbivisor/orbivisord.toml";
/// 이름 바꾸기 앞의 설정 파일
pub const LEGACY_CONFIG: &str = "/etc/opl/opld.toml";
/// 데몬이 연 제어 소켓 옆에 두는 옛 이름 링크(옛 CLI·스크립트)
pub const LEGACY_SOCKET: &str = "opld.sock";

/// (옛, 새) 디렉터리 짝. 뿌리에서 본 상대 경로다. 두 이름은 같은 부모 아래에 있다(상대 링크).
const DIRS: [(&str, &str); 4] = [
    ("etc/opl", "etc/orbivisor"),
    ("var/lib/opl", "var/lib/orbivisor"),
    ("run/opl", "run/orbivisor"),
    ("var/log/opl", "var/log/orbivisor"),
];

/// 데이터 디렉터리 짝의 새 이름(이 짝을 맞추지 못하면 시작하지 않는다)
const DATA_DIR: &str = "var/lib/orbivisor";
/// 런타임 디렉터리 짝의 새 이름
const RUN_DIR: &str = "run/orbivisor";

/// 설정 파일 짝(새 디렉터리 안). 디렉터리를 옮긴 뒤 `opld.toml`을 `orbivisord.toml`로 맞바꾼다.
const CONFIG_FILES: (&str, &str) = ("etc/orbivisor/opld.toml", "etc/orbivisor/orbivisord.toml");

/// 배치 맞추기가 한 일. 데몬이 시작 로그로 낸다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// 옛 것을 새 이름으로 옮기고 옛 이름을 링크로 남겼다.
    Moved { old: PathBuf, new: PathBuf },
    /// 새 것만 있어 옛 이름 링크를 만들었다.
    Linked { old: PathBuf, new: PathBuf },
    /// 옛 것을 옮기지 못해(`reason`) 새 이름을 옛 이름으로의 링크로 만들었다. `mount`: 옛 것이 마운트 지점이다.
    LinkedNew {
        old: PathBuf,
        new: PathBuf,
        reason: String,
        mount: bool,
    },
    /// 지난 맞추기가 도중에 멈춰 남은 자기 링크(`orbivisor → orbivisor`)를 지웠다.
    RemovedSelfLink { path: PathBuf },
    /// 둘 다 진짜라 손대지 않았다.
    BothReal { old: PathBuf, new: PathBuf },
    /// 옮기지도 링크를 만들지도 못했다. 기본 경로는 옛 자리로 돈다([`existing`]).
    Failed {
        old: PathBuf,
        new: PathBuf,
        error: String,
    },
}

impl Note {
    /// 운영자가 볼 일(경고)인지
    pub fn is_warning(&self) -> bool {
        matches!(
            self,
            Note::LinkedNew { .. } | Note::BothReal { .. } | Note::Failed { .. }
        )
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Note::Moved { old, new } => write!(
                f,
                "moved {} to {} and left {} as a link",
                old.display(),
                new.display(),
                old.display()
            ),
            Note::Linked { old, new } => {
                write!(f, "linked {} to {}", old.display(), new.display())
            }
            Note::LinkedNew {
                old,
                new,
                reason,
                mount,
            } => {
                write!(
                    f,
                    "cannot move {} to {} ({reason}); made {} a link to {} instead",
                    old.display(),
                    new.display(),
                    new.display(),
                    old.display()
                )?;
                if *mount {
                    // 마운트 지점을 옮기는 절차(리뷰 2 N4). fstab만 고치면 systemd가 링크를 따라 옛 자리에 다시 마운트한다.
                    write!(
                        f,
                        ". {old} is a mount point; leaving {new} as a link works. To finish the rename, stop the VMs on \
                         this node and orbivisord, remove the link {new}, create the directory {new}, move the mount \
                         (fstab, systemd mount unit) to {new} and mount it there, then start orbivisord: it replaces the \
                         empty {old} with a link. Changing only fstab mounts it at {old} again through the link",
                        old = old.display(),
                        new = new.display()
                    )?;
                }
                Ok(())
            }
            Note::RemovedSelfLink { path } => write!(
                f,
                "removed {}, a link to itself left by an interrupted layout change",
                path.display()
            ),
            Note::BothReal { old, new } => write!(
                f,
                "both {} and {} exist; left them as they are (merge them by hand and make {} a link)",
                old.display(),
                new.display(),
                old.display()
            ),
            Note::Failed { old, new, error } => write!(
                f,
                "cannot move {} to {} or link it ({error}); still using {}",
                old.display(),
                new.display(),
                old.display()
            ),
        }
    }
}

/// 뿌리 `root` 아래의 배치를 맞춘다.
pub fn migrate(root: &Path) -> Vec<Note> {
    migrate_with(root, exchange, link)
}

/// [`migrate`]와 같되 맞바꿈과 링크 만들기를 받는다(시험이 실패를 흉내 낸다).
fn migrate_with(
    root: &Path,
    swap: impl Fn(&Path, &Path) -> io::Result<()>,
    ln: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Vec<Note> {
    let mut notes = Vec::new();
    for (old, new) in DIRS {
        align(
            &root.join(old),
            &root.join(new),
            true,
            &swap,
            &ln,
            &mut notes,
        );
    }
    let (old, new) = CONFIG_FILES;
    align(
        &root.join(old),
        &root.join(new),
        false,
        &swap,
        &ln,
        &mut notes,
    );
    notes
}

/// 상대 링크 `link → target`을 만든다.
fn link(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 짝 하나를 맞춘다. `dir`이면 디렉터리 짝, 아니면 파일 짝이다.
fn align(
    old: &Path,
    new: &Path,
    dir: bool,
    swap: &impl Fn(&Path, &Path) -> io::Result<()>,
    ln: &impl Fn(&Path, &Path) -> io::Result<()>,
    notes: &mut Vec<Note>,
) {
    let kind = |m: std::fs::Metadata| if dir { m.is_dir() } else { m.is_file() };
    // 진짜(링크 아님)인지, 링크를 따라 풀리는지
    let real = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(kind);
    let resolves = |p: &Path| std::fs::metadata(p).is_ok_and(kind);
    let present = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let failed = |error: String| Note::Failed {
        old: old.to_path_buf(),
        new: new.to_path_buf(),
        error,
    };
    // 링크 대상은 같은 디렉터리 안의 이름(상대 링크)이다.
    let (Some(new_name), Some(old_name)) = (new.file_name(), old.file_name()) else {
        return;
    };
    // 맞바꿈 직전에 멈춘 맞추기가 남긴 자기 링크(리뷰 1 M1): 지우고 없는 것으로 본다.
    if std::fs::read_link(new).is_ok_and(|t| t.as_os_str() == new_name) {
        if let Err(e) = std::fs::remove_file(new) {
            notes.push(failed(format!("cannot remove the link to itself: {e}")));
            return;
        }
        notes.push(Note::RemovedSelfLink {
            path: new.to_path_buf(),
        });
    }
    if !present(new) && resolves(old) {
        let mut mount = false;
        let reason = if real(old) {
            match ln(Path::new(new_name), new).and_then(|()| {
                swap(old, new).inspect_err(|_| {
                    let _ = std::fs::remove_file(new);
                })
            }) {
                Ok(()) => {
                    notes.push(Note::Moved {
                        old: old.to_path_buf(),
                        new: new.to_path_buf(),
                    });
                    return;
                }
                Err(e) => {
                    mount = dir && is_mount_point(old);
                    e.to_string()
                }
            }
        } else {
            format!("{} is a link", old.display())
        };
        // 거꾸로 새 이름을 옛 이름으로의 링크로(옛 것이 다른 곳으로의 링크여도 옛 이름을 거쳐 풀린다).
        notes.push(match ln(Path::new(old_name), new) {
            Ok(()) => Note::LinkedNew {
                old: old.to_path_buf(),
                new: new.to_path_buf(),
                reason,
                mount,
            },
            Err(e) => failed(format!("{reason}; cannot link {}: {e}", new.display())),
        });
        return;
    }
    if real(new) && !present(old) {
        // 새로 설치: 옛 이름 링크(저장된 옛 경로·옛 스크립트가 풀리게). 옛 이름의 부모가 없으면(예: 설정 파일 짝에서
        // 새 디렉터리가 진짜가 아님) 만들지 않는다.
        if old.parent().is_some_and(|p| !p.is_dir()) {
            return;
        }
        notes.push(match ln(Path::new(new_name), old) {
            Ok(()) => Note::Linked {
                old: old.to_path_buf(),
                new: new.to_path_buf(),
            },
            Err(e) => failed(e.to_string()),
        });
        return;
    }
    if real(old) && real(new) {
        // 옛 것이 빈 디렉터리이고 마운트 지점이 아니면(마운트 지점을 새 이름으로 옮긴 뒤 남은 것 — 리뷰 2 N4) 지우고
        // 옛 이름 링크를 둔다. 남겨 두면 옛 경로(옛 접두사로 저장한 경로, 설정의 `data_dir = "/var/lib/opl"`)가 빈
        // 디렉터리로 풀린다.
        if dir && is_empty_dir(old) && !is_mount_point(old) && std::fs::remove_dir(old).is_ok() {
            notes.push(match ln(Path::new(new_name), old) {
                Ok(()) => Note::Linked {
                    old: old.to_path_buf(),
                    new: new.to_path_buf(),
                },
                Err(e) => failed(e.to_string()),
            });
            return;
        }
        notes.push(Note::BothReal {
            old: old.to_path_buf(),
            new: new.to_path_buf(),
        });
    }
}

/// `p`가 빈 디렉터리인지
fn is_empty_dir(p: &Path) -> bool {
    std::fs::read_dir(p).is_ok_and(|mut d| d.next().is_none())
}

/// 디렉터리 `p`가 마운트 지점인지(부모와 장치가 다름)
fn is_mount_point(p: &Path) -> bool {
    let dev = |p: &Path| std::fs::symlink_metadata(p).map(|m| m.dev());
    match (dev(p), p.parent().map(dev)) {
        (Ok(a), Some(Ok(b))) => a != b,
        _ => false,
    }
}

/// 배치 맞추기 뒤 시작하면 안 되는 일: 데이터 디렉터리의 옛 것이 있는데 새 이름으로 옮기지도 링크하지도 못했다.
/// 그대로 시작하면 설정이 데이터 디렉터리를 적지 않은 노드는 빈 새 디렉터리로 새 노드가 될 수 있다(리뷰 1 S1).
pub fn fatal(notes: &[Note]) -> Option<&Note> {
    notes.iter().find(|n| match n {
        Note::Failed { old, new, .. } => new.ends_with(DATA_DIR) && old.is_dir() && !new.is_dir(),
        _ => false,
    })
}

/// 기본 경로 `path`(새 이름 아래)를 이 노드에서 쓸 자리로 바꾼다: 그 경로가 든 새 디렉터리가 없고 옛 디렉터리가
/// 있으면(배치를 맞추지 않았거나 못 맞춘 노드 — root가 아님, 기본이 아닌 `--config`) 옛 이름 아래로. 런타임 짝
/// (`/run`)은 둘 다 진짜 디렉터리여도 옛 이름 아래로 — 옛 판으로 되돌렸다가 다시 올린 노드에서는 옛 데몬이 띄운 VM의
/// QMP·swtpm 소켓이 옛 디렉터리에 있다(리뷰 2 N1, 다음 부팅에 `/run`이 비면 맞춰진다). 그 밖에는 그대로.
/// 설정 파일에 적은 값에는 쓰지 않는다(기본값만).
pub fn existing(path: &Path) -> PathBuf {
    existing_in(Path::new("/"), path)
}

fn existing_in(root: &Path, path: &Path) -> PathBuf {
    for (old, new) in DIRS {
        let Ok(rest) = path.strip_prefix(Path::new("/").join(new)) else {
            continue;
        };
        let real = |p: &str| std::fs::symlink_metadata(root.join(p)).is_ok_and(|m| m.is_dir());
        let missing = std::fs::symlink_metadata(root.join(new)).is_err();
        let both_run = new == RUN_DIR && real(old) && real(new);
        if (missing || both_run) && std::fs::metadata(root.join(old)).is_ok_and(|m| m.is_dir()) {
            let old = Path::new("/").join(old);
            return if rest.as_os_str().is_empty() {
                old
            } else {
                old.join(rest)
            };
        }
        break;
    }
    path.to_path_buf()
}

/// 이 노드가 기능 `orbivisor-layout`을 알릴 수 있는지. 다른 노드가 클러스터 저장소에 적는 경로(RBD 풀의 Ceph 설정,
/// ACME 훅, 올린 파일)가 있는 `/etc`·`/var/lib` 짝마다 옛 이름이 없거나(풀리지 않음) 두 이름이 같은 곳(같은 장치·inode)으로
/// 풀리면 참이다 — 맞바꾼 노드, 새 이름을 옛 이름으로의 링크로 만든 노드, 새로 설치한 노드. 맞추지 못한 노드(옛 이름만
/// 있음, 둘 다 진짜)는 새 이름으로 적힌 경로를 열지 못하거나 다른 파일을 열므로 알리지 않는다.
pub fn aligned(root: &Path) -> bool {
    let id = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.dev(), m.ino()));
    DIRS[..2]
        .iter()
        .all(|(old, new)| match id(&root.join(old)) {
            None => true,
            Some(old) => id(&root.join(new)) == Some(old),
        })
}

/// 두 이름을 원자적으로 맞바꾼다.
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    // SAFETY: 두 경로는 NUL로 끝나는 C 문자열이고 호출 동안 산다.
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: 두 경로는 NUL로 끝나는 C 문자열이고 호출 동안 산다.
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let rc: libc::c_int = {
        let _ = (a, b);
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// 데몬이 시작할 때 배치를 맞출지: root이고 `--config`가 없거나 기본 두 경로 중 하나일 때만(시험·개발 노드가 남의
/// 디렉터리를 옮기지 않게).
pub fn should_migrate(euid_is_root: bool, config: Option<&Path>) -> bool {
    euid_is_root && config.is_none_or(|c| c == Path::new(CONFIG) || c == Path::new(LEGACY_CONFIG))
}

/// root인데 기본이 아닌 `--config`라 배치를 맞추지 않았고 배치가 맞지 않을 때의 경고(리뷰 2). 그런 노드는 기능
/// `orbivisor-layout`을 알리지 않아 클러스터가 계속 옛 접두사로 적으므로(안전한 쪽) 운영자가 까닭을 알 수 있게 한다.
pub fn not_migrated_warning(
    euid_is_root: bool,
    config: Option<&Path>,
    root: &Path,
) -> Option<String> {
    let config = config?;
    if !euid_is_root || should_migrate(euid_is_root, Some(config)) || aligned(root) {
        return None;
    }
    Some(format!(
        "the old directories (/etc/opl, /var/lib/opl) are not moved to the orbivisor names because --config {} is not \
         a default path ({CONFIG} or {LEGACY_CONFIG}); the node keeps the old layout and the cluster keeps writing \
         the old path prefixes. Start orbivisord once without --config (or with a default path) to move them",
        config.display()
    ))
}

/// 읽을 설정 파일과 안내(경고). `--config`가 있으면 그것, 없으면 새 경로, 그것이 없고 옛 경로가 있으면 옛 경로.
pub fn config_path(given: Option<&Path>, root: &Path) -> (PathBuf, Option<String>) {
    if let Some(given) = given {
        return (given.to_path_buf(), None);
    }
    let new = root.join(CONFIG.trim_start_matches('/'));
    let old = root.join(LEGACY_CONFIG.trim_start_matches('/'));
    match (std::fs::metadata(&new), std::fs::metadata(&old)) {
        (Ok(n), Ok(o)) => {
            // 맞바꾼 뒤에는 옛 것이 새 것으로의 링크다(같은 파일). 진짜 파일이 둘일 때만 경고한다.
            if (n.dev(), n.ino()) == (o.dev(), o.ino()) {
                return (new, None);
            }
            // 옛 경로를 원자적으로 바꿔 쓰는 도구(`sed -i`, Ansible `copy`)는 링크 자리에 진짜 파일을 둔다(리뷰 1 M2).
            // 그 고친 값을 버리지 않게 더 늦게 고친 쪽을 읽고, 어느 것을 읽는지 밝힌다.
            let old_is_newer = matches!(
                (o.modified(), n.modified()),
                (Ok(o), Ok(n)) if o > n
            );
            let (read, ignored) = if old_is_newer { (old, new) } else { (new, old) };
            let warning = format!(
                "both {} and {} are files (a tool replaced the link with a copy?); reading {}, the one changed last, \
                 and ignoring {}. Put the settings in {} and make {} a link to it",
                read.display(),
                ignored.display(),
                read.display(),
                ignored.display(),
                root.join(CONFIG.trim_start_matches('/')).display(),
                root.join(LEGACY_CONFIG.trim_start_matches('/')).display()
            );
            (read, Some(warning))
        }
        (Err(_), Ok(_)) => {
            let warning = format!(
                "reading the old configuration path {}; move it to {}",
                old.display(),
                new.display()
            );
            (old, Some(warning))
        }
        _ => (new, None),
    }
}

/// 설정 글에 적힌 옛 경로(`/etc/opl` 등). 옛 이름 링크로 그대로 돌지만 시작 로그에 한 번 안내한다.
pub fn legacy_paths_in(text: &str) -> Vec<&'static str> {
    ["/etc/opl", "/var/lib/opl", "/run/opl", "/var/log/opl"]
        .into_iter()
        .filter(|p| {
            text.match_indices(p).any(|(i, _)| {
                text[i + p.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_'))
            })
        })
        .collect()
}

/// 제어 소켓 옆에 옛 이름 링크(`opld.sock → orbivisord.sock`)를 둔다. 있던 것(옛 데몬의 소켓·링크)은 바꾼다.
pub fn link_legacy_socket(socket: &Path) -> io::Result<PathBuf> {
    let link = socket.with_file_name(LEGACY_SOCKET);
    let target = socket
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let tmp = socket.with_file_name(format!(".{LEGACY_SOCKET}.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    std::fs::rename(&tmp, &link).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link_target(p: &Path) -> PathBuf {
        std::fs::read_link(p).unwrap()
    }

    fn is_real_dir(p: &Path) -> bool {
        std::fs::symlink_metadata(p).unwrap().is_dir()
    }

    /// 옛 배치의 노드: 옛 디렉터리 넷과 설정 파일
    fn old_layout(root: &Path) {
        for (old, _) in DIRS {
            std::fs::create_dir_all(root.join(old)).unwrap();
        }
        std::fs::write(root.join("etc/opl/opld.toml"), "name = \"n\"\n").unwrap();
        std::fs::write(root.join("var/lib/opl/node-id"), "x").unwrap();
    }

    #[test]
    fn moves_the_old_layout_and_leaves_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        old_layout(root);
        // 맞바꿈 앞에 옛 경로로 연 파일(돌고 있는 QEMU·ovsdb가 쥔 fd)
        let held = std::fs::File::open(root.join("var/lib/opl/node-id")).unwrap();
        let before = held.metadata().unwrap().ino();
        let dir_before = std::fs::metadata(root.join("var/lib/opl")).unwrap().ino();

        let notes = migrate(root);

        for (old, new) in DIRS {
            assert!(is_real_dir(&root.join(new)), "{new}");
            assert_eq!(
                link_target(&root.join(old)),
                Path::new("orbivisor"),
                "{old}"
            );
            assert!(
                notes.contains(&Note::Moved {
                    old: root.join(old),
                    new: root.join(new)
                }),
                "{notes:?}"
            );
        }
        // 옛 경로·새 경로·열린 fd가 같은 inode다.
        assert_eq!(held.metadata().unwrap().ino(), before);
        for p in ["var/lib/opl/node-id", "var/lib/orbivisor/node-id"] {
            assert_eq!(
                std::fs::metadata(root.join(p)).unwrap().ino(),
                before,
                "{p}"
            );
        }
        assert_eq!(
            std::fs::metadata(root.join("var/lib/orbivisor"))
                .unwrap()
                .ino(),
            dir_before
        );
        // 설정 파일도 맞바꾼다: 진짜는 orbivisord.toml, opld.toml은 링크.
        let conf = root.join("etc/orbivisor/orbivisord.toml");
        assert!(std::fs::symlink_metadata(&conf).unwrap().is_file());
        assert_eq!(
            link_target(&root.join("etc/orbivisor/opld.toml")),
            Path::new("orbivisord.toml")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("etc/opl/opld.toml")).unwrap(),
            "name = \"n\"\n"
        );
        assert!(notes.contains(&Note::Moved {
            old: root.join("etc/orbivisor/opld.toml"),
            new: conf,
        }));
        assert_eq!(notes.len(), 5, "{notes:?}");
        assert!(notes.iter().all(|n| !n.is_warning()));
    }

    #[test]
    fn running_again_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        old_layout(root);
        assert_eq!(migrate(root).len(), 5);
        assert_eq!(migrate(root), vec![]);
        assert_eq!(migrate(root), vec![]);
        assert_eq!(
            link_target(&root.join("var/lib/opl")),
            Path::new("orbivisor")
        );
        assert!(is_real_dir(&root.join("var/lib/orbivisor")));
    }

    #[test]
    fn a_new_install_gets_old_name_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("etc/orbivisor")).unwrap();
        std::fs::create_dir_all(root.join("var/lib/orbivisor")).unwrap();
        std::fs::create_dir_all(root.join("var/log")).unwrap();
        std::fs::write(root.join("etc/orbivisor/orbivisord.toml"), "x").unwrap();
        let notes = migrate(root);
        assert_eq!(
            notes,
            vec![
                Note::Linked {
                    old: root.join("etc/opl"),
                    new: root.join("etc/orbivisor")
                },
                Note::Linked {
                    old: root.join("var/lib/opl"),
                    new: root.join("var/lib/orbivisor")
                },
                Note::Linked {
                    old: root.join("etc/orbivisor/opld.toml"),
                    new: root.join("etc/orbivisor/orbivisord.toml")
                },
            ]
        );
        assert_eq!(link_target(&root.join("etc/opl")), Path::new("orbivisor"));
        assert_eq!(
            std::fs::read_to_string(root.join("etc/opl/opld.toml")).unwrap(),
            "x"
        );
        // 둘 다 없는 짝(run, log)은 그대로 둔다.
        assert!(!root.join("run/opl").exists() && !root.join("var/log/opl").exists());
        assert_eq!(migrate(root), vec![]);
    }

    #[test]
    fn leaves_two_real_directories_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("var/lib/opl")).unwrap();
        std::fs::write(root.join("var/lib/opl/node-id"), "x").unwrap();
        std::fs::create_dir_all(root.join("var/lib/orbivisor")).unwrap();
        let notes = migrate(root);
        assert_eq!(
            notes,
            vec![Note::BothReal {
                old: root.join("var/lib/opl"),
                new: root.join("var/lib/orbivisor")
            }]
        );
        assert!(notes[0].is_warning());
        assert!(
            is_real_dir(&root.join("var/lib/opl")) && is_real_dir(&root.join("var/lib/orbivisor"))
        );
    }

    #[test]
    fn an_empty_old_directory_next_to_a_real_new_one_becomes_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 마운트 지점을 /var/lib/orbivisor로 옮긴 뒤(링크를 지우고 디렉터리를 만들어 거기에 마운트) 남은 빈 옛 디렉터리
        std::fs::create_dir_all(root.join("var/lib/opl")).unwrap();
        std::fs::create_dir_all(root.join("var/lib/orbivisor")).unwrap();
        std::fs::write(root.join("var/lib/orbivisor/node-id"), "x").unwrap();
        // 내용이 있는 옛 디렉터리는 그대로(둘 다 진짜)
        std::fs::create_dir_all(root.join("var/log/opl")).unwrap();
        std::fs::write(root.join("var/log/opl/a.log"), "").unwrap();
        std::fs::create_dir_all(root.join("var/log/orbivisor")).unwrap();
        let notes = migrate(root);
        assert_eq!(
            notes,
            vec![
                Note::Linked {
                    old: root.join("var/lib/opl"),
                    new: root.join("var/lib/orbivisor")
                },
                Note::BothReal {
                    old: root.join("var/log/opl"),
                    new: root.join("var/log/orbivisor")
                },
            ]
        );
        assert_eq!(
            link_target(&root.join("var/lib/opl")),
            Path::new("orbivisor")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("var/lib/opl/node-id")).unwrap(),
            "x"
        );
        assert!(aligned(root));
    }

    #[test]
    fn the_mount_point_advice_moves_the_mount_and_says_why_fstab_alone_is_not_enough() {
        let note = Note::LinkedNew {
            old: PathBuf::from("/var/lib/opl"),
            new: PathBuf::from("/var/lib/orbivisor"),
            reason: "Device or resource busy".into(),
            mount: true,
        }
        .to_string();
        for part in [
            "leaving /var/lib/orbivisor as a link works",
            "remove the link /var/lib/orbivisor, create the directory /var/lib/orbivisor",
            "replaces the empty /var/lib/opl with a link",
            "Changing only fstab mounts it at /var/lib/opl again",
        ] {
            assert!(note.contains(part), "{part}: {note}");
        }
    }

    #[test]
    fn runtime_paths_stay_in_the_old_directory_when_both_are_real() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 옛 판으로 되돌렸다가 다시 올린 노드: 옛 데몬이 /run/opl을 진짜로 만들었고(VM 소켓이 그 안에) 새 이름 단위가
        // /run/orbivisor도 만들었다.
        std::fs::create_dir_all(root.join("run/opl/vm/1")).unwrap();
        std::fs::create_dir_all(root.join("run/orbivisor/ovn")).unwrap();
        assert_eq!(
            existing_in(root, Path::new("/run/orbivisor/vm")),
            Path::new("/run/opl/vm")
        );
        // 데이터 짝은 둘 다 진짜면 새 이름 그대로(맞바꿈 규칙과 같음)
        std::fs::create_dir_all(root.join("var/lib/opl")).unwrap();
        std::fs::create_dir_all(root.join("var/lib/orbivisor")).unwrap();
        assert_eq!(
            existing_in(root, Path::new("/var/lib/orbivisor")),
            Path::new("/var/lib/orbivisor")
        );
        // 맞춘 /run(옛 이름이 링크)은 새 이름
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("run/orbivisor")).unwrap();
        std::os::unix::fs::symlink("orbivisor", dir.path().join("run/opl")).unwrap();
        assert_eq!(
            existing_in(dir.path(), Path::new("/run/orbivisor/x")),
            Path::new("/run/orbivisor/x")
        );
    }

    fn fail(_: &Path, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EINVAL))
    }

    #[test]
    fn a_failed_swap_links_the_new_name_to_the_old_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        old_layout(root);
        let notes = migrate_with(root, fail, link);
        // 디렉터리 넷과 설정 파일(옛 이름 디렉터리 안에서 맞바꿈도 실패)
        assert_eq!(notes.len(), 5, "{notes:?}");
        assert!(
            notes
                .iter()
                .all(|n| matches!(n, Note::LinkedNew { mount: false, .. }) && n.is_warning()),
            "{notes:?}"
        );
        for (old, new) in DIRS {
            assert!(is_real_dir(&root.join(old)), "{old}");
            assert_eq!(link_target(&root.join(new)), Path::new("opl"), "{new}");
        }
        // 새 기본 경로가 옛 자리로 풀린다(빈 새 디렉터리가 아님).
        assert_eq!(
            std::fs::read_to_string(root.join("var/lib/orbivisor/node-id")).unwrap(),
            "x"
        );
        assert_eq!(
            link_target(&root.join("etc/orbivisor/orbivisord.toml")),
            Path::new("opld.toml")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("etc/orbivisor/orbivisord.toml")).unwrap(),
            "name = \"n\"\n"
        );
        assert!(notes[0].to_string().contains("a link to"), "{}", notes[0]);
        assert!(fatal(&notes).is_none());
        assert!(aligned(root));
        // 다시 해도 같다(새 이름이 풀리는 링크다).
        assert_eq!(migrate(root), vec![]);
    }

    #[test]
    fn an_old_directory_that_is_a_link_gets_a_new_name_link() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // `/var/lib/opl → /data/opl`처럼 데이터를 옮겨 둔 노드
        std::fs::create_dir_all(root.join("data/opl")).unwrap();
        std::fs::write(root.join("data/opl/node-id"), "x").unwrap();
        std::fs::create_dir_all(root.join("var/lib")).unwrap();
        std::os::unix::fs::symlink(root.join("data/opl"), root.join("var/lib/opl")).unwrap();
        let notes = migrate(root);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            matches!(&notes[0], Note::LinkedNew { reason, .. } if reason.contains("is a link"))
        );
        assert_eq!(
            link_target(&root.join("var/lib/orbivisor")),
            Path::new("opl")
        );
        assert_eq!(
            std::fs::read_to_string(root.join("var/lib/orbivisor/node-id")).unwrap(),
            "x"
        );
        assert!(aligned(root));
    }

    #[test]
    fn a_data_directory_that_cannot_be_moved_or_linked_stops_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        old_layout(root);
        let notes = migrate_with(root, fail, |target, l| {
            if target == Path::new("opl") || target == Path::new("opld.toml") {
                Err(io::Error::from_raw_os_error(libc::EROFS))
            } else {
                link(target, l)
            }
        });
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(
            notes
                .iter()
                .all(|n| matches!(n, Note::Failed { .. }) && n.is_warning())
        );
        for (_, new) in DIRS {
            assert!(std::fs::symlink_metadata(root.join(new)).is_err(), "{new}");
        }
        // 데이터 디렉터리 짝만 시작을 막는다. 나머지는 경고(기본 경로가 옛 자리로 돈다).
        let stop = fatal(&notes).unwrap();
        assert!(matches!(stop, Note::Failed { new, .. } if new.ends_with("var/lib/orbivisor")));
        assert!(stop.to_string().contains("still using"));
        let without_data: Vec<_> = notes
            .iter()
            .filter(
                |n| !matches!(n, Note::Failed { new, .. } if new.ends_with("var/lib/orbivisor")),
            )
            .cloned()
            .collect();
        assert!(fatal(&without_data).is_none());
        assert!(!aligned(root));
        for (old, new) in DIRS {
            assert_eq!(
                existing_in(root, &Path::new("/").join(new).join("x")),
                Path::new("/").join(old).join("x")
            );
        }
    }

    #[test]
    fn removes_a_self_link_left_by_an_interrupted_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        old_layout(root);
        // 자기 링크를 둔 뒤 맞바꾸기 전에 멈춤
        std::os::unix::fs::symlink("orbivisor", root.join("var/lib/orbivisor")).unwrap();
        let notes = migrate(root);
        assert!(notes.contains(&Note::RemovedSelfLink {
            path: root.join("var/lib/orbivisor")
        }));
        assert!(notes.contains(&Note::Moved {
            old: root.join("var/lib/opl"),
            new: root.join("var/lib/orbivisor")
        }));
        assert!(is_real_dir(&root.join("var/lib/orbivisor")));
        assert_eq!(
            link_target(&root.join("var/lib/opl")),
            Path::new("orbivisor")
        );
    }

    #[test]
    fn default_paths_fall_back_to_old_directories_only_when_the_new_one_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let data = Path::new("/var/lib/orbivisor");
        // 아무것도 없음(새로 설치·시험): 새 이름
        assert_eq!(existing_in(root, data), data);
        // 맞추지 않은 옛 배치(root가 아님, 기본이 아닌 --config): 옛 이름
        old_layout(root);
        assert_eq!(existing_in(root, data), Path::new("/var/lib/opl"));
        assert_eq!(
            existing_in(root, Path::new("/etc/orbivisor/ceph")),
            Path::new("/etc/opl/ceph")
        );
        assert_eq!(existing_in(root, Path::new("/srv/x")), Path::new("/srv/x"));
        assert!(!aligned(root));
        // 맞춘 뒤: 새 이름
        migrate(root);
        assert_eq!(existing_in(root, data), data);
        assert_eq!(
            existing_in(root, Path::new("/etc/orbivisor/ceph")),
            Path::new("/etc/orbivisor/ceph")
        );
        assert!(aligned(root));
    }

    #[test]
    fn the_layout_feature_needs_both_names_in_one_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // 새로 설치(옛 이름 없음)
        assert!(aligned(root));
        std::fs::create_dir_all(root.join("var/lib/orbivisor")).unwrap();
        assert!(aligned(root));
        // 둘 다 진짜
        std::fs::create_dir_all(root.join("var/lib/opl")).unwrap();
        assert!(!aligned(root));
        // /run·/var/log 짝은 보지 않는다(저장되는 경로가 없다).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("run/opl")).unwrap();
        assert!(aligned(dir.path()));
    }

    #[test]
    fn swaps_really_exchange_two_names() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::os::unix::fs::symlink("b", &b).unwrap();
        exchange(&a, &b).unwrap();
        assert!(is_real_dir(&b));
        assert_eq!(link_target(&a), Path::new("b"));
    }

    #[test]
    fn migrates_only_as_root_with_a_default_config() {
        assert!(should_migrate(true, None));
        assert!(should_migrate(true, Some(Path::new(CONFIG))));
        assert!(should_migrate(true, Some(Path::new(LEGACY_CONFIG))));
        assert!(!should_migrate(true, Some(Path::new("/tmp/x.toml"))));
        assert!(!should_migrate(false, None));
    }

    #[test]
    fn warns_when_a_custom_config_keeps_the_old_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let custom = Path::new("/etc/opl/node.toml");
        // 새로 설치(옛 이름 없음)·root가 아님·--config 없음·기본 경로면 경고 없음
        assert_eq!(not_migrated_warning(true, Some(custom), root), None);
        old_layout(root);
        assert_eq!(not_migrated_warning(false, Some(custom), root), None);
        assert_eq!(not_migrated_warning(true, None, root), None);
        assert_eq!(
            not_migrated_warning(true, Some(Path::new(CONFIG)), root),
            None
        );
        let warning = not_migrated_warning(true, Some(custom), root).unwrap();
        assert!(
            warning.contains("--config /etc/opl/node.toml is not a default path"),
            "{warning}"
        );
        // 맞춘 뒤면 경고 없음
        migrate(root);
        assert_eq!(not_migrated_warning(true, Some(custom), root), None);
    }

    #[test]
    fn picks_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let given = Path::new("/x/y.toml");
        assert_eq!(config_path(Some(given), root), (given.to_path_buf(), None));
        // 아무것도 없으면 새 경로(읽기에서 실패한다).
        let new = root.join("etc/orbivisor/orbivisord.toml");
        let old = root.join("etc/opl/opld.toml");
        assert_eq!(config_path(None, root), (new.clone(), None));
        // 옛 것만
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "").unwrap();
        let (path, warning) = config_path(None, root);
        assert_eq!(path, old);
        assert!(warning.unwrap().contains("old configuration path"));
        // 둘 다 진짜면 새 것과 경고
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, "").unwrap();
        let (path, warning) = config_path(None, root);
        assert_eq!(path, new);
        assert!(warning.unwrap().contains("both"));
        // 옛 경로를 도구가 링크 대신 파일로 바꿔 썼으면(더 늦게 고침) 그것을 읽고 밝힌다.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let (path, warning) = config_path(None, root);
        assert_eq!(path, old);
        let warning = warning.unwrap();
        assert!(
            warning.contains(&format!("reading {}", old.display())),
            "{warning}"
        );
        // 맞바꾼 뒤(옛 것이 링크)는 새 것, 경고 없음
        let dir = tempfile::tempdir().unwrap();
        old_layout(dir.path());
        migrate(dir.path());
        assert_eq!(
            config_path(None, dir.path()),
            (dir.path().join("etc/orbivisor/orbivisord.toml"), None)
        );
    }

    #[test]
    fn finds_old_paths_in_the_config() {
        assert_eq!(
            legacy_paths_in("data_dir = \"/var/lib/opl\"\nrun_dir = \"/run/opl/x\"\n"),
            vec!["/var/lib/opl", "/run/opl"]
        );
        assert!(legacy_paths_in("data_dir = \"/var/lib/orbivisor\"\n# /etc/oplite\n").is_empty());
        assert_eq!(
            legacy_paths_in("client_dir = \"/etc/opl\""),
            vec!["/etc/opl"]
        );
    }

    #[test]
    fn links_the_old_socket_name() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("orbivisord.sock");
        std::fs::write(&sock, "").unwrap();
        // 옛 데몬이 남긴 소켓 파일도 바꾼다.
        std::fs::write(dir.path().join("opld.sock"), "old").unwrap();
        let link = link_legacy_socket(&sock).unwrap();
        assert_eq!(link, dir.path().join("opld.sock"));
        assert_eq!(link_target(&link), Path::new("orbivisord.sock"));
        // 다시 해도 같다.
        link_legacy_socket(&sock).unwrap();
        assert_eq!(link_target(&link), Path::new("orbivisord.sock"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
