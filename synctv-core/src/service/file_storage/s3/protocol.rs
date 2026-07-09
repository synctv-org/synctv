use std::{collections::BTreeMap, fmt::Write as _};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

pub(super) const AWS4_ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub(super) const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

const PATH_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

const QUERY_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

pub(super) struct S3SigningContext<'a> {
    access_key_id: &'a str,
    secret_access_key: &'a str,
    region: &'a str,
}

pub(super) struct S3CompletedPart<'a> {
    part_number: i32,
    etag: &'a str,
    checksum_sha256: Option<&'a str>,
}

impl<'a> S3CompletedPart<'a> {
    pub(super) const fn new(
        part_number: i32,
        etag: &'a str,
        checksum_sha256: Option<&'a str>,
    ) -> Self {
        Self {
            part_number,
            etag,
            checksum_sha256,
        }
    }
}

impl<'a> S3SigningContext<'a> {
    pub(super) const fn new(
        access_key_id: &'a str,
        secret_access_key: &'a str,
        region: &'a str,
    ) -> Self {
        Self {
            access_key_id,
            secret_access_key,
            region,
        }
    }

    pub(super) fn presign_url(
        &self,
        method: &str,
        mut url: url::Url,
        date: DateTime<Utc>,
        expires_seconds: i64,
        headers: &BTreeMap<String, String>,
    ) -> Result<String> {
        let host = host_header(&url)?;
        let credential_scope = credential_scope(date, self.region);
        let credential = format!("{}/{}", self.access_key_id.trim(), credential_scope);
        let mut signed_header_pairs = headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        signed_header_pairs.push(("host", host.as_str()));
        signed_header_pairs.sort_by_key(|(key, _)| *key);
        let signed_headers_value = signed_headers(&signed_header_pairs);
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("X-Amz-Algorithm", AWS4_ALGORITHM);
            pairs.append_pair("X-Amz-Credential", &credential);
            pairs.append_pair("X-Amz-Date", &amz_datetime(date));
            pairs.append_pair("X-Amz-Expires", &expires_seconds.to_string());
            pairs.append_pair("X-Amz-SignedHeaders", &signed_headers_value);
        }
        let canonical_request = canonical_request(
            method,
            &url,
            &signed_header_pairs,
            &canonical_query(&url),
            UNSIGNED_PAYLOAD,
        )?;
        let string_to_sign = string_to_sign(date, self.region, &canonical_request);
        let signature = sign_hex(
            self.secret_access_key.trim(),
            date,
            self.region,
            &string_to_sign,
        )?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("X-Amz-Signature", &signature);
        }
        Ok(url.to_string())
    }

    pub(super) fn authorization_header(
        &self,
        method: &str,
        url: &url::Url,
        headers: &BTreeMap<String, String>,
        date: DateTime<Utc>,
        payload_hash: &str,
    ) -> Result<String> {
        let mut all_headers = headers.clone();
        all_headers.insert("host".to_string(), host_header(url)?);
        let header_pairs = all_headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let canonical_query = canonical_query(url);
        let canonical_request =
            canonical_request(method, url, &header_pairs, &canonical_query, payload_hash)?;
        let string_to_sign = string_to_sign(date, self.region, &canonical_request);
        let signature = sign_hex(
            self.secret_access_key.trim(),
            date,
            self.region,
            &string_to_sign,
        )?;
        let signed_headers = signed_headers(&header_pairs);
        Ok(format!(
            "{AWS4_ALGORITHM} Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key_id.trim(),
            credential_scope(date, self.region),
            signed_headers,
            signature
        ))
    }
}

pub(super) fn complete_multipart_upload_body(parts: &[S3CompletedPart<'_>]) -> Result<String> {
    let mut body = String::from("<CompleteMultipartUpload>");
    for part in parts {
        if part.part_number <= 0 || part.etag.trim().is_empty() {
            return Err(Error::InvalidInput(
                "S3 multipart completion requires positive part numbers and ETags".to_string(),
            ));
        }
        body.push_str("<Part><PartNumber>");
        body.push_str(&part.part_number.to_string());
        body.push_str("</PartNumber><ETag>");
        body.push_str(&escape_xml(part.etag.trim()));
        body.push_str("</ETag>");
        if let Some(checksum) = part.checksum_sha256 {
            body.push_str("<ChecksumSHA256>");
            body.push_str(&escape_xml(&sha256_hex_to_base64(checksum)?));
            body.push_str("</ChecksumSHA256>");
        }
        body.push_str("</Part>");
    }
    body.push_str("</CompleteMultipartUpload>");
    Ok(body)
}

pub(super) fn extract_xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(body[start..end].to_string())
}

pub(super) fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(super) fn sha256_hex_to_base64(value: &str) -> Result<String> {
    let bytes = hex::decode(value.trim()).map_err(|_| {
        Error::InvalidInput("checksum_sha256 must be a 64-character hex string".to_string())
    })?;
    if bytes.len() != 32 {
        return Err(Error::InvalidInput(
            "checksum_sha256 must be a 64-character hex string".to_string(),
        ));
    }
    Ok(BASE64_STANDARD.encode(bytes))
}

pub(super) fn amz_datetime(date: DateTime<Utc>) -> String {
    date.format("%Y%m%dT%H%M%SZ").to_string()
}

fn amz_date(date: DateTime<Utc>) -> String {
    date.format("%Y%m%d").to_string()
}

pub(super) fn credential_scope(date: DateTime<Utc>, region: &str) -> String {
    format!("{}/{}/s3/aws4_request", amz_date(date), region.trim())
}

pub(super) fn string_to_sign(date: DateTime<Utc>, region: &str, canonical_request: &str) -> String {
    let request_hash = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    format!(
        "{AWS4_ALGORITHM}\n{}\n{}\n{}",
        amz_datetime(date),
        credential_scope(date, region),
        request_hash
    )
}

pub(super) fn sign_hex(
    secret: &str,
    date: DateTime<Utc>,
    region: &str,
    string_to_sign: &str,
) -> Result<String> {
    let date_key = hmac_sha256(
        format!("AWS4{secret}").as_bytes(),
        amz_date(date).as_bytes(),
    )?;
    let date_region_key = hmac_sha256(&date_key, region.trim().as_bytes())?;
    let date_region_service_key = hmac_sha256(&date_region_key, b"s3")?;
    let signing_key = hmac_sha256(&date_region_service_key, b"aws4_request")?;
    Ok(hex::encode(hmac_sha256(
        &signing_key,
        string_to_sign.as_bytes(),
    )?))
}

fn hmac_sha256(key: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|error| Error::Internal(format!("failed to initialize HMAC-SHA256: {error}")))?;
    mac.update(payload);
    Ok(mac.finalize().into_bytes().to_vec())
}

pub(super) fn host_header(url: &url::Url) -> Result<String> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidInput("S3 URL is missing host".to_string()))?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

pub(super) fn canonical_request(
    method: &str,
    url: &url::Url,
    headers: &[(&str, &str)],
    canonical_query: &str,
    payload_hash: &str,
) -> Result<String> {
    let mut canonical_headers = headers
        .iter()
        .map(|(name, value)| {
            (
                name.trim().to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect::<Vec<_>>();
    canonical_headers.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical_header_lines = String::new();
    for (name, value) in &canonical_headers {
        writeln!(&mut canonical_header_lines, "{name}:{value}").map_err(|error| {
            Error::Internal(format!("failed to build canonical headers: {error}"))
        })?;
    }
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method,
        canonical_uri(url),
        canonical_query,
        canonical_header_lines,
        signed_headers(headers),
        payload_hash
    ))
}

pub(super) fn signed_headers(headers: &[(&str, &str)]) -> String {
    let mut names = headers
        .iter()
        .map(|(name, _)| name.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names.join(";")
}

fn canonical_uri(url: &url::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        return "/".to_string();
    }
    path.split('/')
        .map(|segment| utf8_percent_encode(segment, PATH_ENCODE_SET).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn canonical_query(url: &url::Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| {
            (
                utf8_percent_encode(&key, QUERY_ENCODE_SET).to_string(),
                utf8_percent_encode(&value, QUERY_ENCODE_SET).to_string(),
            )
        })
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{TestOptionExt, TestResultExt};
    use chrono::TimeZone as _;

    #[test]
    fn checksum_hex_converts_to_s3_base64() {
        assert_eq!(
            sha256_hex_to_base64(
                "e3b0c44298fc1c149afbf4c8996fb924\
                 27ae41e4649b934ca495991b7852b855"
            )
            .checked("checksum should convert"),
            "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="
        );
    }

    #[test]
    fn xml_helpers_extract_and_escape_wire_values() {
        assert_eq!(
            extract_xml_tag("<Root><UploadId>abc123</UploadId></Root>", "UploadId"),
            Some("abc123".to_string())
        );
        assert_eq!(
            escape_xml("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
    }

    #[test]
    fn complete_multipart_upload_body_escapes_parts_and_checksums() {
        let parts = [
            S3CompletedPart::new(
                1,
                "\"etag&one\"",
                Some(
                    "e3b0c44298fc1c149afbf4c8996fb924\
                     27ae41e4649b934ca495991b7852b855",
                ),
            ),
            S3CompletedPart::new(2, "\"etag-two\"", None),
        ];

        assert_eq!(
            complete_multipart_upload_body(&parts).checked("multipart XML should build"),
            concat!(
                "<CompleteMultipartUpload>",
                "<Part><PartNumber>1</PartNumber><ETag>&quot;etag&amp;one&quot;</ETag>",
                "<ChecksumSHA256>47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=</ChecksumSHA256>",
                "</Part>",
                "<Part><PartNumber>2</PartNumber><ETag>&quot;etag-two&quot;</ETag></Part>",
                "</CompleteMultipartUpload>"
            )
        );
    }

    #[test]
    fn complete_multipart_upload_body_rejects_invalid_parts() {
        let error = complete_multipart_upload_body(&[S3CompletedPart::new(0, "\"etag\"", None)])
            .failed("invalid part should fail");

        assert!(matches!(
            error,
            Error::InvalidInput(message)
                if message == "S3 multipart completion requires positive part numbers and ETags"
        ));
    }

    #[test]
    fn aws4_canonical_request_sorts_and_normalizes_headers() {
        let url = url::Url::parse(
            "https://storage.example.test/bucket/path with spaces/file.txt?z=last&a=first",
        )
        .checked("url should parse");
        let request = canonical_request(
            "PUT",
            &url,
            &[
                ("X-Amz-Date", "20260102T030405Z"),
                ("host", "storage.example.test"),
                ("x-amz-meta-name", "  alpha   beta  "),
            ],
            &canonical_query(&url),
            UNSIGNED_PAYLOAD,
        )
        .checked("canonical request should build");

        assert!(request.contains("/bucket/path%2520with%2520spaces/file.txt\n"));
        assert!(request.contains("a=first&z=last\n"));
        assert!(request.contains("x-amz-meta-name:alpha beta\n"));
        assert!(request.ends_with("host;x-amz-date;x-amz-meta-name\nUNSIGNED-PAYLOAD"));
    }

    #[test]
    fn aws4_signing_uses_trimmed_region() {
        let date = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .checked("date should build");

        assert_eq!(amz_datetime(date), "20260102T030405Z");
        assert_eq!(
            credential_scope(date, " us-east-1 "),
            "20260102/us-east-1/s3/aws4_request"
        );
        assert_eq!(
            sign_hex("secret", date, " us-east-1 ", "payload")
                .checked("signature should build")
                .len(),
            64
        );
    }

    #[test]
    fn signing_context_builds_presigned_url_and_authorization_header() {
        let date = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .checked("date should build");
        let signing = S3SigningContext::new(" access ", "secret", " us-east-1 ");
        let mut headers = BTreeMap::new();
        headers.insert("x-amz-checksum-sha256".to_string(), "abc".to_string());

        let url = signing
            .presign_url(
                "PUT",
                url::Url::parse("https://storage.example.test/bucket/object.bin")
                    .checked("url should parse"),
                date,
                900,
                &headers,
            )
            .checked("presigned URL should build");
        assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
        assert!(url.contains("X-Amz-Credential=access%2F20260102%2Fus-east-1%2Fs3%2Faws4_request"));
        assert!(url.contains("X-Amz-Signature="));

        let auth = signing
            .authorization_header(
                "PUT",
                &url::Url::parse("https://storage.example.test/bucket/object.bin")
                    .checked("url should parse"),
                &headers,
                date,
                UNSIGNED_PAYLOAD,
            )
            .checked("authorization header should build");
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=access/"));
        assert!(auth.contains("SignedHeaders=host;x-amz-checksum-sha256"));
        assert!(auth.contains("Signature="));
    }
}
