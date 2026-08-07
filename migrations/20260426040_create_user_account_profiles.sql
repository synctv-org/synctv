CREATE OR REPLACE VIEW user_account_profiles AS
SELECT u.id,
       u.username,
       u.signup_method,
       u.role,
       u.avatar_file_reference_id,
       CASE
           WHEN active_ban.user_id IS NULL THEN 1::SMALLINT
           ELSE 2::SMALLINT
       END AS status,
       (active_ban.user_id IS NOT NULL) AS is_banned,
       active_ban.starts_at AS banned_at,
       active_ban.banned_by,
       active_ban.reason AS banned_reason,
       u.created_at,
       u.updated_at,
       u.version,
       u.deleted_at
FROM users u
LEFT JOIN LATERAL (
    SELECT ub.user_id,
           ub.starts_at,
           ub.banned_by,
           ub.reason
    FROM user_bans ub
    WHERE ub.user_id = u.id
      AND ub.revoked_at IS NULL
      AND (ub.ends_at IS NULL OR ub.ends_at > CURRENT_TIMESTAMP)
    ORDER BY ub.starts_at DESC
    LIMIT 1
) active_ban ON TRUE;
