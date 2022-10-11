use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::convert::{TryFrom, TryInto};
use std::fmt;

extern crate hex;

use serde::de::Error as SerdeError;
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use format::{BlobRef, BlobRefKind, WireFormatError};

const SHA256_BLOCK_SIZE: usize = 32;
const NAME_ANNOTATION: &str = "org.opencontainers.image.ref.name";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest([u8; SHA256_BLOCK_SIZE]);

impl Digest {
    pub fn underlying(&self) -> [u8; SHA256_BLOCK_SIZE] {
        let mut dest = [0_u8; SHA256_BLOCK_SIZE];
        dest.copy_from_slice(&self.0);
        dest
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", hex::encode(&self.0))
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let val = format!("sha256:{}", hex::encode(&self.0));
        serializer.serialize_str(&val)
    }
}

impl TryFrom<BlobRef> for Digest {
    type Error = WireFormatError;
    fn try_from(v: BlobRef) -> std::result::Result<Self, Self::Error> {
        match v.kind {
            BlobRefKind::Other { digest } => Ok(Digest(digest)),
            BlobRefKind::Local => Err(WireFormatError::LocalRefError(Backtrace::capture())),
        }
    }
}

impl TryFrom<&BlobRef> for Digest {
    type Error = WireFormatError;
    fn try_from(v: &BlobRef) -> std::result::Result<Self, Self::Error> {
        match v.kind {
            BlobRefKind::Other { digest } => Ok(Digest(digest)),
            BlobRefKind::Local => Err(WireFormatError::LocalRefError(Backtrace::capture())),
        }
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Digest, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DigestVisitor;

        impl<'de> Visitor<'de> for DigestVisitor {
            type Value = Digest;

            fn expecting(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_fmt(format_args!("expected 'sha256:<hex encoded hash>'"))
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: SerdeError,
            {
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() != 2 {
                    return Err(SerdeError::custom(format!("bad digest {}", s)));
                }

                match parts[0] {
                    "sha256" => {
                        let buf =
                            hex::decode(parts[1]).map_err(|e| SerdeError::custom(e.to_string()))?;

                        let len = buf.len();
                        let digest: [u8; SHA256_BLOCK_SIZE] = buf.try_into().map_err(|_| {
                            SerdeError::custom(format!("invalid sha256 block length {}", len))
                        })?;
                        Ok(Digest(digest))
                    }
                    _ => Err(SerdeError::custom(format!(
                        "unknown digest type {}",
                        parts[0]
                    ))),
                }
            }
        }

        deserializer.deserialize_str(DigestVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    pub digest: Digest,
    pub size: u64,
    pub media_type: String,
    pub annotations: HashMap<String, String>,
}

impl Descriptor {
    pub fn new(digest: [u8; 32], size: u64, media_type: String) -> Descriptor {
        Descriptor {
            digest: Digest(digest),
            size,
            media_type,
            annotations: HashMap::new(),
        }
    }

    pub fn set_name(&mut self, name: String) {
        self.annotations.insert(NAME_ANNOTATION.to_string(), name);
    }

    pub fn get_name(&self) -> Option<&String> {
        self.annotations.get(NAME_ANNOTATION)
    }

    pub(crate) fn remove_name(&mut self) {
        self.annotations.remove_entry(NAME_ANNOTATION);
    }
}
