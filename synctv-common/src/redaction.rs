/// Redact username and password portions of a URL-like string.
///
/// The caller supplies placeholders for inputs that have no URL scheme and for
/// inputs that cannot be parsed or manually masked.
#[must_use]
pub fn mask_url_credentials(
    url: &str,
    missing_scheme_placeholder: &str,
    invalid_url_placeholder: &str,
) -> String {
    if !url.contains("://") {
        return missing_scheme_placeholder.to_string();
    }

    if let Ok(mut parsed) = reqwest::Url::parse(url) {
        if !parsed.username().is_empty() && parsed.set_username("***").is_err() {
            return mask_url_credentials_manually(url, invalid_url_placeholder);
        }
        if parsed.password().is_some() && parsed.set_password(Some("***")).is_err() {
            return mask_url_credentials_manually(url, invalid_url_placeholder);
        }
        return parsed.to_string();
    }

    mask_url_credentials_manually(url, invalid_url_placeholder)
}

fn mask_url_credentials_manually(url: &str, invalid_url_placeholder: &str) -> String {
    if let Some(at_pos) = url.rfind('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            let host_part = &url[at_pos..];
            return format!("{scheme}***:***{host_part}");
        }
    }

    invalid_url_placeholder.to_string()
}

#[cfg(test)]
mod tests {
    use super::mask_url_credentials;

    #[test]
    fn masks_standard_url_credentials() {
        assert_eq!(
            mask_url_credentials(
                "postgresql://synctv:secret@db.internal:5432/synctv",
                "<missing>",
                "<invalid>",
            ),
            "postgresql://***:***@db.internal:5432/synctv"
        );
    }

    #[test]
    fn preserves_urls_without_credentials() {
        assert_eq!(
            mask_url_credentials("https://example.com/path", "<missing>", "<invalid>"),
            "https://example.com/path"
        );
    }

    #[test]
    fn uses_missing_scheme_placeholder() {
        assert_eq!(
            mask_url_credentials("example.com/path", "<missing>", "<invalid>"),
            "<missing>"
        );
    }
}
