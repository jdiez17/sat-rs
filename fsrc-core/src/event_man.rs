//! Event management and forwarding
//!
//! This module provides components to perform event routing. The most important component for this
//! task is the [EventManager]. It receives all events and then routes them to event subscribers
//! where appropriate.
#![cfg_attr(feature = "doc-images",
cfg_attr(all(),
doc = ::embed_doc_image::embed_image!("event_man_arch", "images/event_man_arch.png"
)))]
#![cfg_attr(
    not(feature = "doc-images"),
    doc = "**Doc images not enabled**. Compile with feature `doc-images` and Rust version >= 1.54 \
           to enable."
)]
//! One common use case for satellite systems is to offer a light-weight publish-subscribe mechanism
//! and IPC mechanism for software and hardware events which are also packaged as telemetry (TM) or
//! can trigger a system response.
//!
//! The following graph shows how the event flow for such a setup could look like:
//!
//! ![Event flow][event_man_arch]
//!
//! The event manager has a listener table abstracted by the [ListenerTable], which maps
//! listener groups identified by [ListenerKey]s to a [sender ID][SenderId].
//! It also contains a sender table abstracted by the [SenderTable] which maps these sender IDs
//! to a concrete [SendEventProvider]s. A simple approach would be to use one send event provider
//! for each OBSW thread and then subscribe for all interesting events for a particular thread
//! using the send event provider ID.
//!
//! This can be done with the [EventManager] like this:
//!
//!  1. Provide a concrete [EventReceiver] implementation. This abstraction allow to use different
//!     message queue backends. A straightforward implementation where dynamic memory allocation is
//!     not a big concern could use [std::sync::mpsc::channel] to do this and is provided in
//!     form of the [MpscEventReceiver].
//!  2. To set up event creators, create channel pairs using some message queue implementation.
//!     Each event creator gets a (cloned) sender component which allows it to send events to the
//!     manager.
//!  3. The event manager receives the receiver component as part of a [EventReceiver]
//!     implementation so all events are routed to the manager.
//!  4. Create the [send event providers][SendEventProvider]s which allow routing events to
//!     subscribers. You can now use their [sender IDs][SendEventProvider::id] to subscribe for
//!     event groups, for example by using the [EventManager::subscribe_single] method.
//!  5. Add the send provider as well using the [EventManager::add_sender] call so the event
//!     manager can route listener groups to a the send provider.
//!
//! Some components like a PUS Event Service or PUS Event Action Service might require all
//! events to package them as telemetry or start actions where applicable.
//! Other components might only be interested in certain events. For example, a thermal system
//! handler might only be interested in temperature events generated by a thermal sensor component.
//!
//! # Examples
//!
//! You can check [integration test](https://egit.irs.uni-stuttgart.de/rust/fsrc-launchpad/src/branch/main/fsrc-core/tests/pus_events.rs)
//! for a concrete example using multi-threading where events are routed to
//! different threads.
use crate::events::{EventU16, EventU32, GenericEvent, LargestEventRaw, LargestGroupIdRaw};
use crate::params::{Params, ParamsHeapless};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::slice::Iter;
use hashbrown::HashMap;

#[cfg(feature = "std")]
pub use stdmod::*;

#[derive(PartialEq, Eq, Hash, Copy, Clone, Debug)]
pub enum ListenerKey {
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

pub type SenderId = u32;

pub trait SendEventProvider<Provider: GenericEvent, AuxDataProvider = Params> {
    type Error;

    fn id(&self) -> SenderId;
    fn send_no_data(&mut self, event: Provider) -> Result<(), Self::Error> {
        self.send(event, None)
    }
    fn send(
        &mut self,
        event: Provider,
        aux_data: Option<AuxDataProvider>,
    ) -> Result<(), Self::Error>;
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

pub trait ListenerTable {
    fn get_listeners(&self) -> Vec<ListenerKey>;
    fn contains_listener(&self, key: &ListenerKey) -> bool;
    fn get_listener_ids(&self, key: &ListenerKey) -> Option<Iter<SenderId>>;
    fn add_listener(&mut self, key: ListenerKey, sender_id: SenderId) -> bool;
    fn remove_duplicates(&mut self, key: &ListenerKey);
}

pub trait SenderTable<SendProviderError, Event: GenericEvent = EventU32, AuxDataProvider = Params> {
    fn contains_send_event_provider(&self, id: &SenderId) -> bool;
    fn get_send_event_provider(
        &mut self,
        id: &SenderId,
    ) -> Option<&mut Box<dyn SendEventProvider<Event, AuxDataProvider, Error = SendProviderError>>>;
    fn add_send_event_provider(
        &mut self,
        send_provider: Box<
            dyn SendEventProvider<Event, AuxDataProvider, Error = SendProviderError>,
        >,
    ) -> bool;
}

/// Generic event manager implementation.
///
/// # Generics
///
///  * `SendProviderError`: [SendEventProvider] error type
///  * `Event`: Concrete event provider, currently either [EventU32] or [EventU16]
///  * `AuxDataProvider`: Concrete auxiliary data provider, currently either [Params] or
///     [ParamsHeapless]
pub struct EventManager<SendProviderError, Event: GenericEvent = EventU32, AuxDataProvider = Params>
{
    listener_table: Box<dyn ListenerTable>,
    sender_table: Box<dyn SenderTable<SendProviderError, Event, AuxDataProvider>>,
    event_receiver: Box<dyn EventReceiver<Event, AuxDataProvider>>,
}

/// Safety: It is safe to implement [Send] because all fields in the [EventManager] are [Send]
/// as well
#[cfg(feature = "std")]
unsafe impl<E, Event: GenericEvent + Send, AuxDataProvider: Send> Send
    for EventManager<E, Event, AuxDataProvider>
{
}

#[cfg(feature = "std")]
pub type EventManagerWithMpscQueue<Event, AuxDataProvider> = EventManager<
    std::sync::mpsc::SendError<(Event, Option<AuxDataProvider>)>,
    Event,
    AuxDataProvider,
>;

#[derive(Debug)]
pub enum EventRoutingResult<Event: GenericEvent, AuxDataProvider> {
    /// No event was received
    Empty,
    /// An event was received and routed.
    /// The first tuple entry will contain the number of recipients.
    Handled(u32, Event, Option<AuxDataProvider>),
}

#[derive(Debug)]
pub enum EventRoutingError<E> {
    SendError(E),
    NoSendersForKey(ListenerKey),
    NoSenderForId(SenderId),
}

#[derive(Debug)]
pub struct EventRoutingErrorsWithResult<Event: GenericEvent, AuxDataProvider, E> {
    pub result: EventRoutingResult<Event, AuxDataProvider>,
    pub errors: [Option<EventRoutingError<E>>; 3],
}

impl<E, Event: GenericEvent + Copy> EventManager<E, Event> {
    pub fn remove_duplicates(&mut self, key: &ListenerKey) {
        self.listener_table.remove_duplicates(key)
    }

    /// Subscribe for a unique event.
    pub fn subscribe_single(&mut self, event: &Event, sender_id: SenderId) {
        self.update_listeners(ListenerKey::Single(event.raw_as_largest_type()), sender_id);
    }

    /// Subscribe for an event group.
    pub fn subscribe_group(&mut self, group_id: LargestGroupIdRaw, sender_id: SenderId) {
        self.update_listeners(ListenerKey::Group(group_id), sender_id);
    }

    /// Subscribe for all events received by the manager.
    ///
    /// For example, this can be useful for a handler component which sends every event as
    /// a telemetry packet.
    pub fn subscribe_all(&mut self, sender_id: SenderId) {
        self.update_listeners(ListenerKey::All, sender_id);
    }
}

impl<E: 'static, Event: GenericEvent + Copy + 'static, AuxDataProvider: Clone + 'static>
    EventManager<E, Event, AuxDataProvider>
{
    /// Create an event manager where the sender table will be the [DefaultSenderTableProvider]
    /// and the listener table will be the [DefaultListenerTableProvider].
    pub fn new(event_receiver: Box<dyn EventReceiver<Event, AuxDataProvider>>) -> Self {
        let listener_table = Box::new(DefaultListenerTableProvider::default());
        let sender_table =
            Box::new(DefaultSenderTableProvider::<E, Event, AuxDataProvider>::default());
        Self::new_custom_tables(listener_table, sender_table, event_receiver)
    }
}

impl<E, Event: GenericEvent + Copy, AuxDataProvider: Clone>
    EventManager<E, Event, AuxDataProvider>
{
    pub fn new_custom_tables(
        listener_table: Box<dyn ListenerTable>,
        sender_table: Box<dyn SenderTable<E, Event, AuxDataProvider>>,
        event_receiver: Box<dyn EventReceiver<Event, AuxDataProvider>>,
    ) -> Self {
        EventManager {
            listener_table,
            sender_table,
            event_receiver,
        }
    }

    pub fn add_sender(
        &mut self,
        send_provider: impl SendEventProvider<Event, AuxDataProvider, Error = E> + 'static,
    ) {
        if !self
            .sender_table
            .contains_send_event_provider(&send_provider.id())
        {
            self.sender_table
                .add_send_event_provider(Box::new(send_provider));
        }
    }

    fn update_listeners(&mut self, key: ListenerKey, sender_id: SenderId) {
        self.listener_table.add_listener(key, sender_id);
    }

    /// This function will use the cached event receiver and try to receive one event.
    /// If an event was received, it will try to route that event to all subscribed event listeners.
    /// If this works without any issues, the [EventRoutingResult] will contain context information
    /// about the routed event.
    ///
    /// This function will track up to 3 errors returned as part of the
    /// [EventRoutingErrorsWithResult] error struct.
    pub fn try_event_handling(
        &mut self,
    ) -> Result<
        EventRoutingResult<Event, AuxDataProvider>,
        EventRoutingErrorsWithResult<Event, AuxDataProvider, E>,
    > {
        let mut err_idx = 0;
        let mut err_slice = [None, None, None];
        let mut num_recipients = 0;
        let mut add_error = |error: EventRoutingError<E>| {
            if err_idx < 3 {
                err_slice[err_idx] = Some(error);
                err_idx += 1;
            }
        };
        let mut send_handler =
            |key: &ListenerKey, event: Event, aux_data: &Option<AuxDataProvider>| {
                if self.listener_table.contains_listener(key) {
                    if let Some(ids) = self.listener_table.get_listener_ids(key) {
                        for id in ids {
                            if let Some(sender) = self.sender_table.get_send_event_provider(id) {
                                if let Err(e) = sender.send(event, aux_data.clone()) {
                                    add_error(EventRoutingError::SendError(e));
                                } else {
                                    num_recipients += 1;
                                }
                            } else {
                                add_error(EventRoutingError::NoSenderForId(*id));
                            }
                        }
                    } else {
                        add_error(EventRoutingError::NoSendersForKey(*key));
                    }
                }
            };
        if let Some((event, aux_data)) = self.event_receiver.receive() {
            let single_key = ListenerKey::Single(event.raw_as_largest_type());
            send_handler(&single_key, event, &aux_data);
            let group_key = ListenerKey::Group(event.group_id_as_largest_type());
            send_handler(&group_key, event, &aux_data);
            send_handler(&ListenerKey::All, event, &aux_data);
            if err_idx > 0 {
                return Err(EventRoutingErrorsWithResult {
                    result: EventRoutingResult::Handled(num_recipients, event, aux_data),
                    errors: err_slice,
                });
            }
            return Ok(EventRoutingResult::Handled(num_recipients, event, aux_data));
        }
        Ok(EventRoutingResult::Empty)
    }
}

#[derive(Default)]
pub struct DefaultListenerTableProvider {
    listeners: HashMap<ListenerKey, Vec<SenderId>>,
}

pub struct DefaultSenderTableProvider<
    SendProviderError,
    Event: GenericEvent = EventU32,
    AuxDataProvider = Params,
> {
    senders: HashMap<
        SenderId,
        Box<dyn SendEventProvider<Event, AuxDataProvider, Error = SendProviderError>>,
    >,
}

impl<SendProviderError, Event: GenericEvent, AuxDataProvider> Default
    for DefaultSenderTableProvider<SendProviderError, Event, AuxDataProvider>
{
    fn default() -> Self {
        Self {
            senders: HashMap::new(),
        }
    }
}

impl ListenerTable for DefaultListenerTableProvider {
    fn get_listeners(&self) -> Vec<ListenerKey> {
        let mut key_list = Vec::new();
        for key in self.listeners.keys() {
            key_list.push(*key);
        }
        key_list
    }

    fn contains_listener(&self, key: &ListenerKey) -> bool {
        self.listeners.contains_key(key)
    }

    fn get_listener_ids(&self, key: &ListenerKey) -> Option<Iter<SenderId>> {
        self.listeners.get(key).map(|vec| vec.iter())
    }

    fn add_listener(&mut self, key: ListenerKey, sender_id: SenderId) -> bool {
        if let Some(existing_list) = self.listeners.get_mut(&key) {
            existing_list.push(sender_id);
        } else {
            let new_list = vec![sender_id];
            self.listeners.insert(key, new_list);
        }
        true
    }

    fn remove_duplicates(&mut self, key: &ListenerKey) {
        if let Some(list) = self.listeners.get_mut(key) {
            list.sort_unstable();
            list.dedup();
        }
    }
}

impl<SendProviderError, Event: GenericEvent, AuxDataProvider>
    SenderTable<SendProviderError, Event, AuxDataProvider>
    for DefaultSenderTableProvider<SendProviderError, Event, AuxDataProvider>
{
    fn contains_send_event_provider(&self, id: &SenderId) -> bool {
        self.senders.contains_key(id)
    }

    fn get_send_event_provider(
        &mut self,
        id: &SenderId,
    ) -> Option<&mut Box<dyn SendEventProvider<Event, AuxDataProvider, Error = SendProviderError>>>
    {
        self.senders.get_mut(id).filter(|sender| sender.id() == *id)
    }

    fn add_send_event_provider(
        &mut self,
        send_provider: Box<
            dyn SendEventProvider<Event, AuxDataProvider, Error = SendProviderError>,
        >,
    ) -> bool {
        let id = send_provider.id();
        if self.senders.contains_key(&id) {
            return false;
        }
        self.senders.insert(id, send_provider).is_none()
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
        res: EventRoutingResult<EventU32, Params>,
        expected: EventU32,
        expected_num_sent: u32,
    ) {
        assert!(matches!(res, EventRoutingResult::Handled { .. }));
        if let EventRoutingResult::Handled(num_recipients, event, _aux_data) = res {
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
        event_man.subscribe_single(&event_grp_0, single_event_listener.id());
        event_man.add_sender(single_event_listener);
        let (group_event_sender_0, group_event_receiver_0) = channel();
        let group_event_listener = MpscEventSenderQueue {
            id: 1,
            mpsc_sender: group_event_sender_0,
        };
        event_man.subscribe_group(event_grp_1_0.group_id(), group_event_listener.id());
        event_man.add_sender(group_event_listener);

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
        event_man.subscribe_single(&event_grp_0, single_event_listener.id());
        event_man.add_sender(single_event_listener);
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
        assert!(matches!(hres, EventRoutingResult::Empty));

        let event_grp_0 = EventU32::new(Severity::INFO, 0, 0).unwrap();
        let event_grp_1_0 = EventU32::new(Severity::HIGH, 1, 0).unwrap();
        let (event_grp_0_sender, event_grp_0_receiver) = channel();
        let event_grp_0_and_1_listener = MpscEventSenderQueue {
            id: 0,
            mpsc_sender: event_grp_0_sender,
        };
        event_man.subscribe_group(event_grp_0.group_id(), event_grp_0_and_1_listener.id());
        event_man.subscribe_group(event_grp_1_0.group_id(), event_grp_0_and_1_listener.id());
        event_man.add_sender(event_grp_0_and_1_listener);

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
        let event_listener_0_sender_id = event_listener_0.id();
        event_man.subscribe_single(&event_0, event_listener_0_sender_id);
        event_man.add_sender(event_listener_0);
        let event_listener_1_sender_id = event_listener_1.id();
        event_man.subscribe_single(&event_0, event_listener_1_sender_id);
        event_man.add_sender(event_listener_1);
        event_sender
            .send((event_0, None))
            .expect("Triggering Event 0 failed");
        let res = event_man.try_event_handling();
        assert!(res.is_ok());
        check_handled_event(res.unwrap(), event_0, 2);
        check_next_event(event_0, &event_0_rx_0);
        check_next_event(event_0, &event_0_rx_1);
        event_man.subscribe_group(event_1.group_id(), event_listener_0_sender_id);
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

        // Do double insertion and then remove duplicates
        event_man.subscribe_group(event_1.group_id(), event_listener_0_sender_id);
        event_man.remove_duplicates(&ListenerKey::Group(event_1.group_id()));
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
        event_man.subscribe_all(all_events_listener.id());
        event_man.add_sender(all_events_listener);
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
