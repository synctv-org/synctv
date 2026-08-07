use super::*;

pub(super) async fn execute_review(review_command: ReviewCommand) -> Result<()> {
    match review_command.command {
        ReviewSubcommand::UserRegistration(command) => match command.command {
            ReviewUserRegistrationSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list user registration reviews",
                    list_user_registration_reviews,
                    management_proto::ListUserRegistrationReviewsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        status: args.status.to_proto(),
                        search: args.search.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewUserRegistrationSubcommand::Approve(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "approve user registration review",
                    approve_user_registration_review,
                    management_proto::ApproveUserRegistrationReviewRequest {
                        request_id: args.request_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewUserRegistrationSubcommand::Reject(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reject user registration review",
                    reject_user_registration_review,
                    management_proto::RejectUserRegistrationReviewRequest {
                        request_id: args.request_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        ReviewSubcommand::RoomCreation(command) => match command.command {
            ReviewRoomCreationSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room creation reviews",
                    list_room_creation_reviews,
                    management_proto::ListRoomCreationReviewsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        status: args.status.to_proto(),
                        requested_by: args.requested_by.unwrap_or_default(),
                        search: args.search.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomCreationSubcommand::Approve(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "approve room creation review",
                    approve_room_creation_review,
                    management_proto::ApproveRoomCreationReviewRequest {
                        request_id: args.request_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomCreationSubcommand::Reject(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reject room creation review",
                    reject_room_creation_review,
                    management_proto::RejectRoomCreationReviewRequest {
                        request_id: args.request_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
        ReviewSubcommand::RoomJoin(command) => match command.command {
            ReviewRoomJoinSubcommand::List(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "list room join reviews",
                    list_room_join_reviews,
                    management_proto::ListRoomJoinReviewsRequest {
                        page: args.page,
                        page_size: args.page_size,
                        status: args.status.to_proto(),
                        room_id: args.room_id.unwrap_or_default(),
                        user_id: args.user_id.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomJoinSubcommand::Approve(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "approve room join review",
                    approve_room_join_review,
                    management_proto::ApproveRoomJoinReviewRequest {
                        request_id: args.request_id,
                    }
                )?;
                args.remote.print_output(&response)
            }
            ReviewRoomJoinSubcommand::Reject(args) => {
                let session = connect_remote_access(&args.remote).await?;
                let response = management_unary_call!(
                    session,
                    "reject room join review",
                    reject_room_join_review,
                    management_proto::RejectRoomJoinReviewRequest {
                        request_id: args.request_id,
                        reason: args.reason.unwrap_or_default(),
                    }
                )?;
                args.remote.print_output(&response)
            }
        },
    }
}
