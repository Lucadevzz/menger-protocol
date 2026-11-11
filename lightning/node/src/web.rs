//! The local web interface: one page and a small JSON API, served on this computer only.
//!
//! The page is the app; every button goes through the API. Three locks keep other web pages out:
//! - the server listens on a loopback address only;
//! - it answers only requests addressed to that address by name (`Host`), which defeats DNS
//!   rebinding, where a hostile site makes its own name point at this computer;
//! - every API call carries a secret drawn at start-up and written into the page. A page from
//!   another site cannot read it, and cannot send that header at all without the browser asking
//!   this server first, which it never agrees to.
//!
//! The page also refuses to be shown inside another site's frame, so nobody can trick a click.

use anyhow::{anyhow, bail, Result};
use std::io::Read;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

/// The page of the app.
pub const PAGE: &str = include_str!("interface.html");

/// The header that carries the secret.
pub const TOKEN_HEADER: &str = "x-menger-jeton";
const MAX_BODY_BYTES: u64 = 16 * 1024;
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// An API call, handed to whoever owns the party, who answers on `reply`.
pub struct ApiCall {
    pub method: String,
    pub path: String,
    pub body: String,
    pub reply: mpsc::Sender<ApiReply>,
}

#[derive(Debug)]
pub struct ApiReply {
    pub status: u16,
    pub body: String,
}

impl ApiReply {
    pub fn ok(value: serde_json::Value) -> Self {
        ApiReply { status: 200, body: value.to_string() }
    }

    pub fn error(status: u16, message: impl Into<String>) -> Self {
        ApiReply { status, body: serde_json::json!({ "erreur": message.into() }).to_string() }
    }
}

pub struct Interface {
    pub url: String,
    pub token: String,
}

/// Serves the page and the API on `addr`, a loopback address such as `127.0.0.1:8421` (port 0
/// picks a free one). API calls that pass the checks go to `forward`, which returns false once
/// nobody listens any more.
pub fn serve(addr: &str, forward: impl Fn(ApiCall) -> bool + Send + 'static) -> Result<Interface> {
    let wanted: SocketAddr = addr.parse().map_err(|_| anyhow!("adresse d'interface invalide : {addr}"))?;
    if !wanted.ip().is_loopback() {
        bail!("l'interface n'écoute que sur cet ordinateur : utilise 127.0.0.1");
    }
    let server = Server::http(wanted).map_err(|e| anyhow!("interface sur {addr} : {e}"))?;
    let port = server.server_addr().to_ip().map_or(wanted.port(), |a| a.port());
    let token = crate::keys::random_hex(16)?;
    let page = PAGE.replace("__JETON__", &token);
    let secret = token.clone();
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            handle(request, port, &secret, &page, &forward);
        }
    });
    Ok(Interface { url: format!("http://127.0.0.1:{port}"), token })
}

fn handle(mut request: Request, port: u16, token: &str, page: &str, forward: &impl Fn(ApiCall) -> bool) {
    let host = header(&request, "host").unwrap_or_default();
    let path = request.url().split('?').next().unwrap_or_default().to_string();
    let reply = if !host_allowed(&host, port) {
        ApiReply::error(403, "adresse refusée")
    } else if request.method() == &Method::Get && (path == "/" || path == "/index.html") {
        let _ = request.respond(with_headers(Response::from_string(page), "text/html; charset=utf-8"));
        return;
    } else if !path.starts_with("/api/") {
        ApiReply::error(404, "page inconnue")
    } else if !token_matches(header(&request, TOKEN_HEADER).as_deref(), token) {
        ApiReply::error(403, "jeton manquant ou faux")
    } else if request.body_length().unwrap_or(0) as u64 > MAX_BODY_BYTES {
        ApiReply::error(413, "requête trop longue")
    } else {
        let method = request.method().as_str().to_string();
        let mut body = String::new();
        let read = request.as_reader().take(MAX_BODY_BYTES).read_to_string(&mut body);
        if read.is_err() {
            ApiReply::error(400, "requête illisible")
        } else {
            let (tx, rx) = mpsc::channel();
            if forward(ApiCall { method, path, body, reply: tx }) {
                rx.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| ApiReply::error(504, "le nœud ne répond pas"))
            } else {
                ApiReply::error(503, "le nœud s'est arrêté")
            }
        }
    };
    let response = Response::from_string(reply.body).with_status_code(reply.status);
    let _ = request.respond(with_headers(response, "application/json"));
}

fn with_headers<R: Read>(response: Response<R>, content_type: &str) -> Response<R> {
    let headers = [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("X-Frame-Options", "DENY"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; \
             connect-src 'self'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'",
        ),
    ];
    headers.iter().fold(response, |r, (name, value)| {
        r.with_header(Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header"))
    })
}

fn header(request: &Request, name: &str) -> Option<String> {
    request.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.to_string())
}

/// The names under which this computer reaches the server.
pub fn host_allowed(host: &str, port: u16) -> bool {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")]
        .iter()
        .any(|h| h.eq_ignore_ascii_case(host.trim()))
}

/// Compares in constant time, so that timing does not reveal the secret.
pub fn token_matches(given: Option<&str>, token: &str) -> bool {
    let Some(given) = given else { return false };
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}


#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new().try_proxy_from_env(false).timeout(Duration::from_secs(5)).build()
    }

    fn status(result: Result<ureq::Response, ureq::Error>) -> u16 {
        match result {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn only_this_computer_and_only_the_page() {
        assert!(serve("0.0.0.0:0", |_| true).is_err(), "never on the network");
        let interface = serve("127.0.0.1:0", |call: ApiCall| {
            let _ = call.reply.send(ApiReply::ok(serde_json::json!({ "chemin": call.path, "corps": call.body })));
            true
        })
        .unwrap();
        let a = agent();
        let page = a.get(&interface.url).call().unwrap();
        assert_eq!(page.header("X-Frame-Options"), Some("DENY"));
        assert!(page.into_string().unwrap().contains(&interface.token), "the page knows the secret");

        let api = format!("{}/api/etat", interface.url);
        assert_eq!(status(a.get(&api).call()), 403, "no secret");
        assert_eq!(status(a.get(&api).set(TOKEN_HEADER, "faux").call()), 403, "wrong secret");
        let ok = a.post(&api).set(TOKEN_HEADER, &interface.token).send_string("{}").unwrap();
        assert_eq!(ok.into_json::<serde_json::Value>().unwrap()["chemin"], "/api/etat");
        assert_eq!(status(a.get(&format!("{}/ailleurs", interface.url)).call()), 404);
    }

    #[test]
    fn names_and_secrets() {
        assert!(host_allowed("127.0.0.1:8421", 8421));
        assert!(host_allowed("localhost:8421", 8421));
        assert!(!host_allowed("evil.example:8421", 8421), "DNS rebinding");
        assert!(!host_allowed("127.0.0.1:8422", 8421));
        assert!(token_matches(Some("ab12"), "ab12"));
        assert!(!token_matches(Some("ab13"), "ab12"));
        assert!(!token_matches(Some("ab1"), "ab12"));
        assert!(!token_matches(None, "ab12"));
    }
}
