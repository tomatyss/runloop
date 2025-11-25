use runloop_rmp::Header;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn next_msg_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub fn uuid_to_u128(id: Uuid) -> u128 {
    id.as_u128()
}

pub fn build_header(schema_id: u16, trace_id: u128, msg_id: u64) -> Header {
    Header {
        schema_id,
        created_at_ms: current_millis(),
        ttl_ms: 30_000,
        trace_id,
        msg_id,
        ..Header::default()
    }
}
