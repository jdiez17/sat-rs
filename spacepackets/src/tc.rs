use crate::ecss::{PusPacket, PusVersion};
use crate::CCSDS_HEADER_LEN;
use std::mem::size_of;

type CrcType = u16;

/// PUS C secondary header length is fixed
pub const PUC_TC_SECONDARY_HEADER_LEN: usize = size_of::<zc::PusTcDataFieldHeader>();
pub const PUS_TC_MIN_LEN_WITHOUT_APP_DATA: usize =
    CCSDS_HEADER_LEN + PUC_TC_SECONDARY_HEADER_LEN + size_of::<CrcType>();
const PUS_VERSION: PusVersion = PusVersion::PusC;

#[derive(Copy, Clone, PartialEq, Debug)]
enum AckOpts {
    Acceptance = 0b1000,
    Start = 0b0100,
    Progress = 0b0010,
    Completion = 0b0001,
}

pub const ACK_ALL: u8 = AckOpts::Acceptance as u8
    | AckOpts::Start as u8
    | AckOpts::Progress as u8
    | AckOpts::Completion as u8;

pub trait PusTcSecondaryHeader {
    fn ack_flags(&self) -> u8;
    fn service(&self) -> u8;
    fn subservice(&self) -> u8;
    fn source_id(&self) -> u16;
}

impl<T: PusPacket> PusTcSecondaryHeader for T {
    fn ack_flags(&self) -> u8 {
        self.ack_flags()
    }

    fn service(&self) -> u8 {
        self.service()
    }

    fn subservice(&self) -> u8 {
        self.subservice()
    }

    fn source_id(&self) -> u16 {
        self.source_id()
    }
}

pub mod zc {
    use crate::ecss::{PusError, PusVersion};
    use crate::tc::{srd, PusTcSecondaryHeader};
    use zerocopy::{AsBytes, FromBytes, NetworkEndian, Unaligned, U16};

    #[derive(FromBytes, AsBytes, Unaligned)]
    #[repr(C)]
    pub struct PusTcDataFieldHeader {
        version_ack: u8,
        service: u8,
        subservice: u8,
        source_id: U16<NetworkEndian>,
    }

    impl TryFrom<srd::PusTcDataFieldHeader> for PusTcDataFieldHeader {
        type Error = PusError;
        fn try_from(value: srd::PusTcDataFieldHeader) -> Result<Self, Self::Error> {
            if value.version != PusVersion::PusC {
                return Err(PusError::VersionNotSupported(value.version));
            }
            Ok(PusTcDataFieldHeader {
                version_ack: ((value.version as u8) << 4) | value.ack,
                service: value.service,
                subservice: value.subservice,
                source_id: U16::from(value.source_id),
            })
        }
    }

    impl PusTcSecondaryHeader for PusTcDataFieldHeader {
        fn ack_flags(&self) -> u8 {
            self.version_ack & 0b1111
        }

        fn service(&self) -> u8 {
            self.service
        }

        fn subservice(&self) -> u8 {
            self.subservice
        }

        fn source_id(&self) -> u16 {
            self.source_id.get()
        }
    }

    impl PusTcDataFieldHeader {
        pub fn to_bytes(&self, slice: &mut (impl AsMut<[u8]> + ?Sized)) -> Option<()> {
            self.write_to(slice.as_mut())
        }

        pub fn from_bytes(slice: &(impl AsRef<[u8]> + ?Sized)) -> Option<Self> {
            Self::read_from(slice.as_ref())
        }
    }
}

pub mod srd {
    use crate::ecss::{PusError, PusPacket, PusVersion, CRC_CCITT_FALSE};
    use crate::srd::SpHeader;
    use crate::tc::{PusTcSecondaryHeader, ACK_ALL, PUS_TC_MIN_LEN_WITHOUT_APP_DATA, PUS_VERSION};
    use crate::{zc, CcsdsPacket, PacketError, PacketId, PacketSequenceCtrl, PacketType};
    use delegate::delegate;
    use serde::{Deserialize, Serialize};
    use std::mem::size_of;
    use zerocopy::AsBytes;

    #[derive(PartialEq, Copy, Clone, Serialize, Deserialize)]
    pub struct PusTcDataFieldHeader {
        pub service: u8,
        pub subservice: u8,
        pub source_id: u16,
        pub ack: u8,
        pub version: PusVersion,
    }

    impl PusTcSecondaryHeader for PusTcDataFieldHeader {
        fn ack_flags(&self) -> u8 {
            self.ack
        }

        fn service(&self) -> u8 {
            self.service
        }

        fn subservice(&self) -> u8 {
            self.subservice
        }

        fn source_id(&self) -> u16 {
            self.source_id
        }
    }
    impl TryFrom<super::zc::PusTcDataFieldHeader> for PusTcDataFieldHeader {
        type Error = ();

        fn try_from(value: super::zc::PusTcDataFieldHeader) -> Result<Self, Self::Error> {
            Ok(PusTcDataFieldHeader {
                service: value.service(),
                subservice: value.subservice(),
                source_id: value.source_id(),
                ack: value.ack_flags(),
                version: PUS_VERSION,
            })
        }
    }

    impl PusTcDataFieldHeader {
        pub fn new(service: u8, subservice: u8, ack: u8) -> Self {
            PusTcDataFieldHeader {
                service,
                subservice,
                ack: ack & 0b1111,
                source_id: 0,
                version: PusVersion::PusC,
            }
        }
    }

    #[derive(PartialEq, Copy, Clone, Serialize, Deserialize)]
    pub struct PusTc<'slice> {
        pub sph: SpHeader,
        pub data_field_header: PusTcDataFieldHeader,
        #[serde(skip)]
        raw_data: Option<&'slice [u8]>,
        app_data: Option<&'slice [u8]>,
        crc16: Option<u16>,
    }

    impl<'slice> PusTc<'slice> {
        pub fn new(
            sph: &mut SpHeader,
            service: u8,
            subservice: u8,
            app_data: Option<&'slice [u8]>,
        ) -> Self {
            sph.packet_id.ptype = PacketType::Tc;
            PusTc {
                sph: *sph,
                raw_data: None,
                app_data,
                data_field_header: PusTcDataFieldHeader::new(service, subservice, ACK_ALL),
                crc16: None,
            }
        }

        pub fn len_packed(&self) -> usize {
            let mut length = super::PUS_TC_MIN_LEN_WITHOUT_APP_DATA;
            if let Some(app_data) = self.app_data {
                length += app_data.len();
            }
            length
        }

        /// Calculate the CCSDS space packet data length field and sets it
        pub fn set_ccsds_data_len(&mut self) {
            self.sph.data_len =
                self.len_packed() as u16 - size_of::<crate::zc::SpHeader>() as u16 - 1;
        }

        fn crc_from_raw_data(&self) -> Result<u16, PusError> {
            if let Some(raw_data) = self.raw_data {
                if raw_data.len() < 2 {
                    return Err(PusError::RawDataTooShort(raw_data.len()));
                }
                return Ok(u16::from_be_bytes(
                    raw_data[raw_data.len() - 2..raw_data.len()]
                        .try_into()
                        .unwrap(),
                ));
            }
            Err(PusError::NoRawData)
        }

        pub fn calc_crc16(&mut self) {
            let mut digest = CRC_CCITT_FALSE.digest();
            let sph_zc = crate::zc::SpHeader::from(self.sph);
            digest.update(sph_zc.as_bytes());
            let pus_tc_header =
                super::zc::PusTcDataFieldHeader::try_from(self.data_field_header).unwrap();
            digest.update(pus_tc_header.as_bytes());
            if let Some(app_data) = self.app_data {
                digest.update(app_data);
            }
            self.crc16 = Some(digest.finalize())
        }

        /// This function updates two important internal fields: The CCSDS packet length in the
        /// space packet header and the CRC16 field. This function should be called before
        /// the TC packet is serialized
        pub fn update_packet_fields(&mut self) {
            self.set_ccsds_data_len();
            self.calc_crc16();
        }

        pub fn copy_to_buf(
            &self,
            slice: &mut (impl AsMut<[u8]> + ?Sized),
        ) -> Result<usize, PusError> {
            if self.crc16.is_none() {
                return Err(PusError::CrcCalculationMissing);
            }
            let mut_slice = slice.as_mut();
            let mut curr_idx = 0;
            let sph_zc = crate::zc::SpHeader::from(self.sph);
            let tc_header_len = size_of::<super::zc::PusTcDataFieldHeader>();
            let mut total_size = super::PUS_TC_MIN_LEN_WITHOUT_APP_DATA;
            if let Some(app_data) = self.app_data {
                total_size += app_data.len();
            };
            if total_size > mut_slice.len() {
                return Err(PusError::OtherPacketError(
                    PacketError::ToBytesSliceTooSmall(total_size),
                ));
            }
            sph_zc
                .to_bytes(&mut mut_slice[curr_idx..curr_idx + 6])
                .ok_or(PusError::OtherPacketError(
                    PacketError::ToBytesZeroCopyError,
                ))?;
            curr_idx += 6;
            // The PUS version is hardcoded to PUS C
            let pus_tc_header =
                super::zc::PusTcDataFieldHeader::try_from(self.data_field_header).unwrap();

            pus_tc_header
                .to_bytes(&mut mut_slice[curr_idx..curr_idx + tc_header_len])
                .ok_or(PusError::OtherPacketError(
                    PacketError::ToBytesZeroCopyError,
                ))?;
            curr_idx += tc_header_len;
            if let Some(app_data) = self.app_data {
                mut_slice[curr_idx..curr_idx + app_data.len()].copy_from_slice(app_data);
                curr_idx += app_data.len();
            }
            mut_slice[curr_idx..curr_idx + 2]
                .copy_from_slice(self.crc16.unwrap().to_be_bytes().as_slice());
            curr_idx += 2;
            Ok(curr_idx)
        }

        pub fn append_to_vec(&self, vec: &mut Vec<u8>) -> Result<usize, PusError> {
            if self.crc16.is_none() {
                return Err(PusError::CrcCalculationMissing);
            }
            let sph_zc = crate::zc::SpHeader::from(self.sph);
            let mut appended_len = super::PUS_TC_MIN_LEN_WITHOUT_APP_DATA;
            if let Some(app_data) = self.app_data {
                appended_len += app_data.len();
            };
            vec.extend_from_slice(sph_zc.as_bytes());
            // The PUS version is hardcoded to PUS C
            let pus_tc_header =
                super::zc::PusTcDataFieldHeader::try_from(self.data_field_header).unwrap();
            vec.extend_from_slice(pus_tc_header.as_bytes());
            if let Some(app_data) = self.app_data {
                vec.extend_from_slice(app_data);
            }
            vec.extend_from_slice(self.crc16.unwrap().to_be_bytes().as_slice());
            Ok(appended_len)
        }

        pub fn new_from_raw_slice(
            slice: &'slice (impl AsRef<[u8]> + ?Sized),
        ) -> Result<Self, PusError> {
            let slice_ref = slice.as_ref();
            let raw_data_len = slice_ref.len();
            if raw_data_len < PUS_TC_MIN_LEN_WITHOUT_APP_DATA {
                return Err(PusError::RawDataTooShort(raw_data_len));
            }
            let sph = zc::SpHeader::from_bytes(slice).unwrap();
            let total_len = sph.total_len();
            if raw_data_len < total_len {
                return Err(PusError::RawDataTooShort(raw_data_len));
            }
            let sec_header = crate::tc::zc::PusTcDataFieldHeader::from_bytes(slice).ok_or(
                PusError::OtherPacketError(PacketError::FromBytesZeroCopyError),
            )?;

            let mut pus_tc = PusTc {
                sph: SpHeader::from(sph),
                data_field_header: PusTcDataFieldHeader::try_from(sec_header).unwrap(),
                raw_data: Some(slice_ref),
                app_data: Some(&slice_ref[PUS_TC_MIN_LEN_WITHOUT_APP_DATA..total_len - 2]),
                crc16: None,
            };
            pus_tc.verify()?;
            Ok(pus_tc)
        }

        fn verify(&mut self) -> Result<(), PusError> {
            let mut digest = CRC_CCITT_FALSE.digest();
            if self.raw_data.is_none() {
                return Err(PusError::NoRawData);
            }
            let raw_data = self.raw_data.unwrap();
            digest.update(raw_data.as_ref());
            if digest.finalize() == 0 {
                return Ok(());
            }
            let crc16 = self.crc_from_raw_data()?;
            Err(PusError::IncorrectCrc(crc16))
        }
    }

    //noinspection RsTraitImplementation
    impl CcsdsPacket for PusTc<'_> {
        delegate!(to self.sph {
            fn ccsds_version(&self) -> u8;
            fn packet_id(&self) -> PacketId;
            fn psc(&self) -> PacketSequenceCtrl;
            fn data_len(&self) -> u16;
        });
    }

    //noinspection RsTraitImplementation
    impl PusPacket for PusTc<'_> {
        delegate!(to self.data_field_header {
            fn service(&self) -> u8;
            fn subservice(&self) -> u8;
            fn source_id(&self) -> u16;
            fn ack_flags(&self) -> u8;
        });

        fn user_data(&self) -> Option<&[u8]> {
            self.app_data
        }

        fn crc16(&self) -> Option<u16> {
            self.crc16
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ecss::PusPacket;
    use crate::srd::SpHeader;
    use crate::tc::srd::PusTc;
    use crate::tc::ACK_ALL;
    use crate::CcsdsPacket;
    use postcard::to_stdvec;

    #[test]
    fn test_tc() {
        let mut sph = SpHeader::tc(0x01, 0).unwrap();
        let mut pus_tc = PusTc::new(&mut sph, 17, 1, None);
        assert_eq!(pus_tc.service(), 17);
        assert_eq!(pus_tc.subservice(), 1);
        assert_eq!(pus_tc.user_data(), None);
        assert_eq!(pus_tc.source_id(), 0);
        assert_eq!(pus_tc.apid(), 0x01);
        assert_eq!(pus_tc.ack_flags(), ACK_ALL);
        assert_eq!(pus_tc.crc16(), None);
        let _out = to_stdvec(&pus_tc).unwrap();
        let mut test_buf = [0; 32];
        pus_tc.update_packet_fields();
        assert_eq!(pus_tc.len_packed(), 13);
        let size = pus_tc
            .copy_to_buf(test_buf.as_mut_slice())
            .expect("Error writing TC to buffer");
        println!("Test buffer: {:02x?} with {size} written bytes", test_buf);

        let mut test_vec = Vec::new();
        let size = pus_tc
            .append_to_vec(&mut test_vec)
            .expect("Error writing TC to vector");
        println!("Test Vector: {:02x?} with {size} written bytes", test_vec);
    }
}
