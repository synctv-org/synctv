tokio::task_local! {
    pub static CURRENT_REQUEST_ID: String;
}
