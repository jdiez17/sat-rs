mod ccsds;
mod pus;
mod tmtc;

use crate::tmtc::{core_tmtc_task, CoreTmtcArgs, TmStore, PUS_APID};
use fsrc_core::event_man::{
    EventManagerWithMpscQueue, MpscEventReceiver, MpscEventU32SendProvider, SendEventProvider,
};
use fsrc_core::events::EventU32;
use fsrc_core::hal::host::udp_server::UdpTcServer;
use fsrc_core::pool::{LocalPool, PoolCfg, SharedPool, StoreAddr};
use fsrc_core::pus::event_man::{
    DefaultPusMgmtBackendProvider, EventReporter, EventRequest, EventRequestWithToken,
    PusEventDispatcher,
};
use fsrc_core::pus::verification::{
    MpscVerifSender, VerificationReporterCfg, VerificationReporterWithSender,
};
use fsrc_core::pus::{EcssTmError, EcssTmSender};
use fsrc_core::tmtc::CcsdsError;
use fsrc_example::{OBSW_SERVER_ADDR, SERVER_PORT};
use spacepackets::time::{CdsShortTimeProvider, TimeWriter};
use spacepackets::tm::PusTm;
use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc::channel;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::thread;

struct TmFunnel {
    tm_funnel_rx: mpsc::Receiver<StoreAddr>,
    tm_server_tx: mpsc::Sender<StoreAddr>,
}

struct UdpTmtcServer {
    udp_tc_server: UdpTcServer<CcsdsError<()>>,
    tm_rx: mpsc::Receiver<StoreAddr>,
    tm_store: SharedPool,
}

unsafe impl Send for UdpTmtcServer {}

struct EventTmSender {
    store_helper: TmStore,
    sender: mpsc::Sender<StoreAddr>,
}

impl EventTmSender {
    fn new(store_helper: TmStore, sender: mpsc::Sender<StoreAddr>) -> Self {
        Self {
            store_helper,
            sender,
        }
    }
}

impl EcssTmSender for EventTmSender {
    type Error = mpsc::SendError<StoreAddr>;

    fn send_tm(&mut self, tm: PusTm) -> Result<(), EcssTmError<Self::Error>> {
        let addr = self.store_helper.add_pus_tm(&tm);
        self.sender.send(addr).map_err(EcssTmError::SendError)
    }
}
fn main() {
    println!("Running OBSW example");
    let pool_cfg = PoolCfg::new(vec![(8, 32), (4, 64), (2, 128)]);
    let tm_pool = LocalPool::new(pool_cfg);
    let tm_store: SharedPool = Arc::new(RwLock::new(Box::new(tm_pool)));
    let tm_store_helper = TmStore {
        pool: tm_store.clone(),
    };
    let addr = SocketAddr::new(IpAddr::V4(OBSW_SERVER_ADDR), SERVER_PORT);
    let (tm_funnel_tx, tm_funnel_rx) = channel();
    let (tm_server_tx, tm_server_rx) = channel();
    let sender = MpscVerifSender::new(tm_store.clone(), tm_funnel_tx.clone());
    let verif_cfg = VerificationReporterCfg::new(PUS_APID, 1, 2, 8).unwrap();
    let reporter_with_sender_0 = Arc::new(Mutex::new(VerificationReporterWithSender::new(
        verif_cfg,
        Box::new(sender),
    )));

    // Create event handling components
    let (event_request_tx, event_request_rx) = channel::<EventRequestWithToken>();
    let (event_sender, event_man_rx) = channel();
    let event_recv = MpscEventReceiver::<EventU32>::new(event_man_rx);
    let mut event_man = EventManagerWithMpscQueue::new(Box::new(event_recv));
    let event_reporter = EventReporter::new(PUS_APID, 128).unwrap();
    let pus_tm_backend = DefaultPusMgmtBackendProvider::<EventU32>::default();
    let mut pus_event_dispatcher =
        PusEventDispatcher::new(event_reporter, Box::new(pus_tm_backend));
    let (pus_event_man_tx, pus_event_man_rx) = channel();
    let pus_event_man_send_provider = MpscEventU32SendProvider::new(1, pus_event_man_tx);
    let reporter1 = reporter_with_sender_0.clone();
    event_man.subscribe_all(pus_event_man_send_provider.id());

    // Create clones here to allow move for thread 0
    let core_args = CoreTmtcArgs {
        tm_store: tm_store_helper.clone(),
        tm_sender: tm_funnel_tx.clone(),
        event_sender,
        event_request_tx,
    };
    let jh0 = thread::spawn(move || {
        core_tmtc_task(core_args, tm_server_rx, addr, reporter_with_sender_0);
    });

    let jh1 = thread::spawn(move || {
        let tm_funnel = TmFunnel {
            tm_server_tx,
            tm_funnel_rx,
        };
        loop {
            if let Ok(addr) = tm_funnel.tm_funnel_rx.recv() {
                tm_funnel
                    .tm_server_tx
                    .send(addr)
                    .expect("Sending TM to server failed");
            }
        }
    });

    let jh2 = thread::spawn(move || {
        let mut timestamp: [u8; 7] = [0; 7];
        let mut sender = EventTmSender::new(tm_store_helper, tm_funnel_tx);
        let mut time_provider = CdsShortTimeProvider::new(0, 0);
        let report_completion = |event_req: EventRequestWithToken, timestamp: &[u8]| {
            let mut reporter = reporter1
                .lock()
                .expect("Locking Verification reporter failed");
            reporter
                .completion_success(event_req.token, timestamp)
                .expect("Sending completion success failed");
        };
        loop {
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
            if let Ok((event, _param)) = pus_event_man_rx.try_recv() {
                update_time(&mut time_provider, &mut timestamp);
                pus_event_dispatcher
                    .generate_pus_event_tm_generic(&mut sender, &timestamp, event, None)
                    .expect("Sending TM as event failed");
            }
        }
    });

    jh0.join().expect("Joining UDP TMTC server thread failed");
    jh1.join().expect("Joining TM Funnel thread failed");
    jh2.join().expect("Joining Event Manager thread failed");
}

pub fn update_time(time_provider: &mut CdsShortTimeProvider, timestamp: &mut [u8]) {
    time_provider
        .update_from_now()
        .expect("Could not get current time");
    time_provider
        .write_to_bytes(timestamp)
        .expect("Writing timestamp failed");
}