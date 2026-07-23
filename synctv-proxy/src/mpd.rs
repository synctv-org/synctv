use std::borrow::Cow;
use std::io::Cursor;

use quick_xml::events::{BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MpdResourceKind {
    Media,
    Manifest,
}

struct ElementContext {
    name: String,
    inherited_base: Url,
    descendant_base: Url,
    first_base_url: Option<Url>,
    url_text: Option<String>,
}

pub fn rewrite_mpd_with_url_mapper<F>(
    mpd: &str,
    source_url: &str,
    mut proxy_scope_for: F,
) -> Result<String, anyhow::Error>
where
    F: FnMut(&str, MpdResourceKind) -> String,
{
    let source = Url::parse(source_url)
        .map_err(|error| anyhow::anyhow!("invalid MPD source URL: {error}"))?;
    let root_has_base_url = root_has_base_url(mpd)?;
    let mut reader = Reader::from_str(mpd);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::with_capacity(mpd.len() + 256)));
    let mut contexts = Vec::<ElementContext>::new();
    let mut root_started = false;

    loop {
        match reader.read_event()? {
            Event::Start(start) => {
                let name = local_name(start.name().as_ref());
                let inherited_base = contexts.last().map_or_else(
                    || source.clone(),
                    |context| {
                        if name == "BaseURL" {
                            context.inherited_base.clone()
                        } else {
                            context.descendant_base.clone()
                        }
                    },
                );
                let rewritten = rewrite_start(
                    &start,
                    reader.decoder(),
                    &inherited_base,
                    &mut proxy_scope_for,
                )?;
                writer.write_event(Event::Start(rewritten))?;

                let mut context = ElementContext {
                    name: name.clone(),
                    inherited_base: inherited_base.clone(),
                    descendant_base: inherited_base,
                    first_base_url: None,
                    url_text: matches!(name.as_str(), "BaseURL" | "Location").then(String::new),
                };
                if !root_started {
                    root_started = true;
                    if name != "MPD" {
                        return Err(anyhow::anyhow!("DASH document root must be MPD"));
                    }
                    if !root_has_base_url {
                        let directory = source.join(".")?;
                        let local = ensure_trailing_slash(proxy_scope_for(
                            directory.as_str(),
                            MpdResourceKind::Media,
                        ));
                        writer.write_event(Event::Start(BytesStart::new("BaseURL")))?;
                        writer.write_event(Event::Text(BytesText::new(&local)))?;
                        writer.write_event(Event::End(BytesEnd::new("BaseURL")))?;
                        context.descendant_base = directory.clone();
                        context.first_base_url = Some(directory);
                    }
                }
                contexts.push(context);
            }
            Event::Empty(start) => {
                let inherited_base = contexts
                    .last()
                    .map_or_else(|| source.clone(), |context| context.descendant_base.clone());
                let rewritten = rewrite_start(
                    &start,
                    reader.decoder(),
                    &inherited_base,
                    &mut proxy_scope_for,
                )?;
                writer.write_event(Event::Empty(rewritten))?;
            }
            Event::Text(text) => {
                let Some(context) = contexts.last_mut() else {
                    writer.write_event(Event::Text(text.into_owned()))?;
                    continue;
                };
                let Some(url_text) = context.url_text.as_mut() else {
                    writer.write_event(Event::Text(text.into_owned()))?;
                    continue;
                };
                let decoded = text.decode()?;
                let unescaped = quick_xml::escape::unescape(&decoded)?;
                url_text.push_str(&unescaped);
            }
            Event::CData(cdata) => {
                let Some(context) = contexts.last_mut() else {
                    writer.write_event(Event::CData(cdata.into_owned()))?;
                    continue;
                };
                let Some(url_text) = context.url_text.as_mut() else {
                    writer.write_event(Event::CData(cdata.into_owned()))?;
                    continue;
                };
                let decoded = cdata.decode()?;
                url_text.push_str(&decoded);
            }
            Event::GeneralRef(reference) => {
                let Some(url_text) = contexts
                    .last_mut()
                    .and_then(|context| context.url_text.as_mut())
                else {
                    writer.write_event(Event::GeneralRef(reference.into_owned()))?;
                    continue;
                };
                let reference = reference.decode()?;
                url_text.push_str(match reference.as_ref() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "apos" => "'",
                    "quot" => "\"",
                    other => {
                        return Err(anyhow::anyhow!(
                            "unsupported entity reference in MPD URL: {other}"
                        ));
                    }
                });
            }
            Event::End(end) => {
                let mut context = contexts
                    .pop()
                    .ok_or_else(|| anyhow::anyhow!("unbalanced MPD end element"))?;
                if let Some(url_text) = context.url_text.take() {
                    let raw = url_text.trim();
                    if !raw.is_empty() {
                        let resolved = context.inherited_base.join(raw)?;
                        let kind = if context.name == "Location" {
                            MpdResourceKind::Manifest
                        } else {
                            MpdResourceKind::Media
                        };
                        let local = proxy_scope_for(resolved.as_str(), kind);
                        let local = if resolved.path().ends_with('/') {
                            ensure_trailing_slash(local)
                        } else {
                            local
                        };
                        if context.name == "BaseURL" {
                            context.first_base_url = Some(resolved.clone());
                            context.descendant_base = resolved;
                        }
                        writer.write_event(Event::Text(BytesText::new(&local)))?;
                    }
                }
                writer.write_event(Event::End(end.into_owned()))?;
                if context.name == "BaseURL" {
                    if let (Some(parent), Some(base_url)) =
                        (contexts.last_mut(), context.first_base_url)
                    {
                        if parent.first_base_url.is_none() {
                            parent.descendant_base = base_url.clone();
                            parent.first_base_url = Some(base_url);
                        }
                    }
                }
            }
            Event::Eof => break,
            event => writer.write_event(event.into_owned())?,
        }
    }

    String::from_utf8(writer.into_inner().into_inner())
        .map_err(|error| anyhow::anyhow!("rewritten MPD is not UTF-8: {error}"))
}

fn root_has_base_url(mpd: &str) -> Result<bool, anyhow::Error> {
    let mut reader = Reader::from_str(mpd);
    let mut depth = 0usize;
    let mut root_seen = false;
    loop {
        match reader.read_event()? {
            Event::Start(start) => {
                if !root_seen {
                    root_seen = true;
                } else if depth == 1 && local_name(start.name().as_ref()) == "BaseURL" {
                    return Ok(true);
                }
                depth += 1;
            }
            Event::Empty(start)
                if root_seen && depth == 1 && local_name(start.name().as_ref()) == "BaseURL" =>
            {
                return Ok(true);
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => return Ok(false),
            _ => {}
        }
    }
}

fn rewrite_start<F>(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    inherited_base: &Url,
    proxy_scope_for: &mut F,
) -> Result<BytesStart<'static>, anyhow::Error>
where
    F: FnMut(&str, MpdResourceKind) -> String,
{
    let qualified_name = String::from_utf8(start.name().as_ref().to_vec())?;
    let element_name = local_name(start.name().as_ref());
    let mut rewritten = BytesStart::new(qualified_name);
    for attribute in start.attributes() {
        let attribute = attribute?;
        let qualified_key = String::from_utf8(attribute.key.as_ref().to_vec())?;
        let attribute_name = local_name(attribute.key.as_ref());
        let value =
            attribute.decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)?;
        let kind = uri_attribute_kind(&element_name, &qualified_key, &attribute_name);
        let rewritten_value = match kind {
            Some(MpdResourceKind::Media) if safe_relative_media_reference(&value) => value,
            Some(kind) => Cow::Owned(rewrite_uri_reference(
                &value,
                inherited_base,
                kind,
                proxy_scope_for,
            )?),
            None => value,
        };
        rewritten.push_attribute((qualified_key.as_str(), rewritten_value.as_ref()));
    }
    Ok(rewritten.into_owned())
}

fn safe_relative_media_reference(raw: &str) -> bool {
    if Url::parse(raw).is_ok() {
        return false;
    }
    let path = raw.split(['?', '#']).next().unwrap_or_default();
    if path.is_empty() {
        return false;
    }

    let mut decoded = path.to_string();
    loop {
        let next = percent_encoding::percent_decode_str(&decoded)
            .decode_utf8_lossy()
            .into_owned();
        if next == decoded {
            break;
        }
        decoded = next;
    }
    !decoded.starts_with('/')
        && !decoded.starts_with('\\')
        && !decoded.contains('\\')
        && decoded
            .split('/')
            .all(|segment| segment != "." && segment != "..")
}

fn uri_attribute_kind(
    element_name: &str,
    qualified_attribute_name: &str,
    attribute_name: &str,
) -> Option<MpdResourceKind> {
    let media = match element_name {
        "SegmentTemplate" => matches!(
            attribute_name,
            "media" | "initialization" | "bitstreamSwitching"
        ),
        "SegmentURL" => matches!(attribute_name, "media" | "index"),
        "Initialization" | "RepresentationIndex" | "BitstreamSwitching" => {
            attribute_name == "sourceURL"
        }
        "UTCTiming" => attribute_name == "value",
        _ => false,
    };
    if media {
        Some(MpdResourceKind::Media)
    } else if qualified_attribute_name.ends_with(":href") {
        Some(MpdResourceKind::Manifest)
    } else {
        None
    }
}

fn rewrite_uri_reference<F>(
    raw: &str,
    inherited_base: &Url,
    kind: MpdResourceKind,
    proxy_scope_for: &mut F,
) -> Result<String, anyhow::Error>
where
    F: FnMut(&str, MpdResourceKind) -> String,
{
    let resolved = inherited_base.join(raw)?;
    let path = resolved.path();
    if path.ends_with('/') {
        return Ok(ensure_trailing_slash(proxy_scope_for(
            resolved.as_str(),
            kind,
        )));
    }

    let dynamic_start = path
        .split_inclusive('/')
        .scan(0usize, |offset, segment| {
            let start = *offset;
            *offset += segment.len();
            Some((start, segment))
        })
        .find_map(|(start, segment)| segment.contains('$').then_some(start));
    let suffix_start =
        dynamic_start.unwrap_or_else(|| path.rfind('/').map_or(0, |index| index + 1));
    let mut scope = resolved.clone();
    scope.set_path(&path[..suffix_start]);
    scope.set_query(None);
    scope.set_fragment(None);

    let mut suffix = path[suffix_start..].to_string();
    if let Some(query) = resolved.query() {
        suffix.push('?');
        suffix.push_str(query);
    }
    if let Some(fragment) = resolved.fragment() {
        suffix.push('#');
        suffix.push_str(fragment);
    }
    Ok(format!(
        "{}{}",
        ensure_trailing_slash(proxy_scope_for(scope.as_str(), kind)),
        suffix
    ))
}

fn ensure_trailing_slash(mut value: String) -> String {
    if !value.ends_with('/') {
        value.push('/');
    }
    value
}

fn local_name(name: &[u8]) -> String {
    let name = name.rsplit(|byte| *byte == b':').next().unwrap_or(name);
    String::from_utf8_lossy(name).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper(url: &str, kind: MpdResourceKind) -> String {
        let kind = match kind {
            MpdResourceKind::Media => "media",
            MpdResourceKind::Manifest => "manifest",
        };
        format!("/proxy/{kind}/{}", crate::percent_encode(url))
    }

    #[test]
    fn inserts_root_scope_for_relative_segment_template() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r#"<MPD><Period><AdaptationSet><SegmentTemplate initialization="init-$RepresentationID$.m4s" media="video/$RepresentationID$/seg-$Number%05d$.m4s?token=x"/></AdaptationSet></Period></MPD>"#,
            "https://cdn.example.com/path/manifest.mpd?auth=1",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(
            rewritten.contains(
                "<BaseURL>/proxy/media/https%3A%2F%2Fcdn%2Eexample%2Ecom%2Fpath%2F/</BaseURL>"
            ),
            "{rewritten}"
        );
        assert!(rewritten.contains("$RepresentationID$"));
        assert!(rewritten.contains("$Number%05d$"));
        assert!(rewritten.contains("token=x"));
    }

    #[test]
    fn rewrites_nested_base_urls_and_segment_list_resources() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r#"<MPD><BaseURL>https://a.example/root/</BaseURL><Period><BaseURL>video/</BaseURL><AdaptationSet><Representation><SegmentList><Initialization sourceURL="init.mp4"/><SegmentURL media="seg-1.m4s" index="seg-1.idx"/></SegmentList></Representation></AdaptationSet></Period></MPD>"#,
            "https://origin.example/manifest.mpd",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(rewritten.contains("https%3A%2F%2Fa%2Eexample%2Froot%2F"));
        assert!(rewritten.contains("https%3A%2F%2Fa%2Eexample%2Froot%2Fvideo%2F"));
        assert!(rewritten.contains("init.mp4"));
        assert!(rewritten.contains("seg-1.m4s"));
        assert!(rewritten.contains("seg-1.idx"));
    }

    #[test]
    fn rewrites_dynamic_manifest_location_as_manifest_resource() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r"<MPD><Location>next/manifest?id=2&amp;token=x</Location></MPD>",
            "https://cdn.example.com/live/current.mpd",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(rewritten.contains("/proxy/manifest/"));
        assert!(
            rewritten.contains("manifest%3Fid%3D2%26token%3Dx"),
            "{rewritten}"
        );
        assert!(!rewritten.contains("manifest%3Fid%3D2%26token%3Dx/"));
    }

    #[test]
    fn preserves_directory_and_exact_file_base_url_semantics() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r#"<MPD><Period><AdaptationSet><Representation id="directory"><BaseURL>video/</BaseURL></Representation><Representation id="file"><BaseURL>audio.mp4</BaseURL><SegmentBase indexRange="0-100"/></Representation></AdaptationSet></Period></MPD>"#,
            "https://cdn.example.com/root/manifest.mpd",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(
            rewritten.contains("https%3A%2F%2Fcdn%2Eexample%2Ecom%2Froot%2Fvideo%2F/"),
            "{rewritten}"
        );
        assert!(
            rewritten.contains("https%3A%2F%2Fcdn%2Eexample%2Ecom%2Froot%2Faudio%2Emp4</BaseURL>"),
            "{rewritten}"
        );
    }

    #[test]
    fn preserves_relative_templates_across_multiple_base_url_choices() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r#"<MPD><BaseURL>https://primary.example/video/</BaseURL><BaseURL>https://backup.example/video/</BaseURL><Period><AdaptationSet><SegmentTemplate initialization="init-$RepresentationID$.m4s" media="segments/$Number$.m4s"/></AdaptationSet></Period></MPD>"#,
            "https://origin.example/manifest.mpd",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(rewritten.contains("https%3A%2F%2Fprimary%2Eexample%2Fvideo%2F/"));
        assert!(rewritten.contains("https%3A%2F%2Fbackup%2Eexample%2Fvideo%2F/"));
        assert!(rewritten.contains("initialization=\"init-$RepresentationID$.m4s\""));
        assert!(rewritten.contains("media=\"segments/$Number$.m4s\""));
    }

    #[test]
    fn resolves_sibling_relative_base_urls_from_the_same_parent() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r"<MPD><BaseURL>primary/</BaseURL><BaseURL>backup/</BaseURL></MPD>",
            "https://cdn.example.com/root/manifest.mpd",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(
            rewritten.contains("https%3A%2F%2Fcdn%2Eexample%2Ecom%2Froot%2Fprimary%2F/"),
            "{rewritten}"
        );
        assert!(
            rewritten.contains("https%3A%2F%2Fcdn%2Eexample%2Ecom%2Froot%2Fbackup%2F/"),
            "{rewritten}"
        );
        assert!(!rewritten.contains("primary%2Fbackup"), "{rewritten}");
    }

    #[test]
    fn resolves_nested_sibling_base_urls_from_their_parent_level() {
        let rewritten = rewrite_mpd_with_url_mapper(
            r"<MPD><BaseURL>primary/</BaseURL><BaseURL>backup/</BaseURL><Period><BaseURL>video/</BaseURL><BaseURL>audio/</BaseURL></Period></MPD>",
            "https://cdn.example.com/root/manifest.mpd",
            mapper,
        )
        .expect("MPD should rewrite");

        assert!(
            rewritten.contains("https%3A%2F%2Fcdn%2Eexample%2Ecom%2Froot%2Fprimary%2Fvideo%2F/"),
            "{rewritten}"
        );
        assert!(
            rewritten.contains("https%3A%2F%2Fcdn%2Eexample%2Ecom%2Froot%2Fprimary%2Faudio%2F/"),
            "{rewritten}"
        );
        assert!(!rewritten.contains("video%2Faudio"), "{rewritten}");
    }
}
