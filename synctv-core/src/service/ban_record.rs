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

    pub async fn list(&self, query: &BanRecordListQuery) -> Result<BanRecordPage> {
        self.repository.list(query).await
    }
}
