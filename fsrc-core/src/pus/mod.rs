//! All PUS support modules
//!
//! Currenty includes:
//!
//!  1. PUS Verification Service 1 module inside [verification]. Requires [alloc] support.
use downcast_rs::{impl_downcast, Downcast};
use spacepackets::ecss::PusError;
use spacepackets::time::TimestampError;
use spacepackets::tm::PusTm;
use spacepackets::{ByteConversionError, SizeMissmatch};

pub mod event;
pub mod verification;

/// Generic error type which is also able to wrap a user send error with the user supplied type E.
#[derive(Debug, Clone)]
pub enum EcssTmError<E> {
    /// Errors related to sending the verification telemetry to a TM recipient
    SendError(E),
    /// Errors related to the time stamp format of the telemetry
    TimestampError(TimestampError),
    /// Errors related to byte conversion, for example insufficient buffer size for given data
    ByteConversionError(ByteConversionError),
    /// Errors related to PUS packet format
    PusError(PusError),
}

impl<E> From<ByteConversionError> for EcssTmError<E> {
    fn from(e: ByteConversionError) -> Self {
        EcssTmError::ByteConversionError(e)
    }
}

/// Generic trait for a user supplied sender object. This sender object is responsible for sending
/// telemetry to a TM sink. The [Downcast] trait
/// is implemented to allow passing the sender as a boxed trait object and still retrieve the
/// concrete type at a later point.
pub trait EcssTmSender<E>: Downcast + Send {
    fn send_tm(&mut self, tm: PusTm) -> Result<(), EcssTmError<E>>;
}

impl_downcast!(EcssTmSender<E>);

pub(crate) fn source_buffer_large_enough<E>(cap: usize, len: usize) -> Result<(), EcssTmError<E>> {
    if len > cap {
        return Err(EcssTmError::ByteConversionError(
            ByteConversionError::ToSliceTooSmall(SizeMissmatch {
                found: cap,
                expected: len,
            }),
        ));
    }
    Ok(())
}
