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

// pub struct Amf0Object {
//     pub key: String,
//     pub value: Amf0ValueType,
// }

// impl Amf0Object {
//     pub fn clone(self) -> Amf0Object {
//         Amf0Object {
//             key: self.key.clone(),
//             value: self.value.clone(),
//         }
//     }
// }

// pub struct AmfObjectMap{
//     properties: HashMap<String,Amf0Object>,

// }

// impl AmfObjectMap {
//     fn Clone(self) -> AmfObjectMap {
//        self.properties.clone()
//     }
// }

// pub struct UnOrderedMap {
//     properties: Vec<Amf0Object>,
// }

// impl UnOrderedMap {
//     pub fn new() -> UnOrderedMap {
//         UnOrderedMap {
//             properties: Vec::new(),
//         }
//     }
//     pub fn insert(self, key: String, val: Amf0ValueType) -> Option<Amf0ValueType> {
//         for i in &self.properties {
//             if i.key == key {
//                 let tmpVal = i.value.clone();
//                 i.value = val;
//                 return Some(tmpVal);
//             }
//         }

//         let obj = Amf0Object {
//             key,
//             value: val,
//         };
//         self.properties.push(obj);

//         None
//     }
//     fn get_by_key(self, key: String) -> Option<Amf0ValueType> {
//         for i in self.properties {
//             if i.key == key {
//                 return Some(i.value);
//             }
//         }
//         None
//     }

//     pub fn get(self, idx: usize) -> Amf0Object {
//         self.properties[idx]
//     }

//     pub fn len(self) -> usize {
//         self.properties.len()
//     }

//     pub fn Clone(self) -> UnOrderedMap {
//         UnOrderedMap {
//             properties: self.properties.clone(),
//         }
//     }
// }
