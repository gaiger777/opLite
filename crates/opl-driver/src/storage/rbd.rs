//! Ceph RBD 풀: Ceph 풀(네임스페이스) 안에 볼륨마다 raw 이미지 `opl-{id}`를 둔다. QEMU가 `rbd`
//! 블록 드라이버로 이미지를 직접 연다(커널 매핑 없음).

use opl_types::runtime::{ImageFormat, RbdTarget};
use uuid::Uuid;

use crate::error::DriverError;

/// 풀에 사용자가 없을 때 쓰는 Ceph 클라이언트 id
const DEFAULT_USER: &str = "admin";

/// 볼륨의 이미지 이름
fn volume_image(id: Uuid) -> String {
    format!("opl-{id}")
}

/// QEMU가 여는 볼륨의 RBD 이미지. 사용자가 없으면 `admin`이다.
pub(super) fn volume_target(
    pool: &str,
    namespace: Option<&String>,
    user: Option<&String>,
    conf: Option<&String>,
    id: Uuid,
) -> RbdTarget {
    RbdTarget {
        pool: pool.to_string(),
        namespace: namespace.cloned(),
        image: volume_image(id),
        user: user.map_or(DEFAULT_USER, String::as_str).to_string(),
        conf: conf.cloned(),
    }
}

/// RBD 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "rbd pools hold raw volumes".into(),
        )),
    }
}
