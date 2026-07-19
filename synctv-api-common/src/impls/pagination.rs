//! Generic pagination utilities for accumulating multi-page queries.

/// Fetch all items from a paginated source by calling `fetch_page` repeatedly
/// until all items are loaded or the returned page is empty.
///
/// The closure receives a 1-based page number and returns `(items, total_count)`.
/// Pagination stops when:
/// - The page is empty, OR
/// - The accumulated item count reaches or exceeds `total_count`
///
/// Returns all accumulated items or the first error encountered.
pub async fn paginate_all<T, F, Fut>(mut fetch_page: F) -> synctv_core::Result<Vec<T>>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = synctv_core::Result<(Vec<T>, i64)>>,
{
    let mut page = 1;
    let mut all_items = Vec::new();

    loop {
        let (page_items, total) = fetch_page(page).await?;

        if page_items.is_empty() {
            break;
        }

        all_items.extend(page_items);
        let loaded = i64::try_from(all_items.len()).map_err(|_| {
            synctv_core::Error::Internal("paginated item count exceeds i64::MAX".to_string())
        })?;
        if loaded >= total {
            break;
        }

        page += 1;
    }

    Ok(all_items)
}
