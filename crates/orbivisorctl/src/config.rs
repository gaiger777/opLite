//! 로그인 정보(서버 주소, API 토큰)를 담는 파일. 기본 위치는 `~/.config/orbivisor/orbivisorctl.json`이다. 그 파일이
//! 없으면 이름 바꾸기 앞의 `~/.config/opl/oplctl.json`을 읽고, 저장은 새 파일에 한다. 옛 파일은 남기되 `login`·`logout`이
//! 그 안의 토큰을 지운다(로그아웃한 토큰이 옛 파일에 평문으로 남지 않게 — 리뷰 1 M3).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliConfig {
    pub server: Option<String>,
    pub token: Option<String>,
    /// 서버의 클러스터 CA 인증서(PEM). HTTPS 서버는 이 CA로만 확인한다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_pem: Option<String>,
    /// 서버 인증서를 클러스터 CA 대신 OS 신뢰 저장소로 확인한다(`login --system-ca`, 관리자 인증서
    /// `[api] tls_cert`를 둔 노드). 이때 `ca_pem`은 없다.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub system_ca: bool,
}

/// 기본 위치의 (읽을 파일, 저장할 파일)
pub fn default_paths() -> (PathBuf, PathBuf) {
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    paths_in(Path::new(&home))
}

/// 기본 위치의 옛 파일에서 토큰을 지운다(옛 파일이 없거나 토큰이 없으면 그대로).
pub fn clear_legacy_token() -> anyhow::Result<()> {
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    clear_legacy_token_in(Path::new(&home))
}

fn clear_legacy_token_in(home: &Path) -> anyhow::Result<()> {
    let old = home.join(LEGACY_FILE);
    let mut config = CliConfig::load(&old)?;
    if config.token.take().is_some() {
        config.save(&old)?;
    }
    Ok(())
}

/// 이름 바꾸기 앞의 로그인 파일(홈에서)
const LEGACY_FILE: &str = ".config/opl/oplctl.json";

/// 홈 디렉터리 `home`에서의 (읽을 파일, 저장할 파일). 새 파일이 없고 옛 파일이 있으면 옛 파일을 읽는다.
fn paths_in(home: &Path) -> (PathBuf, PathBuf) {
    let new = home.join(".config/orbivisor/orbivisorctl.json");
    let old = home.join(LEGACY_FILE);
    let read = if !new.exists() && old.exists() {
        old
    } else {
        new.clone()
    };
    (read, new)
}

impl CliConfig {
    /// 파일이 없으면 빈 설정이다.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// 토큰이 들어 있으므로 주인만 읽을 수 있게(0600) 쓴다.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 임시 파일에 다 쓴 뒤 바꿔 넣어, 도중에 죽어도 이전 설정이 그대로 남게 한다.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // `mode`는 새 파일에만 쓰인다. 남아 있던 임시 파일도 주인만 읽게 한다.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec_pretty(self)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_old_file_until_the_new_one_exists() {
        let home = tempfile::tempdir().unwrap();
        let new = home.path().join(".config/orbivisor/orbivisorctl.json");
        let old = home.path().join(".config/opl/oplctl.json");
        assert_eq!(paths_in(home.path()), (new.clone(), new.clone()));
        let config = CliConfig {
            server: Some("https://a:8006".into()),
            ..CliConfig::default()
        };
        config.save(&old).unwrap();
        let (read, write) = paths_in(home.path());
        assert_eq!((&read, &write), (&old, &new));
        assert_eq!(CliConfig::load(&read).unwrap(), config);
        // 다음 저장은 새 파일에. 옛 파일은 그대로 남고 그 뒤로는 새 파일을 읽는다.
        CliConfig::default().save(&write).unwrap();
        assert_eq!(paths_in(home.path()), (new.clone(), new));
        assert_eq!(CliConfig::load(&old).unwrap(), config);
    }

    #[test]
    fn clears_the_token_in_the_old_file() {
        let home = tempfile::tempdir().unwrap();
        let old = home.path().join(LEGACY_FILE);
        // 옛 파일이 없으면 만들지 않는다.
        clear_legacy_token_in(home.path()).unwrap();
        assert!(!old.exists());
        let config = CliConfig {
            server: Some("https://a:8006".into()),
            token: Some("opl_x_y".into()),
            ..CliConfig::default()
        };
        config.save(&old).unwrap();
        clear_legacy_token_in(home.path()).unwrap();
        assert_eq!(
            CliConfig::load(&old).unwrap(),
            CliConfig {
                token: None,
                ..config
            }
        );
    }

    #[test]
    fn saves_privately_and_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/oplctl.json");
        assert_eq!(CliConfig::load(&path).unwrap(), CliConfig::default());
        let config = CliConfig {
            server: Some("http://127.0.0.1:8006".into()),
            token: Some("opl_x_y".into()),
            ca_pem: Some("CA".into()),
            system_ca: false,
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        config.save(&path).unwrap();
        assert_eq!(CliConfig::load(&path).unwrap(), config);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// `system_ca`는 참일 때만 적고, 없는 옛 파일은 거짓으로 읽는다.
    #[test]
    fn system_ca_is_saved_only_when_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oplctl.json");
        let config = CliConfig {
            server: Some("https://api.example.com".into()),
            token: Some("opl_x_y".into()),
            ca_pem: None,
            system_ca: true,
        };
        config.save(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["system_ca"], true);
        assert_eq!(CliConfig::load(&path).unwrap(), config);
        CliConfig {
            system_ca: false,
            ..config
        }
        .save(&path)
        .unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("system_ca")
        );
        assert!(!CliConfig::load(&path).unwrap().system_ca);
    }
}
