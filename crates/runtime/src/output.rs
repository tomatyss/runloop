use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

/// Fixed-size ring buffer for capturing stdout/stderr bytes.
#[derive(Debug, Clone)]
pub struct OutputRing {
    capacity: usize,
    data: Arc<Mutex<VecDeque<u8>>>,
}

impl OutputRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            data: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
        }
    }

    pub fn push(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut guard = self.data.lock();
        for &byte in bytes {
            if guard.len() == self.capacity {
                guard.pop_front();
            }
            guard.push_back(byte);
        }
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.data.lock().iter().copied().collect()
    }
}
