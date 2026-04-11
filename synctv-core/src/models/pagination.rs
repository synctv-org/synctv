//! Pagination support for repository queries
//!
//! Provides type-safe pagination with configurable limits to prevent OOM and slow queries.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Default page size for list queries
pub const DEFAULT_PAGE_SIZE: u32 = 20;

/// Maximum allowed page size to prevent OOM
pub const MAX_PAGE_SIZE: u32 = 100;

/// Minimum page number (1-indexed)
pub const MIN_PAGE: u32 = 1;

/// Maximum allowed offset to prevent full-scan `DoS` attacks.
///
/// Requests with `page * page_size > MAX_OFFSET` are rejected with an error.
/// At `MAX_PAGE_SIZE = 100` items per page this allows up to 1000 pages (100k items).
pub const MAX_OFFSET: u64 = 100_000;

/// Pagination parameters for list queries
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageParams {
    /// Page number (1-indexed)
    pub page: u32,
    /// Number of items per page
    pub page_size: u32,
}

impl PageParams {
    /// Create pagination parameters with validation
    ///
    /// # Arguments
    /// * `page` - Page number (1-indexed), defaults to 1 if None
    /// * `page_size` - Items per page, defaults to `DEFAULT_PAGE_SIZE` if None, capped at `MAX_PAGE_SIZE`
    ///
    /// # Examples
    /// ```
    /// use synctv_core::models::PageParams;
    ///
    /// // Default pagination (page 1, 20 items)
    /// let params = PageParams::new(None, None);
    /// assert_eq!(params.page, 1);
    /// assert_eq!(params.page_size, 20);
    ///
    /// // Custom pagination
    /// let params = PageParams::new(Some(2), Some(50));
    /// assert_eq!(params.page, 2);
    /// assert_eq!(params.page_size, 50);
    ///
    /// // Exceeds max - capped at MAX_PAGE_SIZE
    /// let params = PageParams::new(Some(1), Some(200));
    /// assert_eq!(params.page_size, 100);
    /// ```
    #[must_use]
    pub fn new(page: Option<u32>, page_size: Option<u32>) -> Self {
        let page = page.unwrap_or(MIN_PAGE).max(MIN_PAGE);
        let page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);

        Self { page, page_size }
    }

    /// Validate that the computed offset is within the safe range.
    ///
    /// Returns `Err(Error::InvalidInput)` when `(page - 1) * page_size > MAX_OFFSET`
    /// to prevent expensive full-scan `DoS` attacks via extremely large page numbers.
    ///
    /// # Examples
    /// ```
    /// use synctv_core::models::PageParams;
    ///
    /// // Page 1002 with page_size 100 -> offset 100_100 -- rejected (exceeds MAX_OFFSET of 100_000)
    /// let params = PageParams::new(Some(1002), Some(100));
    /// assert!(params.validate().is_err());
    ///
    /// // Page 1001 with page_size 100 -> offset 100_000 -- accepted (equals MAX_OFFSET)
    /// let params = PageParams::new(Some(1001), Some(100));
    /// assert!(params.validate().is_ok());
    /// ```
    pub fn validate(&self) -> Result<()> {
        if self.offset() > MAX_OFFSET {
            return Err(Error::InvalidInput(format!(
                "Requested offset {} exceeds the maximum allowed offset of {}. \
                 Use a smaller page number.",
                self.offset(),
                MAX_OFFSET
            )));
        }
        Ok(())
    }

    /// Calculate OFFSET for SQL query
    ///
    /// # Examples
    /// ```
    /// use synctv_core::models::PageParams;
    ///
    /// let params = PageParams::new(Some(1), Some(20));
    /// assert_eq!(params.offset(), 0);
    ///
    /// let params = PageParams::new(Some(2), Some(20));
    /// assert_eq!(params.offset(), 20);
    ///
    /// let params = PageParams::new(Some(3), Some(50));
    /// assert_eq!(params.offset(), 100);
    /// ```
    #[must_use]
    pub const fn offset(&self) -> u64 {
        (self.page as u64 - 1) * self.page_size as u64
    }

    /// Get LIMIT for SQL query
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.page_size as u64
    }
}

impl Default for PageParams {
    fn default() -> Self {
        Self::new(None, None)
    }
}

/// Paginated response containing items and metadata
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Items in current page
    pub items: Vec<T>,
    /// Total number of items across all pages
    pub total: u64,
    /// Current page number (1-indexed)
    pub page: u32,
    /// Number of items per page
    pub page_size: u32,
    /// Total number of pages
    pub total_pages: u32,
}

impl<T> Page<T> {
    /// Create a paginated response
    ///
    /// # Arguments
    /// * `items` - Items for the current page
    /// * `total` - Total count of items across all pages
    /// * `params` - Pagination parameters used for the query
    ///
    /// # Examples
    /// ```
    /// use synctv_core::models::{Page, PageParams};
    ///
    /// let params = PageParams::new(Some(1), Some(20));
    /// let items = vec![1, 2, 3];
    /// let page = Page::new(items, 100, params);
    ///
    /// assert_eq!(page.items.len(), 3);
    /// assert_eq!(page.total, 100);
    /// assert_eq!(page.total_pages, 5); // ceil(100 / 20)
    /// ```
    #[must_use]
    pub fn new(items: Vec<T>, total: u64, params: PageParams) -> Self {
        let total_pages = if params.page_size == 0 {
            0
        } else {
            let page_size = u64::from(params.page_size);
            let page_count = total.saturating_add(page_size - 1) / page_size;
            u32::try_from(page_count).unwrap_or(u32::MAX)
        };

        Self {
            items,
            total,
            page: params.page,
            page_size: params.page_size,
            total_pages,
        }
    }

    /// Check if there is a next page
    #[must_use]
    pub const fn has_next(&self) -> bool {
        self.page < self.total_pages
    }

    /// Check if there is a previous page
    #[must_use]
    pub const fn has_prev(&self) -> bool {
        self.page > 1
    }

    /// Get the next page number if available
    #[must_use]
    pub const fn next_page(&self) -> Option<u32> {
        if self.has_next() {
            Some(self.page + 1)
        } else {
            None
        }
    }

    /// Get the previous page number if available
    #[must_use]
    pub const fn prev_page(&self) -> Option<u32> {
        if self.has_prev() {
            Some(self.page - 1)
        } else {
            None
        }
    }

    /// Map the items to a different type
    ///
    /// Useful for converting between domain models and DTOs
    pub fn map<U, F>(self, f: F) -> Page<U>
    where
        F: FnMut(T) -> U,
    {
        Page {
            items: self.items.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            page_size: self.page_size,
            total_pages: self.total_pages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== PageParams Tests ==========

    #[test]
    fn test_page_params_new_with_none() {
        let params = PageParams::new(None, None);
        assert_eq!(params.page, 1);
        assert_eq!(params.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn test_page_params_custom_values() {
        let params = PageParams::new(Some(3), Some(50));
        assert_eq!(params.page, 3);
        assert_eq!(params.page_size, 50);
    }

    #[test]
    fn test_page_params_caps_at_max() {
        let params = PageParams::new(Some(1), Some(200));
        assert_eq!(params.page_size, MAX_PAGE_SIZE);
    }

    #[test]
    fn test_page_params_minimum_page() {
        let params = PageParams::new(Some(0), None);
        assert_eq!(params.page, MIN_PAGE);
    }

    #[test]
    fn test_page_params_minimum_page_size() {
        let params = PageParams::new(None, Some(0));
        assert_eq!(params.page_size, 1);
    }

    #[test]
    fn test_offset_calculation() {
        assert_eq!(PageParams::new(Some(1), Some(20)).offset(), 0);
        assert_eq!(PageParams::new(Some(2), Some(20)).offset(), 20);
        assert_eq!(PageParams::new(Some(3), Some(20)).offset(), 40);
        assert_eq!(PageParams::new(Some(5), Some(50)).offset(), 200);
    }

    #[test]
    fn test_offset_uses_wide_arithmetic_for_large_pages() {
        let params = PageParams::new(Some(u32::MAX), Some(20));
        assert_eq!(params.offset(), (u64::from(u32::MAX) - 1) * 20);
    }

    #[test]
    fn test_limit() {
        let params = PageParams::new(Some(1), Some(20));
        assert_eq!(params.limit(), 20);

        let params = PageParams::new(Some(1), Some(50));
        assert_eq!(params.limit(), 50);
    }

    // ========== Page Tests ==========

    #[test]
    fn test_page_creation() {
        let params = PageParams::new(Some(1), Some(20));
        let items = vec![1, 2, 3];
        let page = Page::new(items, 100, params);

        assert_eq!(page.items, vec![1, 2, 3]);
        assert_eq!(page.total, 100);
        assert_eq!(page.page, 1);
        assert_eq!(page.page_size, 20);
        assert_eq!(page.total_pages, 5); // ceil(100 / 20)
    }

    #[test]
    fn test_page_total_pages_calculation() {
        let params = PageParams::new(Some(1), Some(20));

        // Exact multiple
        let page = Page::new(vec![1], 100, params);
        assert_eq!(page.total_pages, 5);

        // Remainder
        let page = Page::new(vec![1], 101, params);
        assert_eq!(page.total_pages, 6);

        // Less than one page
        let page = Page::new(vec![1], 10, params);
        assert_eq!(page.total_pages, 1);

        // Empty
        let page: Page<i32> = Page::new(vec![], 0, params);
        assert_eq!(page.total_pages, 0);
    }

    #[test]
    fn test_has_next() {
        let params = PageParams::new(Some(1), Some(20));
        let page = Page::new(vec![1], 100, params); // Page 1 of 5
        assert!(page.has_next());

        let params = PageParams::new(Some(5), Some(20));
        let page = Page::new(vec![1], 100, params); // Page 5 of 5
        assert!(!page.has_next());
    }

    #[test]
    fn test_has_prev() {
        let params = PageParams::new(Some(1), Some(20));
        let page = Page::new(vec![1], 100, params);
        assert!(!page.has_prev());

        let params = PageParams::new(Some(2), Some(20));
        let page = Page::new(vec![1], 100, params);
        assert!(page.has_prev());
    }

    #[test]
    fn test_next_page() {
        let params = PageParams::new(Some(1), Some(20));
        let page = Page::new(vec![1], 100, params);
        assert_eq!(page.next_page(), Some(2));

        let params = PageParams::new(Some(5), Some(20));
        let page = Page::new(vec![1], 100, params);
        assert_eq!(page.next_page(), None);
    }

    #[test]
    fn test_prev_page() {
        let params = PageParams::new(Some(1), Some(20));
        let page = Page::new(vec![1], 100, params);
        assert_eq!(page.prev_page(), None);

        let params = PageParams::new(Some(3), Some(20));
        let page = Page::new(vec![1], 100, params);
        assert_eq!(page.prev_page(), Some(2));
    }

    #[test]
    fn test_page_map() {
        let params = PageParams::new(Some(1), Some(20));
        let page = Page::new(vec![1, 2, 3], 100, params);
        let mapped = page.map(|x| x * 2);

        assert_eq!(mapped.items, vec![2, 4, 6]);
        assert_eq!(mapped.total, 100);
        assert_eq!(mapped.page, 1);
    }

    #[test]
    fn test_serialization() {
        let params = PageParams::new(Some(1), Some(20));
        let page = Page::new(vec![1, 2, 3], 100, params);

        let json = serde_json::to_string(&page).unwrap();
        let deserialized: Page<i32> = serde_json::from_str(&json).unwrap();

        assert_eq!(page, deserialized);
    }
}
