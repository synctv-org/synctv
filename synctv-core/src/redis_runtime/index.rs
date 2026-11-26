//! Conditional cleanup of secondary Redis indexes after a snapshot read.

use std::sync::LazyLock;

use redis::{RedisResult, Script, aio::ConnectionLike};

pub enum IndexKind {
    Set,
    SortedSet,
}

/// A member rejected by the reader and the metadata it saw at the time.
pub struct StaleMember {
    pub member: String,
    pub metadata_key: String,
    pub observed_value: Option<Vec<u8>>,
}

impl StaleMember {
    /// A directory or namespace whose members are themselves Redis keys.
    #[must_use]
    pub fn missing_indexed_key(key: String) -> Self {
        Self {
            metadata_key: key.clone(),
            member: key,
            observed_value: None,
        }
    }
}

// Redis runs a script without interleaving other clients. Bound the number of
// comparisons per invocation so large indexes yield between batches.
const PRUNE_BATCH_SIZE: usize = 128;
static PRUNE: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r"
        local removed = 0
        local arg = 2
        for i = 2, #KEYS do
            local member, kind = ARGV[arg], ARGV[arg + 1]
            arg = arg + 2
            local stale
            if kind == 'missing' then
                stale = redis.call('EXISTS', KEYS[i]) == 0
            else
                -- MGET also treats a change to a non-string type as a mismatch.
                stale = redis.call('MGET', KEYS[i])[1] == ARGV[arg]
                arg = arg + 1
            end
            if stale then
                removed = removed + redis.call(ARGV[1], KEYS[1], member)
            end
        end
        return removed
        ",
    )
});

/// Remove a member only while its backing key still matches the observation.
/// Returns the number actually removed, including across batch boundaries.
pub async fn prune_stale_members(
    conn: &mut impl ConnectionLike,
    kind: IndexKind,
    index_key: &str,
    candidates: &[StaleMember],
) -> RedisResult<usize> {
    let command = match kind {
        IndexKind::Set => "SREM",
        IndexKind::SortedSet => "ZREM",
    };
    let mut removed = 0;
    for chunk in candidates.chunks(PRUNE_BATCH_SIZE) {
        let mut script = PRUNE.prepare_invoke();
        script.key(index_key).arg(command);
        for candidate in chunk {
            script.key(&candidate.metadata_key).arg(&candidate.member);
            if let Some(value) = &candidate.observed_value {
                script.arg("value").arg(value);
            } else {
                script.arg("missing");
            }
        }
        removed += script.invoke_async::<usize>(conn).await?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::AsyncCommands;
    use synctv_core_testing::{start_redis, test_redis_key_prefix};

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn prune_checks_observed_metadata_across_batches() -> RedisResult<()> {
        let (_redis, mut conn) = start_redis().await;
        let prefix = test_redis_key_prefix("prune-observations");
        let index = format!("{prefix}index");
        let mut candidates: Vec<_> = (0..=PRUNE_BATCH_SIZE)
            .map(|i| StaleMember::missing_indexed_key(format!("{prefix}missing:{i}")))
            .collect();
        let recovered = format!("{prefix}recovered");
        let changed = format!("{prefix}changed");
        let retyped = format!("{prefix}retyped");
        let empty = format!("{prefix}empty");
        let disappeared = format!("{prefix}disappeared");
        candidates.push(StaleMember::missing_indexed_key(recovered.clone()));
        for key in [&changed, &retyped, &empty, &disappeared] {
            candidates.push(StaleMember {
                member: key.clone(),
                metadata_key: key.clone(),
                observed_value: Some(Vec::new()),
            });
        }
        let members: Vec<_> = candidates.iter().map(|entry| &entry.member).collect();
        redis::pipe()
            .sadd(&index, &members)
            .ignore()
            .hset(&recovered, "field", "active")
            .ignore()
            .hset(&retyped, "field", "active")
            .ignore()
            .set(&changed, "renewed")
            .ignore()
            .set(&empty, "")
            .ignore()
            .query_async::<()>(&mut conn)
            .await?;

        let removed = prune_stale_members(&mut conn, IndexKind::Set, &index, &candidates).await?;
        assert_eq!(removed, PRUNE_BATCH_SIZE + 2);
        let remaining: std::collections::HashSet<String> = conn.smembers(&index).await?;
        assert_eq!(remaining, [recovered, changed, disappeared, retyped].into());
        Ok(())
    }
}
