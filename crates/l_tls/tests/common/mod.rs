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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Start `openssl s_server` serving a status page over TLS.
pub fn s_server(cert: &str, key: &str, version_flag: &str) -> Server {
    let port = free_port();
    let dir = fixtures();
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
    let server = Server { child, port };
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "s_server did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    server
}

pub fn connect_tcp(port: u16) -> Tcp {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    Tcp(s)
}
