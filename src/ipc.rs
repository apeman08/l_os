//! Kernel endpoints: bounded message queues that processes reach only through
//! handles. Endpoints are created by the kernel (e.g. by the code that sets up
//! a group of processes) and live until [`reset`].

use l_core::ipc::{Message, Queue};
use spin::Mutex;

pub const MAX_ENDPOINTS: usize = 8;

static ENDPOINTS: Mutex<[Option<Queue>; MAX_ENDPOINTS]> = Mutex::new([const { None }; MAX_ENDPOINTS]);

/// Create an endpoint; returns its id for use in `Object::Endpoint`.
pub fn create_endpoint() -> Option<u16> {
    let mut eps = ENDPOINTS.lock();
    let id = eps.iter().position(Option::is_none)?;
    eps[id] = Some(Queue::new());
    Some(id as u16)
}

pub fn exists(id: u16) -> bool {
    ENDPOINTS.lock().get(id as usize).is_some_and(Option::is_some)
}

/// Drop all endpoints and their queued messages.
pub fn reset() {
    for e in ENDPOINTS.lock().iter_mut() {
        *e = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    NoSuchEndpoint,
    Full,
}

pub fn send(id: u16, msg: Message) -> Result<(), SendError> {
    let mut eps = ENDPOINTS.lock();
    let q = eps.get_mut(id as usize).and_then(Option::as_mut).ok_or(SendError::NoSuchEndpoint)?;
    q.push(msg).map_err(|_| SendError::Full)
}

/// Look at the next message without removing it.
pub fn peek(id: u16) -> Option<Message> {
    ENDPOINTS.lock().get(id as usize).and_then(Option::as_ref).and_then(|q| q.peek().copied())
}

pub fn pop(id: u16) -> Option<Message> {
    ENDPOINTS.lock().get_mut(id as usize).and_then(Option::as_mut).and_then(Queue::pop)
}
