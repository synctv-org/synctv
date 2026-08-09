use super::*;

pub(super) async fn execute_ban_records_list(
    remote: &RemoteAccessArgs,
    target_type: i32,
    active: Option<bool>,
    user_id: String,
    room_id: String,
    page: i32,
    page_size: i32,
) -> Result<()> {
    let session = connect_remote_access(remote).await?;
    let response = management_unary_call!(
        session,
        "list ban records",
        list_ban_records,
        management_proto::ListBanRecordsRequest {
            page,
            page_size,
            target_type,
            active,
            user_id,
            room_id,
        }
    )?;
    remote.print_output(&response)
}
