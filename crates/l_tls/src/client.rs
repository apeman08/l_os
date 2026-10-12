//! A blocking TLS 1.3 client stream on top of any byte transport.
//!
//! Uses rustls' *unbuffered* interface (the only one available without `std`)
//! and drives it with a small loop. The transport is whatever carries bytes:
//! a socket handle on Larp OS, a `TcpStream` in the host tests.

use alloc::{string::String, sync::Arc, vec, vec::Vec};
use rustls::{
    ClientConfig,
    client::UnbufferedClientConnection,
    unbuffered::{AppDataRecord, ConnectionState, EncodeError, EncryptError, InsufficientSizeError, UnbufferedStatus},
};
use rustls_pki_types::ServerName;

/// Moves bytes. Both calls block until they make progress.
pub trait Transport {
    /// Send some of `data`; returns how many bytes were accepted (>0), or an error.
    fn send(&mut self, data: &[u8]) -> Result<usize, TransportError>;
    /// Receive into `buf`; `Ok(0)` means the peer closed the connection.
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, TransportError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportError(pub i64);

#[derive(Debug)]
pub enum Error {
    /// The byte transport failed.
    Transport(TransportError),
    /// TLS failed: bad certificate, wrong name, protocol error, ...
    Tls(rustls::Error),
    /// The server closed the connection before the handshake finished.
    ClosedDuringHandshake,
    /// The peer closed without a TLS close_notify (possible truncation).
    TruncatedStream,
    /// Buffer limit exceeded.
    TooMuchData,
}

impl From<rustls::Error> for Error {
    fn from(e: rustls::Error) -> Self {
        Error::Tls(e)
    }
}

const MAX_BUFFER: usize = 64 * 1024;

fn flush_buf<T: Transport>(io: &mut T, outgoing: &mut Vec<u8>) -> Result<(), Error> {
    let mut sent = 0;
    while sent < outgoing.len() {
        let n = io.send(&outgoing[sent..]).map_err(Error::Transport)?;
        if n == 0 {
            return Err(Error::Transport(TransportError(0)));
        }
        sent += n;
    }
    outgoing.clear();
    Ok(())
}

pub struct TlsStream<T: Transport> {
    conn: UnbufferedClientConnection,
    io: T,
    /// Received TLS bytes not yet consumed by rustls.
    incoming: Vec<u8>,
    /// TLS bytes produced by rustls not yet sent.
    outgoing: Vec<u8>,
    /// Decrypted application data not yet returned to the caller.
    plaintext: Vec<u8>,
    peer_eof: bool,
    got_close_notify: bool,
}

impl<T: Transport> TlsStream<T> {
    /// Connect and complete the handshake, validating the server certificate
    /// for `server_name` against the config's roots. On any error the
    /// transport is dropped unused: nothing is ever sent in the clear.
    pub fn connect(config: Arc<ClientConfig>, server_name: &str, io: T) -> Result<Self, Error> {
        let name = ServerName::try_from(String::from(server_name)).map_err(|_| Error::Tls(rustls::Error::General("invalid server name".into())))?;
        let conn = UnbufferedClientConnection::new(config, name)?;
        let mut s = TlsStream {
            conn,
            io,
            incoming: Vec::new(),
            outgoing: Vec::new(),
            plaintext: Vec::new(),
            peer_eof: false,
            got_close_notify: false,
        };
        s.drive(None)?;
        Ok(s)
    }

    /// Run the state machine until the connection is ready for application
    /// data (`app_data == None`), or until `app_data` has been fully queued
    /// for sending and flushed, or until some plaintext is available.
    fn drive(&mut self, mut app_data: Option<&[u8]>) -> Result<(), Error> {
        let mut want_plaintext = false;
        let mut pending_app = app_data.take();
        loop {
            let UnbufferedStatus { mut discard, state } = self.conn.process_tls_records(&mut self.incoming);
            let state = state?;
            let mut done = false;
            match state {
                ConnectionState::ReadTraffic(mut rt) => {
                    while let Some(record) = rt.next_record() {
                        let AppDataRecord { discard: d, payload } = record?;
                        discard += d;
                        if self.plaintext.len() + payload.len() > MAX_BUFFER {
                            return Err(Error::TooMuchData);
                        }
                        self.plaintext.extend_from_slice(payload);
                    }
                    if !self.plaintext.is_empty() {
                        done = true;
                    }
                    want_plaintext = true;
                }
                ConnectionState::EncodeTlsData(mut enc) => {
                    let start = self.outgoing.len();
                    // Grow until the record fits.
                    let mut grow = 1024;
                    loop {
                        self.outgoing.resize(start + grow, 0);
                        match enc.encode(&mut self.outgoing[start..]) {
                            Ok(n) => {
                                self.outgoing.truncate(start + n);
                                break;
                            }
                            Err(EncodeError::InsufficientSize(InsufficientSizeError { required_size })) => {
                                grow = required_size;
                            }
                            Err(EncodeError::AlreadyEncoded) => {
                                self.outgoing.truncate(start);
                                break;
                            }
                        }
                    }
                }
                ConnectionState::TransmitTlsData(tx) => {
                    flush_buf(&mut self.io, &mut self.outgoing)?;
                    tx.done();
                }
                ConnectionState::BlockedHandshake => {
                    self.fill_incoming()?;
                    if self.peer_eof {
                        return Err(Error::ClosedDuringHandshake);
                    }
                }
                ConnectionState::WriteTraffic(mut wt) => {
                    if let Some(data) = pending_app.take() {
                        let start = self.outgoing.len();
                        let mut grow = data.len() + 64;
                        loop {
                            self.outgoing.resize(start + grow, 0);
                            match wt.encrypt(data, &mut self.outgoing[start..]) {
                                Ok(n) => {
                                    self.outgoing.truncate(start + n);
                                    break;
                                }
                                Err(EncryptError::InsufficientSize(InsufficientSizeError { required_size })) => {
                                    grow = required_size;
                                }
                                Err(EncryptError::EncryptExhausted) => {
                                    return Err(Error::Tls(rustls::Error::General("encryption limit reached".into())));
                                }
                            }
                        }
                        flush_buf(&mut self.io, &mut self.outgoing)?;
                        done = true;
                    } else if !want_plaintext {
                        done = true; // handshake complete, nothing else to do
                    } else {
                        // Waiting for the server's data.
                        self.fill_incoming()?;
                        if self.peer_eof {
                            return if self.got_close_notify { Ok(()) } else { Err(Error::TruncatedStream) };
                        }
                    }
                }
                ConnectionState::PeerClosed | ConnectionState::Closed => {
                    self.got_close_notify = true;
                    self.peer_eof = true;
                    done = true;
                }
                // Early data and anything newer are not used by this client.
                _ => return Err(Error::Tls(rustls::Error::General("unexpected TLS state".into()))),
            }
            self.incoming.drain(..discard);
            if done {
                return Ok(());
            }
        }
    }

    fn flush(&mut self) -> Result<(), Error> {
        flush_buf(&mut self.io, &mut self.outgoing)
    }

    fn fill_incoming(&mut self) -> Result<(), Error> {
        if self.incoming.len() > MAX_BUFFER {
            return Err(Error::TooMuchData);
        }
        let mut buf = vec![0u8; 4096];
        let n = self.io.recv(&mut buf).map_err(Error::Transport)?;
        if n == 0 {
            self.peer_eof = true;
        } else {
            self.incoming.extend_from_slice(&buf[..n]);
        }
        Ok(())
    }

    /// Send application data (encrypted), blocking until it is handed to the transport.
    pub fn write_all(&mut self, data: &[u8]) -> Result<(), Error> {
        for chunk in data.chunks(8 * 1024) {
            self.drive(Some(chunk))?;
        }
        Ok(())
    }

    /// Read decrypted application data. `Ok(0)` means the server closed the
    /// connection cleanly (with close_notify); an unclean close is an error.
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        if self.plaintext.is_empty() {
            if self.peer_eof {
                return if self.got_close_notify { Ok(0) } else { Err(Error::TruncatedStream) };
            }
            self.drive_for_read()?;
        }
        let n = out.len().min(self.plaintext.len());
        out[..n].copy_from_slice(&self.plaintext[..n]);
        self.plaintext.drain(..n);
        Ok(n)
    }

    fn drive_for_read(&mut self) -> Result<(), Error> {
        // Loop: process buffered records first, then wait for more bytes.
        loop {
            let before = self.plaintext.len();
            self.drive(None).or_else(|e| match e {
                Error::TruncatedStream if self.got_close_notify => Ok(()),
                other => Err(other),
            })?;
            if self.plaintext.len() > before || self.peer_eof {
                return Ok(());
            }
            // `drive(None)` returns immediately in WriteTraffic without reading;
            // pull more bytes so the next pass can decrypt them.
            self.fill_incoming()?;
            if self.peer_eof && self.plaintext.is_empty() && !self.got_close_notify {
                return Err(Error::TruncatedStream);
            }
        }
    }

    /// Send close_notify and drop the connection.
    pub fn close(mut self) {
        // Best effort: queue close_notify via the state machine, then flush.
        let _ = self.drive_close();
    }

    fn drive_close(&mut self) -> Result<(), Error> {
        let UnbufferedStatus { state, .. } = self.conn.process_tls_records(&mut self.incoming);
        if let Ok(ConnectionState::WriteTraffic(mut wt)) = state {
            let start = self.outgoing.len();
            self.outgoing.resize(start + 64, 0);
            if let Ok(n) = wt.queue_close_notify(&mut self.outgoing[start..]) {
                self.outgoing.truncate(start + n);
                self.flush()?;
            }
        }
        Ok(())
    }
}
