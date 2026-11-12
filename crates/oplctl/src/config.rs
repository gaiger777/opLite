//! 로그인 정보(서버 주소, API 토큰)를 담는 파일. 기본 위치는 `~/.config/opl/oplctl.json`이다.

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

pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
    Path::new(&home).join(".config/opl/oplctl.json")
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
