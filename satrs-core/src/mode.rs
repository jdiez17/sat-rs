use core::mem::size_of;
use crate::tmtc::TargetId;
use serde::{Deserialize, Serialize};
use spacepackets::{ByteConversionError, SizeMissmatch};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ModeAndSubmode {
    mode: u32,
    submode: u16,
}

impl ModeAndSubmode {
    pub const fn new_mode_only(mode: u32) -> Self {
        Self {
            mode,
            submode: 0
        }
    }

    pub const fn new(mode: u32, submode: u16) -> Self {
        Self {
            mode,
            submode
        }
    }

    pub fn raw_len() -> usize {
        size_of::<u32>() + size_of::<u16>()
    }

    pub fn from_be_bytes(buf: &[u8]) -> Result<Self, ByteConversionError> {
        if buf.len() < 6 {
            return Err(ByteConversionError::FromSliceTooSmall(SizeMissmatch {
                expected: 6,
                found: buf.len()
            }));
        }
        Ok(Self {
            mode: u32::from_be_bytes(buf[0..4].try_into().unwrap()),
            submode: u16::from_be_bytes(buf[4..6].try_into().unwrap())
        })
    }
}
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ModeCommand {
    address: TargetId,
    mode_submode: ModeAndSubmode,
}

impl ModeCommand {
    pub const fn new(address: TargetId, mode_submode: ModeAndSubmode) -> Self {
        Self {
            address,
            mode_submode
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum ModeRequest {
    SetMode(ModeCommand),
    ReadMode(TargetId),
    AnnounceMode(TargetId),
    AnnounceModeRecursive(TargetId),
}
