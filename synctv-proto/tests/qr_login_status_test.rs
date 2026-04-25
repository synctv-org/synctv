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

/// Test that enum can be created from i32 values
#[test]
fn test_qr_login_status_from_i32() {
    // Prost uses try_from for i32 -> enum conversion
    assert_eq!(
        QrLoginStatus::try_from(0).expect("0 must map to Unspecified"),
        QrLoginStatus::Unspecified
    );
    assert_eq!(
        QrLoginStatus::try_from(1).expect("1 must map to Expired"),
        QrLoginStatus::Expired
    );
    assert_eq!(
        QrLoginStatus::try_from(2).expect("2 must map to NotScanned"),
        QrLoginStatus::NotScanned
    );
    assert_eq!(
        QrLoginStatus::try_from(3).expect("3 must map to Scanned"),
        QrLoginStatus::Scanned
    );
    assert_eq!(
        QrLoginStatus::try_from(4).expect("4 must map to Success"),
        QrLoginStatus::Success
    );

    // Unknown values should return error (prost strict mode)
    assert!(QrLoginStatus::try_from(5).is_err());
    assert!(QrLoginStatus::try_from(-1).is_err());
}

/// Test string name conversion
#[test]
fn test_qr_login_status_str_name() {
    assert_eq!(
        QrLoginStatus::Unspecified.as_str_name(),
        "QR_LOGIN_STATUS_UNSPECIFIED"
    );
    assert_eq!(
        QrLoginStatus::Expired.as_str_name(),
        "QR_LOGIN_STATUS_EXPIRED"
    );
    assert_eq!(
        QrLoginStatus::NotScanned.as_str_name(),
        "QR_LOGIN_STATUS_NOT_SCANNED"
    );
    assert_eq!(
        QrLoginStatus::Scanned.as_str_name(),
        "QR_LOGIN_STATUS_SCANNED"
    );
    assert_eq!(
        QrLoginStatus::Success.as_str_name(),
        "QR_LOGIN_STATUS_SUCCESS"
    );
}

/// Test from_str_name conversion
#[test]
fn test_qr_login_status_from_str_name() {
    assert_eq!(
        QrLoginStatus::from_str_name("QR_LOGIN_STATUS_UNSPECIFIED"),
        Some(QrLoginStatus::Unspecified)
    );
    assert_eq!(
        QrLoginStatus::from_str_name("QR_LOGIN_STATUS_EXPIRED"),
        Some(QrLoginStatus::Expired)
    );
    assert_eq!(
        QrLoginStatus::from_str_name("QR_LOGIN_STATUS_NOT_SCANNED"),
        Some(QrLoginStatus::NotScanned)
    );
    assert_eq!(
        QrLoginStatus::from_str_name("QR_LOGIN_STATUS_SCANNED"),
        Some(QrLoginStatus::Scanned)
    );
    assert_eq!(
        QrLoginStatus::from_str_name("QR_LOGIN_STATUS_SUCCESS"),
        Some(QrLoginStatus::Success)
    );
    assert_eq!(QrLoginStatus::from_str_name("INVALID"), None);
}

/// Test that `QrStatusResponse` stores `QrLoginStatus` numeric values.
#[test]
fn test_qr_status_response_uses_enum() {
    use synctv_proto::providers::bilibili::QrStatusResponse;

    let response = QrStatusResponse {
        status: QrLoginStatus::Success as i32,
    };

    // Verify the status field accepts enum values
    assert_eq!(response.status, QrLoginStatus::Success as i32);

    let expired_response = QrStatusResponse {
        status: QrLoginStatus::Expired as i32,
    };
    assert_eq!(expired_response.status, QrLoginStatus::Expired as i32);
}
