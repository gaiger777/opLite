//! 배치 맞추기(계획 74 설계 §3.4): 이름 바꾸기 앞의 디렉터리(`/etc/opl`, `/var/lib/opl`, `/run/opl`, `/var/log/opl`)를
//! 새 이름(`orbivisor`)으로 옮기고 옛 이름을 새 이름으로의 상대 링크로 남긴다.
//!
//! 옮기기는 `mv` + `ln`이 아니라 맞바꿈이다. 먼저 새 이름에 자기 자신을 가리키는 링크(`orbivisor → orbivisor`)를 두고 두
//! 이름을 원자적으로 바꾼다(Linux `renameat2(RENAME_EXCHANGE)`, macOS `renamex_np(RENAME_SWAP)`). 바꾼 뒤 옛 이름은
//! `orbivisor`를 가리키는 링크이므로 어느 순간에도 옛 경로가 풀린다 — 업그레이드 중에도 옛 데몬·QEMU·swtpm·ovsdb가 옛 경로를
//! 이름으로 연다. 같은 파일 시스템 안의 이름 바꾸기라 열린 fd·inode는 그대로다.
//!
//! 몇 번 불러도 같다: 이미 맞춘 짝은 건드리지 않는다.

use std::ffi::CString;
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// 새 설정 파일
pub const CONFIG: &str = "/etc/orbivisor/orbivisord.toml";
/// 이름 바꾸기 앞의 설정 파일
pub const LEGACY_CONFIG: &str = "/etc/opl/opld.toml";
/// 데몬이 연 제어 소켓 옆에 두는 옛 이름 링크(옛 CLI·스크립트)
pub const LEGACY_SOCKET: &str = "opld.sock";

/// (옛, 새) 디렉터리 짝. 뿌리에서 본 상대 경로다.
const DIRS: [(&str, &str); 4] = [
    ("etc/opl", "etc/orbivisor"),
    ("var/lib/opl", "var/lib/orbivisor"),
    ("run/opl", "run/orbivisor"),
    ("var/log/opl", "var/log/orbivisor"),
];

/// 설정 파일 짝(새 디렉터리 안). 디렉터리를 옮긴 뒤 `opld.toml`을 `orbivisord.toml`로 맞바꾼다.
const CONFIG_FILES: (&str, &str) = ("etc/orbivisor/opld.toml", "etc/orbivisor/orbivisord.toml");

/// 배치 맞추기가 한 일. 데몬이 시작 로그로 낸다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    /// 옛 것을 새 이름으로 옮기고 옛 이름을 링크로 남겼다.
    Moved { old: PathBuf, new: PathBuf },
    /// 새 것만 있어 옛 이름 링크를 만들었다.
    Linked { old: PathBuf, new: PathBuf },
    /// 둘 다 진짜라 손대지 않았다.
    BothReal { old: PathBuf, new: PathBuf },
    /// 옮기거나 링크를 만들지 못했다(맞바꿈을 받지 않는 파일 시스템 등). 옛 경로로 계속 돈다.
    Failed {
        old: PathBuf,
        new: PathBuf,
        error: String,
    },
}

impl Note {
    /// 운영자가 볼 일(경고)인지
    pub fn is_warning(&self) -> bool {
        matches!(self, Note::BothReal { .. } | Note::Failed { .. })
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
            Note::BothReal { old, new } => write!(
                f,
                "both {} and {} exist; left them as they are (merge them by hand and make {} a link)",
                old.display(),
                new.display(),
                old.display()
            ),
            Note::Failed { old, new, error } => write!(
                f,
                "cannot move {} to {} ({error}); still using {}",
                old.display(),
                new.display(),
                old.display()
            ),
        }
    }
}

/// 뿌리 `root` 아래의 배치를 맞춘다.
pub fn migrate(root: &Path) -> Vec<Note> {
    migrate_with(root, exchange)
}

/// [`migrate`]와 같되 맞바꿈을 받는다(시험이 맞바꿈 실패를 흉내 낸다).
fn migrate_with(root: &Path, swap: impl Fn(&Path, &Path) -> io::Result<()>) -> Vec<Note> {
    let mut notes = Vec::new();
    for (old, new) in DIRS {
        notes.extend(align(&root.join(old), &root.join(new), true, &swap));
    }
    let (old, new) = CONFIG_FILES;
    notes.extend(align(&root.join(old), &root.join(new), false, &swap));
    notes
}

/// 짝 하나를 맞춘다. `dir`이면 디렉터리 짝, 아니면 파일 짝이다.
fn align(
    old: &Path,
    new: &Path,
    dir: bool,
    swap: &impl Fn(&Path, &Path) -> io::Result<()>,
) -> Option<Note> {
    let real = |p: &Path| {
        std::fs::symlink_metadata(p)
            .ok()
            .filter(|m| if dir { m.is_dir() } else { m.is_file() })
            .is_some()
    };
    let present = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    let note = |error: io::Error| Note::Failed {
        old: old.to_path_buf(),
        new: new.to_path_buf(),
        error: error.to_string(),
    };
    // 링크 대상은 같은 디렉터리 안의 이름(상대 링크)이다.
    let target = new.file_name()?;
    if real(old) && !present(new) {
        if let Err(e) = std::os::unix::fs::symlink(target, new) {
            return Some(note(e));
        }
        if let Err(e) = swap(old, new) {
            let _ = std::fs::remove_file(new);
            return Some(note(e));
        }
        return Some(Note::Moved {
            old: old.to_path_buf(),
            new: new.to_path_buf(),
        });
    }
    if real(new) && !present(old) {
        // 새로 설치: 옛 이름 링크(저장된 옛 경로·옛 스크립트가 풀리게). 옛 이름의 부모가 없으면(예: 설정 파일 짝에서
        // 새 디렉터리가 진짜가 아님) 만들지 않는다.
        if old.parent().is_some_and(|p| !p.is_dir()) {
            return None;
        }
        return Some(match std::os::unix::fs::symlink(target, old) {
            Ok(()) => Note::Linked {
                old: old.to_path_buf(),
                new: new.to_path_buf(),
            },
            Err(e) => note(e),
        });
    }
    if real(old) && real(new) {
        return Some(Note::BothReal {
            old: old.to_path_buf(),
            new: new.to_path_buf(),
        });
    }
    None
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

/// 읽을 설정 파일과 안내(경고). `--config`가 있으면 그것, 없으면 새 경로, 그것이 없고 옛 경로가 있으면 옛 경로.
pub fn config_path(given: Option<&Path>, root: &Path) -> (PathBuf, Option<String>) {
    if let Some(given) = given {
        return (given.to_path_buf(), None);
    }
    let new = root.join(CONFIG.trim_start_matches('/'));
    let old = root.join(LEGACY_CONFIG.trim_start_matches('/'));
    match (std::fs::metadata(&new), std::fs::metadata(&old)) {
        (Ok(n), Ok(o)) => {
            use std::os::unix::fs::MetadataExt;
            // 맞바꾼 뒤에는 옛 것이 새 것으로의 링크다(같은 파일). 진짜 파일이 둘일 때만 경고한다.
            let two = (n.dev(), n.ino()) != (o.dev(), o.ino());
            let warning = two.then(|| {
                format!(
                    "both {} and {} exist; using {} (make {} a link to it)",
                    new.display(),
                    old.display(),
                    new.display(),
                    old.display()
                )
            });
            (new, warning)
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
    use std::os::unix::fs::MetadataExt;

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
    fn a_failed_swap_removes_the_self_link_and_keeps_the_old_layout() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        old_layout(root);
        let notes = migrate_with(root, |_, _| Err(io::Error::from_raw_os_error(libc::EINVAL)));
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert!(notes.iter().all(|n| matches!(n, Note::Failed { .. })));
        for (old, new) in DIRS {
            assert!(is_real_dir(&root.join(old)), "{old}");
            assert!(std::fs::symlink_metadata(root.join(new)).is_err(), "{new}");
        }
        assert!(notes[0].to_string().contains("still using"));
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
