use synctv_proto::common::{RoomMemberRole, UserRole};

#[test]
fn user_role_wire_values_match_database_authority_order() {
    assert_eq!(UserRole::Root as i32, 1);
    assert_eq!(UserRole::Admin as i32, 2);
    assert_eq!(UserRole::User as i32, 3);
}

#[test]
fn room_member_role_wire_values_match_database_authority_order() {
    assert_eq!(RoomMemberRole::Creator as i32, 1);
    assert_eq!(RoomMemberRole::Admin as i32, 2);
    assert_eq!(RoomMemberRole::Member as i32, 3);
    assert_eq!(RoomMemberRole::Guest as i32, 4);
}
