//! Event management and forwarding
//!
//! This module provides components to perform event routing. The most important component for this
//! task is the [EventManager]. It uses a map of event listeners and uses a dynamic [EventReceiver]
//! instance to receive all events and then route them to event subscribers where appropriate.
//!
//! One common use case for satellite systems is to offer a light-weight publish-subscribe mechanism
//! and IPC mechanism for software and hardware events which are also packaged as telemetry or can
//! trigger a system response. This can be done with the [EventManager] like this:
//!
//!  1. Provide a concrete [SendEventProvider] implementation and a concrete [EventReceiver]
//!     implementation. These abstraction allow to use different message queue backends.
//!     A straightforward implementation where dynamic memory allocation is not a big concern could
//!     use [std::sync::mpsc::channel] to do this. It is recommended that these implementations
//!     derive [Clone].
//!  2. Each event creator gets a (cloned) sender component which allows it to send events to the
//!     manager.
//!  3. The event manager receives the receiver component so all events are routed to the
//!     manager.
//!  4. Additional channels are created for each event receiver and/or subscriber.
//!     The sender component is used with the [SendEventProvider] trait and the subscription API
//!     provided by the [EventManager] to subscribe for individual events, whole group of events or
//!     all events. The receiver/subscribers can then receive all subscribed events via the receiver
//!     end.
//!
//! Some components like a PUS Event Service or PUS Event Action Service might require all
//! events to package them as telemetry or start actions where applicable.
//! Other components might only be interested in certain events. For example, a thermal system
//! handler might only be interested in temperature events generated by a thermal sensor component.
use crate::events::{EventU16, EventU32, GenericEvent, LargestEventRaw, LargestGroupIdRaw};
use crate::params::{Params, ParamsHeapless};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use hashbrown::HashMap;

#[cfg(feature = "std")]
pub use stdmod::*;

#[derive(PartialEq, Eq, Hash, Copy, Clone)]
enum ListenerType {
    Single(LargestEventRaw),
    Group(LargestGroupIdRaw),
    All,
}

pub type EventWithHeaplessAuxData<Event> = (Event, Option<ParamsHeapless>);
pub type EventU32WithHeaplessAuxData = EventWithHeaplessAuxData<EventU32>;
pub type EventU16WithHeaplessAuxData = EventWithHeaplessAuxData<EventU16>;

pub type EventWithAuxData<Event> = (Event, Option<Params>);
pub type EventU32WithAuxData = EventWithAuxData<EventU32>;
pub type EventU16WithAuxData = EventWithAuxData<EventU16>;

pub trait SendEventProvider<Provider: GenericEvent, AuxDataProvider = Params> {
    type Error;

    fn id(&self) -> u32;
    fn send_no_data(&mut self, event: Provider) -> Result<(), Self::Error> {
        self.send(event, None)
    }
    fn send(
        &mut self,
        event: Provider,
        aux_data: Option<AuxDataProvider>,
    ) -> Result<(), Self::Error>;
}

struct Listener<E, Event: GenericEvent, AuxDataProvider = Params> {
    ltype: ListenerType,
    send_provider: Box<dyn SendEventProvider<Event, AuxDataProvider, Error = E>>,
}

/// Generic abstraction for an event receiver.
pub trait EventReceiver<Event: GenericEvent, AuxDataProvider = Params> {
    /// This function has to be provided by any event receiver. A receive call may or may not return
    /// an event.
    ///
    /// To allow returning arbitrary additional auxiliary data, a mutable slice is passed to the
    /// [Self::receive] call as well. Receivers can write data to this slice, but care must be taken
    /// to avoid panics due to size missmatches or out of bound writes.
    fn receive(&mut self) -> Option<(Event, Option<AuxDataProvider>)>;
}

/// Generic event manager implementation.
///
/// # Generics
///
///  * `SendProviderError`: [SendEventProvider] error type
///  * `Event`: Concrete event provider, currently either [EventU32] or [EventU16]
///  * `AuxDataProvider`: Concrete auxiliary data provder, currently either [Params] or
///     [ParamsHeapless]
pub struct EventManager<SendProviderError, Event: GenericEvent = EventU32, AuxDataProvider = Params>
{
    listeners: HashMap<ListenerType, Vec<Listener<SendProviderError, Event, AuxDataProvider>>>,
    event_receiver: Box<dyn EventReceiver<Event, AuxDataProvider>>,
}

/// Safety: It is safe to implement [Send] because all fields in the [EventManager] are [Send]
/// as well
#[cfg(feature = "std")]
unsafe impl<E, Event: GenericEvent + Send, AuxDataProvider: Send> Send
    for EventManager<E, Event, AuxDataProvider>
{
}

pub enum HandlerResult<Provider: GenericEvent, AuxDataProvider> {
    Empty,
    Handled(u32, Provider, Option<AuxDataProvider>),
}

impl<E, Event: GenericEvent + Copy> EventManager<E, Event> {
    pub fn new(event_receiver: Box<dyn EventReceiver<Event>>) -> Self {
        EventManager {
            listeners: HashMap::new(),
            event_receiver,
        }
    }
    pub fn subscribe_single(
        &mut self,
        event: Event,
        dest: impl SendEventProvider<Event, Error = E> + 'static,
    ) {
        self.update_listeners(ListenerType::Single(event.raw_as_largest_type()), dest);
    }

    pub fn subscribe_group(
        &mut self,
        group_id: LargestGroupIdRaw,
        dest: impl SendEventProvider<Event, Error = E> + 'static,
    ) {
        self.update_listeners(ListenerType::Group(group_id), dest);
    }

    /// Subscribe for all events received by the manager.
    ///
    /// For example, this can be useful for a handler component which sends every event as
    /// a telemetry packet.
    pub fn subscribe_all(
        &mut self,
        send_provider: impl SendEventProvider<Event, Error = E> + 'static,
    ) {
        self.update_listeners(ListenerType::All, send_provider);
    }

    /// Helper function which removes single subscriptions for which a group subscription already
    /// exists.
    pub fn remove_single_subscriptions_for_group(
        &mut self,
        group_id: LargestGroupIdRaw,
        dest: impl SendEventProvider<Event, Error = E> + 'static,
    ) {
        if self.listeners.contains_key(&ListenerType::Group(group_id)) {
            for (ltype, listeners) in &mut self.listeners {
                if let ListenerType::Single(_) = ltype {
                    listeners.retain(|f| f.send_provider.id() != dest.id());
                }
            }
        }
    }
}

impl<E, Event: GenericEvent + Copy, AuxDataProvider: Clone>
    EventManager<E, Event, AuxDataProvider>
{
    fn update_listeners(
        &mut self,
        key: ListenerType,
        dest: impl SendEventProvider<Event, AuxDataProvider, Error = E> + 'static,
    ) {
        if !self.listeners.contains_key(&key) {
            self.listeners.insert(
                key,
                vec![Listener {
                    ltype: key,
                    send_provider: Box::new(dest),
                }],
            );
        } else {
            let vec = self.listeners.get_mut(&key).unwrap();
            // To prevent double insertions
            for entry in vec.iter() {
                if entry.ltype == key && entry.send_provider.id() == dest.id() {
                    return;
                }
            }
            vec.push(Listener {
                ltype: key,
                send_provider: Box::new(dest),
            });
        }
    }

    pub fn try_event_handling(&mut self) -> Result<HandlerResult<Event, AuxDataProvider>, E> {
        let mut err_status = None;
        let mut num_recipients = 0;
        let mut send_handler =
            |event: Event,
             aux_data: Option<AuxDataProvider>,
             llist: &mut Vec<Listener<E, Event, AuxDataProvider>>| {
                for listener in llist.iter_mut() {
                    if let Err(e) = listener.send_provider.send(event, aux_data.clone()) {
                        err_status = Some(Err(e));
                    } else {
                        num_recipients += 1;
                    }
                }
            };
        if let Some((event, aux_data)) = self.event_receiver.receive() {
            let single_key = ListenerType::Single(event.raw_as_largest_type());
            if self.listeners.contains_key(&single_key) {
                send_handler(
                    event,
                    aux_data.clone(),
                    self.listeners.get_mut(&single_key).unwrap(),
                );
            }
            let group_key = ListenerType::Group(event.group_id_as_largest_type());
            if self.listeners.contains_key(&group_key) {
                send_handler(
                    event,
                    aux_data.clone(),
                    self.listeners.get_mut(&group_key).unwrap(),
                );
            }
            if let Some(all_receivers) = self.listeners.get_mut(&ListenerType::All) {
                send_handler(event, aux_data.clone(), all_receivers);
            }
            if let Some(err) = err_status {
                return err;
            }
            return Ok(HandlerResult::Handled(num_recipients, event, aux_data));
        }
        Ok(HandlerResult::Empty)
    }
}

#[cfg(feature = "std")]
pub mod stdmod {
    use super::*;
    use crate::event_man::{EventReceiver, EventWithAuxData};
    use crate::events::{EventU16, EventU32, GenericEvent};
    use crate::params::Params;
    use std::sync::mpsc::{Receiver, SendError, Sender};

    pub struct MpscEventReceiver<Event: GenericEvent + Send = EventU32> {
        mpsc_receiver: Receiver<(Event, Option<Params>)>,
    }

    impl<Event: GenericEvent + Send> MpscEventReceiver<Event> {
        pub fn new(receiver: Receiver<(Event, Option<Params>)>) -> Self {
            Self {
                mpsc_receiver: receiver,
            }
        }
    }
    impl<Event: GenericEvent + Send> EventReceiver<Event> for MpscEventReceiver<Event> {
        fn receive(&mut self) -> Option<EventWithAuxData<Event>> {
            if let Ok(event_and_data) = self.mpsc_receiver.try_recv() {
                return Some(event_and_data);
            }
            None
        }
    }

    pub type MpscEventU32Receiver = MpscEventReceiver<EventU32>;
    pub type MpscEventU16Receiver = MpscEventReceiver<EventU16>;

    #[derive(Clone)]
    pub struct MpscEventSendProvider<Event: GenericEvent + Send> {
        id: u32,
        sender: Sender<(Event, Option<Params>)>,
    }

    /// Safety: Send is safe to implement because both the ID and the MPSC sender are Send
    //unsafe impl<Event: GenericEvent> Send for MpscEventSendProvider<Event> {}

    impl<Event: GenericEvent + Send> MpscEventSendProvider<Event> {
        pub fn new(id: u32, sender: Sender<(Event, Option<Params>)>) -> Self {
            Self { id, sender }
        }
    }

    impl<Event: GenericEvent + Send> SendEventProvider<Event> for MpscEventSendProvider<Event> {
        type Error = SendError<(Event, Option<Params>)>;

        fn id(&self) -> u32 {
            self.id
        }
        fn send(&mut self, event: Event, aux_data: Option<Params>) -> Result<(), Self::Error> {
            self.sender.send((event, aux_data))
        }
    }

    pub type MpscEventU32SendProvider = MpscEventSendProvider<EventU32>;
    pub type MpscEventU16SendProvider = MpscEventSendProvider<EventU16>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_man::EventManager;
    use crate::events::{EventU32, GenericEvent, Severity};
    use crate::params::ParamsRaw;
    use alloc::boxed::Box;
    use std::format;
    use std::sync::mpsc::{channel, Receiver, SendError, Sender};

    #[derive(Clone)]
    struct MpscEventSenderQueue {
        id: u32,
        mpsc_sender: Sender<EventU32WithAuxData>,
    }

    impl MpscEventSenderQueue {
        fn new(id: u32, mpsc_sender: Sender<EventU32WithAuxData>) -> Self {
            Self { id, mpsc_sender }
        }
    }

    impl SendEventProvider<EventU32> for MpscEventSenderQueue {
        type Error = SendError<EventU32WithAuxData>;

        fn id(&self) -> u32 {
            self.id
        }
        fn send(&mut self, event: EventU32, aux_data: Option<Params>) -> Result<(), Self::Error> {
            self.mpsc_sender.send((event, aux_data))
        }
    }

    fn check_next_event(
        expected: EventU32,
        receiver: &Receiver<EventU32WithAuxData>,
    ) -> Option<Params> {
        if let Ok(event) = receiver.try_recv() {
            assert_eq!(event.0, expected);
            return event.1;
        }
        None
    }

    fn check_handled_event(
        res: HandlerResult<EventU32, Params>,
        expected: EventU32,
        expected_num_sent: u32,
    ) {
        assert!(matches!(res, HandlerResult::Handled { .. }));
        if let HandlerResult::Handled(num_recipients, event, _aux_data) = res {
            assert_eq!(event, expected);
            assert_eq!(num_recipients, expected_num_sent);
        }
    }

    fn generic_event_man() -> (
        Sender<EventU32WithAuxData>,
        EventManager<SendError<EventU32WithAuxData>>,
    ) {
        let (event_sender, manager_queue) = channel();
        let event_man_receiver = MpscEventReceiver::new(manager_queue);
        (
            event_sender,
            EventManager::new(Box::new(event_man_receiver)),
        )
    }

    #[test]
    fn test_basic() {
        let (event_sender, mut event_man) = generic_event_man();
        let event_grp_0 = EventU32::new(Severity::INFO, 0, 0).unwrap();
        let event_grp_1_0 = EventU32::new(Severity::HIGH, 1, 0).unwrap();
        let (single_event_sender, single_event_receiver) = channel();
        let single_event_listener = MpscEventSenderQueue::new(0, single_event_sender);
        event_man.subscribe_single(event_grp_0, single_event_listener);
        let (group_event_sender_0, group_event_receiver_0) = channel();
        let group_event_listener = MpscEventSenderQueue {
            id: 1,
            mpsc_sender: group_event_sender_0,
        };
        event_man.subscribe_group(event_grp_1_0.group_id(), group_event_listener);

        // Test event with one listener
        event_sender
            .send((event_grp_0, None))
            .expect("Sending single error failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_grp_0, 1);
        check_next_event(event_grp_0, &single_event_receiver);

        // Test event which is sent to all group listeners
        event_sender
            .send((event_grp_1_0, None))
            .expect("Sending group error failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_grp_1_0, 1);
        check_next_event(event_grp_1_0, &group_event_receiver_0);
    }

    #[test]
    fn test_with_basic_aux_data() {
        let (event_sender, mut event_man) = generic_event_man();
        let event_grp_0 = EventU32::new(Severity::INFO, 0, 0).unwrap();
        let (single_event_sender, single_event_receiver) = channel();
        let single_event_listener = MpscEventSenderQueue::new(0, single_event_sender);
        event_man.subscribe_single(event_grp_0, single_event_listener);
        event_sender
            .send((event_grp_0, Some(Params::Heapless((2_u32, 3_u32).into()))))
            .expect("Sending group error failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_grp_0, 1);
        let aux = check_next_event(event_grp_0, &single_event_receiver);
        assert!(aux.is_some());
        let aux = aux.unwrap();
        if let Params::Heapless(ParamsHeapless::Raw(ParamsRaw::U32Pair(pair))) = aux {
            assert_eq!(pair.0, 2);
            assert_eq!(pair.1, 3);
        } else {
            panic!("{}", format!("Unexpected auxiliary value type {:?}", aux));
        }
    }

    /// Test listening for multiple groups
    #[test]
    fn test_multi_group() {
        let (event_sender, mut event_man) = generic_event_man();
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        let hres = res.unwrap();
        assert!(matches!(hres, HandlerResult::Empty));

        let event_grp_0 = EventU32::new(Severity::INFO, 0, 0).unwrap();
        let event_grp_1_0 = EventU32::new(Severity::HIGH, 1, 0).unwrap();
        let (event_grp_0_sender, event_grp_0_receiver) = channel();
        let event_grp_0_and_1_listener = MpscEventSenderQueue {
            id: 0,
            mpsc_sender: event_grp_0_sender,
        };
        event_man.subscribe_group(event_grp_0.group_id(), event_grp_0_and_1_listener.clone());
        event_man.subscribe_group(event_grp_1_0.group_id(), event_grp_0_and_1_listener);

        event_sender
            .send((event_grp_0, None))
            .expect("Sending Event Group 0 failed");
        event_sender
            .send((event_grp_1_0, None))
            .expect("Sendign Event Group 1 failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_grp_0, 1);
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_grp_1_0, 1);

        check_next_event(event_grp_0, &event_grp_0_receiver);
        check_next_event(event_grp_1_0, &event_grp_0_receiver);
    }

    /// Test listening to the same event from multiple listeners. Also test listening
    /// to both group and single events from one listener
    #[test]
    fn test_listening_to_same_event_and_multi_type() {
        let (event_sender, mut event_man) = generic_event_man();
        let event_0 = EventU32::new(Severity::INFO, 0, 5).unwrap();
        let event_1 = EventU32::new(Severity::HIGH, 1, 0).unwrap();
        let (event_0_tx_0, event_0_rx_0) = channel();
        let (event_0_tx_1, event_0_rx_1) = channel();
        let event_listener_0 = MpscEventSenderQueue {
            id: 0,
            mpsc_sender: event_0_tx_0,
        };
        let event_listener_1 = MpscEventSenderQueue {
            id: 1,
            mpsc_sender: event_0_tx_1,
        };
        event_man.subscribe_single(event_0, event_listener_0.clone());
        event_man.subscribe_single(event_0, event_listener_1);
        event_sender
            .send((event_0, None))
            .expect("Triggering Event 0 failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_0, 2);
        check_next_event(event_0, &event_0_rx_0);
        check_next_event(event_0, &event_0_rx_1);
        event_man.subscribe_group(event_1.group_id(), event_listener_0.clone());
        event_sender
            .send((event_0, None))
            .expect("Triggering Event 0 failed");
        event_sender
            .send((event_1, None))
            .expect("Triggering Event 1 failed");

        // 3 Events messages will be sent now
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_0, 2);
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_1, 1);
        // Both the single event and the group event should arrive now
        check_next_event(event_0, &event_0_rx_0);
        check_next_event(event_1, &event_0_rx_0);

        // Double insertion should be detected, result should remain the same
        event_man.subscribe_group(event_1.group_id(), event_listener_0);
        event_sender
            .send((event_1, None))
            .expect("Triggering Event 1 failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_1, 1);
    }

    #[test]
    fn test_all_events_listener() {
        let (event_sender, manager_queue) = channel();
        let event_man_receiver = MpscEventReceiver::new(manager_queue);
        let mut event_man: EventManager<SendError<EventU32WithAuxData>> =
            EventManager::new(Box::new(event_man_receiver));
        let event_0 = EventU32::new(Severity::INFO, 0, 5).unwrap();
        let event_1 = EventU32::new(Severity::HIGH, 1, 0).unwrap();
        let (event_0_tx_0, all_events_rx) = channel();
        let all_events_listener = MpscEventSenderQueue {
            id: 0,
            mpsc_sender: event_0_tx_0,
        };
        event_man.subscribe_all(all_events_listener);
        event_sender
            .send((event_0, None))
            .expect("Triggering event 0 failed");
        event_sender
            .send((event_1, None))
            .expect("Triggering event 1 failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_0, 1);
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_1, 1);
        check_next_event(event_0, &all_events_rx);
        check_next_event(event_1, &all_events_rx);
    }
}
