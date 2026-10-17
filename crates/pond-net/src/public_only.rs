//! A client for pages someone else chose, such as search results: it connects only to public
//! addresses, so a result can never make the pond read its own API, the Matter server or the
//! household's router.
//!
//! Names are judged where they are looked up ([`PublicOnlyResolver`]), which covers every
//! redirect hop too. reqwest never looks up an IP-literal host, so [`check_target`] judges those:
//! the caller runs it on the first URL, and the redirect policy runs it on every hop.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::Url;

/// The error type reqwest's `Resolving` carries.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Why a fetch was refused before it connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not `http` or `https`.
    Scheme(String),
    /// The URL names no host.
    NoHost,
    /// An IP-literal host outside the public internet.
    NotPublic(IpAddr),
    /// The name has no public address.
    NoPublicAddress(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheme(scheme) => write!(f, "refused a {scheme}: URL; only http and https"),
            Self::NoHost => write!(f, "refused a URL with no host"),
            Self::NotPublic(ip) => write!(f, "refused {ip}: not a public address"),
            Self::NoPublicAddress(host) => write!(f, "refused {host}: it has no public address"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Whether `url` may be fetched: `http` or `https`, and a public address when its host is an IP
/// literal. A host name is judged when it is looked up.
pub fn check_target(url: &Url) -> Result<(), Refusal> {
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(Refusal::Scheme(other.to_string())),
    }
    let Some(host) = url.host_str() else {
        return Err(Refusal::NoHost);
    };
    // The parser has already rewritten IPv4 spellings like `2130706433` to dotted form.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>() {
        Ok(ip) => public_or_refused(ip),
        Err(_) => Ok(()),
    }
}

fn public_or_refused(ip: IpAddr) -> Result<(), Refusal> {
    if is_public(ip) {
        Ok(())
    } else {
        Err(Refusal::NotPublic(ip))
    }
}

/// Whether `ip` is on the public internet. Refused: loopback, private, link-local, CGNAT,
/// unique-local, multicast, unspecified, and the other ranges IANA marks not globally reachable
/// (documentation, benchmarking, reserved, broadcast). An IPv6 form that embeds an IPv4 address
/// (mapped, NAT64, 6to4) is judged by the address it embeds.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let refused = a == 0 // this network, unspecified
        || a == 10 // private
        || a == 127 // loopback
        || (a == 100 && (64..=127).contains(&b)) // CGNAT
        || (a == 169 && b == 254) // link-local
        || (a == 172 && (16..=31).contains(&b)) // private
        || (a == 192 && b == 168) // private
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 192 && b == 0 && c == 2) // documentation
        || (a == 198 && (18..=19).contains(&b)) // benchmarking
        || (a == 198 && b == 51 && c == 100) // documentation
        || (a == 203 && b == 0 && c == 113) // documentation
        || a >= 224; // multicast, reserved, broadcast
    !refused
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    let embedded_v4 = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    match s {
        // Unspecified, loopback and the deprecated IPv4-compatible block.
        [0, 0, 0, 0, 0, 0, _, _] => false,
        // NAT64: a gateway would carry the request to the IPv4 address inside.
        [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] => is_public_v4(embedded_v4(hi, lo)),
        // Local-use NAT64.
        [0x64, 0xff9b, 1, ..] => false,
        // Discard-only.
        [0x100, 0, 0, 0, ..] => false,
        // Teredo and documentation.
        [0x2001, 0, ..] | [0x2001, 0xdb8, ..] => false,
        // 6to4: the IPv4 address is the next 32 bits.
        [0x2002, hi, lo, ..] => is_public_v4(embedded_v4(hi, lo)),
        [first, ..] => {
            let unique_local = first & 0xfe00 == 0xfc00;
            let link_local = first & 0xffc0 == 0xfe80;
            let site_local = first & 0xffc0 == 0xfec0;
            let multicast = first & 0xff00 == 0xff00;
            !(unique_local || link_local || site_local || multicast)
        }
    }
}

/// Looks names up as [`crate::dns::PondResolver`] does, then keeps only public addresses; a name
/// with none left is refused.
#[derive(Debug, Clone, Copy, Default)]
pub struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let ipv4_only = crate::dns::ipv4_only();
        Box::pin(async move {
            let lookup_host = host.clone();
            let addrs =
                tokio::task::spawn_blocking(move || crate::dns::lookup(&lookup_host, ipv4_only))
                    .await??;
            let public: Vec<SocketAddr> = addrs
                .into_iter()
                .filter(|addr| is_public(addr.ip()))
                .collect();
            if public.is_empty() {
                return Err(Box::new(Refusal::NoPublicAddress(host)) as BoxError);
            }
            Ok::<Addrs, BoxError>(Box::new(public.into_iter()))
        })
    }
}

/// At most `max_redirects` hops, each a target [`check_target`] accepts and the installed
/// redirect gate permits.
fn redirect_policy(max_redirects: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        // `previous` holds every URL already requested, the first included.
        if attempt.previous().len() > max_redirects {
            return attempt.error("too many redirects");
        }
        if let Err(refused) = check_target(attempt.url()) {
            return attempt.error(refused);
        }
        match crate::REDIRECT_GATE
            .get()
            .map(|gate| gate(attempt.url().as_str()))
        {
            Some(Err(refused)) => attempt.error(refused),
            _ => attempt.follow(),
        }
    })
}

/// [`crate::client_builder`] with the public-only resolver, a redirect policy of at most
/// `max_redirects` checked hops, and no proxy: a proxy looks names up itself, past the resolver.
pub fn client_builder(max_redirects: usize) -> reqwest::ClientBuilder {
    crate::client_builder()
        .dns_resolver(PublicOnlyResolver)
        .redirect(redirect_policy(max_redirects))
        .no_proxy()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn every_non_public_range_is_refused() {
        for refused in [
            "0.0.0.0",
            "0.1.2.3",
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "224.0.0.1",
            "239.255.255.250",
            "255.255.255.255",
            "240.0.0.1",
            "192.0.2.10",
            "198.18.0.1",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "64:ff9b:1::1",
        ] {
            assert!(!is_public(ip(refused)), "{refused} must be refused");
        }
    }

    #[test]
    fn ipv6_forms_of_refused_ipv4_addresses_are_refused() {
        for refused in [
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:192.168.1.1",
            "::ffff:172.16.0.1",
            "::ffff:169.254.169.254",
            "::ffff:100.64.0.1",
            "::ffff:224.0.0.1",
            "::ffff:0.0.0.0",
            // IPv4-compatible, NAT64 and 6to4 forms of loopback and a private address.
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "2002:c0a8:101::",
        ] {
            assert!(!is_public(ip(refused)), "{refused} must be refused");
        }
    }

    #[test]
    fn public_addresses_pass() {
        for public in [
            "8.8.8.8",
            "1.1.1.1",
            "100.63.255.255",
            "100.128.0.1",
            "172.32.0.1",
            "192.169.0.1",
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:808:808::",
        ] {
            assert!(is_public(ip(public)), "{public} must pass");
        }
    }

    #[test]
    fn ip_literal_urls_in_refused_ranges_are_refused() {
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1:4000/api/v1/settings",
            "http://[::1]:4000/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:7f00:1]/",
            "http://10.0.0.1/admin",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            // The URL parser reads these as 127.0.0.1; the check sees what reqwest would.
            "http://2130706433/",
            "http://0x7f.1/",
            "http://0177.0.0.1/",
        ] {
            let parsed = Url::parse(url).unwrap();
            assert!(
                matches!(check_target(&parsed), Err(Refusal::NotPublic(_))),
                "{url} must be refused"
            );
        }
    }

    #[test]
    fn only_web_schemes_with_a_host_pass() {
        for url in ["ftp://example.com/", "file:///etc/passwd", "gopher://x/"] {
            let parsed = Url::parse(url).unwrap();
            assert!(
                matches!(check_target(&parsed), Err(Refusal::Scheme(_))),
                "{url}"
            );
        }
        for url in [
            "https://example.com/a",
            "http://8.8.8.8/",
            "http://[2606:4700::1]/",
        ] {
            assert_eq!(check_target(&Url::parse(url).unwrap()), Ok(()), "{url}");
        }
    }

    #[tokio::test]
    async fn a_name_that_resolves_only_to_loopback_is_refused() {
        let err = match PublicOnlyResolver
            .resolve("localhost".parse().unwrap())
            .await
        {
            Ok(addrs) => panic!(
                "localhost resolved to {:?}",
                addrs.collect::<Vec<SocketAddr>>()
            ),
            Err(e) => e,
        };
        assert!(err.to_string().contains("no public address"), "{err}");
    }

    /// A one-connection server on loopback that answers `answer` and reports whether it was asked.
    fn serve_once(answer: String) -> (u16, std::thread::JoinHandle<bool>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut buf = [0u8; 1024];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(answer.as_bytes());
                        return true;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
            false
        });
        (port, handle)
    }

    fn redirect_to(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )
    }

    const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";

    /// `page.test` stands for a public site: the override skips the resolver for it alone.
    fn client_with_public_page(port: u16, max_redirects: usize) -> reqwest::Client {
        client_builder(max_redirects)
            .resolve("page.test", SocketAddr::from(([127, 0, 0, 1], port)))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn a_redirect_to_a_loopback_literal_is_never_followed() {
        let (private_port, private) = serve_once(OK.to_string());
        let (page_port, page) = serve_once(redirect_to(&format!(
            "http://127.0.0.1:{private_port}/api/v1/settings"
        )));
        let error = client_with_public_page(page_port, 3)
            .get(format!("http://page.test:{page_port}/"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect(), "{error:?}");
        assert!(page.join().unwrap(), "the first hop was requested");
        assert!(!private.join().unwrap(), "loopback was reached");
    }

    #[tokio::test]
    async fn a_redirect_to_a_name_for_loopback_is_never_followed() {
        let (private_port, private) = serve_once(OK.to_string());
        let (page_port, page) =
            serve_once(redirect_to(&format!("http://localhost:{private_port}/")));
        let error = client_with_public_page(page_port, 3)
            .get(format!("http://page.test:{page_port}/"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_connect() || error.is_request(), "{error:?}");
        assert!(page.join().unwrap());
        assert!(!private.join().unwrap(), "loopback was reached by name");
    }

    /// A server on loopback that answers each connection from `answers(port)` in turn, and
    /// reports how many it served.
    fn serve_chain(
        answers: impl FnOnce(u16) -> Vec<String>,
    ) -> (u16, std::thread::JoinHandle<usize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let answers = answers(port);
        let handle = std::thread::spawn(move || {
            let mut served = 0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            let mut pending = answers.into_iter();
            let mut next = pending.next();
            while let Some(answer) = next.as_ref() {
                if std::time::Instant::now() > deadline {
                    break;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut buf = [0u8; 1024];
                        let _ = stream.read(&mut buf);
                        let _ = stream.write_all(answer.as_bytes());
                        served += 1;
                        next = pending.next();
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
            served
        });
        (port, handle)
    }

    /// `hops` redirects within `page.test`, then a 200.
    fn chain(hops: usize) -> impl FnOnce(u16) -> Vec<String> {
        move |port| {
            let mut answers: Vec<String> = (1..=hops)
                .map(|n| redirect_to(&format!("http://page.test:{port}/{n}")))
                .collect();
            answers.push(OK.to_string());
            answers
        }
    }

    #[tokio::test]
    async fn three_redirects_are_followed() {
        let (port, server) = serve_chain(chain(3));
        let body = client_with_public_page(port, 3)
            .get(format!("http://page.test:{port}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        assert_eq!(server.join().unwrap(), 4);
    }

    #[tokio::test]
    async fn a_fourth_redirect_is_not() {
        let (port, server) = serve_chain(chain(4));
        let error = client_with_public_page(port, 3)
            .get(format!("http://page.test:{port}/"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect(), "{error:?}");
        assert_eq!(
            server.join().unwrap(),
            4,
            "the fourth hop's target must not be requested"
        );
    }
}
