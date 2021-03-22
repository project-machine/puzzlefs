use std::collections::HashMap;

extern crate hex;

use serde::{Deserialize, Serialize};

const NAME_ANNOTATION: &str = "org.opencontainers.image.ref.name";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Descriptor {
    pub digest: [u8; 32],
    pub len: u64,
    // TODO: media_type
    pub annotations: HashMap<String, String>,
}

impl Descriptor {
    pub fn new(digest: [u8; 32], len: u64) -> Descriptor {
        Descriptor {
            digest,
            len,
            annotations: HashMap::new(),
        }
    }

    pub fn digest_as_str(&self) -> String {
        hex::encode(&self.digest)
    }

    pub fn set_name(&mut self, name: String) {
        self.annotations.insert(NAME_ANNOTATION.to_string(), name);
    }

    pub fn get_name(&self) -> Option<&String> {
        self.annotations.get(NAME_ANNOTATION)
    }
}
