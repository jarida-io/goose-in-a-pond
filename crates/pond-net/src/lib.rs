//! The pond's HTTP clients. Every outbound client is built here, so each one looks names up the
//! way [`dns`] decides; [`public_only`] builds the one for pages a third party chose. `crates/pond-core/tests/egress_guard.rs` fails the build when a crate
//! builds a `reqwest` client any other way.

pub mod dns;
pub mod public_only;

use std::sync::OnceLock;

/// Judges a redirect target before a client follows it: `Err` refuses the hop.
pub type RedirectGate = fn(&str) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;

static REDIRECT_GATE: OnceLock<RedirectGate> = OnceLock::new();

/// Install the gate every client built here asks about each redirect; the first call wins. The
/// server installs the network-mode check, which otherwise sees only the first URL of a request.
pub fn set_redirect_gate(gate: RedirectGate) {
    let _ = REDIRECT_GATE.set(gate);
}

/// reqwest's own limit, kept: a client built here follows at most this many redirects.
const MAX_REDIRECTS: usize = 10;

fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        match REDIRECT_GATE.get().map(|gate| gate(attempt.url().as_str())) {
            Some(Err(refused)) => attempt.error(refused),
            _ => attempt.follow(),
        }
    })
}

/// A client builder that resolves names through [`dns::PondResolver`] and asks the redirect gate
/// about every hop. Use it wherever a crate would call `reqwest::Client::builder()`; the caller's
/// own settings go on top as usual, a redirect policy of its own included.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .dns_resolver(dns::PondResolver)
        .redirect(redirect_policy())
}

/// As `reqwest::Client::new()`, through [`client_builder`].
///
/// # Panics
///
/// As `reqwest::Client::new()` does: when the TLS backend cannot be initialised.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("the HTTP client could not be built (TLS backend)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// One request through a pond client, by name, to a server on this machine.
    #[tokio::test]
    async fn a_pond_client_reaches_a_server_by_name() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .unwrap();
        });

        let body = client()
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        server.join().unwrap();
    }

    /// A server that answers each connection from `answers` in turn, and records each request line.
    fn serve(answers: Vec<String>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0u8; 1024];
                let n = stream.read(&mut request).unwrap();
                let text = String::from_utf8_lossy(&request[..n]);
                seen.push(text.lines().next().unwrap_or_default().to_string());
                stream.write_all(answer.as_bytes()).unwrap();
            }
            seen
        });
        (port, server)
    }

    fn redirect_to(location: &str) -> String {
        format!("HTTP/1.1 302 Found\r\nlocation: {location}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
    }

    /// The gate this test binary installs: it refuses any path under `/refused`.
    fn gate(url: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if url.contains("/refused") {
            Err("refused by the test gate".into())
        } else {
            Ok(())
        }
    }

    #[tokio::test]
    async fn every_redirect_hop_is_asked_of_the_gate() {
        set_redirect_gate(gate);
        let ok = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
        let (port, server) = serve(vec![redirect_to("/allowed"), ok.to_string()]);
        let body = client()
            .get(format!("http://127.0.0.1:{port}/start"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        assert_eq!(server.join().unwrap().len(), 2);

        // A second hop the gate refuses is never requested.
        let (port, server) = serve(vec![redirect_to("/allowed"), redirect_to("/refused/x")]);
        let error = client()
            .get(format!("http://127.0.0.1:{port}/start"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect(), "{error:?}");
        let source = std::error::Error::source(&error).map(ToString::to_string);
        assert_eq!(source.as_deref(), Some("refused by the test gate"));
        let seen = server.join().unwrap();
        assert!(
            seen.iter().all(|line| !line.contains("/refused")),
            "{seen:?}"
        );
    }

    /// The public-only client asks the same gate about every hop, on top of its own checks.
    #[tokio::test]
    async fn the_public_only_client_asks_the_gate_too() {
        set_redirect_gate(gate);
        let (port, server) = serve(vec![redirect_to("/refused/x")]);
        let error = public_only::client_builder(3)
            .resolve(
                "page.test",
                std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            )
            .build()
            .unwrap()
            .get(format!("http://page.test:{port}/start"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect(), "{error:?}");
        let source = std::error::Error::source(&error).map(ToString::to_string);
        assert_eq!(source.as_deref(), Some("refused by the test gate"));
        assert_eq!(server.join().unwrap().len(), 1);
    }
}
