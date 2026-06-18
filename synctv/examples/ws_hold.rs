use futures_util::StreamExt;
use serde::Deserialize;
use tokio_tungstenite::connect_async;

#[derive(Debug, Deserialize)]
struct LoginResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct TicketResponse {
    ticket: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = std::env::var("SYNCTV_E2E_HTTP").expect("SYNCTV_E2E_HTTP is required");
    let room_id = std::env::var("SYNCTV_E2E_ROOM").expect("SYNCTV_E2E_ROOM is required");
    let username = std::env::var("SYNCTV_E2E_USERNAME").expect("SYNCTV_E2E_USERNAME is required");
    let password = std::env::var("SYNCTV_E2E_PASSWORD").expect("SYNCTV_E2E_PASSWORD is required");

    let client = reqwest::Client::new();
    let login: LoginResponse = client
        .post(format!("{base}/api/auth/direct-password/login"))
        .json(&serde_json::json!({
            "username": username,
            "password": password,
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let ticket: TicketResponse = client
        .post(format!("{base}/api/tickets"))
        .bearer_auth(&login.access_token)
        .json(&serde_json::json!({ "room_id": room_id }))
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
