//! Message queues for kernel endpoints (pure logic).

use crate::handles::{Object, Rights};

pub const MAX_MSG: usize = 64;
pub const QUEUE_CAP: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub len: usize,
    pub data: [u8; MAX_MSG],
    /// A capability travelling with the message (already attenuated).
    pub xfer: Option<(Object, Rights)>,
}

impl Message {
    pub fn new(bytes: &[u8], xfer: Option<(Object, Rights)>) -> Option<Message> {
        if bytes.len() > MAX_MSG {
            return None;
        }
        let mut data = [0u8; MAX_MSG];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Message { len: bytes.len(), data, xfer })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

/// Bounded FIFO. Senders are told when it is full instead of blocking.
pub struct Queue {
    items: [Option<Message>; QUEUE_CAP],
    head: usize,
    len: usize,
}

impl Queue {
    pub const fn new() -> Self {
        Self { items: [None; QUEUE_CAP], head: 0, len: 0 }
    }

    pub fn push(&mut self, m: Message) -> Result<(), Message> {
        if self.len == QUEUE_CAP {
            return Err(m);
        }
        self.items[(self.head + self.len) % QUEUE_CAP] = Some(m);
        self.len += 1;
        Ok(())
    }

    pub fn peek(&self) -> Option<&Message> {
        if self.len == 0 { None } else { self.items[self.head].as_ref() }
    }

    pub fn pop(&mut self) -> Option<Message> {
        if self.len == 0 {
            return None;
        }
        let m = self.items[self.head].take();
        self.head = (self.head + 1) % QUEUE_CAP;
        self.len -= 1;
        m
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_size_limit() {
        assert!(Message::new(&[0; MAX_MSG], None).is_some());
        assert!(Message::new(&[0; MAX_MSG + 1], None).is_none());
        assert_eq!(Message::new(b"abc", None).unwrap().bytes(), b"abc");
    }

    #[test]
    fn fifo_order_wraparound_and_full() {
        let mut q = Queue::new();
        assert!(q.pop().is_none() && q.peek().is_none());
        for round in 0..3u8 {
            for i in 0..QUEUE_CAP as u8 {
                q.push(Message::new(&[round, i], None).unwrap()).unwrap();
            }
            assert!(q.push(Message::new(&[9], None).unwrap()).is_err()); // full
            assert_eq!(q.len(), QUEUE_CAP);
            for i in 0..QUEUE_CAP as u8 {
                assert_eq!(q.peek().unwrap().bytes(), &[round, i]);
                assert_eq!(q.pop().unwrap().bytes(), &[round, i]);
            }
            assert!(q.pop().is_none());
        }
    }

    #[test]
    fn transferred_capability_travels_with_message() {
        let mut q = Queue::new();
        q.push(Message::new(b"x", Some((Object::Console, Rights::WRITE))).unwrap()).unwrap();
        assert_eq!(q.pop().unwrap().xfer, Some((Object::Console, Rights::WRITE)));
    }
}
