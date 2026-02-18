-- 审计日志分区自动化管理
--
-- 功能：
-- 1. 自动化分区管理函数（包含索引创建）
-- 2. 创建未来6个月的分区
-- 3. 分区健康检查函数
--
-- 说明：所有索引创建由函数自动处理，无需手动指定日期

-- ============================================================================
-- 第一部分：自动化分区管理函数
-- ============================================================================

-- 函数1: 创建单个分区及其所有索引
--
-- 此函数会自动为新分区创建所有必要的复合索引
-- 使用场景：应用启动时调用，或定时任务调用
CREATE OR REPLACE FUNCTION create_audit_logs_partition(
    partition_date DATE DEFAULT CURRENT_DATE
) RETURNS JSON AS $$
DECLARE
    partition_name TEXT;
    start_date TEXT;
    end_date TEXT;
    index_count INTEGER := 0;
BEGIN
    -- 生成分区名称：audit_logs_YYYY_MM
    partition_name := 'audit_logs_' || TO_CHAR(partition_date, 'YYYY_MM');

    -- 计算分区范围（整月）
    start_date := TO_CHAR(partition_date, 'YYYY-MM') || '-01';
    end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM') || '-01';

    -- 创建分区（如果不存在）
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF audit_logs
         FOR VALUES FROM (%L) TO (%L)',
        partition_name, start_date, end_date
    );

    RAISE NOTICE 'Created partition: %', partition_name;

    -- 自动创建所有必要的复合索引（幂等，重复执行不会报错）

    -- 复合索引1: 操作者审计历史 (actor_id, created_at DESC)
    -- 优化查询：SELECT * FROM audit_logs WHERE actor_id = ? AND created_at > ? ORDER BY created_at DESC
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(actor_id, created_at DESC) WHERE actor_id IS NOT NULL',
        partition_name || '_idx_actor_created', partition_name
    );
    index_count := index_count + 1;

    -- 复合索引2: 操作类型 (action, created_at DESC)
    -- 优化查询：SELECT * FROM audit_logs WHERE action = 'user_banned' ORDER BY created_at DESC
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(action, created_at DESC)',
        partition_name || '_idx_action_created', partition_name
    );
    index_count := index_count + 1;

    -- 复合索引3: 目标对象 (target_type, target_id, created_at DESC)
    -- 优化查询：SELECT * FROM audit_logs WHERE target_type = 'user' AND target_id = ? ORDER BY created_at DESC
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(target_type, target_id, created_at DESC) WHERE target_type IS NOT NULL',
        partition_name || '_idx_target_created', partition_name
    );
    index_count := index_count + 1;

    -- 索引4: IP地址 (用于安全分析)
    -- 优化查询：SELECT * FROM audit_logs WHERE ip_address = ?
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON %I(ip_address) WHERE ip_address IS NOT NULL',
        partition_name || '_idx_ip_address', partition_name
    );
    index_count := index_count + 1;

    -- 返回结果
    RETURN json_build_object(
        'partition_name', partition_name,
        'start_date', start_date,
        'end_date', end_date,
        'indexes_created', index_count,
        'status', 'success'
    );
END;
$$ LANGUAGE plpgsql;

-- 函数2: 批量创建未来分区
--
-- 自动创建未来N个月的分区，每个分区都会自动包含所有索引
CREATE OR REPLACE FUNCTION create_audit_logs_partitions(
    months_ahead INTEGER DEFAULT 6
) RETURNS JSON AS $$
DECLARE
    i INTEGER;
    partition_date DATE;
    result JSON;
    partitions JSONB := '[]'::JSONB;
    success_count INTEGER := 0;
BEGIN
    -- 从当前月开始创建（与 create_chat_message_partitions 行为一致）
    partition_date := DATE_TRUNC('month', CURRENT_DATE);

    FOR i IN 1..months_ahead LOOP
        result := create_audit_logs_partition(partition_date);
        partitions := partitions || result::JSONB;
        success_count := success_count + 1;
        partition_date := partition_date + INTERVAL '1 month';
    END LOOP;

    RAISE NOTICE 'Created % audit log partitions', months_ahead;

    RETURN json_build_object(
        'status', 'completed',
        'total_requested', months_ahead,
        'success_count', success_count,
        'partitions', partitions
    );
END;
$$ LANGUAGE plpgsql;

-- 函数3: 为现有分区补充索引
--
-- 如果某些分区已经存在但缺少索引，此函数会补充创建
CREATE OR REPLACE FUNCTION ensure_existing_partitions_indexes(
    partition_count INTEGER DEFAULT 4
) RETURNS JSON AS $$
DECLARE
    partition_record RECORD;
    index_count INTEGER := 0;
    updated_partitions JSON := '[]'::JSON;
BEGIN
    -- 查找最近N个月的分区
    FOR partition_record IN
        SELECT tablename
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'audit_logs_%'
          AND tablename NOT LIKE '%_idx%'
          AND tablename NOT SIMILAR TO '%(_pkey|_[0-9]+)%'
        ORDER BY tablename DESC
        LIMIT partition_count
    LOOP
        -- 为每个分区调用 create_audit_logs_partition
        -- 因为CREATE INDEX IF NOT EXISTS是幂等的，重复执行不会报错
        DECLARE
            result JSON;
            partition_date DATE;
        BEGIN
            -- 从表名提取日期 (audit_logs_YYYY_MM)
            partition_date := TO_DATE(
                SUBSTRING(partition_record.tablename FROM 13 FOR 7) || '-01',
                'YYYY-MM-DD'
            );

            -- 调用分区创建函数（会自动创建索引）
            result := create_audit_logs_partition(partition_date);
            updated_partitions := updated_partitions || result;
            index_count := index_count + (result->>'indexes_created')::INTEGER;
        EXCEPTION WHEN OTHERS THEN
            RAISE NOTICE 'Failed to ensure indexes for %: %', partition_record.tablename, SQLERRM;
        END;
    END LOOP;

    RETURN json_build_object(
        'status', 'completed',
        'partitions_updated', json_array_length(updated_partitions),
        'total_indexes_created', index_count,
        'partitions', updated_partitions
    );
END;
$$ LANGUAGE plpgsql;

-- 函数4: 删除旧分区
--
-- 删除N个月前的旧分区，释放磁盘空间
CREATE OR REPLACE FUNCTION drop_old_audit_logs_partitions(
    keep_months INTEGER DEFAULT 12
) RETURNS JSON AS $$
DECLARE
    cutoff_date DATE;
    partition_record RECORD;
    dropped JSON := '[]'::JSON;
    drop_count INTEGER := 0;
BEGIN
    -- 计算保留截止日期
    cutoff_date := CURRENT_DATE - (keep_months || ' months')::INTERVAL;

    -- 查找需要删除的分区
    FOR partition_record IN
        SELECT tablename
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'audit_logs_%'
          AND tablename < 'audit_logs_' || TO_CHAR(cutoff_date, 'YYYY_MM')
        ORDER BY tablename
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %I', partition_record.tablename);
        dropped := dropped || json_build_object('partition', partition_record.tablename);
        drop_count := drop_count + 1;

        RAISE NOTICE 'Dropped partition: %', partition_record.tablename;
    END LOOP;

    RETURN json_build_object(
        'status', 'success',
        'dropped_count', drop_count,
        'keep_months', keep_months,
        'dropped_partitions', dropped
    );
END;
$$ LANGUAGE plpgsql;

-- 函数5: 检查分区健康状态
--
-- 检查当前月和未来6个月的分区是否存在
CREATE OR REPLACE FUNCTION check_audit_logs_partitions() RETURNS JSON AS $$
DECLARE
    current_month TEXT;
    expected_months TEXT[] := ARRAY[]::TEXT[];
    missing_partitions JSON := '[]'::JSON;
    partition_record RECORD;
    total_partitions INTEGER := 0;
    total_size BIGINT := 0;
BEGIN
    -- 检查当前月和未来6个月
    FOR i IN 0..6 LOOP
        current_month := 'audit_logs_' || TO_CHAR(CURRENT_DATE + (i || ' months')::INTERVAL, 'YYYY_MM');
        expected_months := array_append(expected_months, current_month);
    END LOOP;

    -- 查找缺失的分区
    FOREACH current_month IN ARRAY expected_months
    LOOP
        IF NOT EXISTS (
            SELECT 1 FROM pg_tables
            WHERE schemaname = 'public' AND tablename = current_month
        ) THEN
            missing_partitions := missing_partitions || json_build_object(
                'partition_name', current_month,
                'status', 'missing'
            );
        END IF;
    END LOOP;

    -- 统计现有分区和大小
    FOR partition_record IN
        SELECT
            tablename,
            pg_total_relation_size(format('%I.%I', schemaname, tablename)) as size
        FROM pg_tables
        WHERE schemaname = 'public'
          AND tablename LIKE 'audit_logs_%'
          AND tablename NOT LIKE '%_idx%'
        ORDER BY tablename DESC
    LOOP
        total_partitions := total_partitions + 1;
        total_size := total_size + partition_record.size;
    END LOOP;

    RETURN json_build_object(
        'status', 'checked',
        'total_partitions', total_partitions,
        'total_size_mb', ROUND(total_size::NUMERIC / 1024 / 1024, 2),
        'total_size_gb', ROUND(total_size::NUMERIC / 1024 / 1024 / 1024, 2),
        'missing_partitions', missing_partitions,
        'missing_count', json_array_length(missing_partitions),
        'health_status', CASE
            WHEN json_array_length(missing_partitions) = 0 THEN 'healthy'
            ELSE 'warning'
        END
    );
END;
$$ LANGUAGE plpgsql;

-- ============================================================================
-- 第二部分：初始化（首次运行时执行）
-- ============================================================================

-- 步骤1: 创建未来6个月的分区
-- 这些分区会自动包含所有索引
-- NOTE: Must run before ensure_existing_partitions_indexes so partitions exist
SELECT create_audit_logs_partitions(6) AS future_partitions;

-- 步骤2: 为现有的4个分区补充索引（幂等，可安全重复执行）
-- 注意：CREATE INDEX IF NOT EXISTS 是幂等的，重复执行不会报错
SELECT ensure_existing_partitions_indexes(4) AS initialization_result;

-- ============================================================================
-- 第三部分：函数注释
-- ============================================================================

COMMENT ON FUNCTION create_audit_logs_partition(DATE) IS
'创建单个审计日志分区及其所有复合索引（幂等）。参数：分区日期（默认当前日期）';

COMMENT ON FUNCTION create_audit_logs_partitions(INTEGER) IS
'批量创建未来N个月的审计日志分区，每个分区自动包含所有索引。参数：月数（默认6）';

COMMENT ON FUNCTION ensure_existing_partitions_indexes(INTEGER) IS
'为现有分区补充创建索引（幂等）。用于初始化或修复缺失的索引。参数：处理的分区数量（默认4）';

COMMENT ON FUNCTION drop_old_audit_logs_partitions(INTEGER) IS
'删除N个月前的旧审计日志分区。参数：保留月数（默认12）';

COMMENT ON FUNCTION check_audit_logs_partitions() IS
'检查审计日志分区的健康状态，返回缺失的分区和统计信息';

-- ============================================================================
-- 使用说明
-- ============================================================================

/*
维护策略：

方案1: 应用启动时自动管理（推荐）
---------------------------------------
在Rust应用启动时调用：

use synctv_core::service::audit_partition_manager::ensure_audit_partitions_on_startup;

#[tokio::main]
async fn main() -> Result<()> {
    let pool = PgPool::connect(&database_url).await?;

    // 自动为现有分区补充索引，并创建未来6个月的分区
    ensure_audit_partitions_on_startup(&pool).await?;

    // ... 启动应用
}

方案2: 定时任务（生产环境推荐）
---------------------------------------
使用cron每月自动创建：

# 每月1号凌晨2点创建下个月的分区
0 2 1 * * psql -U synctv_user -d synctv_db -c "SELECT create_audit_logs_partitions(1);"

# 每周日凌晨3点检查分区健康状态
0 3 * * 0 psql -U synctv_user -d synctv_db -c "SELECT check_audit_logs_partitions();"

# 每月15号凌晨4点清理12个月前的旧数据
0 4 15 * * psql -U synctv_user -d synctv_db -c "SELECT drop_old_audit_logs_partitions(12);"

方案3: 手动管理（开发/调试）
---------------------------------------
-- 检查分区健康状态
SELECT check_audit_logs_partitions();

-- 为现有分区补充索引
SELECT ensure_existing_partitions_indexes(4);

-- 创建未来6个月的分区
SELECT create_audit_logs_partitions(6);

-- 删除旧分区
SELECT drop_old_audit_logs_partitions(12);

查询优化示例：
---------------------------------------
-- 操作者审计历史（使用索引）
SELECT * FROM audit_logs
WHERE actor_id = 'xxx'
  AND created_at > CURRENT_DATE - INTERVAL '30 days'
ORDER BY created_at DESC;

-- 目标对象审计历史（使用索引）
SELECT * FROM audit_logs
WHERE target_type = 'user'
  AND target_id = 'xxx'
  AND created_at > CURRENT_DATE - INTERVAL '7 days'
ORDER BY created_at DESC;

-- 按操作类型查询（使用索引）
SELECT * FROM audit_logs
WHERE action = 'user_banned'
ORDER BY created_at DESC
LIMIT 100;

完整文档: docs/audit_log_management.md
*/
