//! VM을 켜기 직전의 방화벽 확인. 규칙을 넣는 일은 opld의 방화벽 에이전트가 [`FirewallGate`]로 넣는다
//! (reconciler는 nftables를 모른다).

use async_trait::async_trait;

/// 보안 그룹을 건 NIC가 있는 VM을 켜기(마이그레이션 받기 포함) 직전에 부른다. 지금 클러스터 상태로
/// 이 노드의 방화벽 규칙을 넣고, 넣지 못하면 그 이유를 돌려준다(VM을 켜지 않는다).
#[async_trait]
pub trait FirewallGate: Send + Sync {
    async fn sync(&self) -> Result<(), String>;
}
