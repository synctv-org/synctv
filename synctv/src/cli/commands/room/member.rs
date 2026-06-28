use super::super::prelude::*;

#[derive(Debug, Args)]
pub struct RoomMemberCommand {
    #[command(subcommand)]
    pub command: RoomMemberSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum RoomMemberSubcommand {
    /// List room members
    List(RoomMembersArgs),
    /// Add an existing user as an active room member using a management override
    Add(RoomMemberAddArgs),
    /// Update a room member's role or permission bitmasks
    SetPermissions(RoomMemberSetPermissionsArgs),
    /// Kick a room member
    Kick(RoomMemberKickArgs),
}

#[derive(Debug, Args)]
pub struct RoomMembersArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[arg(value_name = "ROOM_ID", allow_hyphen_values = true)]
    pub room_id: String,

    #[arg(long, default_value_t = 1)]
    pub page: i32,

    #[arg(long, default_value_t = 50)]
    pub page_size: i32,

    #[arg(long)]
    pub search: Option<String>,

    #[arg(long, value_enum)]
    pub role: Option<CliRoomMemberRole>,

    #[arg(long, value_enum)]
    pub sort_by: Option<CliRoomMemberSortField>,

    #[arg(long = "sort-dir", value_enum, default_value_t = CliSortDirection::Asc)]
    pub sort_dir: CliSortDirection,
}

impl RoomMembersArgs {
    pub(in crate::cli) fn resolved_room_id(&self) -> &str {
        &self.room_id
    }
}

#[derive(Debug, Args)]
pub struct RoomMemberSetPermissionsArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum)]
    pub role: Option<CliRoomMemberRole>,

    #[arg(
        long,
        value_parser = parse_member_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub added_permissions: Option<PermissionOverrideBits>,

    #[arg(
        long,
        value_parser = parse_member_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub removed_permissions: Option<PermissionOverrideBits>,

    #[arg(
        long,
        value_parser = parse_admin_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Admin permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub admin_added_permissions: Option<PermissionOverrideBits>,

    #[arg(
        long,
        value_parser = parse_admin_permission_bits_arg,
        value_name = "BITS|NAMES",
        help = "Admin permission override as a u64 bitmask, comma-separated names, or JSON array of names"
    )]
    pub admin_removed_permissions: Option<PermissionOverrideBits>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermissionOverrideBits(u64);

impl From<PermissionOverrideBits> for u64 {
    fn from(value: PermissionOverrideBits) -> Self {
        value.0
    }
}

fn parse_member_permission_bits_arg(
    raw: &str,
) -> std::result::Result<PermissionOverrideBits, String> {
    parse_permission_bits_from_named_set(raw, CLI_MEMBER_NAMED_PERMISSIONS)
}

fn parse_admin_permission_bits_arg(
    raw: &str,
) -> std::result::Result<PermissionOverrideBits, String> {
    parse_permission_bits_from_named_set(raw, CLI_ADMIN_NAMED_PERMISSIONS)
}

fn parse_permission_bits_from_named_set(
    raw: &str,
    named_permissions: &[(&str, u64)],
) -> std::result::Result<PermissionOverrideBits, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("permission override must not be empty".to_string());
    }

    if let Ok(bits) = trimmed.parse::<u64>() {
        reject_unknown_permission_bits(bits, named_permissions)?;
        return Ok(PermissionOverrideBits(bits));
    }

    let names = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<String>>(trimmed).map_err(|error| {
            format!("permission JSON array must contain permission names: {error}")
        })?
    } else {
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };

    if names.is_empty() {
        return Err("permission override must include at least one permission name".to_string());
    }

    let mut bits = 0_u64;
    for name in names {
        let canonical = name.replace('-', "_").to_ascii_lowercase();
        let Some((_, bit)) = named_permissions
            .iter()
            .find(|(permission_name, _)| *permission_name == canonical)
        else {
            let allowed = named_permissions
                .iter()
                .map(|(permission_name, _)| *permission_name)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "unknown permission name '{name}'. Allowed: {allowed}"
            ));
        };
        bits |= *bit;
    }

    reject_unknown_permission_bits(bits, named_permissions)?;

    Ok(PermissionOverrideBits(bits))
}

fn reject_unknown_permission_bits(
    bits: u64,
    named_permissions: &[(&str, u64)],
) -> std::result::Result<(), String> {
    let allowed_mask = named_permissions
        .iter()
        .fold(0_u64, |mask, (_, bit)| mask | *bit);
    let invalid = bits & !allowed_mask;
    if invalid == 0 {
        return Ok(());
    }

    let allowed = named_permissions
        .iter()
        .map(|(permission_name, _)| *permission_name)
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "permission override contains bits outside this role bitspace (unknown bits 0x{invalid:x}). Allowed: {allowed}"
    ))
}

#[derive(Debug, Args)]
pub struct RoomMemberAddArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_enum, default_value_t = CliRoomMemberRole::Member)]
    pub role: CliRoomMemberRole,

    #[arg(long)]
    pub notify: bool,
}

#[derive(Debug, Args)]
pub struct RoomMemberKickArgs {
    #[command(flatten)]
    pub room: RoomScopedRemoteArgs,

    #[command(flatten)]
    pub user: UserRefArgs,

    #[arg(long, value_name = "SECONDS")]
    pub kick_cooldown_seconds: i64,
}
