//! # PUS Verification Service 1 Module
//!
//! This module allows packaging and sending PUS Service 1 packets. It is conforming to section
//! 8 of the PUS standard ECSS-E-ST-70-41C.
//!
//! The core object to report TC verification progress is the [VerificationReporter]. It exposes
//! an API which uses type-state programming to avoid calling the verification steps in
//! an invalid order.
//!
//! # Example
//! TODO: Cross Ref integration test which will be provided
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::mem::size_of;
use delegate::delegate;
use downcast_rs::{impl_downcast, Downcast};
use spacepackets::ecss::{EcssEnumeration, PusError};
use spacepackets::tc::PusTc;
use spacepackets::time::TimestampError;
use spacepackets::tm::{PusTm, PusTmSecondaryHeader};
use spacepackets::{ByteConversionError, SizeMissmatch, SpHeader};
use spacepackets::{CcsdsPacket, PacketId, PacketSequenceCtrl};

#[cfg(feature = "std")]
pub use stdmod::{CrossbeamVerifSender, StdVerifSender, StdVerifSenderError};

/// This is a request identifier as specified in 5.4.11.2 c. of the PUS standard
/// This field equivalent to the first two bytes of the CCSDS space packet header.
#[derive(Debug, Eq, Copy, Clone)]
pub struct RequestId {
    version_number: u8,
    packet_id: PacketId,
    psc: PacketSequenceCtrl,
}

impl Hash for RequestId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw().hash(state);
    }
}

// Implement manually to satisfy derive_hash_xor_eq lint
impl PartialEq for RequestId {
    fn eq(&self, other: &Self) -> bool {
        self.version_number == other.version_number
            && self.packet_id == other.packet_id
            && self.psc == other.psc
    }
}

impl RequestId {
    pub const SIZE_AS_BYTES: usize = size_of::<u32>();

    pub fn raw(&self) -> u32 {
        ((self.version_number as u32) << 29)
            | ((self.packet_id.raw() as u32) << 16)
            | self.psc.raw() as u32
    }

    pub fn to_bytes(&self, buf: &mut [u8]) {
        let raw = self.raw();
        buf.copy_from_slice(raw.to_be_bytes().as_slice());
    }

    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 4 {
            return None;
        }
        let raw = u32::from_be_bytes(buf[0..Self::SIZE_AS_BYTES].try_into().unwrap());
        Some(Self {
            version_number: ((raw >> 29) & 0b111) as u8,
            packet_id: PacketId::from(((raw >> 16) & 0xffff) as u16),
            psc: PacketSequenceCtrl::from((raw & 0xffff) as u16),
        })
    }
}
impl RequestId {
    /// This allows extracting the request ID from a given PUS telecommand.
    pub fn new(tc: &PusTc) -> Self {
        RequestId {
            version_number: tc.ccsds_version(),
            packet_id: tc.packet_id(),
            psc: tc.psc(),
        }
    }
}

/// Generic error type which is also able to wrap a user send error with the user supplied type E.
#[derive(Debug, Clone)]
pub enum VerificationError<E> {
    /// Errors related to sending the verification telemetry to a TM recipient
    SendError(E),
    /// Errors related to the time stamp format of the telemetry
    TimestampError(TimestampError),
    /// Errors related to byte conversion, for example unsufficient buffer size for given data
    ByteConversionError(ByteConversionError),
    /// Errors related to PUS packet format
    PusError(PusError),
}

/// If a verification operation fails, the passed token will be returned as well. This allows
/// re-trying the operation at a later point.
#[derive(Debug, Clone)]
pub struct VerificationErrorWithToken<E, T>(VerificationError<E>, VerificationToken<T>);

/// Generic trait for a user supplied sender object. This sender object is responsible for sending
/// PUS Service 1 Verification Telemetry to a verification TM recipient. The [Downcast] trait
/// is implemented to allow passing the sender as a boxed trait object and still retrieve the
/// concrete type at a later point.
pub trait VerificationSender<E>: Downcast + Send {
    fn send_verification_tm(&mut self, tm: PusTm) -> Result<(), VerificationError<E>>;
}

impl_downcast!(VerificationSender<E>);

/// Support token to allow type-state programming. This prevents calling the verification
/// steps in an invalid order.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct VerificationToken<STATE> {
    state: PhantomData<STATE>,
    req_id: RequestId,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StateNone;
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StateAccepted;
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StateStarted;

impl<STATE> VerificationToken<STATE> {
    fn new(req_id: RequestId) -> VerificationToken<StateNone> {
        VerificationToken {
            state: PhantomData,
            req_id,
        }
    }

    pub fn req_id(&self) -> RequestId {
        self.req_id
    }
}

pub struct VerificationReporterCfg {
    pub apid: u16,
    pub dest_id: u16,
    pub step_field_width: usize,
    pub fail_code_field_width: usize,
    pub max_fail_data_len: usize,
}

impl VerificationReporterCfg {
    /// Create a new configuration for the verification reporter. This includes following parameters:
    ///
    /// 1. Destination ID and APID, which could remain constant after construction. These parameters
    ///    can be tweaked in the reporter after construction.
    /// 2. Maximum expected field sizes. The parameters of this configuration struct will be used
    ///    to determine required maximum buffer sizes and there will be no addition allocation or
    ///    configurable buffer parameters after [VerificationReporter] construction.
    ///
    /// This means the user has supply the maximum expected field sizes of verification messages
    /// before constructing the reporter.
    pub fn new(
        apid: u16,
        step_field_width: usize,
        fail_code_field_width: usize,
        max_fail_data_len: usize,
    ) -> Self {
        Self {
            apid,
            dest_id: 0,
            step_field_width,
            fail_code_field_width,
            max_fail_data_len,
        }
    }
}

/// Composite helper struct to pass failure parameters to the [VerificationReporter]
pub struct FailParams<'a> {
    time_stamp: &'a [u8],
    failure_code: &'a dyn EcssEnumeration,
    failure_data: Option<&'a [u8]>,
}

impl<'a> FailParams<'a> {
    pub fn new(
        time_stamp: &'a [u8],
        failure_code: &'a impl EcssEnumeration,
        failure_data: Option<&'a [u8]>,
    ) -> Self {
        Self {
            time_stamp,
            failure_code,
            failure_data,
        }
    }
}

/// Composite helper struct to pass step failure parameters to the [VerificationReporter]
pub struct FailParamsWithStep<'a> {
    bp: FailParams<'a>,
    step: &'a dyn EcssEnumeration,
}

impl<'a> FailParamsWithStep<'a> {
    pub fn new(
        time_stamp: &'a [u8],
        step: &'a impl EcssEnumeration,
        failure_code: &'a impl EcssEnumeration,
        failure_data: Option<&'a [u8]>,
    ) -> Self {
        Self {
            bp: FailParams::new(time_stamp, failure_code, failure_data),
            step,
        }
    }
}

/// Primary verification handler. It provides an API to send PUS 1 verification telemetry packets
/// and verify the various steps of telecommand handling as specified in the PUS standard.
pub struct VerificationReporter {
    pub apid: u16,
    pub dest_id: u16,
    msg_count: u16,
    source_data_buf: Vec<u8>,
}

impl VerificationReporter {
    pub fn new(cfg: VerificationReporterCfg) -> Self {
        Self {
            apid: cfg.apid,
            dest_id: cfg.dest_id,
            msg_count: 0,
            source_data_buf: vec![
                0;
                RequestId::SIZE_AS_BYTES
                    + cfg.step_field_width as usize
                    + cfg.fail_code_field_width as usize
                    + cfg.max_fail_data_len
            ],
        }
    }

    pub fn allowed_source_data_len(&self) -> usize {
        self.source_data_buf.capacity()
    }

    /// Initialize verification handling by passing a TC reference. This returns a token required
    /// to call the acceptance functions
    pub fn add_tc(&mut self, pus_tc: &PusTc) -> VerificationToken<StateNone> {
        self.add_tc_with_req_id(RequestId::new(pus_tc))
    }

    /// Same as [Self::add_tc] but pass a request ID instead of the direct telecommand.
    /// This can be useful if the executing thread does not have full access to the telecommand.
    pub fn add_tc_with_req_id(&mut self, req_id: RequestId) -> VerificationToken<StateNone> {
        VerificationToken::<StateNone>::new(req_id)
    }

    /// Package and send a PUS TM\[1, 1\] packet, see 8.1.2.1 of the PUS standard
    pub fn acceptance_success<E>(
        &mut self,
        token: VerificationToken<StateNone>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        time_stamp: &[u8],
    ) -> Result<VerificationToken<StateAccepted>, VerificationErrorWithToken<E, StateNone>> {
        let tm = self
            .create_pus_verif_success_tm(
                1,
                1,
                &token.req_id,
                time_stamp,
                None::<&dyn EcssEnumeration>,
            )
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(VerificationToken {
            state: PhantomData,
            req_id: token.req_id,
        })
    }

    /// Package and send a PUS TM\[1, 2\] packet, see 8.1.2.2 of the PUS standard
    pub fn acceptance_failure<E>(
        &mut self,
        token: VerificationToken<StateNone>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        params: FailParams,
    ) -> Result<(), VerificationErrorWithToken<E, StateNone>> {
        let tm = self
            .create_pus_verif_fail_tm(1, 2, &token.req_id, None::<&dyn EcssEnumeration>, &params)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(())
    }

    /// Package and send a PUS TM\[1, 3\] packet, see 8.1.2.3 of the PUS standard.
    ///
    /// Requires a token previously acquired by calling [Self::acceptance_success].
    pub fn start_success<E>(
        &mut self,
        token: VerificationToken<StateAccepted>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        time_stamp: &[u8],
    ) -> Result<VerificationToken<StateStarted>, VerificationErrorWithToken<E, StateAccepted>> {
        let tm = self
            .create_pus_verif_success_tm(
                1,
                3,
                &token.req_id,
                time_stamp,
                None::<&dyn EcssEnumeration>,
            )
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(VerificationToken {
            state: PhantomData,
            req_id: token.req_id,
        })
    }

    /// Package and send a PUS TM\[1, 4\] packet, see 8.1.2.4 of the PUS standard.
    ///
    /// Requires a token previously acquired by calling [Self::acceptance_success]. It consumes
    /// the token because verification handling is done.
    pub fn start_failure<E>(
        &mut self,
        token: VerificationToken<StateAccepted>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        params: FailParams,
    ) -> Result<(), VerificationErrorWithToken<E, StateAccepted>> {
        let tm = self
            .create_pus_verif_fail_tm(1, 4, &token.req_id, None::<&dyn EcssEnumeration>, &params)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(())
    }

    /// Package and send a PUS TM\[1, 5\] packet, see 8.1.2.5 of the PUS standard.
    ///
    /// Requires a token previously acquired by calling [Self::start_success].
    pub fn step_success<E>(
        &mut self,
        token: &VerificationToken<StateStarted>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        time_stamp: &[u8],
        step: impl EcssEnumeration,
    ) -> Result<(), VerificationError<E>> {
        let tm = self.create_pus_verif_success_tm(1, 5, &token.req_id, time_stamp, Some(&step))?;
        sender.send_verification_tm(tm)?;
        self.msg_count += 1;
        Ok(())
    }

    /// Package and send a PUS TM\[1, 6\] packet, see 8.1.2.6 of the PUS standard.
    ///
    /// Requires a token previously acquired by calling [Self::start_success]. It consumes the
    /// token because verification handling is done.
    pub fn step_failure<E>(
        &mut self,
        token: VerificationToken<StateStarted>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        params: FailParamsWithStep,
    ) -> Result<(), VerificationErrorWithToken<E, StateStarted>> {
        let tm = self
            .create_pus_verif_fail_tm(1, 6, &token.req_id, Some(params.step), &params.bp)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(())
    }

    /// Package and send a PUS TM\[1, 7\] packet, see 8.1.2.7 of the PUS standard.
    ///
    /// Requires a token previously acquired by calling [Self::start_success]. It consumes the
    /// token because verification handling is done.
    pub fn completion_success<E>(
        &mut self,
        token: VerificationToken<StateStarted>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        time_stamp: &[u8],
    ) -> Result<(), VerificationErrorWithToken<E, StateStarted>> {
        let tm = self
            .create_pus_verif_success_tm(
                1,
                7,
                &token.req_id,
                time_stamp,
                None::<&dyn EcssEnumeration>,
            )
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(())
    }

    /// Package and send a PUS TM\[1, 8\] packet, see 8.1.2.8 of the PUS standard.
    ///
    /// Requires a token previously acquired by calling [Self::start_success]. It consumes the
    /// token because verification handling is done.
    pub fn completion_failure<E>(
        &mut self,
        token: VerificationToken<StateStarted>,
        sender: &mut (impl VerificationSender<E> + ?Sized),
        params: FailParams,
    ) -> Result<(), VerificationErrorWithToken<E, StateStarted>> {
        let tm = self
            .create_pus_verif_fail_tm(1, 8, &token.req_id, None::<&dyn EcssEnumeration>, &params)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        sender
            .send_verification_tm(tm)
            .map_err(|e| VerificationErrorWithToken(e, token))?;
        self.msg_count += 1;
        Ok(())
    }

    fn create_pus_verif_success_tm<'a, E>(
        &'a mut self,
        service: u8,
        subservice: u8,
        req_id: &RequestId,
        time_stamp: &'a [u8],
        step: Option<&(impl EcssEnumeration + ?Sized)>,
    ) -> Result<PusTm, VerificationError<E>> {
        let mut source_data_len = size_of::<u32>();
        if let Some(step) = step {
            source_data_len += step.byte_width() as usize;
        }
        self.source_buffer_large_enough(source_data_len)?;
        let mut idx = 0;
        req_id.to_bytes(&mut self.source_data_buf[0..RequestId::SIZE_AS_BYTES]);
        idx += RequestId::SIZE_AS_BYTES;
        if let Some(step) = step {
            // Size check was done beforehand
            step.to_bytes(&mut self.source_data_buf[idx..idx + step.byte_width() as usize])
                .unwrap();
        }
        let mut sp_header = SpHeader::tm(self.apid, 0, 0).unwrap();
        Ok(self.create_pus_verif_tm_base(
            service,
            subservice,
            &mut sp_header,
            time_stamp,
            source_data_len,
        ))
    }

    fn create_pus_verif_fail_tm<'a, E>(
        &'a mut self,
        service: u8,
        subservice: u8,
        req_id: &RequestId,
        step: Option<&(impl EcssEnumeration + ?Sized)>,
        params: &'a FailParams,
    ) -> Result<PusTm, VerificationError<E>> {
        let mut idx = 0;
        let mut source_data_len =
            RequestId::SIZE_AS_BYTES + params.failure_code.byte_width() as usize;
        if let Some(step) = step {
            source_data_len += step.byte_width() as usize;
        }
        if let Some(failure_data) = params.failure_data {
            source_data_len += failure_data.len();
        }
        self.source_buffer_large_enough(source_data_len)?;
        req_id.to_bytes(&mut self.source_data_buf[0..RequestId::SIZE_AS_BYTES]);
        idx += RequestId::SIZE_AS_BYTES;
        if let Some(step) = step {
            // Size check done beforehand
            step.to_bytes(&mut self.source_data_buf[idx..idx + step.byte_width() as usize])
                .unwrap();
            idx += step.byte_width() as usize;
        }
        params
            .failure_code
            .to_bytes(
                &mut self.source_data_buf[idx..idx + params.failure_code.byte_width() as usize],
            )
            .map_err(|e| VerificationError::<E>::ByteConversionError(e))?;
        idx += params.failure_code.byte_width() as usize;
        if let Some(failure_data) = params.failure_data {
            self.source_data_buf[idx..idx + failure_data.len()].copy_from_slice(failure_data);
        }
        let mut sp_header = SpHeader::tm(self.apid, 0, 0).unwrap();
        Ok(self.create_pus_verif_tm_base(
            service,
            subservice,
            &mut sp_header,
            params.time_stamp,
            source_data_len,
        ))
    }

    fn source_buffer_large_enough<E>(&self, len: usize) -> Result<(), VerificationError<E>> {
        if len > self.source_data_buf.capacity() {
            return Err(VerificationError::ByteConversionError(
                ByteConversionError::ToSliceTooSmall(SizeMissmatch {
                    found: self.source_data_buf.capacity(),
                    expected: len,
                }),
            ));
        }
        Ok(())
    }

    fn create_pus_verif_tm_base<'a>(
        &'a mut self,
        service: u8,
        subservice: u8,
        sp_header: &mut SpHeader,
        time_stamp: &'a [u8],
        source_data_len: usize,
    ) -> PusTm {
        let tm_sec_header = PusTmSecondaryHeader::new(
            service,
            subservice,
            self.msg_count,
            self.dest_id,
            time_stamp,
        );
        PusTm::new(
            sp_header,
            tm_sec_header,
            Some(&self.source_data_buf[0..source_data_len]),
            true,
        )
    }
}

/// Helper object which caches the sender passed as a trait object. Provides the same
/// API as [VerificationReporter] but without the explicit sender arguments.
pub struct VerificationReporterWithSender<E> {
    reporter: VerificationReporter,
    pub sender: Box<dyn VerificationSender<E>>,
}

impl<E: 'static> VerificationReporterWithSender<E> {
    pub fn new(cfg: VerificationReporterCfg, sender: Box<dyn VerificationSender<E>>) -> Self {
        Self::new_from_reporter(VerificationReporter::new(cfg), sender)
    }

    pub fn new_from_reporter(
        reporter: VerificationReporter,
        sender: Box<dyn VerificationSender<E>>,
    ) -> Self {
        Self { reporter, sender }
    }

    delegate! {
        to self.reporter {
            pub fn add_tc(&mut self, pus_tc: &PusTc) -> VerificationToken<StateNone>;
            pub fn add_tc_with_req_id(&mut self, req_id: RequestId) -> VerificationToken<StateNone>;
        }
    }

    pub fn acceptance_success(
        &mut self,
        token: VerificationToken<StateNone>,
        time_stamp: &[u8],
    ) -> Result<VerificationToken<StateAccepted>, VerificationErrorWithToken<E, StateNone>> {
        self.reporter
            .acceptance_success(token, self.sender.as_mut(), time_stamp)
    }

    pub fn acceptance_failure(
        &mut self,
        token: VerificationToken<StateNone>,
        params: FailParams,
    ) -> Result<(), VerificationErrorWithToken<E, StateNone>> {
        self.reporter
            .acceptance_failure(token, self.sender.as_mut(), params)
    }

    pub fn start_success(
        &mut self,
        token: VerificationToken<StateAccepted>,
        time_stamp: &[u8],
    ) -> Result<VerificationToken<StateStarted>, VerificationErrorWithToken<E, StateAccepted>> {
        self.reporter
            .start_success(token, self.sender.as_mut(), time_stamp)
    }

    pub fn start_failure(
        &mut self,
        token: VerificationToken<StateAccepted>,
        params: FailParams,
    ) -> Result<(), VerificationErrorWithToken<E, StateAccepted>> {
        self.reporter
            .start_failure(token, self.sender.as_mut(), params)
    }

    pub fn step_success(
        &mut self,
        token: &VerificationToken<StateStarted>,
        time_stamp: &[u8],
        step: impl EcssEnumeration,
    ) -> Result<(), VerificationError<E>> {
        self.reporter
            .step_success(token, self.sender.as_mut(), time_stamp, step)
    }

    pub fn step_failure(
        &mut self,
        token: VerificationToken<StateStarted>,
        params: FailParamsWithStep,
    ) -> Result<(), VerificationErrorWithToken<E, StateStarted>> {
        self.reporter
            .step_failure(token, self.sender.as_mut(), params)
    }

    pub fn completion_success(
        &mut self,
        token: VerificationToken<StateStarted>,
        time_stamp: &[u8],
    ) -> Result<(), VerificationErrorWithToken<E, StateStarted>> {
        self.reporter
            .completion_success(token, self.sender.as_mut(), time_stamp)
    }

    pub fn completion_failure(
        &mut self,
        token: VerificationToken<StateStarted>,
        params: FailParams,
    ) -> Result<(), VerificationErrorWithToken<E, StateStarted>> {
        self.reporter
            .completion_failure(token, self.sender.as_mut(), params)
    }
}

#[cfg(feature = "std")]
mod stdmod {
    use crate::pool::{LocalPool, StoreAddr, StoreError};
    use crate::pus::verification::{VerificationError, VerificationSender};
    use delegate::delegate;
    use spacepackets::tm::PusTm;
    use std::sync::{mpsc, Arc, RwLock, RwLockWriteGuard};

    #[derive(Debug, Eq, PartialEq)]
    pub enum StdVerifSenderError {
        PoisonError,
        StoreError(StoreError),
        RxDisconnected(StoreAddr),
    }

    trait SendBackend: Send {
        fn send(&self, addr: StoreAddr) -> Result<(), StoreAddr>;
    }

    struct StdSenderBase<S> {
        pub ignore_poison_error: bool,
        tm_store: Arc<RwLock<LocalPool>>,
        tx: S,
    }

    impl<S: SendBackend> StdSenderBase<S> {
        pub fn new(tm_store: Arc<RwLock<LocalPool>>, tx: S) -> Self {
            Self {
                ignore_poison_error: false,
                tm_store,
                tx,
            }
        }
    }

    impl SendBackend for mpsc::Sender<StoreAddr> {
        fn send(&self, addr: StoreAddr) -> Result<(), StoreAddr> {
            self.send(addr).map_err(|_| addr)
        }
    }

    pub struct StdVerifSender {
        base: StdSenderBase<mpsc::Sender<StoreAddr>>,
    }

    /// Verification sender with a [mpsc::Sender] backend.
    /// It implements the [VerificationSender] trait to be used as PUS Verification TM sender.
    impl StdVerifSender {
        pub fn new(tm_store: Arc<RwLock<LocalPool>>, tx: mpsc::Sender<StoreAddr>) -> Self {
            Self {
                base: StdSenderBase::new(tm_store, tx),
            }
        }
    }

    //noinspection RsTraitImplementation
    impl VerificationSender<StdVerifSenderError> for StdVerifSender {
        delegate!(
            to self.base {
                fn send_verification_tm(&mut self, tm: PusTm) -> Result<(), VerificationError<StdVerifSenderError>>;
            }
        );
    }
    unsafe impl Sync for StdVerifSender {}
    unsafe impl Send for StdVerifSender {}

    impl SendBackend for crossbeam_channel::Sender<StoreAddr> {
        fn send(&self, addr: StoreAddr) -> Result<(), StoreAddr> {
            self.send(addr).map_err(|_| addr)
        }
    }

    /// Verification sender with a [crossbeam_channel::Sender] backend.
    /// It implements the [VerificationSender] trait to be used as PUS Verification TM sender
    pub struct CrossbeamVerifSender {
        base: StdSenderBase<crossbeam_channel::Sender<StoreAddr>>,
    }

    impl CrossbeamVerifSender {
        pub fn new(
            tm_store: Arc<RwLock<LocalPool>>,
            tx: crossbeam_channel::Sender<StoreAddr>,
        ) -> Self {
            Self {
                base: StdSenderBase::new(tm_store, tx),
            }
        }
    }

    //noinspection RsTraitImplementation
    impl VerificationSender<StdVerifSenderError> for CrossbeamVerifSender {
        delegate!(
            to self.base {
                fn send_verification_tm(&mut self, tm: PusTm) -> Result<(), VerificationError<StdVerifSenderError>>;
            }
        );
    }

    unsafe impl Sync for CrossbeamVerifSender {}
    unsafe impl Send for CrossbeamVerifSender {}

    impl<S: SendBackend + 'static> VerificationSender<StdVerifSenderError> for StdSenderBase<S> {
        fn send_verification_tm(
            &mut self,
            tm: PusTm,
        ) -> Result<(), VerificationError<StdVerifSenderError>> {
            let operation = |mut mg: RwLockWriteGuard<LocalPool>| {
                let (addr, buf) = mg.free_element(tm.len_packed()).map_err(|e| {
                    VerificationError::SendError(StdVerifSenderError::StoreError(e))
                })?;
                tm.write_to(buf).map_err(VerificationError::PusError)?;
                drop(mg);
                self.tx.send(addr).map_err(|_| {
                    VerificationError::SendError(StdVerifSenderError::RxDisconnected(addr))
                })?;
                Ok(())
            };
            match self.tm_store.write() {
                Ok(lock) => operation(lock),
                Err(poison_error) => {
                    if self.ignore_poison_error {
                        operation(poison_error.into_inner())
                    } else {
                        Err(VerificationError::SendError(
                            StdVerifSenderError::PoisonError,
                        ))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::pus::verification::{
        FailParams, FailParamsWithStep, RequestId, StateNone, VerificationError,
        VerificationReporter, VerificationReporterCfg, VerificationReporterWithSender,
        VerificationSender, VerificationToken,
    };
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::vec::Vec;
    use spacepackets::ecss::{EcssEnumU16, EcssEnumU32, EcssEnumU8, EcssEnumeration, PusPacket};
    use spacepackets::tc::{PusTc, PusTcSecondaryHeader};
    use spacepackets::tm::{PusTm, PusTmSecondaryHeaderT};
    use spacepackets::{ByteConversionError, CcsdsPacket, SpHeader};
    use std::collections::VecDeque;

    const TEST_APID: u16 = 0x02;
    const EMPTY_STAMP: [u8; 7] = [0; 7];

    #[derive(Debug, Eq, PartialEq)]
    struct TmInfo {
        pub subservice: u8,
        pub apid: u16,
        pub msg_counter: u16,
        pub dest_id: u16,
        pub time_stamp: [u8; 7],
        pub req_id: RequestId,
        pub additional_data: Option<Vec<u8>>,
    }

    #[derive(Default)]
    struct TestSender {
        pub service_queue: VecDeque<TmInfo>,
    }

    impl VerificationSender<()> for TestSender {
        fn send_verification_tm(&mut self, tm: PusTm) -> Result<(), VerificationError<()>> {
            assert_eq!(PusPacket::service(&tm), 1);
            assert!(tm.source_data().is_some());
            let mut time_stamp = [0; 7];
            time_stamp.clone_from_slice(&tm.time_stamp()[0..7]);
            let src_data = tm.source_data().unwrap();
            assert!(src_data.len() >= 4);
            let req_id = RequestId::from_bytes(&src_data[0..RequestId::SIZE_AS_BYTES]).unwrap();
            let mut vec = None;
            if src_data.len() > 4 {
                let mut new_vec = Vec::new();
                new_vec.extend_from_slice(&src_data[RequestId::SIZE_AS_BYTES..]);
                vec = Some(new_vec);
            }
            self.service_queue.push_back(TmInfo {
                subservice: PusPacket::subservice(&tm),
                apid: tm.apid(),
                msg_counter: tm.msg_counter(),
                dest_id: tm.dest_id(),
                time_stamp,
                req_id,
                additional_data: vec,
            });
            Ok(())
        }
    }

    #[derive(Debug, Copy, Clone, Eq, PartialEq)]
    struct DummyError {}
    #[derive(Default)]
    struct FallibleSender {}

    impl VerificationSender<DummyError> for FallibleSender {
        fn send_verification_tm(&mut self, _: PusTm) -> Result<(), VerificationError<DummyError>> {
            Err(VerificationError::SendError(DummyError {}))
        }
    }

    struct TestBase<'a> {
        vr: VerificationReporter,
        #[allow(dead_code)]
        tc: PusTc<'a>,
    }

    impl<'a> TestBase<'a> {
        fn rep(&mut self) -> &mut VerificationReporter {
            &mut self.vr
        }
    }
    struct TestBaseWithHelper<'a, E> {
        helper: VerificationReporterWithSender<E>,
        #[allow(dead_code)]
        tc: PusTc<'a>,
    }

    impl<'a, E> TestBaseWithHelper<'a, E> {
        fn rep(&mut self) -> &mut VerificationReporter {
            &mut self.helper.reporter
        }
    }

    fn base_reporter() -> VerificationReporter {
        let cfg = VerificationReporterCfg::new(TEST_APID, 1, 2, 8);
        VerificationReporter::new(cfg)
    }

    fn base_tc_init(app_data: Option<&[u8]>) -> (PusTc, RequestId) {
        let mut sph = SpHeader::tc(TEST_APID, 0x34, 0).unwrap();
        let tc_header = PusTcSecondaryHeader::new_simple(17, 1);
        let pus_tc = PusTc::new(&mut sph, tc_header, app_data, true);
        let req_id = RequestId::new(&pus_tc);
        (pus_tc, req_id)
    }

    fn base_init(api_sel: bool) -> (TestBase<'static>, VerificationToken<StateNone>) {
        let mut reporter = base_reporter();
        let (tc, req_id) = base_tc_init(None);
        let init_tok;
        if api_sel {
            init_tok = reporter.add_tc_with_req_id(req_id);
        } else {
            init_tok = reporter.add_tc(&tc);
        }
        (TestBase { vr: reporter, tc }, init_tok)
    }

    fn base_with_helper_init() -> (
        TestBaseWithHelper<'static, ()>,
        VerificationToken<StateNone>,
    ) {
        let mut reporter = base_reporter();
        let (tc, _) = base_tc_init(None);
        let init_tok = reporter.add_tc(&tc);
        let sender = TestSender::default();
        let helper = VerificationReporterWithSender::new_from_reporter(reporter, Box::new(sender));
        (TestBaseWithHelper { helper, tc }, init_tok)
    }

    fn acceptance_check(sender: &mut TestSender, req_id: &RequestId) {
        let cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 1,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: None,
            req_id: req_id.clone(),
        };
        assert_eq!(sender.service_queue.len(), 1);
        let info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_basic_acceptance_success() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        b.vr.acceptance_success(tok, &mut sender, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        acceptance_check(&mut sender, &tok.req_id);
    }

    #[test]
    fn test_basic_acceptance_success_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        b.helper
            .acceptance_success(tok, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        acceptance_check(sender, &tok.req_id);
    }

    #[test]
    fn test_acceptance_send_fails() {
        let (mut b, tok) = base_init(false);
        let mut faulty_sender = FallibleSender::default();
        let res =
            b.vr.acceptance_success(tok, &mut faulty_sender, &EMPTY_STAMP);
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert_eq!(err.1, tok);
        match err.0 {
            VerificationError::SendError(e) => {
                assert_eq!(e, DummyError {})
            }
            _ => panic!("{}", format!("Unexpected error {:?}", err.0)),
        }
    }

    fn acceptance_fail_check(sender: &mut TestSender, req_id: RequestId, stamp_buf: [u8; 7]) {
        let cmp_info = TmInfo {
            time_stamp: stamp_buf,
            subservice: 2,
            dest_id: 5,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: Some([0, 2].to_vec()),
            req_id,
        };
        assert_eq!(sender.service_queue.len(), 1);
        let info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_basic_acceptance_failure() {
        let (mut b, tok) = base_init(true);
        b.rep().dest_id = 5;
        let stamp_buf = [1, 2, 3, 4, 5, 6, 7];
        let mut sender = TestSender::default();
        let fail_code = EcssEnumU16::new(2);
        let fail_params = FailParams::new(stamp_buf.as_slice(), &fail_code, None);
        b.vr.acceptance_failure(tok, &mut sender, fail_params)
            .expect("Sending acceptance success failed");
        acceptance_fail_check(&mut sender, tok.req_id, stamp_buf);
    }

    #[test]
    fn test_basic_acceptance_failure_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        b.rep().dest_id = 5;
        let stamp_buf = [1, 2, 3, 4, 5, 6, 7];
        let fail_code = EcssEnumU16::new(2);
        let fail_params = FailParams::new(stamp_buf.as_slice(), &fail_code, None);
        b.helper
            .acceptance_failure(tok, fail_params)
            .expect("Sending acceptance success failed");
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        acceptance_fail_check(sender, tok.req_id, stamp_buf);
    }

    #[test]
    fn test_acceptance_fail_data_too_large() {
        let (mut b, tok) = base_with_helper_init();
        b.rep().dest_id = 5;
        let stamp_buf = [1, 2, 3, 4, 5, 6, 7];
        let fail_code = EcssEnumU16::new(2);
        let fail_data: [u8; 16] = [0; 16];
        // 4 req ID + 1 byte step + 2 byte error code + 8 byte fail data
        assert_eq!(b.rep().allowed_source_data_len(), 15);
        let fail_params =
            FailParams::new(stamp_buf.as_slice(), &fail_code, Some(fail_data.as_slice()));
        let res = b.helper.acceptance_failure(tok, fail_params);
        assert!(res.is_err());
        let err_with_token = res.unwrap_err();
        assert_eq!(err_with_token.1, tok);
        match err_with_token.0 {
            VerificationError::ByteConversionError(e) => match e {
                ByteConversionError::ToSliceTooSmall(missmatch) => {
                    assert_eq!(
                        missmatch.expected,
                        fail_data.len() + RequestId::SIZE_AS_BYTES + fail_code.byte_width()
                    );
                    assert_eq!(missmatch.found, b.rep().allowed_source_data_len());
                }
                _ => {
                    panic!("{}", format!("Unexpected error {:?}", e))
                }
            },
            _ => {
                panic!("{}", format!("Unexpected error {:?}", err_with_token.0))
            }
        }
    }

    #[test]
    fn test_basic_acceptance_failure_with_fail_data() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        let fail_code = EcssEnumU8::new(10);
        let fail_data = EcssEnumU32::new(12);
        let mut fail_data_raw = [0; 4];
        fail_data.to_bytes(&mut fail_data_raw).unwrap();
        let fail_params = FailParams::new(&EMPTY_STAMP, &fail_code, Some(fail_data_raw.as_slice()));
        b.vr.acceptance_failure(tok, &mut sender, fail_params)
            .expect("Sending acceptance success failed");
        let cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 2,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: Some([10, 0, 0, 0, 12].to_vec()),
            req_id: tok.req_id,
        };
        assert_eq!(sender.service_queue.len(), 1);
        let info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    fn start_fail_check(sender: &mut TestSender, req_id: RequestId, fail_data_raw: [u8; 4]) {
        assert_eq!(sender.service_queue.len(), 2);
        let mut cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 1,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: None,
            req_id,
        };
        let mut info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 4,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 1,
            additional_data: Some([&[22], fail_data_raw.as_slice()].concat().to_vec()),
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_start_failure() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        let fail_code = EcssEnumU8::new(22);
        let fail_data: i32 = -12;
        let mut fail_data_raw = [0; 4];
        fail_data_raw.copy_from_slice(fail_data.to_be_bytes().as_slice());
        let fail_params = FailParams::new(&EMPTY_STAMP, &fail_code, Some(fail_data_raw.as_slice()));

        let accepted_token =
            b.vr.acceptance_success(tok, &mut sender, &EMPTY_STAMP)
                .expect("Sending acceptance success failed");
        let empty =
            b.vr.start_failure(accepted_token, &mut sender, fail_params)
                .expect("Start failure failure");
        assert_eq!(empty, ());
        start_fail_check(&mut sender, tok.req_id, fail_data_raw);
    }

    #[test]
    fn test_start_failure_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        let fail_code = EcssEnumU8::new(22);
        let fail_data: i32 = -12;
        let mut fail_data_raw = [0; 4];
        fail_data_raw.copy_from_slice(fail_data.to_be_bytes().as_slice());
        let fail_params = FailParams::new(&EMPTY_STAMP, &fail_code, Some(fail_data_raw.as_slice()));

        let accepted_token = b
            .helper
            .acceptance_success(tok, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let empty = b
            .helper
            .start_failure(accepted_token, fail_params)
            .expect("Start failure failure");
        assert_eq!(empty, ());
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        start_fail_check(sender, tok.req_id, fail_data_raw);
    }

    fn step_success_check(sender: &mut TestSender, req_id: RequestId) {
        let mut cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 1,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: None,
            req_id,
        };
        let mut info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
        cmp_info = TmInfo {
            time_stamp: [0, 1, 0, 1, 0, 1, 0],
            subservice: 3,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 1,
            additional_data: None,
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
        cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 5,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 2,
            additional_data: Some([0].to_vec()),
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
        cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 5,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 3,
            additional_data: Some([1].to_vec()),
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_steps_success() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        let accepted_token = b
            .rep()
            .acceptance_success(tok, &mut sender, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let started_token = b
            .rep()
            .start_success(accepted_token, &mut sender, &[0, 1, 0, 1, 0, 1, 0])
            .expect("Sending start success failed");
        let mut empty = b
            .rep()
            .step_success(
                &started_token,
                &mut sender,
                &EMPTY_STAMP,
                EcssEnumU8::new(0),
            )
            .expect("Sending step 0 success failed");
        assert_eq!(empty, ());
        empty =
            b.vr.step_success(
                &started_token,
                &mut sender,
                &EMPTY_STAMP,
                EcssEnumU8::new(1),
            )
            .expect("Sending step 1 success failed");
        assert_eq!(empty, ());
        assert_eq!(sender.service_queue.len(), 4);
        step_success_check(&mut sender, tok.req_id);
    }

    #[test]
    fn test_steps_success_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        let accepted_token = b
            .helper
            .acceptance_success(tok, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let started_token = b
            .helper
            .start_success(accepted_token, &[0, 1, 0, 1, 0, 1, 0])
            .expect("Sending start success failed");
        let mut empty = b
            .helper
            .step_success(&started_token, &EMPTY_STAMP, EcssEnumU8::new(0))
            .expect("Sending step 0 success failed");
        assert_eq!(empty, ());
        empty = b
            .helper
            .step_success(&started_token, &EMPTY_STAMP, EcssEnumU8::new(1))
            .expect("Sending step 1 success failed");
        assert_eq!(empty, ());
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        assert_eq!(sender.service_queue.len(), 4);
        step_success_check(sender, tok.req_id);
    }

    fn check_step_failure(sender: &mut TestSender, req_id: RequestId, fail_data_raw: [u8; 4]) {
        assert_eq!(sender.service_queue.len(), 4);
        let mut cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 1,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: None,
            req_id,
        };
        let mut info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        cmp_info = TmInfo {
            time_stamp: [0, 1, 0, 1, 0, 1, 0],
            subservice: 3,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 1,
            additional_data: None,
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 5,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 2,
            additional_data: Some([0].to_vec()),
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 6,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 3,
            additional_data: Some(
                [
                    [1].as_slice(),
                    &[0, 0, 0x10, 0x20],
                    fail_data_raw.as_slice(),
                ]
                .concat()
                .to_vec(),
            ),
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_step_failure() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        let req_id = tok.req_id;
        let fail_code = EcssEnumU32::new(0x1020);
        let fail_data: f32 = -22.3232;
        let mut fail_data_raw = [0; 4];
        fail_data_raw.copy_from_slice(fail_data.to_be_bytes().as_slice());
        let fail_step = EcssEnumU8::new(1);
        let fail_params = FailParamsWithStep::new(
            &EMPTY_STAMP,
            &fail_step,
            &fail_code,
            Some(fail_data_raw.as_slice()),
        );

        let accepted_token =
            b.vr.acceptance_success(tok, &mut sender, &EMPTY_STAMP)
                .expect("Sending acceptance success failed");
        let started_token =
            b.vr.start_success(accepted_token, &mut sender, &[0, 1, 0, 1, 0, 1, 0])
                .expect("Sending start success failed");
        let mut empty =
            b.vr.step_success(
                &started_token,
                &mut sender,
                &EMPTY_STAMP,
                EcssEnumU8::new(0),
            )
            .expect("Sending completion success failed");
        assert_eq!(empty, ());
        empty =
            b.vr.step_failure(started_token, &mut sender, fail_params)
                .expect("Step failure failed");
        assert_eq!(empty, ());
        check_step_failure(&mut sender, req_id, fail_data_raw);
    }

    #[test]
    fn test_steps_failure_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        let req_id = tok.req_id;
        let fail_code = EcssEnumU32::new(0x1020);
        let fail_data: f32 = -22.3232;
        let mut fail_data_raw = [0; 4];
        fail_data_raw.copy_from_slice(fail_data.to_be_bytes().as_slice());
        let fail_step = EcssEnumU8::new(1);
        let fail_params = FailParamsWithStep::new(
            &EMPTY_STAMP,
            &fail_step,
            &fail_code,
            Some(fail_data_raw.as_slice()),
        );

        let accepted_token = b
            .helper
            .acceptance_success(tok, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let started_token = b
            .helper
            .start_success(accepted_token, &[0, 1, 0, 1, 0, 1, 0])
            .expect("Sending start success failed");
        let mut empty = b
            .helper
            .step_success(&started_token, &EMPTY_STAMP, EcssEnumU8::new(0))
            .expect("Sending completion success failed");
        assert_eq!(empty, ());
        empty = b
            .helper
            .step_failure(started_token, fail_params)
            .expect("Step failure failed");
        assert_eq!(empty, ());
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        check_step_failure(sender, req_id, fail_data_raw);
    }

    fn completion_fail_check(sender: &mut TestSender, req_id: RequestId) {
        assert_eq!(sender.service_queue.len(), 3);

        let mut cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 1,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: None,
            req_id,
        };
        let mut info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        cmp_info = TmInfo {
            time_stamp: [0, 1, 0, 1, 0, 1, 0],
            subservice: 3,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 1,
            additional_data: None,
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 8,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 2,
            additional_data: Some([0, 0, 0x10, 0x20].to_vec()),
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_completion_failure() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        let req_id = tok.req_id;
        let fail_code = EcssEnumU32::new(0x1020);
        let fail_params = FailParams::new(&EMPTY_STAMP, &fail_code, None);

        let accepted_token =
            b.vr.acceptance_success(tok, &mut sender, &EMPTY_STAMP)
                .expect("Sending acceptance success failed");
        let started_token =
            b.vr.start_success(accepted_token, &mut sender, &[0, 1, 0, 1, 0, 1, 0])
                .expect("Sending start success failed");
        let empty =
            b.vr.completion_failure(started_token, &mut sender, fail_params)
                .expect("Completion failure");
        assert_eq!(empty, ());
        completion_fail_check(&mut sender, req_id);
    }

    #[test]
    fn test_completion_failure_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        let req_id = tok.req_id;
        let fail_code = EcssEnumU32::new(0x1020);
        let fail_params = FailParams::new(&EMPTY_STAMP, &fail_code, None);

        let accepted_token = b
            .helper
            .acceptance_success(tok, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let started_token = b
            .helper
            .start_success(accepted_token, &[0, 1, 0, 1, 0, 1, 0])
            .expect("Sending start success failed");
        let empty = b
            .helper
            .completion_failure(started_token, fail_params)
            .expect("Completion failure");
        assert_eq!(empty, ());
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        completion_fail_check(sender, req_id);
    }

    fn completion_success_check(sender: &mut TestSender, req_id: RequestId) {
        assert_eq!(sender.service_queue.len(), 3);
        let cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 1,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 0,
            additional_data: None,
            req_id,
        };
        let mut info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);

        let cmp_info = TmInfo {
            time_stamp: [0, 1, 0, 1, 0, 1, 0],
            subservice: 3,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 1,
            additional_data: None,
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
        let cmp_info = TmInfo {
            time_stamp: EMPTY_STAMP,
            subservice: 7,
            dest_id: 0,
            apid: TEST_APID,
            msg_counter: 2,
            additional_data: None,
            req_id,
        };
        info = sender.service_queue.pop_front().unwrap();
        assert_eq!(info, cmp_info);
    }

    #[test]
    fn test_complete_success_sequence() {
        let (mut b, tok) = base_init(false);
        let mut sender = TestSender::default();
        let accepted_token =
            b.vr.acceptance_success(tok, &mut sender, &EMPTY_STAMP)
                .expect("Sending acceptance success failed");
        let started_token =
            b.vr.start_success(accepted_token, &mut sender, &[0, 1, 0, 1, 0, 1, 0])
                .expect("Sending start success failed");
        let empty =
            b.vr.completion_success(started_token, &mut sender, &EMPTY_STAMP)
                .expect("Sending completion success failed");
        assert_eq!(empty, ());
        completion_success_check(&mut sender, tok.req_id);
    }

    #[test]
    fn test_complete_success_sequence_with_helper() {
        let (mut b, tok) = base_with_helper_init();
        let accepted_token = b
            .helper
            .acceptance_success(tok, &EMPTY_STAMP)
            .expect("Sending acceptance success failed");
        let started_token = b
            .helper
            .start_success(accepted_token, &[0, 1, 0, 1, 0, 1, 0])
            .expect("Sending start success failed");
        let empty = b
            .helper
            .completion_success(started_token, &EMPTY_STAMP)
            .expect("Sending completion success failed");
        assert_eq!(empty, ());
        let sender: &mut TestSender = b.helper.sender.downcast_mut().unwrap();
        completion_success_check(sender, tok.req_id);
    }
}
