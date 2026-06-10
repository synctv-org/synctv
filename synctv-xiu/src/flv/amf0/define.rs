use indexmap::IndexMap;

#[derive(PartialEq, Clone, Debug)]
pub enum Amf0ValueType {
    Number(f64),
    Boolean(bool),
    UTF8String(String),
    Object(IndexMap<String, Self>),
    StrictArray(Vec<Self>),
    Null,
    EcmaArray(IndexMap<String, Self>),
    LongUTF8String(String),
    END,
}
