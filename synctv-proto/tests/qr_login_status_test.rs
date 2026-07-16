//! Test QRLoginStatus enum values stay aligned with synctv-media-providers.
//!
//! This test verifies that the QRLoginStatus enum values match the
//! QRCodeStatus enum values in synctv-media-providers for the shared wire contract.

use synctv_proto::providers::bilibili::QrLoginStatus;

/// Test that QRLoginStatus enum values match QRCodeStatus values in synctv-media-providers
///
/// The values must be:
/// - 0: Unspecified/Unknown
/// - 1: Expired
/// - 2: NotScanned/Notscanned
/// - 3: Scanned
/// - 4: Success
#[test]
fn test_qr_login_status_values() {
    // Verify enum values match synctv-media-providers::grpc::bilibili::QrCodeStatus
    assert_eq!(QrLoginStatus::Unspecified as i32, 0);
    assert_eq!(QrLoginStatus::Expired as i32, 1);
    assert_eq!(QrLoginStatus::NotScanned as i32, 2);
    assert_eq!(QrLoginStatus::Scanned as i32, 3);
    assert_eq!(QrLoginStatus::Success as i32, 4);
}
