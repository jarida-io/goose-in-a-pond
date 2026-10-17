//! The pond's `_pond._tcp` instance, registered with macOS's own mDNSResponder through the dns_sd
//! API in libSystem. mDNSResponder already owns UDP 5353 and the host's name: an in-process
//! responder beside it renamed the Mac on every start and went deaf to queries (measured
//! 2026-10-17, `docs/architecture/pond-networking.md`).
//!
//! Every `DNSServiceRef` lives on one registrar thread, the only thread that calls dns_sd, so the
//! raw pointers never cross threads. Closing its connection deregisters the service, so even a
//! killed pond sends its goodbyes: mDNSResponder sees the socket close.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use pond_core::network::domain::pond_advert::{AdvertiseError, PondAdvert, POND_SERVICE_TYPE};
use pond_core::network::ports::service_advertiser::ServiceAdvertiser;
use tokio::sync::oneshot;

pub const RESPONDER: &str = "mDNSResponder";

/// mDNSResponder replies once it has probed the name, about a second after the request.
const REGISTER_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the registrar reads what mDNSResponder sent unasked (a rename, or its own restart)
/// and retries a registration it could not make.
const RESPONDER_CHECK_INTERVAL: Duration = Duration::from_secs(5);

mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct DnsServiceOpaque {
        _private: [u8; 0],
    }
    pub type DnsServiceRef = *mut DnsServiceOpaque;
    pub type DnsRecordRef = *mut c_void;
    pub type DnsServiceFlags = u32;
    pub type DnsServiceErrorType = i32;
    pub type DnsServiceRegisterReply = Option<
        unsafe extern "C" fn(
            sd_ref: DnsServiceRef,
            flags: DnsServiceFlags,
            error: DnsServiceErrorType,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            context: *mut c_void,
        ),
    >;

    pub const NO_ERROR: DnsServiceErrorType = 0;
    pub const FLAGS_ADD: DnsServiceFlags = 0x2;
    pub const ERR_BAD_PARAM: DnsServiceErrorType = -65540;
    pub const ERR_BAD_FLAGS: DnsServiceErrorType = -65543;
    pub const ERR_UNSUPPORTED: DnsServiceErrorType = -65544;
    pub const ERR_NAME_CONFLICT: DnsServiceErrorType = -65548;
    pub const ERR_INVALID: DnsServiceErrorType = -65549;
    pub const ERR_INCOMPATIBLE: DnsServiceErrorType = -65551;
    pub const ERR_SERVICE_NOT_RUNNING: DnsServiceErrorType = -65563;
    pub const ERR_DEFUNCT_CONNECTION: DnsServiceErrorType = -65569;
    pub const ERR_POLICY_DENIED: DnsServiceErrorType = -65570;
    pub const ERR_NOT_PERMITTED: DnsServiceErrorType = -65571;

    // libSystem re-exports libsystem_dnssd, which every macOS binary already links.
    extern "C" {
        #[allow(clippy::too_many_arguments)]
        pub fn DNSServiceRegister(
            sd_ref: *mut DnsServiceRef,
            flags: DnsServiceFlags,
            interface_index: u32,
            name: *const c_char,
            regtype: *const c_char,
            domain: *const c_char,
            host: *const c_char,
            port_network_order: u16,
            txt_len: u16,
            txt_record: *const c_void,
            callback: DnsServiceRegisterReply,
            context: *mut c_void,
        ) -> DnsServiceErrorType;
        pub fn DNSServiceUpdateRecord(
            sd_ref: DnsServiceRef,
            record_ref: DnsRecordRef,
            flags: DnsServiceFlags,
            rdlen: u16,
            rdata: *const c_void,
            ttl: u32,
        ) -> DnsServiceErrorType;
        pub fn DNSServiceRefSockFD(sd_ref: DnsServiceRef) -> c_int;
        pub fn DNSServiceProcessResult(sd_ref: DnsServiceRef) -> DnsServiceErrorType;
        pub fn DNSServiceRefDeallocate(sd_ref: DnsServiceRef);
    }
}

/// `Refused` for a request dns_sd would refuse again as written; anything else may pass later.
fn error_for(code: ffi::DnsServiceErrorType, during: &str) -> AdvertiseError {
    match code {
        ffi::ERR_SERVICE_NOT_RUNNING | ffi::ERR_DEFUNCT_CONNECTION => {
            AdvertiseError::Unavailable(format!("{during}: mDNSResponder is not running ({code})"))
        }
        ffi::ERR_POLICY_DENIED | ffi::ERR_NOT_PERMITTED => AdvertiseError::Unavailable(format!(
            "{during}: macOS denied this process local network access ({code})"
        )),
        ffi::ERR_BAD_PARAM
        | ffi::ERR_BAD_FLAGS
        | ffi::ERR_UNSUPPORTED
        | ffi::ERR_NAME_CONFLICT
        | ffi::ERR_INVALID
        | ffi::ERR_INCOMPATIBLE => {
            AdvertiseError::Refused(format!("{during}: dns_sd error {code}"))
        }
        _ => AdvertiseError::Unavailable(format!("{during}: dns_sd error {code}")),
    }
}

/// What mDNSResponder last said about a registration, written by [`on_register_reply`].
#[derive(Debug, Default)]
struct Reply {
    code: Option<ffi::DnsServiceErrorType>,
    added: bool,
    name: Option<String>,
}

unsafe extern "C" fn on_register_reply(
    _sd_ref: ffi::DnsServiceRef,
    flags: ffi::DnsServiceFlags,
    error: ffi::DnsServiceErrorType,
    name: *const c_char,
    _regtype: *const c_char,
    _domain: *const c_char,
    context: *mut c_void,
) {
    // SAFETY: `context` is the `Reply` boxed in the `Registration` that owns this ref, and dns_sd
    // calls back only inside `DNSServiceProcessResult`, on the registrar thread, while it lives.
    let reply = unsafe { &mut *context.cast::<Reply>() };
    reply.code = Some(error);
    reply.added = flags & ffi::FLAGS_ADD != 0;
    if !name.is_null() {
        // SAFETY: dns_sd passes a NUL-terminated string valid for the duration of the call.
        reply.name = Some(
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned(),
        );
    }
}

/// One live registration. Dropping it deallocates the ref, which sends the goodbyes.
struct Registration {
    sd_ref: ffi::DnsServiceRef,
    // A raw allocation rather than a Box: dns_sd's callback writes through it while we hold
    // `&mut self`, and a Box would claim that access as unique.
    reply: *mut Reply,
    instance: String,
    port: u16,
    txt: Vec<u8>,
}

impl Registration {
    fn register(advert: &PondAdvert) -> Result<Self, AdvertiseError> {
        let instance = advert.instance();
        let txt = advert.txt_rdata()?;
        let txt_len = u16::try_from(txt.len())
            .map_err(|_| AdvertiseError::Refused(format!("TXT is {} bytes", txt.len())))?;
        let name = CString::new(instance.as_str())
            .map_err(|_| AdvertiseError::Refused("the instance name holds a NUL byte".into()))?;
        let regtype = CString::new(POND_SERVICE_TYPE)
            .map_err(|_| AdvertiseError::Refused("the service type holds a NUL byte".into()))?;
        let reply = Box::into_raw(Box::<Reply>::default());
        let mut sd_ref: ffi::DnsServiceRef = std::ptr::null_mut();
        // SAFETY: dns_sd copies every string and the TXT before returning. The context outlives
        // the ref: `Registration::drop` deallocates the ref before it frees `reply`.
        // A null domain and host select the default domain and the Mac's own host name.
        let code = unsafe {
            ffi::DNSServiceRegister(
                &mut sd_ref,
                0,
                0,
                name.as_ptr(),
                regtype.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                advert.port.to_be(),
                txt_len,
                txt.as_ptr().cast(),
                Some(on_register_reply),
                reply.cast(),
            )
        };
        if code != ffi::NO_ERROR || sd_ref.is_null() {
            // SAFETY: no ref was made, so dns_sd holds no copy of the pointer.
            drop(unsafe { Box::from_raw(reply) });
            return Err(error_for(code, "DNSServiceRegister"));
        }
        let mut registration = Self {
            sd_ref,
            reply,
            instance,
            port: advert.port,
            txt,
        };
        if registration.read_reply(REGISTER_REPLY_TIMEOUT)? {
            tracing::info!(
                instance = registration.reply().name.as_deref().unwrap_or_default(),
                "mDNSResponder registered _pond._tcp"
            );
        } else {
            tracing::warn!(
                instance = %registration.instance,
                "mDNSResponder has not confirmed _pond._tcp yet; it may still be probing the name"
            );
        }
        Ok(registration)
    }

    fn reply(&self) -> &Reply {
        // SAFETY: allocated in `register` and freed only in `drop`; dns_sd writes it only inside
        // `read_reply`, never while this borrow lives.
        unsafe { &*self.reply }
    }

    /// Handles one message from mDNSResponder if one arrives within `wait`; `Ok(false)` if none
    /// did. An error means the registration is gone and must be made again.
    fn read_reply(&mut self, wait: Duration) -> Result<bool, AdvertiseError> {
        // SAFETY: `sd_ref` is live for as long as `self`.
        let fd = unsafe { ffi::DNSServiceRefSockFD(self.sd_ref) };
        if fd < 0 {
            return Err(AdvertiseError::Unavailable(
                "the connection to mDNSResponder is closed".into(),
            ));
        }
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = libc::c_int::try_from(wait.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: one valid pollfd, owned here for the duration of the call.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(AdvertiseError::Unavailable(format!(
                "waiting on mDNSResponder: {error}"
            )));
        }
        if ready == 0 {
            return Ok(false);
        }
        // SAFETY: the socket is readable, so this reads one message without blocking for long;
        // the callback it runs writes only to `self.reply`.
        let code = unsafe { ffi::DNSServiceProcessResult(self.sd_ref) };
        if code != ffi::NO_ERROR {
            return Err(error_for(code, "DNSServiceProcessResult"));
        }
        let reply = self.reply();
        match reply.code {
            Some(code) if code != ffi::NO_ERROR => Err(error_for(code, "the registration")),
            Some(_) if !reply.added => Err(AdvertiseError::Unavailable(
                "mDNSResponder withdrew the registration".into(),
            )),
            _ => Ok(true),
        }
    }

    fn update_txt(&mut self, txt: Vec<u8>) -> Result<(), AdvertiseError> {
        let len = u16::try_from(txt.len())
            .map_err(|_| AdvertiseError::Refused(format!("TXT is {} bytes", txt.len())))?;
        // SAFETY: a null record ref names the registration's primary TXT; dns_sd copies the rdata.
        let code = unsafe {
            ffi::DNSServiceUpdateRecord(
                self.sd_ref,
                std::ptr::null_mut(),
                0,
                len,
                txt.as_ptr().cast(),
                0,
            )
        };
        if code != ffi::NO_ERROR {
            return Err(error_for(code, "DNSServiceUpdateRecord"));
        }
        self.txt = txt;
        Ok(())
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: the ref came from a successful DNSServiceRegister and is deallocated once,
        // after which dns_sd can no longer call back into `reply`.
        unsafe {
            ffi::DNSServiceRefDeallocate(self.sd_ref);
            drop(Box::from_raw(self.reply));
        }
    }
}

/// The registrar thread's state: what the pond wants advertised, and what mDNSResponder holds.
#[derive(Default)]
struct Registrar {
    wanted: Option<PondAdvert>,
    current: Option<Registration>,
}

impl Registrar {
    fn publish(&mut self, advert: PondAdvert) -> Result<(), AdvertiseError> {
        let txt = advert.txt_rdata()?;
        let instance = advert.instance();
        self.wanted = Some(advert.clone());
        if let Some(registration) = self.current.as_mut() {
            if registration.instance == instance && registration.port == advert.port {
                if registration.txt == txt {
                    return Ok(());
                }
                match registration.update_txt(txt) {
                    Ok(()) => return Ok(()),
                    Err(error @ AdvertiseError::Refused(_)) => return Err(error),
                    Err(error) => {
                        tracing::warn!(%error, "mDNSResponder lost the registration; registering again");
                    }
                }
            }
        }
        // A new name or port, or a lost registration: retire the old one, with its goodbyes.
        self.current = None;
        self.current = Some(Registration::register(&advert)?);
        Ok(())
    }

    fn withdraw(&mut self) {
        self.wanted = None;
        self.current = None;
    }

    fn check(&mut self) {
        if let Some(registration) = self.current.as_mut() {
            match registration.read_reply(Duration::ZERO) {
                Ok(true) => {
                    if let Some(name) = registration.reply().name.as_deref() {
                        if name != registration.instance {
                            tracing::info!(
                                instance = name,
                                "mDNSResponder renamed the pond's instance after a name conflict"
                            );
                        }
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, "mDNSResponder dropped the pond's registration; registering again");
                    self.current = None;
                }
            }
        }
        if self.current.is_some() {
            return;
        }
        let Some(advert) = self.wanted.clone() else {
            return;
        };
        match Registration::register(&advert) {
            Ok(registration) => {
                tracing::info!(instance = %registration.instance, "mDNS: _pond._tcp registered again");
                self.current = Some(registration);
            }
            Err(error) => tracing::debug!(%error, "mDNS: still cannot register _pond._tcp"),
        }
    }
}

enum Command {
    Publish(PondAdvert, oneshot::Sender<Result<(), AdvertiseError>>),
    Withdraw(oneshot::Sender<()>),
}

fn run_registrar(commands: mpsc::Receiver<Command>) {
    let mut registrar = Registrar::default();
    let mut last_check = Instant::now();
    loop {
        match commands.recv_timeout(RESPONDER_CHECK_INTERVAL) {
            Ok(Command::Publish(advert, reply)) => {
                let _ = reply.send(registrar.publish(advert));
            }
            Ok(Command::Withdraw(reply)) => {
                registrar.withdraw();
                let _ = reply.send(());
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The advertiser was dropped; dropping the registrar deregisters.
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if last_check.elapsed() >= RESPONDER_CHECK_INTERVAL {
            registrar.check();
            last_check = Instant::now();
        }
    }
}

pub struct DnsSdAdvertiser {
    commands: mpsc::Sender<Command>,
}

impl DnsSdAdvertiser {
    pub fn new() -> Self {
        let (commands, receiver) = mpsc::channel();
        if let Err(error) = std::thread::Builder::new()
            .name("pond-dns-sd".into())
            .spawn(move || run_registrar(receiver))
        {
            tracing::error!(%error, "could not start the dns_sd registrar; LAN discovery is off");
        }
        Self { commands }
    }
}

impl Default for DnsSdAdvertiser {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ServiceAdvertiser for DnsSdAdvertiser {
    async fn publish(&self, advert: &PondAdvert) -> Result<(), AdvertiseError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Publish(advert.clone(), reply))
            .map_err(|_| {
                AdvertiseError::Unavailable("the dns_sd registrar is not running".into())
            })?;
        answer
            .await
            .map_err(|_| AdvertiseError::Unavailable("the dns_sd registrar stopped".into()))?
    }

    async fn withdraw(&self) {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Withdraw(reply)).is_ok() {
            let _ = done.await;
        }
    }

    fn responder(&self) -> &'static str {
        RESPONDER
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::network::domain::pond_advert::SetupAdvert;

    #[test]
    fn only_a_request_dns_sd_would_refuse_again_is_final() {
        for code in [ffi::ERR_BAD_PARAM, ffi::ERR_NAME_CONFLICT, ffi::ERR_INVALID] {
            assert!(matches!(error_for(code, "x"), AdvertiseError::Refused(_)));
        }
        for code in [
            ffi::ERR_SERVICE_NOT_RUNNING,
            ffi::ERR_DEFUNCT_CONNECTION,
            ffi::ERR_POLICY_DENIED,
            -65537,
        ] {
            assert!(matches!(
                error_for(code, "x"),
                AdvertiseError::Unavailable(_)
            ));
        }
        let AdvertiseError::Unavailable(denied) = error_for(ffi::ERR_POLICY_DENIED, "register")
        else {
            unreachable!()
        };
        assert!(denied.contains("local network"));
    }

    /// `dns-sd -L` with a deadline, since it never exits on its own.
    fn resolve(instance: &str) -> String {
        let mut child = std::process::Command::new("dns-sd")
            .args(["-L", instance, POND_SERVICE_TYPE, "local."])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dns-sd");
        std::thread::sleep(Duration::from_secs(3));
        let _ = child.kill();
        let out = child.wait_with_output().expect("dns-sd output");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Registers with the real mDNSResponder: `cargo test -p pond-connect -- --ignored dns_sd`.
    #[tokio::test]
    #[ignore = "registers with this Mac's mDNSResponder and resolves through dns-sd"]
    async fn mdnsresponder_resolves_the_pond_and_follows_its_claim() {
        let host = format!("giap-test-{}", std::process::id());
        let open =
            PondAdvert::new(&host, 4999, "0.1.0", "sha256/test").with_setup(Some(SetupAdvert {
                id: "7f3a".into(),
                unclaimed: true,
            }));
        let advertiser = DnsSdAdvertiser::new();
        advertiser.publish(&open).await.expect("publish");

        let resolved = resolve(&open.instance());
        assert!(resolved.contains(":4999"), "{resolved}");
        assert!(
            resolved.contains("setup=1") && resolved.contains("id=7f3a"),
            "{resolved}"
        );

        let mut claimed = open.clone();
        claimed.setup = Some(SetupAdvert {
            id: "7f3a".into(),
            unclaimed: false,
        });
        advertiser.publish(&claimed).await.expect("update");
        let resolved = resolve(&open.instance());
        assert!(resolved.contains("id=7f3a"), "{resolved}");
        assert!(!resolved.contains("setup=1"), "{resolved}");

        advertiser.withdraw().await;
        let resolved = resolve(&open.instance());
        assert!(!resolved.contains(":4999"), "still resolvable: {resolved}");
    }
}
