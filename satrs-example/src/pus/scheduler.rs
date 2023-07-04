use crate::pus::{
    AcceptedTc, PartialPusHandlingError, PusPacketHandlerResult, PusPacketHandlingError,
    PusServiceBase,
};
use delegate::delegate;
use satrs_core::pool::{SharedPool, StoreAddr};
use satrs_core::pus::scheduling::PusScheduler;
use satrs_core::pus::verification::{
    pus_11_generic_tc_check, FailParams, StdVerifReporterWithSender, TcStateAccepted,
    VerificationToken,
};
use satrs_core::pus::GenericTcCheckError;
use satrs_core::spacepackets::ecss::{scheduling, PusPacket};
use satrs_core::spacepackets::tc::PusTc;
use satrs_core::spacepackets::time::cds::TimeProvider;
use satrs_core::spacepackets::time::TimeWriter;
use satrs_core::tmtc::tm_helper::{PusTmWithCdsShortHelper, SharedTmStore};
use satrs_example::tmtc_err;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

pub struct PusService11SchedHandler {
    psb: PusServiceBase,
    scheduler: PusScheduler,
}

impl PusService11SchedHandler {
    pub fn new(
        receiver: Receiver<AcceptedTc>,
        tc_pool: SharedPool,
        tm_tx: Sender<StoreAddr>,
        tm_store: SharedTmStore,
        tm_apid: u16,
        verification_handler: StdVerifReporterWithSender,
        scheduler: PusScheduler,
    ) -> Self {
        Self {
            psb: PusServiceBase::new(
                receiver,
                tc_pool,
                tm_tx,
                tm_store,
                tm_apid,
                verification_handler,
            ),
            scheduler,
        }
    }

    pub fn handle_next_packet(&mut self) -> Result<PusPacketHandlerResult, PusPacketHandlingError> {
        return match self.psb.tc_rx.try_recv() {
            Ok((addr, token)) => self.handle_one_tc(addr, token),
            Err(e) => match e {
                TryRecvError::Empty => Ok(PusPacketHandlerResult::Empty),
                TryRecvError::Disconnected => Err(PusPacketHandlingError::QueueDisconnected),
            },
        };
    }
    pub fn handle_one_tc(
        &mut self,
        addr: StoreAddr,
        token: VerificationToken<TcStateAccepted>,
    ) -> Result<PusPacketHandlerResult, PusPacketHandlingError> {
        let mut partial_result = self.psb.update_stamp().err();
        {
            // Keep locked section as short as possible.
            let mut tc_pool = self
                .psb
                .tc_store
                .write()
                .map_err(|e| PusPacketHandlingError::RwGuardError(format!("{e}")))?;
            let tc_guard = tc_pool.read_with_guard(addr);
            let tc_raw = tc_guard.read().unwrap();
            self.psb.pus_buf[0..tc_raw.len()].copy_from_slice(tc_raw);
        }
        let (tc, tc_size) = PusTc::from_bytes(&self.psb.pus_buf).unwrap();
        let std_service = scheduling::Subservice::try_from(tc.subservice());
        if std_service.is_err() {
            return Ok(PusPacketHandlerResult::CustomSubservice(token));
        }
        match std_service.unwrap() {
            scheduling::Subservice::TcEnableScheduling => {
                let start_token = self
                    .psb
                    .verification_handler
                    .start_success(token, Some(&self.psb.stamp_buf))
                    .expect("Error sending start success");

                self.scheduler.enable();
                if self.scheduler.is_enabled() {
                    self.psb
                        .verification_handler
                        .completion_success(start_token, Some(&self.psb.stamp_buf))
                        .expect("Error sending completion success");
                } else {
                    panic!("Failed to enable scheduler");
                }
            }
            scheduling::Subservice::TcDisableScheduling => {
                let start_token = self
                    .psb
                    .verification_handler
                    .start_success(token, Some(&self.psb.stamp_buf))
                    .expect("Error sending start success");

                self.scheduler.disable();
                if !self.scheduler.is_enabled() {
                    self.psb
                        .verification_handler
                        .completion_success(start_token, Some(&self.psb.stamp_buf))
                        .expect("Error sending completion success");
                } else {
                    panic!("Failed to disable scheduler");
                }
            }
            scheduling::Subservice::TcResetScheduling => {
                let start_token = self
                    .psb
                    .verification_handler
                    .start_success(token, Some(&self.psb.stamp_buf))
                    .expect("Error sending start success");

                let mut pool = self.psb.tc_store.write().expect("Locking pool failed");

                self.scheduler
                    .reset(pool.as_mut())
                    .expect("Error resetting TC Pool");

                self.psb
                    .verification_handler
                    .completion_success(start_token, Some(&self.psb.stamp_buf))
                    .expect("Error sending completion success");
            }
            scheduling::Subservice::TcInsertActivity => {
                let start_token = self
                    .psb
                    .verification_handler
                    .start_success(token, Some(&self.psb.stamp_buf))
                    .expect("error sending start success");

                let mut pool = self.psb.tc_store.write().expect("locking pool failed");
                self.scheduler
                    .insert_wrapped_tc::<TimeProvider>(&tc, pool.as_mut())
                    .expect("insertion of activity into pool failed");

                self.psb
                    .verification_handler
                    .completion_success(start_token, Some(&self.psb.stamp_buf))
                    .expect("sending completion success failed");
            }
            _ => {
                return Ok(PusPacketHandlerResult::CustomSubservice(token));
            }
        }
        Ok(PusPacketHandlerResult::CustomSubservice(token))
    }
}
