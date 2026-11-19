//! Redirect handling, against a server in this process.
//!
//! These exercise the one path the unit tests cannot reach. `fetch_following`
//! walks a redirect chain itself so that each hop gets the `robots.txt` of the
//! host it lands on, the internal-address guard, and that host's pacing — and
//! none of that is observable without a server that records what was asked
//! for. A throwaway script verified it once; this is the version CI runs.
//!
//! Deliberately a hand-rolled listener rather than a mock-HTTP crate: the
//! whole contract is "answer four paths and remember the requests", which is
//! shorter to write than a dependency is to justify.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use rustai_core::Error;
use rustai_core::http::{FetchConfig, Fetcher};

const ARTICLE: &str = "<!doctype html><title>Final</title><article><h1>Final page</h1><p>\
    The destination article body, repeated so that it survives extraction and \
    is long enough that the denoiser treats it as content rather than furniture. \
    The destination article body, repeated so that it survives extraction. \
    The destination article body, repeated so that it survives extraction.</p></article>";

const ROBOTS: &str = "User-agent: *\nDisallow: /disallowed\n";

/// A server that answers the redirect shapes under test and records paths.
struct Server {
    base: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Self {
        // Port 0: the OS picks a free one, so parallel test binaries cannot
        // collide on a hard-coded number.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // Sequential on purpose: the assertions are about *which*
                // requests arrive, and one connection at a time keeps the
                // recorded order meaningful.
                let _ = handle(stream, &log);
            }
        });

        Server { base, seen }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("lock").clone()
    }

    fn asked_for(&self, path: &str) -> bool {
        self.seen().iter().any(|p| p == path)
    }
}

fn handle(mut stream: TcpStream, seen: &Mutex<Vec<String>>) -> std::io::Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    seen.lock().expect("lock").push(path.clone());

    let response = match path.as_str() {
        "/robots.txt" => body(200, "text/plain", ROBOTS),
        "/start" => redirect("/final"),
        "/final" | "/disallowed" => body(200, "text/html", ARTICLE),
        "/loop" => redirect("/loop"),
        "/to-disallowed" => redirect("/disallowed"),
        "/to-metadata" => redirect("http://169.254.169.254/latest/meta-data/"),
        _ => body(404, "text/plain", "no"),
    };
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn body(status: u16, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.0 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

fn redirect(location: &str) -> String {
    format!(
        "HTTP/1.0 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\
         Connection: close\r\n\r\n"
    )
}

/// Loopback is refused by default, so every test here has to opt in.
fn fetcher(respect_robots: bool) -> Fetcher {
    Fetcher::with_config(FetchConfig {
        allow_private_hosts: true,
        respect_robots,
        per_host_delay: std::time::Duration::ZERO,
        retries: 0,
        ..Default::default()
    })
    .expect("fetcher")
}

#[tokio::test]
async fn a_302_is_followed_to_the_page_it_points_at() {
    let server = Server::start();
    let page = fetcher(false).fetch(&server.url("/start")).await.expect("fetch");

    assert!(page.body.contains("destination article"), "landed on the wrong page");
    // `url` stays what the caller asked for; `final_url` is where it ended up.
    assert_eq!(page.url, server.url("/start"));
    assert_eq!(page.final_url, server.url("/final"));
    assert_eq!(page.status, 200);
}

#[tokio::test]
async fn a_redirect_loop_ends_instead_of_spinning() {
    let server = Server::start();
    let err = fetcher(false).fetch(&server.url("/loop")).await.expect_err("should give up");

    assert!(matches!(err, Error::Network { .. }), "got {err:?}");
    // Bounded by `max_redirects`, not by the loop running out of patience.
    let hits = server.seen().iter().filter(|p| *p == "/loop").count();
    assert_eq!(hits, FetchConfig::default().max_redirects + 1, "walked {hits} hops");
}

#[tokio::test]
async fn robots_is_read_for_the_host_a_redirect_lands_on() {
    let server = Server::start();
    let err = fetcher(true).fetch(&server.url("/to-disallowed")).await.expect_err("denied");

    assert!(matches!(err, Error::RobotsDenied(_)), "got {err:?}");
    assert!(server.asked_for("/robots.txt"), "robots.txt was never read");
    // The point of the whole change: the target is never fetched.
    assert!(!server.asked_for("/disallowed"), "fetched a disallowed redirect target");
}

#[tokio::test]
async fn the_same_target_is_fetched_once_robots_is_switched_off() {
    let server = Server::start();
    let page = fetcher(false).fetch(&server.url("/to-disallowed")).await.expect("fetch");

    assert!(page.body.contains("destination article"));
    assert!(server.asked_for("/disallowed"));
    assert!(!server.asked_for("/robots.txt"), "robots.txt read despite being off");
}

#[tokio::test]
async fn a_redirect_into_an_internal_address_is_refused() {
    let server = Server::start();
    // `allow_private_hosts` is what let the test reach 127.0.0.1 at all, so
    // the guard has to be on for this one -- which means the first hop is
    // refused too. What matters is that neither request leaves.
    let strict = Fetcher::with_config(FetchConfig {
        respect_robots: false,
        per_host_delay: std::time::Duration::ZERO,
        ..Default::default()
    })
    .expect("fetcher");

    let err = strict.fetch(&server.url("/to-metadata")).await.expect_err("refused");
    assert!(matches!(err, Error::InvalidUrl(_)), "got {err:?}");
    assert!(server.seen().is_empty(), "a request escaped: {:?}", server.seen());
}
