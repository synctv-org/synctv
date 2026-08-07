use sqlx::PgPool;

use crate::repository::BanRecordRepository;
use crate::Result;

pub use crate::repository::{BanRecordListQuery, BanRecordPage, BanRecordRow, BanRecordTargetType};

/// Transport-neutral service for ban record read models.
#[derive(Clone)]
pub struct BanRecordService {
    repository: BanRecordRepository,
}

impl BanRecordService {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            repository: BanRecordRepository::new(pool),
        }
    }

    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            repository: BanRecordRepository::new_with_read_pool(pool, read_pool),
        }
    }

    pub async fn list(&self, query: &BanRecordListQuery) -> Result<BanRecordPage> {
        self.repository.list(query).await
    }
}
