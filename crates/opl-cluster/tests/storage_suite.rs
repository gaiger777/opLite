//! openraft가 제공하는 저장소 적합성 테스트 모음으로 redb 저장소를 검증한다.

use openraft::StorageError;
use openraft::testing::{StoreBuilder, Suite};
use opl_cluster::store::Store;
use opl_cluster::types::{NodeId, TypeConfig};
use opl_cluster::{LogStore, StateMachine};
use tempfile::TempDir;

struct RedbBuilder;

impl StoreBuilder<TypeConfig, LogStore, StateMachine, TempDir> for RedbBuilder {
    async fn build(&self) -> Result<(TempDir, LogStore, StateMachine), StorageError<NodeId>> {
        let dir = TempDir::new().unwrap();
        let store = Store::open(&dir.path().join("opl.redb")).unwrap();
        Ok((dir, store.log_store(), store.state_machine()))
    }
}

#[test]
fn redb_store_passes_openraft_suite() {
    Suite::test_all(RedbBuilder).unwrap();
}
