use std::collections::HashMap;

use percent_encoding::percent_decode_str;
use quick_xml::events::Event;
use quick_xml::Reader;

use super::types::NextcloudDavItem;
use crate::ProviderClientError;

const PROPERTIES: &str = r"
<d:displayname/><d:resourcetype/><d:getcontentlength/><d:getcontenttype/>
<d:getlastmodified/><d:getetag/><oc:fileid/><oc:permissions/><oc:owner-id/>
<oc:owner-display-name/><oc:favorite/><nc:has-preview/><nc:metadata-blurhash/>
<nc:metadata-width/><nc:metadata-height/><nc:metadata-duration/>";

#[must_use]
pub fn propfind_body() -> String {
    format!(
        r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:prop>{PROPERTIES}</d:prop></d:propfind>"#
    )
}

#[must_use]
pub fn search_report(user_id: &str, path: &str, query: &str, offset: u64, limit: u32) -> String {
    let scope = dav_scope(user_id, path);
    let query = escape_xml(&query.replace('%', ""));
    format!(
        r#"<?xml version="1.0"?><d:searchrequest xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:basicsearch><d:select><d:prop>{PROPERTIES}</d:prop></d:select><d:from><d:scope><d:href>{scope}</d:href><d:depth>infinity</d:depth></d:scope></d:from><d:where><d:like><d:prop><d:displayname/></d:prop><d:literal>%{query}%</d:literal></d:like></d:where><d:orderby><d:order><d:prop><d:displayname/></d:prop><d:ascending/></d:order></d:orderby><d:limit><d:nresults>{limit}</d:nresults><nc:firstresult>{offset}</nc:firstresult></d:limit></d:basicsearch></d:searchrequest>"#
    )
}

#[must_use]
pub fn favorites_report(offset: u64, limit: u32) -> String {
    format!(
        r#"<?xml version="1.0"?><oc:filter-files xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:prop>{PROPERTIES}</d:prop><oc:filter-rules><oc:favorite>1</oc:favorite></oc:filter-rules><d:limit><d:nresults>{limit}</d:nresults><nc:firstresult>{offset}</nc:firstresult></d:limit></oc:filter-files>"#
    )
}

pub fn parse_multistatus(
    xml: &str,
    user_id: &str,
) -> Result<Vec<NextcloudDavItem>, ProviderClientError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut items = Vec::new();
    let mut fields = HashMap::<String, String>::new();
    let mut current_field = None::<String>;
    let mut in_response = false;
    let mut is_collection = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "response" {
                    in_response = true;
                    fields.clear();
                    is_collection = false;
                } else if in_response {
                    if name == "collection" {
                        is_collection = true;
                    }
                    current_field = Some(name);
                }
            }
            Ok(Event::Empty(event)) => {
                if in_response && local_name(event.name().as_ref()) == "collection" {
                    is_collection = true;
                }
            }
            Ok(Event::Text(text)) if in_response => {
                if let Some(field) = current_field.as_ref() {
                    let value = text
                        .decode()
                        .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
                    fields
                        .entry(field.clone())
                        .and_modify(|existing| existing.push_str(&value))
                        .or_insert_with(|| value.into_owned());
                }
            }
            Ok(Event::GeneralRef(reference)) if in_response => {
                if let Some(field) = current_field.as_ref() {
                    let decoded = reference
                        .decode()
                        .map_err(|error| ProviderClientError::Parse(error.to_string()))?;
                    let value = match decoded.as_ref() {
                        "amp" => '&',
                        "lt" => '<',
                        "gt" => '>',
                        "quot" => '"',
                        "apos" => '\'',
                        _ => reference
                            .resolve_char_ref()
                            .map_err(|error| ProviderClientError::Parse(error.to_string()))?
                            .ok_or_else(|| {
                                ProviderClientError::Parse(format!(
                                    "unsupported XML entity '&{decoded};'"
                                ))
                            })?,
                    };
                    fields.entry(field.clone()).or_default().push(value);
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "response" && in_response {
                    items.push(item_from_fields(&fields, user_id, is_collection));
                    in_response = false;
                    current_field = None;
                } else if current_field.as_deref() == Some(name.as_str()) {
                    current_field = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(ProviderClientError::Parse(error.to_string())),
            _ => {}
        }
    }
    Ok(items)
}

fn item_from_fields(
    fields: &HashMap<String, String>,
    user_id: &str,
    is_directory: bool,
) -> NextcloudDavItem {
    let href = fields.get("href").cloned().unwrap_or_default();
    let decoded = percent_decode_str(&href).decode_utf8_lossy();
    let marker = format!("/files/{user_id}");
    let path = decoded
        .find(&marker)
        .map_or(decoded.as_ref(), |index| &decoded[index + marker.len()..])
        .trim_end_matches('/')
        .to_string();
    let name = fields
        .get("displayname")
        .cloned()
        .or_else(|| path.rsplit('/').next().map(str::to_string))
        .unwrap_or_default();
    NextcloudDavItem {
        href,
        path,
        name,
        file_id: number(fields, "fileid"),
        size: number(fields, "getcontentlength").max(number(fields, "size")),
        modified_at: value(fields, "getlastmodified"),
        content_type: value(fields, "getcontenttype"),
        etag: value(fields, "getetag").map(|value| value.trim_matches('"').to_string()),
        permissions: value(fields, "permissions"),
        owner_id: value(fields, "owner-id"),
        owner_display_name: value(fields, "owner-display-name"),
        favorite: boolean(fields, "favorite"),
        has_preview: boolean(fields, "has-preview"),
        is_directory,
        blurhash: value(fields, "metadata-blurhash"),
        width: u32::try_from(number(fields, "metadata-width"))
            .ok()
            .filter(|value| *value > 0),
        height: u32::try_from(number(fields, "metadata-height"))
            .ok()
            .filter(|value| *value > 0),
        duration_millis: value(fields, "metadata-duration").and_then(|value| {
            value
                .parse::<f64>()
                .ok()
                .and_then(|seconds| std::time::Duration::try_from_secs_f64(seconds).ok())
                .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        }),
    }
}

fn value(fields: &HashMap<String, String>, key: &str) -> Option<String> {
    fields.get(key).filter(|value| !value.is_empty()).cloned()
}

fn number(fields: &HashMap<String, String>, key: &str) -> u64 {
    fields
        .get(key)
        .and_then(|value| value.parse().ok())
        .unwrap_or_default()
}

fn boolean(fields: &HashMap<String, String>, key: &str) -> bool {
    fields
        .get(key)
        .is_some_and(|value| matches!(value.as_str(), "1" | "true"))
}

fn local_name(name: &[u8]) -> String {
    let name = name.rsplit(|byte| *byte == b':').next().unwrap_or(name);
    String::from_utf8_lossy(name).into_owned()
}

fn dav_scope(user_id: &str, path: &str) -> String {
    let path = path.trim_matches('/');
    if path.is_empty() {
        format!("/files/{}", escape_xml(user_id))
    } else {
        format!("/files/{}/{}", escape_xml(user_id), escape_xml(path))
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nextcloud_dav_properties_and_namespaces() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:response><d:href>/nextcloud/remote.php/dav/files/alice/Videos/My%20Film.mp4</d:href><d:propstat><d:prop><d:displayname>My &amp; Film.mp4</d:displayname><d:getcontentlength>4294967297</d:getcontentlength><d:getcontenttype>video/mp4</d:getcontenttype><d:getetag>&quot;abc&quot;</d:getetag><oc:fileid>9007199254740991</oc:fileid><oc:favorite>1</oc:favorite><nc:has-preview>true</nc:has-preview><nc:metadata-blurhash>LEHV6nWB2yk8pyo0adR*.7kCMdnj</nc:metadata-blurhash><nc:metadata-width>3840</nc:metadata-width><nc:metadata-height>2160</nc:metadata-height><nc:metadata-duration>123.456</nc:metadata-duration></d:prop></d:propstat></d:response></d:multistatus>"#;
        let item = parse_multistatus(xml, "alice")
            .expect("test operation should succeed")
            .remove(0);
        assert_eq!(item.path, "/Videos/My Film.mp4");
        assert_eq!(item.name, "My & Film.mp4");
        assert_eq!(item.file_id, 9_007_199_254_740_991);
        assert_eq!(item.size, 4_294_967_297);
        assert_eq!(item.duration_millis, Some(123_456));
        assert_eq!(item.width, Some(3840));
        assert!(item.favorite);
        assert!(item.has_preview);
    }

    #[test]
    fn generates_paged_search_and_favorites_reports() {
        let search = search_report("alice", "/Videos", "Tom & Jerry", 200, 100);
        assert!(search.contains("/files/alice/Videos"));
        assert!(search.contains("%Tom &amp; Jerry%"));
        assert!(search.contains("<nc:firstresult>200</nc:firstresult>"));
        let favorites = favorites_report(100, 50);
        assert!(favorites.contains("<oc:favorite>1</oc:favorite>"));
        assert!(favorites.contains("<d:nresults>50</d:nresults>"));
    }
}
