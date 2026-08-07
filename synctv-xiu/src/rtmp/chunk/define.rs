pub mod csid_type {
    pub const PROTOCOL_USER_CONTROL: u32 = 2;
    pub const COMMAND_AMF0_AMF3: u32 = 3;
    pub const AUDIO: u32 = 4;
    pub const VIDEO: u32 = 5;
    pub const DATA_AMF0_AMF3: u32 = 6;
}

pub mod chunk_type {
    pub const TYPE_0: u8 = 0;
    pub const TYPE_1: u8 = 1;
    pub const TYPE_2: u8 = 2;
    pub const TYPE_3: u8 = 3;
}

pub const CHUNK_SIZE: u32 = 4096;
pub const INIT_CHUNK_SIZE: u32 = 128;
