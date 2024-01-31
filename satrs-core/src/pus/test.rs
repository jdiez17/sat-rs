use crate::pus::{
    EcssTcReceiver, EcssTmSender, PartialPusHandlingError, PusPacketHandlerResult,
    PusPacketHandlingError, PusTmWrapper,
};
use spacepackets::ecss::tm::{PusTmCreator, PusTmSecondaryHeader};
use spacepackets::ecss::PusPacket;
use spacepackets::SpHeader;
use std::boxed::Box;

use super::verification::VerificationReporterWithSender;
use super::{EcssTcInMemConverter, PusServiceBase, PusServiceHandler};

/// This is a helper class for [std] environments to handle generic PUS 17 (test service) packets.
/// This handler only processes ping requests and generates a ping reply for them accordingly.
pub struct PusService17TestHandler<TcInMemConverter: EcssTcInMemConverter> {
    pub psb: PusServiceHandler<TcInMemConverter>,
}

impl<TcInMemConverter: EcssTcInMemConverter> PusService17TestHandler<TcInMemConverter> {
    pub fn new(
        tc_receiver: Box<dyn EcssTcReceiver>,
        tm_sender: Box<dyn EcssTmSender>,
        tm_apid: u16,
        verification_handler: VerificationReporterWithSender,
        tc_in_mem_converter: TcInMemConverter,
    ) -> Self {
        Self {
            psb: PusServiceHandler::new(
                tc_receiver,
                tm_sender,
                tm_apid,
                verification_handler,
                tc_in_mem_converter,
            ),
        }
    }

    pub fn handle_one_tc(&mut self) -> Result<PusPacketHandlerResult, PusPacketHandlingError> {
        let possible_packet = self.psb.retrieve_and_accept_next_packet()?;
        if possible_packet.is_none() {
            return Ok(PusPacketHandlerResult::Empty);
        }
        let ecss_tc_and_token = possible_packet.unwrap();
        let tc = self
            .psb
            .tc_in_mem_converter
            .convert_ecss_tc_in_memory_to_reader(&ecss_tc_and_token)?;
        if tc.service() != 17 {
            return Err(PusPacketHandlingError::WrongService(tc.service()));
        }
        if tc.subservice() == 1 {
            let mut partial_error = None;
            let time_stamp = PusServiceBase::get_current_timestamp(&mut partial_error);
            let result = self
                .psb
                .common
                .verification_handler
                .get_mut()
                .start_success(ecss_tc_and_token.token, Some(&time_stamp))
                .map_err(|_| PartialPusHandlingError::Verification);
            let start_token = if let Ok(result) = result {
                Some(result)
            } else {
                partial_error = Some(result.unwrap_err());
                None
            };
            // Sequence count will be handled centrally in TM funnel.
            let mut reply_header = SpHeader::tm_unseg(self.psb.common.tm_apid, 0, 0).unwrap();
            let tc_header = PusTmSecondaryHeader::new_simple(17, 2, &time_stamp);
            let ping_reply = PusTmCreator::new(&mut reply_header, tc_header, &[], true);
            let result = self
                .psb
                .common
                .tm_sender
                .send_tm(PusTmWrapper::Direct(ping_reply))
                .map_err(PartialPusHandlingError::TmSend);
            if let Err(err) = result {
                partial_error = Some(err);
            }

            if let Some(start_token) = start_token {
                if self
                    .psb
                    .common
                    .verification_handler
                    .get_mut()
                    .completion_success(start_token, Some(&time_stamp))
                    .is_err()
                {
                    partial_error = Some(PartialPusHandlingError::Verification)
                }
            }
            if let Some(partial_error) = partial_error {
                return Ok(PusPacketHandlerResult::RequestHandledPartialSuccess(
                    partial_error,
                ));
            };
        } else {
            return Ok(PusPacketHandlerResult::CustomSubservice(
                tc.subservice(),
                ecss_tc_and_token.token,
            ));
        }
        Ok(PusPacketHandlerResult::RequestHandled)
    }
}

#[cfg(test)]
mod tests {
    use crate::pool::{LocalPool, PoolCfg, SharedPool};
    use crate::pus::test::PusService17TestHandler;
    use crate::pus::verification::{
        RequestId, VerificationReporterCfg, VerificationReporterWithSender,
    };
    use crate::pus::{
        EcssTcAndToken, EcssTcInStoreConverter, MpscTcInStoreReceiver, MpscTmInStoreSender,
    };
    use crate::tmtc::tm_helper::SharedTmStore;
    use spacepackets::ecss::tc::{PusTcCreator, PusTcSecondaryHeader};
    use spacepackets::ecss::tm::PusTmReader;
    use spacepackets::ecss::{PusPacket, WritablePusPacket};
    use spacepackets::{SequenceFlags, SpHeader};
    use std::boxed::Box;
    use std::sync::{mpsc, RwLock};
    use std::vec;

    const TEST_APID: u16 = 0x101;

    #[test]
    fn test_basic_ping_processing() {
        let mut pus_buf: [u8; 64] = [0; 64];
        let pool_cfg = PoolCfg::new(vec![(16, 16), (8, 32), (4, 64)]);
        let tc_pool = LocalPool::new(pool_cfg.clone());
        let tm_pool = LocalPool::new(pool_cfg);
        let tc_pool_shared = SharedPool::new(RwLock::new(Box::new(tc_pool)));
        let shared_tm_store = SharedTmStore::new(Box::new(tm_pool));
        let tm_pool_shared = shared_tm_store.clone_backing_pool();
        let (test_srv_tc_tx, test_srv_tc_rx) = mpsc::channel();
        let (tm_tx, tm_rx) = mpsc::channel();
        let verif_sender =
            MpscTmInStoreSender::new(0, "verif_sender", shared_tm_store.clone(), tm_tx.clone());
        let verif_cfg = VerificationReporterCfg::new(TEST_APID, 1, 2, 8).unwrap();
        let mut verification_handler =
            VerificationReporterWithSender::new(&verif_cfg, Box::new(verif_sender));
        let test_srv_tm_sender = MpscTmInStoreSender::new(0, "TEST_SENDER", shared_tm_store, tm_tx);
        let test_srv_tc_receiver = MpscTcInStoreReceiver::new(0, "TEST_RECEIVER", test_srv_tc_rx);
        let in_store_converter = EcssTcInStoreConverter::new(tc_pool_shared.clone(), 2048);
        let mut pus_17_handler = PusService17TestHandler::new(
            Box::new(test_srv_tc_receiver),
            Box::new(test_srv_tm_sender),
            TEST_APID,
            verification_handler.clone(),
            in_store_converter,
        );
        // Create a ping TC, verify acceptance.
        let mut sp_header = SpHeader::tc(TEST_APID, SequenceFlags::Unsegmented, 0, 0).unwrap();
        let sec_header = PusTcSecondaryHeader::new_simple(17, 1);
        let ping_tc = PusTcCreator::new_no_app_data(&mut sp_header, sec_header, true);
        let token = verification_handler.add_tc(&ping_tc);
        let token = verification_handler
            .acceptance_success(token, None)
            .unwrap();
        let tc_size = ping_tc.write_to_bytes(&mut pus_buf).unwrap();
        let mut tc_pool = tc_pool_shared.write().unwrap();
        let addr = tc_pool.add(&pus_buf[..tc_size]).unwrap();
        drop(tc_pool);
        // Send accepted TC to test service handler.
        test_srv_tc_tx
            .send(EcssTcAndToken::new(addr, token))
            .unwrap();
        let result = pus_17_handler.handle_one_tc();
        assert!(result.is_ok());
        // We should see 4 replies in the TM queue now: Acceptance TM, Start TM, ping reply and
        // Completion TM
        let mut next_msg = tm_rx.try_recv();
        assert!(next_msg.is_ok());
        let mut tm_addr = next_msg.unwrap();
        let tm_pool = tm_pool_shared.read().unwrap();
        let tm_raw = tm_pool.read(&tm_addr).unwrap();
        let (tm, _) = PusTmReader::new(tm_raw, 0).unwrap();
        assert_eq!(tm.service(), 1);
        assert_eq!(tm.subservice(), 1);
        let req_id = RequestId::from_bytes(tm.user_data()).expect("generating request ID failed");
        assert_eq!(req_id, token.req_id());

        // Acceptance TM
        next_msg = tm_rx.try_recv();
        assert!(next_msg.is_ok());
        tm_addr = next_msg.unwrap();
        let tm_raw = tm_pool.read(&tm_addr).unwrap();
        // Is generated with CDS short timestamp.
        let (tm, _) = PusTmReader::new(tm_raw, 7).unwrap();
        assert_eq!(tm.service(), 1);
        assert_eq!(tm.subservice(), 3);
        let req_id = RequestId::from_bytes(tm.user_data()).expect("generating request ID failed");
        assert_eq!(req_id, token.req_id());

        // Ping reply
        next_msg = tm_rx.try_recv();
        assert!(next_msg.is_ok());
        tm_addr = next_msg.unwrap();
        let tm_raw = tm_pool.read(&tm_addr).unwrap();
        // Is generated with CDS short timestamp.
        let (tm, _) = PusTmReader::new(tm_raw, 7).unwrap();
        assert_eq!(tm.service(), 17);
        assert_eq!(tm.subservice(), 2);
        assert!(tm.user_data().is_empty());

        // TM completion
        next_msg = tm_rx.try_recv();
        assert!(next_msg.is_ok());
        tm_addr = next_msg.unwrap();
        let tm_raw = tm_pool.read(&tm_addr).unwrap();
        // Is generated with CDS short timestamp.
        let (tm, _) = PusTmReader::new(tm_raw, 7).unwrap();
        assert_eq!(tm.service(), 1);
        assert_eq!(tm.subservice(), 7);
        let req_id = RequestId::from_bytes(tm.user_data()).expect("generating request ID failed");
        assert_eq!(req_id, token.req_id());
    }
}
