pub mod errors;

use errors::RtmpUrlParseError;
use errors::RtmpUrlParseErrorValue;

#[derive(Debug, Clone, Default)]
pub struct RtmpUrlParser {
    pub url: String,
    pub host_with_port: String,
    pub host: String,
    pub port: Option<String>,
    pub app_name: String,
    pub stream_name_with_query: String,
    pub stream_name: String,
    pub query: Option<String>,
}

impl RtmpUrlParser {
    #[must_use]
    pub fn new(url: String) -> Self {
        Self {
            url,
            ..Default::default()
        }
    }

    pub fn parse_url(&mut self) -> Result<(), RtmpUrlParseError> {
        if let Some(idx) = self.url.find("rtmp://") {
            let remove_header_left = &self.url[idx + 7..];
            let url_parts: Vec<&str> = remove_header_left.split('/').collect();
            if url_parts.len() != 3 {
                return Err(RtmpUrlParseError {
                    value: RtmpUrlParseErrorValue::Notvalid,
                });
            }

            self.host_with_port = url_parts[0].to_string();
            self.app_name = url_parts[1].to_string();
            self.stream_name_with_query = url_parts[2].to_string();

            self.parse_host_with_port()?;
            (self.stream_name, self.query) =
                Self::parse_stream_name_with_query(&self.stream_name_with_query);
        } else {
            return Err(RtmpUrlParseError {
                value: RtmpUrlParseErrorValue::Notvalid,
            });
        }

        Ok(())
    }

    pub fn parse_host_with_port(&mut self) -> Result<(), RtmpUrlParseError> {
        let data: Vec<&str> = self.host_with_port.split(':').collect();
        self.host = data[0].to_string();
        if data.len() > 1 {
            self.port = Some(data[1].to_string());
        }
        Ok(())
    }
    #[must_use]
    pub fn parse_stream_name_with_query(stream_name_with_query: &str) -> (String, Option<String>) {
        let data: Vec<&str> = stream_name_with_query.split('?').collect();
        let stream_name = data[0].to_string();
        let query = if data.len() > 1 {
            Some(data[1].to_string())
        } else {
            None
        };
        (stream_name, query)
    }
}

#[cfg(test)]
mod tests {

    use super::RtmpUrlParser;
    #[test]
    fn test_rtmp_url_parser() {
        let mut parser = RtmpUrlParser::new(String::from(
            "rtmp://domain.name.cn:1935/app_name/stream_name?auth_key=test_Key",
        ));

        parser.parse_url().unwrap();

        assert_eq!(parser.host, "domain.name.cn", "Parsed host should match");
        assert_eq!(
            parser.port,
            Some("1935".to_string()),
            "Parsed port should be 1935"
        );
        assert_eq!(parser.app_name, "app_name", "Parsed app_name should match");
        assert_eq!(
            parser.stream_name, "stream_name",
            "Parsed stream_name should match"
        );
        assert_eq!(
            parser.query,
            Some("auth_key=test_Key".to_string()),
            "Parsed query string should match"
        );
    }
    #[test]
    fn test_rtmp_url_parser2() {
        let mut parser =
            RtmpUrlParser::new(String::from("rtmp://domain.name.cn/app_name/stream_name"));

        parser.parse_url().unwrap();

        assert_eq!(parser.host, "domain.name.cn", "Parsed host should match");
        assert_eq!(parser.port, None, "Port should be None when not specified");
        assert_eq!(parser.app_name, "app_name", "Parsed app_name should match");
        assert_eq!(
            parser.stream_name, "stream_name",
            "Parsed stream_name should match"
        );
        assert_eq!(parser.query, None, "Query should be None when not present");
    }
}
