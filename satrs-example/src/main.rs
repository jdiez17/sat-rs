mod ccsds;
mod hk;
mod logging;
mod pus;
mod requests;
mod tmtc;

use log::{info, warn};
use std::collections::hash_map::Entry;

use crate::hk::AcsHkIds;
use crate::logging::setup_logger;
use crate::pus::event::Pus5Wrapper;
use crate::pus::scheduler::Pus11Wrapper;
use crate::pus::test::Service17CustomWrapper;
use crate::pus::PusTcMpscRouter;
use crate::requests::{Request, RequestWithToken};
use crate::tmtc::{
    core_tmtc_task, OtherArgs, PusTcSource, TcArgs, TcStore, TmArgs, TmFunnel, PUS_APID,
};
use satrs_core::event_man::{
    EventManagerWithMpscQueue, MpscEventReceiver, MpscEventU32SendProvider, SendEventProvider,
};
use satrs_core::events::EventU32;
use satrs_core::hk::HkRequest;
use satrs_core::pool::{LocalPool, PoolCfg};
use satrs_core::pus::event_man::{
    DefaultPusMgmtBackendProvider, EventReporter, EventRequest, EventRequestWithToken,
    PusEventDispatcher,
};
use satrs_core::pus::event_srv::PusService5EventHandler;
use satrs_core::pus::hk::Subservice as HkSubservice;
use satrs_core::pus::scheduler::PusScheduler;
use satrs_core::pus::scheduler_srv::PusService11SchedHandler;
use satrs_core::pus::test::PusService17TestHandler;
use satrs_core::pus::verification::{
    MpscVerifSender, VerificationReporterCfg, VerificationReporterWithSender,
};
use satrs_core::pus::MpscTmtcInStoreSender;
use satrs_core::seq_count::{SeqCountProviderSimple, SequenceCountProviderCore};
use satrs_core::spacepackets::ecss::{PusPacket, SerializablePusPacket};
use satrs_core::spacepackets::{
    time::cds::TimeProvider,
    time::TimeWriter,
    tm::{PusTm, PusTmSecondaryHeader},
    SequenceFlags, SpHeader,
};
use satrs_core::tmtc::tm_helper::SharedTmStore;
use satrs_core::tmtc::AddressableId;
use satrs_example::{RequestTargetId, OBSW_SERVER_ADDR, SERVER_PORT};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc::{channel, TryRecvError};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

fn main() {
    setup_logger().expect("setting up logging with fern failed");
    println!("Running OBSW example");
    let tm_pool = LocalPool::new(PoolCfg::new(vec![
        (30, 32),
        (15, 64),
        (15, 128),
        (15, 256),
        (15, 1024),
        (15, 2048),
    ]));
    let tm_store = SharedTmStore::new(Arc::new(RwLock::new(Box::new(tm_pool))));
    let tm_store_event = tm_store.clone();
    let tc_pool = LocalPool::new(PoolCfg::new(vec![
        (30, 32),
        (15, 64),
        (15, 128),
        (15, 256),
        (15, 1024),
        (15, 2048),
    ]));
    let tc_store = TcStore {
        pool: Arc::new(RwLock::new(Box::new(tc_pool))),
    };

    let seq_count_provider = SeqCountProviderSimple::new();
    let mut msg_counter_map: HashMap<u8, u16> = HashMap::new();
    let sock_addr = SocketAddr::new(IpAddr::V4(OBSW_SERVER_ADDR), SERVER_PORT);
    let (tc_source_tx, tc_source_rx) = channel();
    let (tm_funnel_tx, tm_funnel_rx) = channel();
    let (tm_server_tx, tm_server_rx) = channel();
    let verif_sender = MpscVerifSender::new(
        0,
        "verif_sender",
        tm_store.backing_pool(),
        tm_funnel_tx.clone(),
    );
    let verif_cfg = VerificationReporterCfg::new(PUS_APID, 1, 2, 8).unwrap();
    // Every software component which needs to generate verification telemetry, gets a cloned
    // verification reporter.
    let verif_reporter = VerificationReporterWithSender::new(&verif_cfg, Box::new(verif_sender));
    let mut reporter_event_handler = verif_reporter.clone();
    let mut reporter_aocs = verif_reporter.clone();

    // Create event handling components
    // These sender handles are used to send event requests, for example to enable or disable
    // certain events
    let (event_request_tx, event_request_rx) = channel::<EventRequestWithToken>();
    // The sender handle is the primary sender handle for all components which want to create events.
    // The event manager will receive the RX handle to receive all the events.
    let (event_sender, event_man_rx) = channel();
    let event_recv = MpscEventReceiver::<EventU32>::new(event_man_rx);
    let test_srv_event_sender = event_sender.clone();
    let mut event_man = EventManagerWithMpscQueue::new(Box::new(event_recv));

    // All events sent to the manager are routed to the PUS event manager, which generates PUS event
    // telemetry for each event.
    let event_reporter = EventReporter::new(PUS_APID, 128).unwrap();
    let pus_tm_backend = DefaultPusMgmtBackendProvider::<EventU32>::default();
    let mut pus_event_dispatcher =
        PusEventDispatcher::new(event_reporter, Box::new(pus_tm_backend));
    let (pus_event_man_tx, pus_event_man_rx) = channel();
    let pus_event_man_send_provider = MpscEventU32SendProvider::new(1, pus_event_man_tx);
    event_man.subscribe_all(pus_event_man_send_provider.id());
    event_man.add_sender(pus_event_man_send_provider);

    // Some request are targetable. This map is used to retrieve sender handles based on a target ID.
    let mut request_map = HashMap::new();
    let (acs_thread_tx, acs_thread_rx) = channel::<RequestWithToken>();
    request_map.insert(RequestTargetId::AcsSubsystem as u32, acs_thread_tx);

    let tc_source_wrapper = PusTcSource {
        tc_store: tc_store.clone(),
        tc_source: tc_source_tx,
    };

    // Create clones here to allow moving the values
    let core_args = OtherArgs {
        sock_addr,
        verif_reporter: verif_reporter.clone(),
        event_sender,
        request_map,
    };
    let tc_args = TcArgs {
        tc_source: tc_source_wrapper.clone(),
        tc_receiver: tc_source_rx,
    };
    let tm_args = TmArgs {
        tm_store: tm_store.clone(),
        tm_sink_sender: tm_funnel_tx.clone(),
        tm_server_rx,
    };

    let aocs_to_funnel = tm_funnel_tx.clone();
    let mut aocs_tm_store = tm_store.clone();

    let (pus_test_tx, pus_test_rx) = channel();
    let (pus_event_tx, pus_event_rx) = channel();
    let (pus_sched_tx, pus_sched_rx) = channel();
    let (pus_hk_tx, pus_hk_rx) = channel();
    let (pus_action_tx, pus_action_rx) = channel();
    let pus_router = PusTcMpscRouter {
        test_service_receiver: pus_test_tx,
        event_service_receiver: pus_event_tx,
        sched_service_receiver: pus_sched_tx,
        hk_service_receiver: pus_hk_tx,
        action_service_receiver: pus_action_tx,
    };
    let pus17_handler = PusService17TestHandler::new(
        pus_test_rx,
        tc_store.pool.clone(),
        tm_funnel_tx.clone(),
        tm_store.clone(),
        PUS_APID,
        verif_reporter.clone(),
    );
    let mut pus_17_wrapper = Service17CustomWrapper {
        pus17_handler,
        test_srv_event_sender,
    };
    let scheduler = PusScheduler::new_with_current_init_time(Duration::from_secs(5))
        .expect("Creating PUS Scheduler failed");
    let pus_11_handler = PusService11SchedHandler::new(
        pus_sched_rx,
        tc_store.pool.clone(),
        tm_funnel_tx.clone(),
        tm_store.clone(),
        PUS_APID,
        verif_reporter.clone(),
        scheduler,
    );
    let mut pus_11_wrapper = Pus11Wrapper {
        pus_11_handler,
        tc_source_wrapper,
    };
    let pus_5_handler = PusService5EventHandler::new(
        pus_event_rx,
        tc_store.pool.clone(),
        tm_funnel_tx.clone(),
        tm_store.clone(),
        PUS_APID,
        verif_reporter,
        event_request_tx,
    );
    let mut pus_5_wrapper = Pus5Wrapper { pus_5_handler };

    info!("Starting TMTC task");
    let jh0 = thread::Builder::new()
        .name("TMTC".to_string())
        .spawn(move || {
            core_tmtc_task(core_args, tc_args, tm_args, pus_router);
        })
        .unwrap();

    info!("Starting TM funnel task");
    let jh1 = thread::Builder::new()
        .name("TM Funnel".to_string())
        .spawn(move || {
            let mut tm_buf: [u8; 2048] = [0; 2048];
            let tm_funnel = TmFunnel {
                tm_server_tx,
                tm_funnel_rx,
            };
            loop {
                if let Ok(addr) = tm_funnel.tm_funnel_rx.recv() {
                    // Read the TM, set sequence counter and message counter, and finally write
                    // it back with the updated CRC.
                    // We could theoretically manipulate the counters and the CRC directly
                    // in place as an optimization, but I don't think this is necessary..
                    let shared_pool = tm_store.backing_pool();
                    let mut pool_guard = shared_pool.write().expect("Locking TM pool failed");
                    let tm_raw = pool_guard
                        .modify(&addr)
                        .expect("Reading TM from pool failed");
                    tm_buf[0..tm_raw.len()].copy_from_slice(&tm_raw);
                    let (mut tm, size) =
                        PusTm::from_bytes(&tm_buf, 7).expect("Creating TM from raw slice failed");
                    tm.sp_header.set_apid(PUS_APID);
                    tm.sp_header
                        .set_seq_count(seq_count_provider.get_and_increment());
                    let entry = msg_counter_map.entry(tm.service()).or_insert(0);
                    tm.sec_header.msg_counter = *entry;
                    if *entry == u16::MAX {
                        *entry = 0;
                    } else {
                        *entry += 1;
                    }
                    tm.calc_crc_on_serialization = true;
                    tm.write_to_bytes(tm_raw)
                        .expect("Writing PUS TM back failed");
                    tm_funnel
                        .tm_server_tx
                        .send(addr)
                        .expect("Sending TM to server failed");
                }
            }
        })
        .unwrap();

    info!("Starting event handling task");
    let jh2 = thread::Builder::new()
        .name("Event".to_string())
        .spawn(move || {
            let mut timestamp: [u8; 7] = [0; 7];
            let mut sender = MpscTmtcInStoreSender::new(
                1,
                "event_sender",
                tm_store_event.backing_pool(),
                tm_funnel_tx,
            );
            let mut time_provider = TimeProvider::new_with_u16_days(0, 0);
            let mut report_completion = |event_req: EventRequestWithToken, timestamp: &[u8]| {
                reporter_event_handler
                    .completion_success(event_req.token.try_into().unwrap(), Some(timestamp))
                    .expect("Sending completion success failed");
            };
            loop {
                // handle event requests
                if let Ok(event_req) = event_request_rx.try_recv() {
                    match event_req.request {
                        EventRequest::Enable(event) => {
                            pus_event_dispatcher
                                .enable_tm_for_event(&event)
                                .expect("Enabling TM failed");
                            update_time(&mut time_provider, &mut timestamp);
                            report_completion(event_req, &timestamp);
                        }
                        EventRequest::Disable(event) => {
                            pus_event_dispatcher
                                .disable_tm_for_event(&event)
                                .expect("Disabling TM failed");
                            update_time(&mut time_provider, &mut timestamp);
                            report_completion(event_req, &timestamp);
                        }
                    }
                }

                // Perform the event routing.
                event_man
                    .try_event_handling()
                    .expect("event handling failed");

                // Perform the generation of PUS event packets
                if let Ok((event, _param)) = pus_event_man_rx.try_recv() {
                    update_time(&mut time_provider, &mut timestamp);
                    pus_event_dispatcher
                        .generate_pus_event_tm_generic(&mut sender, &timestamp, event, None)
                        .expect("Sending TM as event failed");
                }
                thread::sleep(Duration::from_millis(400));
            }
        })
        .unwrap();

    info!("Starting AOCS thread");
    let jh3 = thread::Builder::new()
        .name("AOCS".to_string())
        .spawn(move || {
            let mut timestamp: [u8; 7] = [0; 7];
            let mut time_provider = TimeProvider::new_with_u16_days(0, 0);
            loop {
                match acs_thread_rx.try_recv() {
                    Ok(request) => {
                        info!(
                            "ACS thread: Received HK request {:?}",
                            request.targeted_request
                        );
                        update_time(&mut time_provider, &mut timestamp);
                        match request.targeted_request.request {
                            Request::HkRequest(hk_req) => match hk_req {
                                HkRequest::OneShot(unique_id) => {
                                    let target = request.targeted_request.target_id;
                                    assert_eq!(target, RequestTargetId::AcsSubsystem as u32);
                                    if request.targeted_request.target_id
                                        == AcsHkIds::TestMgmSet as u32
                                    {
                                        let mut sp_header = SpHeader::tm(
                                            PUS_APID,
                                            SequenceFlags::Unsegmented,
                                            0,
                                            0,
                                        )
                                        .unwrap();
                                        let sec_header = PusTmSecondaryHeader::new_simple(
                                            3,
                                            HkSubservice::TmHkPacket as u8,
                                            &timestamp,
                                        );
                                        let mut buf: [u8; 8] = [0; 8];
                                        let addressable_id = AddressableId {
                                            target_id: target,
                                            unique_id,
                                        };
                                        addressable_id.write_to_be_bytes(&mut buf).unwrap();
                                        let pus_tm = PusTm::new(
                                            &mut sp_header,
                                            sec_header,
                                            Some(&buf),
                                            true,
                                        );
                                        let addr = aocs_tm_store.add_pus_tm(&pus_tm);
                                        aocs_to_funnel.send(addr).expect("Sending HK TM failed");
                                    }
                                }
                                HkRequest::Enable(_) => {}
                                HkRequest::Disable(_) => {}
                                HkRequest::ModifyCollectionInterval(_, _) => {}
                            },
                            Request::ModeRequest(_mode_req) => {
                                warn!("mode request handling not implemented yet")
                            }
                        }
                        let started_token = reporter_aocs
                            .start_success(request.token, Some(&timestamp))
                            .expect("Sending start success failed");
                        reporter_aocs
                            .completion_success(started_token, Some(&timestamp))
                            .expect("Sending completion success failed");
                    }
                    Err(e) => match e {
                        TryRecvError::Empty => {}
                        TryRecvError::Disconnected => {
                            warn!("ACS thread: Message Queue TX disconnected!")
                        }
                    },
                }
                thread::sleep(Duration::from_millis(500));
            }
        })
        .unwrap();

    info!("Starting PUS handler thread");
    let jh4 = thread::Builder::new()
        .name("PUS".to_string())
        .spawn(move || loop {
            pus_11_wrapper.release_tcs();
            loop {
                let mut all_queues_empty = true;
                let mut is_srv_finished = |srv_handler_finished: bool| {
                    if !srv_handler_finished {
                        all_queues_empty = false;
                    }
                };
                is_srv_finished(pus_17_wrapper.handle_next_packet());
                is_srv_finished(pus_11_wrapper.handle_next_packet());
                is_srv_finished(pus_5_wrapper.handle_next_packet());
                if all_queues_empty {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(200));
        })
        .unwrap();
    jh0.join().expect("Joining UDP TMTC server thread failed");
    jh1.join().expect("Joining TM Funnel thread failed");
    jh2.join().expect("Joining Event Manager thread failed");
    jh3.join().expect("Joining AOCS thread failed");
    jh4.join().expect("Joining PUS handler thread failed");
}

pub fn update_time(time_provider: &mut TimeProvider, timestamp: &mut [u8]) {
    time_provider
        .update_from_now()
        .expect("Could not get current time");
    time_provider
        .write_to_bytes(timestamp)
        .expect("Writing timestamp failed");
}
