use futures_util::StreamExt;
use synctv_proto::client::{
    login_with_direct_password_request, CreateWebSocketTicketRequest,
    LoginWithDirectPasswordRequest,
};
use tokio_tungstenite::connect_async;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::var("SYNCTV_E2E_HTTP").expect("SYNCTV_E2E_HTTP is required");
    let room_id = std::env::var("SYNCTV_E2E_ROOM").expect("SYNCTV_E2E_ROOM is required");
    let username = std::env::var("SYNCTV_E2E_USERNAME").expect("SYNCTV_E2E_USERNAME is required");
    let password = std::env::var("SYNCTV_E2E_PASSWORD").expect("SYNCTV_E2E_PASSWORD is required");

    let client = reqwest::Client::new();
    let login: synctv_proto::client::LoginResponse = client
        .post(format!("{base}/api/auth/direct-password/login"))
        .json(&LoginWithDirectPasswordRequest {
            identifier: Some(login_with_direct_password_request::Identifier::Username(
                username,
            )),
            password,
        })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let ticket: synctv_proto::client::CreateWebSocketTicketResponse = client
        .post(format!("{base}/api/tickets"))
        .bearer_auth(&login.access_token)
        .json(&CreateWebSocketTicketRequest {
            room_id: room_id.clone(),
        })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let ws_url = format!(
        "{}/ws/rooms/{}?ticket={}",
        base.replacen("http://", "ws://", 1),
        room_id,
        ticket.ticket,
    );
    let (mut ws, response) = connect_async(ws_url).await?;
    println!("connected {}", response.status());

    while let Some(message) = ws.next().await {
        match message {
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
            Ok(_) => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}
