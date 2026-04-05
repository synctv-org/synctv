use {
    super::{amf0_markers, errors::Amf0ReadErrorValue, Amf0ReadError, Amf0ValueType},
    // bytes::BytesMut,
    crate::bytesio::bytes_reader::BytesReader,
    byteorder::BigEndian,
    indexmap::IndexMap,
};

/// Maximum nesting depth for AMF0 objects and arrays
pub const MAX_DEPTH: usize = 32;

/// Maximum number of keys in an AMF0 object or ECMA array
pub const MAX_KEYS: usize = 1000;

pub struct Amf0Reader {
    reader: BytesReader,
    depth: usize,
}

impl Amf0Reader {
    #[must_use]
    pub const fn new(reader: BytesReader) -> Self {
        Self { reader, depth: 0 }
    }

    const fn check_depth(&self) -> Result<(), Amf0ReadError> {
        if self.depth > MAX_DEPTH {
            return Err(Amf0ReadError {
                value: Amf0ReadErrorValue::DepthLimitExceeded {
                    depth: self.depth,
                    max: MAX_DEPTH,
                },
            });
        }
        Ok(())
    }

    const fn enter_nested(&mut self) -> Result<(), Amf0ReadError> {
        self.depth += 1;
        self.check_depth()
    }

    const fn exit_nested(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    pub fn read_all(&mut self) -> Result<Vec<Amf0ValueType>, Amf0ReadError> {
        let mut results = vec![];

        loop {
            let result = self.read_any()?;
            match result {
                Amf0ValueType::END => {
                    break;
                }
                _ => {
                    results.push(result);
                }
            }
        }
        Ok(results)
    }
    pub fn read_any(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        if self.reader.is_empty() {
            return Ok(Amf0ValueType::END);
        }
        let markers = self.reader.read_u8()?;

        if markers == amf0_markers::OBJECT_END {
            return Ok(Amf0ValueType::END);
        }

        match markers {
            amf0_markers::NUMBER => self.read_number(),
            amf0_markers::BOOLEAN => self.read_bool(),
            amf0_markers::STRING => self.read_string(),
            amf0_markers::OBJECT => self.read_object(),
            amf0_markers::NULL => self.read_null(),
            amf0_markers::ECMA_ARRAY => self.read_ecma_array(),
            amf0_markers::STRICT_ARRAY => self.read_strict_array(),
            amf0_markers::LONG_STRING => self.read_long_string(),
            _ => Err(Amf0ReadError {
                value: Amf0ReadErrorValue::UnknownMarker { marker: markers },
            }),
        }
    }
    pub fn read_with_type(&mut self, specified_marker: u8) -> Result<Amf0ValueType, Amf0ReadError> {
        let marker = self.reader.advance_u8()?;

        if marker != specified_marker {
            return Err(Amf0ReadError {
                value: Amf0ReadErrorValue::WrongType,
            });
        }

        self.read_any()
    }

    pub fn read_number(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let number = self.reader.read_f64::<BigEndian>()?;
        let value = Amf0ValueType::Number(number);
        Ok(value)
    }

    pub fn read_bool(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let value = self.reader.read_u8()?;

        match value {
            1 => Ok(Amf0ValueType::Boolean(true)),
            _ => Ok(Amf0ValueType::Boolean(false)),
        }
    }

    /// Maximum length for AMF0 short strings (64 KB)
    const MAX_SHORT_STRING_LENGTH: usize = 64 * 1024;

    pub fn read_raw_string(&mut self) -> Result<String, Amf0ReadError> {
        let l = self.reader.read_u16::<BigEndian>()? as usize;

        if l > Self::MAX_SHORT_STRING_LENGTH {
            return Err(Amf0ReadError {
                value: Amf0ReadErrorValue::StringTooLong {
                    length: l,
                    max: Self::MAX_SHORT_STRING_LENGTH,
                },
            });
        }

        let bytes = self.reader.read_bytes(l)?;
        let val = String::from_utf8(bytes.to_vec())?;

        Ok(val)
    }

    pub fn read_string(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let raw_string = self.read_raw_string()?;
        Ok(Amf0ValueType::UTF8String(raw_string))
    }

    pub const fn read_null(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        Ok(Amf0ValueType::Null)
    }

    pub fn is_read_object_eof(&mut self) -> Result<bool, Amf0ReadError> {
        let marker = self.reader.advance_u24::<BigEndian>()?;
        if marker == u32::from(amf0_markers::OBJECT_END) {
            self.reader.read_u24::<BigEndian>()?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn read_object(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        self.enter_nested()?;
        let result = self.read_object_inner();
        self.exit_nested();
        result
    }

    fn read_object_inner(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let mut properties = IndexMap::new();

        loop {
            let is_eof = self.is_read_object_eof()?;

            if is_eof {
                break;
            }

            // Check key count limit
            if properties.len() >= MAX_KEYS {
                return Err(Amf0ReadError {
                    value: Amf0ReadErrorValue::KeyLimitExceeded {
                        count: properties.len() + 1,
                        max: MAX_KEYS,
                    },
                });
            }

            let key = self.read_raw_string()?;
            let val = self.read_any()?;

            properties.insert(key, val);
        }

        Ok(Amf0ValueType::Object(properties))
    }

    pub fn read_ecma_array(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        self.enter_nested()?;
        let result = self.read_ecma_array_inner();
        self.exit_nested();
        result
    }

    fn read_ecma_array_inner(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let len = self.reader.read_u32::<BigEndian>()?;

        let mut properties = IndexMap::new();

        //here we do not use length to traverse the map, because in some
        //other media server, the length is 0 which is not correct.
        while !self.is_read_object_eof()? {
            // Check key count limit
            if properties.len() >= MAX_KEYS {
                return Err(Amf0ReadError {
                    value: Amf0ReadErrorValue::KeyLimitExceeded {
                        count: properties.len() + 1,
                        max: MAX_KEYS,
                    },
                });
            }

            let key = self.read_raw_string()?;
            let val = self.read_any()?;
            properties.insert(key, val);
        }

        if len != properties.len() as u32 {
            tracing::warn!("the ecma array length is not correct!");
        }

        Ok(Amf0ValueType::Object(properties))
    }

    pub fn read_strict_array(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        self.enter_nested()?;
        let result = self.read_strict_array_inner();
        self.exit_nested();
        result
    }

    fn read_strict_array_inner(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let len = self.reader.read_u32::<BigEndian>()? as usize;

        if len > MAX_KEYS {
            return Err(Amf0ReadError {
                value: Amf0ReadErrorValue::KeyLimitExceeded {
                    count: len,
                    max: MAX_KEYS,
                },
            });
        }

        let mut values = Vec::with_capacity(len);
        for _ in 0..len {
            values.push(self.read_any()?);
        }

        Ok(Amf0ValueType::StrictArray(values))
    }

    /// Maximum length for AMF0 long strings (16 MB)
    const MAX_LONG_STRING_LENGTH: usize = 16 * 1024 * 1024;

    pub fn read_long_string(&mut self) -> Result<Amf0ValueType, Amf0ReadError> {
        let l = self.reader.read_u32::<BigEndian>()? as usize;

        if l > Self::MAX_LONG_STRING_LENGTH {
            return Err(Amf0ReadError {
                value: Amf0ReadErrorValue::StringTooLong {
                    length: l,
                    max: Self::MAX_LONG_STRING_LENGTH,
                },
            });
        }

        let buff = self.reader.read_bytes(l)?;

        let val = String::from_utf8(buff.to_vec())?;
        Ok(Amf0ValueType::LongUTF8String(val))
    }

    // pub fn get_remaining_bytes(&mut self) -> BytesMut {
    //     return self.reader.get_remaining_bytes();
    // }
}

#[cfg(test)]
mod tests {

    #[test]
    fn test_byte_order() {
        use byteorder::{BigEndian, ByteOrder};

        let phi = 1.618_033_988_7;
        let mut buf = [0; 8];
        BigEndian::write_f64(&mut buf, phi);
        assert_eq!(phi, BigEndian::read_f64(&buf));
        println!("tsetstt");
    }

    use super::amf0_markers;
    use super::Amf0Reader;
    use super::Amf0ValueType;

    use crate::bytesio::bytes_reader::BytesReader;
    use bytes::BytesMut;

    use indexmap::IndexMap;

    #[test]
    fn test_amf_reader() {
        let data: [u8; 177] = [
            2, 0, 7, 99, 111, 110, 110, 101, 99, 116, 0, 63, 240, 0, 0, 0, 0, 0, 0, //body
            3, 0, 3, 97, 112, 112, 2, 0, 6, 104, 97, 114, 108, 97, 110, 0, 4, 116, 121, 112, 101,
            2, 0, 10, 110, 111, 110, 112, 114, 105, 118, 97, 116, 101, 0, 8, 102, 108, 97, 115,
            104, 86, 101, 114, 2, 0, 31, 70, 77, 76, 69, 47, 51, 46, 48, 32, 40, 99, 111, 109, 112,
            97, 116, 105, 98, 108, 101, 59, 32, 70, 77, 83, 99, 47, 49, 46, 48, 41, 0, 6, 115, 119,
            102, 85, 114, 108, 2, 0, 28, 114, 116, 109, 112, 58, 47, 47, 108, 111, 99, 97, 108,
            104, 111, 115, 116, 58, 49, 57, 51, 53, 47, 104, 97, 114, 108, 97, 110, 0, 5, 116, 99,
            85, 114, 108, 2, 0, 28, 114, 116, 109, 112, 58, 47, 47, 108, 111, 99, 97, 108, 104,
            111, 115, 116, 58, 49, 57, 51, 53, 47, 104, 97, 114, 108, 97, 110, 0, 0, 9,
        ];

        let mut bytes_reader = BytesReader::new(BytesMut::new());
        bytes_reader.extend_from_slice(&data).unwrap();
        let mut amf_reader = Amf0Reader::new(bytes_reader);

        let command_name = amf_reader.read_with_type(amf0_markers::STRING).unwrap();
        assert_eq!(
            command_name,
            Amf0ValueType::UTF8String(String::from("connect"))
        );

        let transaction_id = amf_reader.read_with_type(amf0_markers::NUMBER).unwrap();
        assert_eq!(transaction_id, Amf0ValueType::Number(1.0));

        let command_obj_raw = amf_reader.read_with_type(amf0_markers::OBJECT).unwrap();
        let mut properties = IndexMap::new();
        properties.insert(
            String::from("app"),
            Amf0ValueType::UTF8String(String::from("harlan")),
        );
        properties.insert(
            String::from("type"),
            Amf0ValueType::UTF8String(String::from("nonprivate")),
        );
        properties.insert(
            String::from("flashVer"),
            Amf0ValueType::UTF8String(String::from("FMLE/3.0 (compatible; FMSc/1.0)")),
        );
        properties.insert(
            String::from("swfUrl"),
            Amf0ValueType::UTF8String(String::from("rtmp://localhost:1935/harlan")),
        );
        properties.insert(
            String::from("tcUrl"),
            Amf0ValueType::UTF8String(String::from("rtmp://localhost:1935/harlan")),
        );
        assert_eq!(command_obj_raw, Amf0ValueType::Object(properties));

        let _ = amf_reader.read_all();

        print!("test");
    }

    // fn uint32_to_int24(num: u32) -> i32 {
    //     // 截取低24位
    //     let mut result = num & 0xFFFFFF;

    //     let mut result2: i32 = result as i32;

    //     // 判断最高位是否为1
    //     if (result & 0x800000) == 0x800000 {
    //         // 获取补码表示
    //         result = (result ^ 0xFFFFFF) + 1;

    //         result2 = result as i32 * -1;
    //     }

    //     result2
    // }

    fn bytes_to_i24(bytes: [u8; 3]) -> i32 {
        let sign_extend_mask = 0xff_ff << 23;
        let value = (i32::from(bytes[0]) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[2]);

        if value & (1 << 23) != 0 {
            // Sign extend the value
            value | sign_extend_mask
        } else {
            value
        }
    }
    #[test]
    fn test_number() {
        let data: [u8; 3] = [0xFF, 0xFF, 0xF0];

        let mut bytes_reader = BytesReader::new(BytesMut::new());

        bytes_reader.extend_from_slice(&data).unwrap();

        let mut t: u32 = 0;

        for _ in 0..3 {
            let time = bytes_reader.read_u8().unwrap();
            //print!("==time0=={}\n", time);
            //print!("==time1=={}\n", self.tag.composition_time);
            t = (t << 8) + u32::from(time);
        }

        println!("number: {}", bytes_to_i24(data));
    }

    #[test]
    fn test_player_connect_reader() {
        // chunk header
        // 0000   03 00 00 00 00 00 aa 14 00 00 00 00
        //amf0 data
        //                                            02 00 07 63  ...............c
        // 0010   6f 6e 6e 65 63 74 00 3f f0 00 00 00 00 00 00 03  onnect.?........
        // 0020   00 03 61 70 70 02 00 04 6c 69 76 65 00 05 74 63  ..app...live..tc
        // 0030   55 72 6c 02 00 1a 72 74 6d 70 3a 2f 2f 6c 6f 63  Url...rtmp://loc
        // 0040   61 6c 68 6f 73 74 3a 31 39 33 35 2f 6c 69 76 65  alhost:1935/live
        // 0050   00 04 66 70 61 64 01 00 00 0c 63 61 70 61 62 69  ..fpad....capabi
        // 0060   6c 69 74 69 65 73 00 40 2e 00 00 00 00 00 00 00  lities.@........
        // 0070   0b 61 75 64 69 6f 43 6f 64 65 63 73 00 40 a8 ee  .audioCodecs.@..
        // 0080   00 00 00 00 00 00 0b 76 69 64 65 6f 43 6f 64 65  .......videoCode 118 105
        // 0090   63 73 00 40 6f 80 00 00 00 00 00 00 0d 76 69 64  cs.@o........vid
        // 00a0   65 6f 46 75 6e 63 74 69 6f 6e 00 3f f0 00 00 00  eoFunction.?....
        // 0b00   00 00 00 00 00 09                                ......

        let data: [u8; 170] = [
            0x02, 0x00, 0x07, 0x63, 0x6f, 0x6e, 0x6e, 0x65, 0x63, 0x74, 0x00, 0x3f, 0xf0, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x03, 0x61, 0x70, 0x70, 0x02, 0x00, 0x04,
            0x6c, 0x69, 0x76, 0x65, 0x00, 0x05, 0x74, 0x63, 0x55, 0x72, 0x6c, 0x02, 0x00, 0x1a,
            0x72, 0x74, 0x6d, 0x70, 0x3a, 0x2f, 0x2f, 0x6c, 0x6f, 0x63, 0x61, 0x6c, 0x68, 0x6f,
            0x73, 0x74, 0x3a, 0x31, 0x39, 0x33, 0x35, 0x2f, 0x6c, 0x69, 0x76, 0x65, 0x00, 0x04,
            0x66, 0x70, 0x61, 0x64, 0x01, 0x00, 0x00, 0x0c, 0x63, 0x61, 0x70, 0x61, 0x62, 0x69,
            0x6c, 0x69, 0x74, 0x69, 0x65, 0x73, 0x00, 0x40, 0x2e, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x0b, 0x61, 0x75, 0x64, 0x69, 0x6f, 0x43, 0x6f, 0x64, 0x65, 0x63, 0x73,
            0x00, 0x40, 0xa8, 0xee, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0b, 0x76, 0x69, 0x64,
            0x65, 0x6f, 0x43, 0x6f, 0x64, 0x65, 0x63, 0x73, 0x00, 0x40, 0x6f, 0x80, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x0d, 0x76, 0x69, 0x64, 0x65, 0x6f, 0x46, 0x75, 0x6e, 0x63,
            0x74, 0x69, 0x6f, 0x6e, 0x00, 0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x09,
        ];

        //76 69 64 65 6f 43 6f 64 65 63 73
        // 118 105 100 101  111 67 111 100 101 99 115

        let mut bytes_reader = BytesReader::new(BytesMut::new());
        bytes_reader.extend_from_slice(&data).unwrap();
        let mut amf_reader = Amf0Reader::new(bytes_reader);

        let command_name = amf_reader.read_with_type(amf0_markers::STRING).unwrap();

        assert_eq!(
            command_name,
            Amf0ValueType::UTF8String(String::from("connect"))
        );

        let transaction_id = amf_reader.read_with_type(amf0_markers::NUMBER).unwrap();
        assert_eq!(transaction_id, Amf0ValueType::Number(1.0));

        let command_obj_raw = amf_reader.read_with_type(amf0_markers::OBJECT);

        if let Err(err) = &command_obj_raw {
            println!("adfa{err}");
        }

        let mut properties = IndexMap::new();

        properties.insert(String::from("audioCodecs"), Amf0ValueType::Number(3191.0));
        properties.insert(String::from("videoCodecs"), Amf0ValueType::Number(252.0));
        properties.insert(String::from("videoFunction"), Amf0ValueType::Number(1.0));
        properties.insert(
            String::from("tcUrl"),
            Amf0ValueType::UTF8String(String::from("rtmp://localhost:1935/live")),
        );

        properties.insert(
            String::from("app"),
            Amf0ValueType::UTF8String(String::from("live")),
        );

        properties.insert(String::from("fpad"), Amf0ValueType::Boolean(false));
        properties.insert(String::from("capabilities"), Amf0ValueType::Number(15.0));

        // let result = amf_writer.write_any(&Amf0ValueType::Object(properties));

        // print::printu8(amf_writer.get_current_bytes());

        assert_eq!(command_obj_raw.unwrap(), Amf0ValueType::Object(properties));
    }
}
