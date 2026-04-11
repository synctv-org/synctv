use {
    super::errors::NetConnectionError,
    crate::bytesio::bytesio::TNetIO,
    crate::flv::amf0::{amf0_writer::Amf0Writer, define::Amf0ValueType},
    crate::rtmp::{
        chunk::{define as chunk_define, packetizer::ChunkPacketizer, ChunkInfo},
        messages::define as messages_define,
    },
    indexmap::IndexMap,
    std::sync::Arc,
    tokio::sync::Mutex,
};
#[derive(Clone, Default, Debug)]
pub struct ConnectProperties {
    pub app: Option<String>,         // Server application name, e.g.: testapp
    pub flash_ver: Option<String>,   // Flash Player version, FMSc/1.0
    pub swf_url: Option<String>,     // URL of the source SWF file file://C:/FlvPlayer.swf
    pub tc_url: Option<String>,      // URL of the Server, rtmp://host:1935/testapp/instance1
    pub fpad: Option<bool>,          // True if proxy is being used.
    pub capabilities: Option<f64>,   // double default: 15
    pub audio_codecs: Option<f64>,   // double default: 4071
    pub video_codecs: Option<f64>,   // double default: 252
    pub video_function: Option<f64>, // double default: 1
    pub object_encoding: Option<f64>,
    pub page_url: Option<String>, // http://host/sample.html
    pub pub_type: Option<String>,
}

impl ConnectProperties {
    #[must_use]
    pub fn new(app_name: String) -> Self {
        Self {
            app: Some(app_name),
            flash_ver: Some("LNX 9,0,124,2".to_string()),
            swf_url: Some(String::new()),
            tc_url: Some(String::new()),
            fpad: Some(false),
            capabilities: Some(15_f64),
            audio_codecs: Some(4071_f64),
            video_codecs: Some(252_f64),
            video_function: Some(1_f64),
            object_encoding: Some(0_f64),
            page_url: Some(String::new()),
            pub_type: Some("nonprivate".to_string()),
        }
    }
    #[must_use]
    pub const fn new_none() -> Self {
        Self {
            app: None,
            flash_ver: None,
            swf_url: None,
            tc_url: None,
            fpad: None,
            capabilities: None,
            audio_codecs: None,
            video_codecs: None,
            video_function: None,
            object_encoding: None,
            page_url: None,
            pub_type: None,
        }
    }
}

pub struct NetConnection {
    amf0_writer: Amf0Writer,
    packetizer: ChunkPacketizer,
}

impl NetConnection {
    pub fn new(io: Arc<Mutex<Box<dyn TNetIO + Send + Sync>>>) -> Self {
        Self {
            amf0_writer: Amf0Writer::new(),
            packetizer: ChunkPacketizer::new(io),
        }
    }

    async fn write_chunk(&mut self) -> Result<(), NetConnectionError> {
        let data = self.amf0_writer.extract_current_bytes();
        let data_len = u32::try_from(data.len()).map_err(|_| NetConnectionError {
            value: super::errors::NetConnectionErrorValue::MessageTooLarge(data.len()),
        })?;
        let mut chunk_info = ChunkInfo::new(
            chunk_define::csid_type::COMMAND_AMF0_AMF3,
            chunk_define::chunk_type::TYPE_0,
            0,
            data_len,
            messages_define::msg_type_id::COMMAND_AMF0,
            0,
            data,
        );

        self.packetizer.write_chunk(&mut chunk_info).await?;
        Ok(())
    }

    pub async fn write_connect_with_value(
        &mut self,
        transaction_id: &f64,
        properties: IndexMap<String, Amf0ValueType>,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer.write_string(&String::from("connect"))?;
        self.amf0_writer.write_number(transaction_id)?;

        self.amf0_writer.write_object(&properties)?;

        self.write_chunk().await
    }
    pub async fn write_connect(
        &mut self,
        transaction_id: &f64,
        properties: &ConnectProperties,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer.write_string(&String::from("connect"))?;
        self.amf0_writer.write_number(transaction_id)?;

        let mut properties_map = IndexMap::new();

        if let Some(app) = properties.app.clone() {
            properties_map.insert(String::from("app"), Amf0ValueType::UTF8String(app));
        }

        if let Some(pub_type) = properties.pub_type.clone() {
            properties_map.insert(String::from("type"), Amf0ValueType::UTF8String(pub_type));
        }

        if let Some(flash_ver) = properties.flash_ver.clone() {
            properties_map.insert(
                String::from("flashVer"),
                Amf0ValueType::UTF8String(flash_ver),
            );
        }

        if let Some(tc_url) = properties.tc_url.clone() {
            properties_map.insert(String::from("tcUrl"), Amf0ValueType::UTF8String(tc_url));
        }

        if let Some(swf_url) = properties.swf_url.clone() {
            properties_map.insert(String::from("swfUrl"), Amf0ValueType::UTF8String(swf_url));
        }

        if let Some(page_url) = properties.page_url.clone() {
            properties_map.insert(String::from("pageUrl"), Amf0ValueType::UTF8String(page_url));
        }

        if let Some(fpad) = properties.fpad {
            properties_map.insert(String::from("fpad"), Amf0ValueType::Boolean(fpad));
        }

        if let Some(capabilities) = properties.capabilities {
            properties_map.insert(
                String::from("capabilities"),
                Amf0ValueType::Number(capabilities),
            );
        }

        if let Some(audio_codecs) = properties.audio_codecs {
            properties_map.insert(
                String::from("audioCodecs"),
                Amf0ValueType::Number(audio_codecs),
            );
        }

        if let Some(video_codecs) = properties.video_codecs {
            properties_map.insert(
                String::from("videoCodecs"),
                Amf0ValueType::Number(video_codecs),
            );
        }

        if let Some(video_function) = properties.video_function {
            properties_map.insert(
                String::from("videoFunction"),
                Amf0ValueType::Number(video_function),
            );
        }

        if let Some(object_encoding) = properties.object_encoding {
            properties_map.insert(
                String::from("objectEncoding"),
                Amf0ValueType::Number(object_encoding),
            );
        }
        self.amf0_writer.write_object(&properties_map)?;

        self.write_chunk().await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn write_connect_response(
        &mut self,
        transaction_id: &f64,
        fmsver: &str,
        capabilities: &f64,
        code: &str,
        level: &str,
        description: &str,
        encoding: &f64,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer.write_string(&String::from("_result"))?;
        self.amf0_writer.write_number(transaction_id)?;

        let mut properties_map_a = IndexMap::new();

        properties_map_a.insert(
            String::from("fmsVer"),
            Amf0ValueType::UTF8String(fmsver.to_owned()),
        );
        properties_map_a.insert(
            String::from("capabilities"),
            Amf0ValueType::Number(*capabilities),
        );

        self.amf0_writer.write_object(&properties_map_a)?;

        let mut properties_map_b = IndexMap::new();

        properties_map_b.insert(
            String::from("level"),
            Amf0ValueType::UTF8String(level.to_owned()),
        );
        properties_map_b.insert(
            String::from("code"),
            Amf0ValueType::UTF8String(code.to_owned()),
        );
        properties_map_b.insert(
            String::from("description"),
            Amf0ValueType::UTF8String(description.to_owned()),
        );
        properties_map_b.insert(
            String::from("objectEncoding"),
            Amf0ValueType::Number(*encoding),
        );

        self.amf0_writer.write_object(&properties_map_b)?;

        self.write_chunk().await
    }

    pub async fn write_create_stream(
        &mut self,
        transaction_id: &f64,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer
            .write_string(&String::from("createStream"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;

        self.write_chunk().await
    }

    pub async fn write_create_stream_response(
        &mut self,
        transaction_id: &f64,
        stream_id: &f64,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer.write_string(&String::from("_result"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_number(stream_id)?;

        self.write_chunk().await
    }

    pub async fn write_get_stream_length(
        &mut self,
        transaction_id: &f64,
        stream_name: &String,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer
            .write_string(&String::from("getStreamLength"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;
        self.amf0_writer.write_string(stream_name)?;

        self.write_chunk().await
    }

    pub async fn error(
        &mut self,
        transaction_id: &f64,
        code: &str,
        level: &str,
        description: &str,
    ) -> Result<(), NetConnectionError> {
        self.amf0_writer.write_string(&String::from("_error"))?;
        self.amf0_writer.write_number(transaction_id)?;
        self.amf0_writer.write_null()?;

        let mut properties_map = IndexMap::new();

        properties_map.insert(
            String::from("level"),
            Amf0ValueType::UTF8String(level.to_owned()),
        );
        properties_map.insert(
            String::from("code"),
            Amf0ValueType::UTF8String(code.to_owned()),
        );
        properties_map.insert(
            String::from("description"),
            Amf0ValueType::UTF8String(description.to_owned()),
        );
        self.amf0_writer.write_object(&properties_map)?;

        self.write_chunk().await
    }
}
