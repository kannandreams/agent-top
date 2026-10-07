//! `serve --listen` decodes request bodies from anything that can reach it.
//! Any input may be refused; none may panic, hang or run out of memory.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|body: &[u8]| {
    let _ = agent_top_core::otlp_decode::decode_protobuf(body);
});
