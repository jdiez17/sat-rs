use crate::pus::{source_buffer_large_enough, EcssTmError, EcssTmSender};
use spacepackets::ecss::EcssEnumeration;
use spacepackets::tm::PusTm;
use spacepackets::tm::PusTmSecondaryHeader;
use spacepackets::{SpHeader, MAX_APID};

#[cfg(feature = "alloc")]
pub use allocvec::EventReporter;

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Subservices {
    TmInfoReport = 1,
    TmLowSeverityReport = 2,
    TmMediumSeverityReport = 3,
    TmHighSeverityReport = 4,
    TcEnableEventGeneration = 5,
    TcDisableEventGeneration = 6,
    TcReportDisabledList = 7,
    TmDisabledEventsReport = 8,
}

impl From<Subservices> for u8 {
    fn from(enumeration: Subservices) -> Self {
        enumeration as u8
    }
}

pub struct EventReporterBase {
    msg_count: u16,
    apid: u16,
    pub dest_id: u16,
}

impl EventReporterBase {
    pub fn new(apid: u16) -> Option<Self> {
        if apid > MAX_APID {
            return None;
        }
        Some(Self {
            msg_count: 0,
            dest_id: 0,
            apid,
        })
    }

    pub fn event_info<E>(
        &mut self,
        buf: &mut [u8],
        sender: &mut (impl EcssTmSender<E> + ?Sized),
        time_stamp: &[u8],
        event_id: impl EcssEnumeration,
        aux_data: Option<&[u8]>,
    ) -> Result<(), EcssTmError<E>> {
        self.generate_and_send_generic_tm(
            buf,
            Subservices::TmInfoReport,
            sender,
            time_stamp,
            event_id,
            aux_data,
        )
    }

    pub fn event_low_severity<E>(
        &mut self,
        buf: &mut [u8],
        sender: &mut (impl EcssTmSender<E> + ?Sized),
        time_stamp: &[u8],
        event_id: impl EcssEnumeration,
        aux_data: Option<&[u8]>,
    ) -> Result<(), EcssTmError<E>> {
        self.generate_and_send_generic_tm(
            buf,
            Subservices::TmLowSeverityReport,
            sender,
            time_stamp,
            event_id,
            aux_data,
        )
    }

    pub fn event_medium_severity<E>(
        &mut self,
        buf: &mut [u8],
        sender: &mut (impl EcssTmSender<E> + ?Sized),
        time_stamp: &[u8],
        event_id: impl EcssEnumeration,
        aux_data: Option<&[u8]>,
    ) -> Result<(), EcssTmError<E>> {
        self.generate_and_send_generic_tm(
            buf,
            Subservices::TmMediumSeverityReport,
            sender,
            time_stamp,
            event_id,
            aux_data,
        )
    }

    pub fn event_high_severity<E>(
        &mut self,
        buf: &mut [u8],
        sender: &mut (impl EcssTmSender<E> + ?Sized),
        time_stamp: &[u8],
        event_id: impl EcssEnumeration,
        aux_data: Option<&[u8]>,
    ) -> Result<(), EcssTmError<E>> {
        self.generate_and_send_generic_tm(
            buf,
            Subservices::TmHighSeverityReport,
            sender,
            time_stamp,
            event_id,
            aux_data,
        )
    }

    fn generate_and_send_generic_tm<E>(
        &mut self,
        buf: &mut [u8],
        subservice: Subservices,
        sender: &mut (impl EcssTmSender<E> + ?Sized),
        time_stamp: &[u8],
        event_id: impl EcssEnumeration,
        aux_data: Option<&[u8]>,
    ) -> Result<(), EcssTmError<E>> {
        let tm = self.generate_generic_event_tm(buf, subservice, time_stamp, event_id, aux_data)?;
        sender.send_tm(tm)?;
        self.msg_count += 1;
        Ok(())
    }

    fn generate_generic_event_tm<'a, E>(
        &'a self,
        buf: &'a mut [u8],
        subservice: Subservices,
        time_stamp: &'a [u8],
        event_id: impl EcssEnumeration,
        aux_data: Option<&[u8]>,
    ) -> Result<PusTm, EcssTmError<E>> {
        let mut src_data_len = event_id.byte_width();
        if let Some(aux_data) = aux_data {
            src_data_len += aux_data.len();
        }
        source_buffer_large_enough(buf.len(), src_data_len)?;
        let mut sp_header = SpHeader::tm(self.apid, 0, 0).unwrap();
        let sec_header = PusTmSecondaryHeader::new(
            5,
            subservice.into(),
            self.msg_count,
            self.dest_id,
            time_stamp,
        );
        let mut current_idx = 0;
        event_id.write_to_bytes(&mut buf[0..event_id.byte_width()])?;
        current_idx += event_id.byte_width();
        if let Some(aux_data) = aux_data {
            buf[current_idx..current_idx + aux_data.len()].copy_from_slice(aux_data);
            current_idx += aux_data.len();
        }
        Ok(PusTm::new(
            &mut sp_header,
            sec_header,
            Some(&buf[0..current_idx]),
            true,
        ))
    }
}

#[cfg(feature = "alloc")]
mod allocvec {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    pub struct EventReporter {
        source_data_buf: Vec<u8>,
        pub reporter: EventReporterBase,
    }

    impl EventReporter {
        pub fn new(apid: u16, max_event_id_and_aux_data: usize) -> Option<Self> {
            let reporter = EventReporterBase::new(apid)?;
            Some(Self {
                source_data_buf: vec![0; max_event_id_and_aux_data],
                reporter,
            })
        }
        pub fn event_info<E>(
            &mut self,
            sender: &mut (impl EcssTmSender<E> + ?Sized),
            time_stamp: &[u8],
            event_id: impl EcssEnumeration,
            aux_data: Option<&[u8]>,
        ) -> Result<(), EcssTmError<E>> {
            self.reporter.event_info(
                self.source_data_buf.as_mut_slice(),
                sender,
                time_stamp,
                event_id,
                aux_data,
            )
        }

        pub fn event_low_severity<E>(
            &mut self,
            sender: &mut (impl EcssTmSender<E> + ?Sized),
            time_stamp: &[u8],
            event_id: impl EcssEnumeration,
            aux_data: Option<&[u8]>,
        ) -> Result<(), EcssTmError<E>> {
            self.reporter.event_low_severity(
                self.source_data_buf.as_mut_slice(),
                sender,
                time_stamp,
                event_id,
                aux_data,
            )
        }

        pub fn event_medium_severity<E>(
            &mut self,
            sender: &mut (impl EcssTmSender<E> + ?Sized),
            time_stamp: &[u8],
            event_id: impl EcssEnumeration,
            aux_data: Option<&[u8]>,
        ) -> Result<(), EcssTmError<E>> {
            self.reporter.event_medium_severity(
                self.source_data_buf.as_mut_slice(),
                sender,
                time_stamp,
                event_id,
                aux_data,
            )
        }

        pub fn event_high_severity<E>(
            &mut self,
            sender: &mut (impl EcssTmSender<E> + ?Sized),
            time_stamp: &[u8],
            event_id: impl EcssEnumeration,
            aux_data: Option<&[u8]>,
        ) -> Result<(), EcssTmError<E>> {
            self.reporter.event_high_severity(
                self.source_data_buf.as_mut_slice(),
                sender,
                time_stamp,
                event_id,
                aux_data,
            )
        }
    }
}
