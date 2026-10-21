//! 로그인 정보(서버 주소, API 토큰)를 담는 파일. 기본 위치는 `~/.config/opl/oplctl.json`이다.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliConfig {
    pub server: Option<String>,
    pub token: Option<String>,
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
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        config.save(&path).unwrap();
        assert_eq!(CliConfig::load(&path).unwrap(), config);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
