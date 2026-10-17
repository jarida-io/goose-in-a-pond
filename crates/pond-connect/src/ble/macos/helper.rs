//! The `pond-server ble-helper` process: a `CBPeripheralManager` on the main dispatch queue,
//! speaking [`protocol`](crate::ble::helper::protocol) on stdin and stdout. Every callback and
//! command runs on the main thread, so the radio's state lives in a main-thread `thread_local`
//! and nothing CoreBluetooth hands out has to cross a thread.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Write};

use dispatch2::DispatchQueue;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly, Message};
use objc2_core_bluetooth::{
    CBATTError, CBATTRequest, CBAdvertisementDataLocalNameKey, CBAdvertisementDataServiceUUIDsKey,
    CBAttributePermissions, CBCentral, CBCharacteristic, CBCharacteristicProperties, CBManager,
    CBManagerAuthorization, CBManagerState, CBMutableCharacteristic, CBMutableService,
    CBPeripheralManager, CBPeripheralManagerDelegate, CBPeripheralManagerOptionShowPowerAlertKey,
    CBService, CBUUID,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSRunLoop,
    NSString,
};

use crate::ble::helper::protocol::{
    encode_line, Authorization, Bytes, Command, Event, RadioState, PROTOCOL_VERSION,
};
use crate::ble::{RX_CHARACTERISTIC_UUID_STR, SERVICE_UUID_STR, TX_CHARACTERISTIC_UUID_STR};
use crate::setup_channel::framing::{ATT_HEADER_LEN, DEFAULT_ATT_MTU, MAX_ATT_MTU};

/// `EX_SOFTWARE`: the helper was started somewhere CoreBluetooth's main queue cannot serve it.
const EX_SOFTWARE: i32 = 70;
/// CoreBluetooth answers within a second when it can; silence this long means it cannot (a
/// permission prompt nobody has answered, or no route to bluetoothd).
const STATE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

thread_local! {
    static HELPER: RefCell<Option<Helper>> = const { RefCell::new(None) };
}

/// Runs the helper until the pond closes its stdin. Never returns.
pub fn run() -> ! {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("ble-helper must run on the main thread");
        std::process::exit(EX_SOFTWARE);
    };
    emit(&Event::Ready {
        protocol: PROTOCOL_VERSION,
    });
    autoreleasepool(|_| {
        // SAFETY: a class property read; it asks nothing of the user.
        let authorization = authorization(unsafe { CBManager::authorization_class() });
        emit(&Event::Authorization { authorization });
        let helper = Helper::new(mtm);
        HELPER.with(|cell| *cell.borrow_mut() = Some(helper));
    });
    if let Ok(when) = dispatch2::DispatchTime::try_from(STATE_WAIT) {
        let _ = DispatchQueue::main().after(when, || with_helper(Helper::report_silence));
    }
    if let Err(error) = std::thread::Builder::new()
        .name("ble-helper-stdin".into())
        .spawn(read_commands)
    {
        eprintln!("ble-helper could not read its commands: {error}");
        std::process::exit(EX_SOFTWARE);
    }
    // Not `dispatch_main`: it `pthread_exit`s the main thread, whose thread-local destructors
    // then drop the manager. The main run loop serves the main queue and keeps the thread alive.
    let run_loop = NSRunLoop::mainRunLoop();
    loop {
        run_loop.run();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn read_commands() {
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Command>(&line) {
            Ok(command) => DispatchQueue::main()
                .exec_async(move || with_helper(|helper| helper.command(command))),
            Err(error) => {
                // Position only: the line may carry a phone's bytes.
                let message = format!("unreadable command at column {}", error.column());
                DispatchQueue::main().exec_async(move || {
                    emit(&Event::Error {
                        context: "protocol".into(),
                        message,
                    })
                })
            }
        }
    }
    // The pond is gone; stop advertising so no orphan keeps showing it.
    DispatchQueue::main().exec_async(|| {
        with_helper(Helper::stop_advertising_now);
        std::process::exit(0);
    });
}

/// One line on stdout. A pond that stopped reading is gone, so the helper goes too.
fn emit(event: &Event) {
    let Ok(line) = encode_line(event) else {
        return;
    };
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(&line)
        .and_then(|()| stdout.flush())
        .is_err()
    {
        std::process::exit(0);
    }
}

fn with_helper(f: impl FnOnce(&mut Helper)) {
    autoreleasepool(|_| {
        HELPER.with(|cell| match cell.try_borrow_mut() {
            Ok(mut helper) => {
                if let Some(helper) = helper.as_mut() {
                    f(helper);
                }
            }
            // CoreBluetooth calls its delegate asynchronously, so this would be a bug here.
            Err(_) => eprintln!("ble-helper re-entered its radio state; one event was dropped"),
        })
    });
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GIAPSetupChannelPeripheralDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl CBPeripheralManagerDelegate for Delegate {
        #[unsafe(method(peripheralManagerDidUpdateState:))]
        fn did_update_state(&self, manager: &CBPeripheralManager) {
            // SAFETY: a plain property read on the manager that called us.
            let state = unsafe { manager.state() };
            with_helper(|helper| helper.on_state(state));
        }

        #[unsafe(method(peripheralManager:didAddService:error:))]
        fn did_add_service(
            &self,
            _manager: &CBPeripheralManager,
            _service: &CBService,
            error: Option<&NSError>,
        ) {
            let error = error.map(describe);
            with_helper(|helper| helper.on_service_added(error));
        }

        #[unsafe(method(peripheralManagerDidStartAdvertising:error:))]
        fn did_start_advertising(&self, _manager: &CBPeripheralManager, error: Option<&NSError>) {
            let error = error.map(describe);
            with_helper(|helper| helper.on_advertising_started(error));
        }

        #[unsafe(method(peripheralManager:central:didSubscribeToCharacteristic:))]
        fn did_subscribe(
            &self,
            _manager: &CBPeripheralManager,
            central: &CBCentral,
            characteristic: &CBCharacteristic,
        ) {
            if is(characteristic, TX_CHARACTERISTIC_UUID_STR) {
                with_helper(|helper| helper.on_subscribed(central));
            }
        }

        #[unsafe(method(peripheralManager:central:didUnsubscribeFromCharacteristic:))]
        fn did_unsubscribe(
            &self,
            _manager: &CBPeripheralManager,
            central: &CBCentral,
            characteristic: &CBCharacteristic,
        ) {
            if is(characteristic, TX_CHARACTERISTIC_UUID_STR) {
                let id = central_id(central);
                with_helper(|helper| helper.on_unsubscribed(&id));
            }
        }

        #[unsafe(method(peripheralManager:didReceiveReadRequest:))]
        fn did_receive_read(&self, manager: &CBPeripheralManager, request: &CBATTRequest) {
            // Neither characteristic is readable: answers travel as notifications.
            // SAFETY: answering the request CoreBluetooth just handed us, once.
            unsafe { manager.respondToRequest_withResult(request, CBATTError::ReadNotPermitted) };
        }

        #[unsafe(method(peripheralManager:didReceiveWriteRequests:))]
        fn did_receive_writes(
            &self,
            manager: &CBPeripheralManager,
            requests: &NSArray<CBATTRequest>,
        ) {
            let requests = requests.to_vec();
            let Some(first) = requests.first() else {
                return;
            };
            let mut result = CBATTError::WriteNotPermitted;
            with_helper(|helper| result = helper.on_writes(&requests));
            // SAFETY: CoreBluetooth asks for exactly one answer, to the first request of the batch.
            unsafe { manager.respondToRequest_withResult(first, result) };
        }

        #[unsafe(method(peripheralManagerIsReadyToUpdateSubscribers:))]
        fn ready_to_update(&self, _manager: &CBPeripheralManager) {
            with_helper(Helper::drain);
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initialiser, on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

fn describe(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

fn uuid(text: &str) -> Retained<CBUUID> {
    // SAFETY: a well-formed 128-bit UUID string, which is all CBUUID requires.
    unsafe { CBUUID::UUIDWithString(&NSString::from_str(text)) }
}

fn is(characteristic: &CBCharacteristic, wanted: &str) -> bool {
    // SAFETY: plain property reads.
    let uuid = unsafe { characteristic.UUID().UUIDString() };
    uuid.to_string().eq_ignore_ascii_case(wanted)
}

fn central_id(central: &CBCentral) -> String {
    // SAFETY: a plain property read.
    unsafe { central.identifier() }.UUIDString().to_string()
}

/// `maximumUpdateValueLength` is the notification payload, i.e. ATT_MTU less its 3-byte header.
fn att_mtu(central: &CBCentral) -> u16 {
    // SAFETY: a plain property read.
    let payload = unsafe { central.maximumUpdateValueLength() };
    let payload = u16::try_from(payload).unwrap_or(u16::MAX);
    payload
        .saturating_add(ATT_HEADER_LEN)
        .clamp(DEFAULT_ATT_MTU, MAX_ATT_MTU)
}

fn authorization(authorization: CBManagerAuthorization) -> Authorization {
    match authorization {
        CBManagerAuthorization::AllowedAlways => Authorization::AllowedAlways,
        CBManagerAuthorization::Denied => Authorization::Denied,
        CBManagerAuthorization::Restricted => Authorization::Restricted,
        _ => Authorization::NotDetermined,
    }
}

fn radio_state(state: CBManagerState) -> RadioState {
    match state {
        CBManagerState::PoweredOn => RadioState::PoweredOn,
        CBManagerState::PoweredOff => RadioState::PoweredOff,
        CBManagerState::Unauthorized => RadioState::Unauthorized,
        CBManagerState::Unsupported => RadioState::Unsupported,
        CBManagerState::Resetting => RadioState::Resetting,
        _ => RadioState::Unknown,
    }
}

struct Link {
    central: Retained<CBCentral>,
    att_mtu: u16,
    /// The pond ended this link's session: its writes are refused until it unsubscribes.
    refused: bool,
}

struct Helper {
    manager: Retained<CBPeripheralManager>,
    _delegate: Retained<Delegate>,
    tx: Option<Retained<CBMutableCharacteristic>>,
    /// CoreBluetooth has reported a state at least once.
    answered: bool,
    powered_on: bool,
    service_added: bool,
    /// What the pond last asked to advertise.
    wanted: Option<String>,
    /// What `startAdvertising` was last called with.
    advertised: Option<String>,
    links: HashMap<u64, Link>,
    by_central: HashMap<String, u64>,
    next_link: u64,
    /// Notifications waiting for CoreBluetooth to have room, in order.
    queue: VecDeque<(u64, Retained<NSData>)>,
}

impl Helper {
    fn new(mtm: MainThreadMarker) -> Self {
        let delegate = Delegate::new(mtm);
        let no = NSNumber::new_bool(false);
        // A headless pond has no one to read "turn Bluetooth on"; the dashboard says it instead.
        // SAFETY: a framework constant, valid for the process's life.
        let key: &NSString = unsafe { CBPeripheralManagerOptionShowPowerAlertKey };
        let value: &AnyObject = &no;
        let options = NSDictionary::from_slices(&[key], &[value]);
        // SAFETY: the delegate outlives the manager (both live in `Helper`), and the main queue is
        // the one `dispatch_main` serves.
        let manager = unsafe {
            CBPeripheralManager::initWithDelegate_queue_options(
                CBPeripheralManager::alloc(),
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(DispatchQueue::main()),
                Some(&options),
            )
        };
        Self {
            manager,
            _delegate: delegate,
            tx: None,
            answered: false,
            powered_on: false,
            service_added: false,
            wanted: None,
            advertised: None,
            links: HashMap::new(),
            by_central: HashMap::new(),
            next_link: 1,
            queue: VecDeque::new(),
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Advertise { local_name } => {
                self.wanted = Some(local_name);
                self.apply_advertising();
            }
            Command::StopAdvertising => {
                self.wanted = None;
                self.apply_advertising();
            }
            Command::Notify { link, data } => {
                if self.links.get(&link).is_some_and(|l| !l.refused) {
                    self.queue.push_back((link, NSData::from_vec(data.0)));
                    self.drain();
                }
            }
            Command::Disconnect { link } => {
                if let Some(open) = self.links.get_mut(&link) {
                    open.refused = true;
                }
                self.queue.retain(|(queued, _)| *queued != link);
            }
        }
    }

    fn report_silence(&mut self) {
        if !self.answered {
            emit(&Event::Error {
                context: "radio".into(),
                message: format!(
                    "CoreBluetooth has not reported the radio's state after {} s",
                    STATE_WAIT.as_secs()
                ),
            });
        }
    }

    fn on_state(&mut self, state: CBManagerState) {
        self.answered = true;
        let state = radio_state(state);
        emit(&Event::State { state });
        let powered_on = state == RadioState::PoweredOn;
        if !powered_on {
            // The radio took the service, the advertisement and every link with it.
            self.service_added = false;
            self.advertised = None;
            self.tx = None;
            self.queue.clear();
            self.by_central.clear();
            let mut links: Vec<u64> = self.links.drain().map(|(link, _)| link).collect();
            links.sort_unstable();
            for link in links {
                emit(&Event::Unsubscribed { link });
            }
        } else if !self.powered_on {
            self.add_service();
        }
        self.powered_on = powered_on;
    }

    fn add_service(&mut self) {
        let rx = {
            // SAFETY: a freshly allocated characteristic with a valid UUID and no cached value.
            unsafe {
                CBMutableCharacteristic::initWithType_properties_value_permissions(
                    CBMutableCharacteristic::alloc(),
                    &uuid(RX_CHARACTERISTIC_UUID_STR),
                    CBCharacteristicProperties::Write,
                    None,
                    CBAttributePermissions::Writeable,
                )
            }
        };
        let tx = {
            // SAFETY: as above; a notify characteristic with no value is never cached.
            unsafe {
                CBMutableCharacteristic::initWithType_properties_value_permissions(
                    CBMutableCharacteristic::alloc(),
                    &uuid(TX_CHARACTERISTIC_UUID_STR),
                    CBCharacteristicProperties::Notify,
                    None,
                    CBAttributePermissions::Readable,
                )
            }
        };
        // SAFETY: a freshly allocated primary service with a valid UUID.
        let service = unsafe {
            CBMutableService::initWithType_primary(
                CBMutableService::alloc(),
                &uuid(SERVICE_UUID_STR),
                true,
            )
        };
        let characteristics: [&CBCharacteristic; 2] = [&rx, &tx];
        // SAFETY: the service is not added yet, so its characteristics may still change.
        unsafe {
            service.setCharacteristics(Some(&NSArray::from_slice(&characteristics)));
            self.manager.removeAllServices();
            self.manager.addService(&service);
        }
        self.tx = Some(tx);
    }

    fn on_service_added(&mut self, error: Option<String>) {
        match error {
            Some(message) => emit(&Event::Error {
                context: "add_service".into(),
                message,
            }),
            None => {
                self.service_added = true;
                emit(&Event::ServiceAdded);
                self.apply_advertising();
            }
        }
    }

    fn apply_advertising(&mut self) {
        if !self.powered_on || !self.service_added || self.wanted == self.advertised {
            return;
        }
        // SAFETY: plain calls on our own manager, on its queue.
        unsafe {
            if self.manager.isAdvertising() {
                self.manager.stopAdvertising();
            }
        }
        let Some(local_name) = self.wanted.clone() else {
            self.advertised = None;
            emit(&Event::AdvertisingStopped);
            return;
        };
        let uuids = NSArray::from_retained_slice(&[uuid(SERVICE_UUID_STR)]);
        let name = NSString::from_str(&local_name);
        // SAFETY: framework constants, valid for the process's life.
        let keys: [&NSString; 2] = unsafe {
            [
                CBAdvertisementDataServiceUUIDsKey,
                CBAdvertisementDataLocalNameKey,
            ]
        };
        let values: [&AnyObject; 2] = [&uuids, &name];
        // The service UUID and the name, and nothing else: CoreBluetooth would allow no more.
        let advertisement = NSDictionary::from_slices(&keys, &values);
        // SAFETY: a dictionary of the two keys CoreBluetooth documents for a peripheral.
        unsafe { self.manager.startAdvertising(Some(&advertisement)) };
        self.advertised = Some(local_name);
    }

    fn on_advertising_started(&mut self, error: Option<String>) {
        match (error, self.advertised.clone()) {
            (None, Some(local_name)) => emit(&Event::Advertising { local_name }),
            (None, None) => {}
            (Some(message), _) => {
                self.advertised = None;
                emit(&Event::Error {
                    context: "advertise".into(),
                    message,
                });
            }
        }
    }

    fn stop_advertising_now(&mut self) {
        // SAFETY: a plain call on our own manager.
        unsafe { self.manager.stopAdvertising() };
        self.advertised = None;
    }

    fn on_subscribed(&mut self, central: &CBCentral) {
        let id = central_id(central);
        let att_mtu = att_mtu(central);
        if let Some(link) = self.by_central.get(&id).copied() {
            if let Some(open) = self.links.get_mut(&link) {
                if open.att_mtu != att_mtu {
                    open.att_mtu = att_mtu;
                    emit(&Event::Mtu { link, att_mtu });
                }
            }
            return;
        }
        let link = self.next_link;
        self.next_link += 1;
        self.by_central.insert(id.clone(), link);
        self.links.insert(
            link,
            Link {
                central: central.retain(),
                att_mtu,
                refused: false,
            },
        );
        emit(&Event::Subscribed {
            link,
            central: id,
            att_mtu,
        });
    }

    fn on_unsubscribed(&mut self, id: &str) {
        let Some(link) = self.by_central.remove(id) else {
            return;
        };
        self.links.remove(&link);
        self.queue.retain(|(queued, _)| *queued != link);
        emit(&Event::Unsubscribed { link });
    }

    /// Forwards one batch of rx writes and says what to answer. Each write is on stdout before
    /// the phone hears success, so a pond that falls behind slows the phone down.
    fn on_writes(&mut self, requests: &[Retained<CBATTRequest>]) -> CBATTError {
        let mut writes = Vec::with_capacity(requests.len());
        let mut offset = 0;
        for request in requests {
            // SAFETY: plain property reads on a request CoreBluetooth handed us.
            let (central, characteristic, at, value) = unsafe {
                (
                    request.central(),
                    request.characteristic(),
                    request.offset(),
                    request.value(),
                )
            };
            if !is(&characteristic, RX_CHARACTERISTIC_UUID_STR) {
                return CBATTError::WriteNotPermitted;
            }
            let id = central_id(&central);
            let Some(link) = self.by_central.get(&id).copied() else {
                // A phone writes only after it subscribed to tx.
                return CBATTError::WriteNotPermitted;
            };
            let Some(open) = self.links.get_mut(&link) else {
                return CBATTError::WriteNotPermitted;
            };
            if open.refused {
                return CBATTError::WriteNotPermitted;
            }
            if at != offset {
                return CBATTError::InvalidOffset;
            }
            let bytes = value.map(|data| data.to_vec()).unwrap_or_default();
            offset += bytes.len();
            let now = att_mtu(&central);
            if now != open.att_mtu {
                open.att_mtu = now;
                emit(&Event::Mtu { link, att_mtu: now });
            }
            writes.push((link, bytes));
        }
        for (link, data) in writes {
            emit(&Event::Write {
                link,
                data: Bytes(data),
            });
        }
        CBATTError::Success
    }

    /// Sends queued notifications until CoreBluetooth's queue is full; it says when it has room.
    fn drain(&mut self) {
        let Some(tx) = self.tx.clone() else {
            return;
        };
        while let Some((link, data)) = self.queue.front() {
            let Some(open) = self.links.get(link).filter(|open| !open.refused) else {
                self.queue.pop_front();
                continue;
            };
            let centrals = NSArray::from_slice(&[&*open.central]);
            // SAFETY: our own characteristic, a subscribed central, a payload within its MTU.
            let sent = unsafe {
                self.manager
                    .updateValue_forCharacteristic_onSubscribedCentrals(data, &tx, Some(&centrals))
            };
            if !sent {
                return;
            }
            self.queue.pop_front();
        }
    }
}
