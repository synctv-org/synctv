use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Deserialize)]
pub struct QnapLogin {
    #[serde(default, deserialize_with = "i64_from_any")]
    pub status: i64,
    #[serde(default)]
    pub sid: String,
    #[serde(default)]
    pub servername: String,
    #[serde(default)]
    pub username: String,
    #[serde(default, deserialize_with = "i64_from_any")]
    pub admingroup: i64,
    #[serde(default, rename = "supportRTT", deserialize_with = "i64_from_any")]
    pub support_rtt: i64,
    pub version: Option<String>,
    pub build: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QnapStatus {
    #[serde(default, deserialize_with = "i64_from_any")]
    pub status: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QnapShare {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default, rename = "cls")]
    pub permission: String,
    #[serde(default, rename = "iconCls")]
    pub icon: String,
    #[serde(default, deserialize_with = "number_from_any")]
    pub real_total: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QnapList {
    #[serde(default, alias = "real_total", deserialize_with = "number_from_any")]
    pub total: u64,
    #[serde(default, deserialize_with = "optional_i64_from_any")]
    pub status: Option<i64>,
    #[serde(default, deserialize_with = "i64_from_any")]
    pub acl: i64,
    #[serde(default, deserialize_with = "i64_from_any")]
    pub rtt_support: i64,
    #[serde(default)]
    pub datas: Vec<QnapFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QnapFile {
    #[serde(default)]
    pub filename: String,
    #[serde(default, deserialize_with = "number_from_any")]
    pub isfolder: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub filesize: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub epochmt: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub filetype: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub mp4_240: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub mp4_360: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub mp4_480: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub mp4_720: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub mp4_1080: u64,
    #[serde(default, alias = "trnas", deserialize_with = "number_from_any")]
    pub transcode_queued: u64,
    #[serde(default, deserialize_with = "number_from_any")]
    pub play: u64,
}

impl QnapFile {
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.isfolder != 0
    }

    #[must_use]
    pub fn available_mp4_resolutions(&self) -> Vec<QnapTranscodeResolution> {
        [
            (self.mp4_240, QnapTranscodeResolution::P240),
            (self.mp4_360, QnapTranscodeResolution::P360),
            (self.mp4_480, QnapTranscodeResolution::P480),
            (self.mp4_720, QnapTranscodeResolution::P720),
            (self.mp4_1080, QnapTranscodeResolution::P1080),
        ]
        .into_iter()
        .filter_map(|(ready, resolution)| (ready != 0).then_some(resolution))
        .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QnapTranscodeResolution {
    P240,
    P360,
    P480,
    P720,
    P1080,
}

impl QnapTranscodeResolution {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::P240 => "240p",
            Self::P360 => "360p",
            Self::P480 => "480p",
            Self::P720 => "720p",
            Self::P1080 => "1080p",
        }
    }

    #[must_use]
    pub const fn viewer_format(self) -> &'static str {
        match self {
            Self::P240 => "mp4_240",
            Self::P360 => "mp4_360",
            Self::P480 => "mp4_480",
            Self::P720 => "mp4_720",
            Self::P1080 => "mp4_1080",
        }
    }

    #[must_use]
    pub const fn mask(self) -> u32 {
        match self {
            Self::P240 => 1,
            Self::P360 => 2,
            Self::P720 => 4,
            Self::P480 => 8,
            Self::P1080 => 16,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QnapHardwareTranscode {
    #[serde(default, rename = "medialibHWTS", deserialize_with = "i64_from_any")]
    pub media_library_hardware: i64,
    #[serde(default, rename = "QTranscode", deserialize_with = "i64_from_any")]
    pub qtranscode: i64,
    #[serde(default, rename = "mmCodex", deserialize_with = "i64_from_any")]
    pub multimedia_codec: i64,
    #[serde(
        default,
        rename = "hdStationSupport",
        deserialize_with = "i64_from_any"
    )]
    pub hd_station_support: i64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NumberValue {
    Number(u64),
    String(String),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SignedNumberValue {
    Number(i64),
    String(String),
}

fn number_from_any<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<NumberValue>::deserialize(deserializer)?;
    match value {
        None => Ok(0),
        Some(NumberValue::Number(value)) => Ok(value),
        Some(NumberValue::String(value)) => value.trim().parse().map_err(serde::de::Error::custom),
    }
}

fn i64_from_any<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    optional_i64_from_any(deserializer).map(Option::unwrap_or_default)
}

fn optional_i64_from_any<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<SignedNumberValue>::deserialize(deserializer)?;
    value
        .map(|value| match value {
            SignedNumberValue::Number(value) => Ok(value),
            SignedNumberValue::String(value) => {
                value.trim().parse().map_err(serde::de::Error::custom)
            }
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qnap_numeric_strings_across_firmware_variants() {
        let login: QnapLogin = serde_json::from_value(serde_json::json!({
            "status": "1",
            "sid": "session",
            "admingroup": "0",
            "supportRTT": "1"
        }))
        .expect("login numeric strings should parse");
        assert_eq!(login.status, 1);
        assert_eq!(login.support_rtt, 1);

        let list: QnapList = serde_json::from_value(serde_json::json!({
            "total": "250",
            "status": "1",
            "acl": "7",
            "rtt_support": "1",
            "datas": []
        }))
        .expect("list numeric strings should parse");
        assert_eq!(list.total, 250);
        assert_eq!(list.status, Some(1));
        assert_eq!(list.acl, 7);
        assert_eq!(list.rtt_support, 1);

        let hardware: QnapHardwareTranscode = serde_json::from_value(serde_json::json!({
            "medialibHWTS": "1",
            "QTranscode": 1,
            "mmCodex": "1",
            "hdStationSupport": "0"
        }))
        .expect("hardware capability numeric strings should parse");
        assert_eq!(hardware.media_library_hardware, 1);
        assert_eq!(hardware.qtranscode, 1);
        assert_eq!(hardware.multimedia_codec, 1);
        assert_eq!(hardware.hd_station_support, 0);
    }
}
