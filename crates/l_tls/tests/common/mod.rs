#![allow(dead_code)]
use l_tls::client::{Transport, TransportError};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tls")
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

/// Entropy for the host tests: the OS generator.
pub fn os_entropy(buf: &mut [u8]) -> bool {
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(buf)).is_ok()
}

pub const NOW: u64 = 1_800_000_000; // Jan 2027, inside the fixtures' validity
pub fn clock_now() -> Option<u64> {
    Some(NOW)
}

pub struct Tcp(pub TcpStream);

impl Transport for Tcp {
    fn send(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        self.0.write(data).map_err(|_| TransportError(-1))
    }
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.0.read(buf).map_err(|_| TransportError(-1))
    }
}

pub struct Server {
    child: Child,
    pub port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NEXT_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Unique per test process and per call (parallel tests must never share a
/// port), and verified free right now.
fn pick_port() -> u16 {
    use std::sync::atomic::Ordering;
    let _ = NEXT_PORT.compare_exchange(0, 20000 + (std::process::id() % 20000) as u16, Ordering::SeqCst, Ordering::SeqCst);
    loop {
        let p = NEXT_PORT.fetch_add(1, Ordering::SeqCst);
        if p < 1024 || p > 60000 {
            NEXT_PORT.store(20000, Ordering::SeqCst);
            continue;
        }
        if TcpListener::bind(("127.0.0.1", p)).is_ok() {
            return p;
        }
    }
}

/// Start `openssl s_server` serving a status page over TLS.
pub fn s_server(cert: &str, key: &str, version_flag: &str) -> Server {
    let dir = fixtures();
    for _attempt in 0..5 {
        let port = pick_port();
        let child = Command::new("openssl")
            .args(["s_server", "-accept", &port.to_string(), "-cert"])
            .arg(dir.join(cert))
            .arg("-key")
            .arg(dir.join(key))
            .args([version_flag, "-www", "-quiet"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("openssl s_server");
        let mut server = Server { child, port };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(_)) = server.child.try_wait() {
                break; // died (port taken?): try another port
            }
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            assert!(Instant::now() < deadline, "s_server did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    panic!("could not start s_server");
}

pub fn connect_tcp(port: u16) -> Tcp {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    Tcp(s)
}
