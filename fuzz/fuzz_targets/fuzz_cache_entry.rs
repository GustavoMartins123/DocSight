#![no_main]

use docsight_cache::{
    CacheKey, EngineIdentity, EntryProducer, decode_entry, decode_untrusted_entry, encode_entry,
    verify_entry,
};
use libfuzzer_sys::fuzz_target;

mod support;

fn engine() -> EngineIdentity {
    EngineIdentity {
        executable_sha256: "a".repeat(64),
        engine_version: "0.0.0-fuzz".to_owned(),
        ir_schema_version: docsight_core::IR_SCHEMA_VERSION.to_owned(),
        layout_profile: "agent-fidelity-v1".to_owned(),
        layout_font_fingerprint: "fonts-fuzz".to_owned(),
    }
}

fuzz_target!(|data: &[u8]| {
    let data = support::bounded(data);
    let Some((source, document)) = support::fixed_source_and_document() else {
        return;
    };
    let key = CacheKey::document_ir(&source, &engine());
    let _ = verify_entry(data, &key);
    let _ = decode_entry(data, &key);
    let _ = decode_untrusted_entry(data, &key);

    let Ok(entry) = encode_entry(&key, &document, EntryProducer::InProcess) else {
        return;
    };
    for mutated in [
        support::mutate_base(&entry, data),
        support::mutate_tail(&entry, data),
    ] {
        if let Ok(decoded) = decode_entry(&mutated, &key) {
            assert_eq!(
                decoded, document,
                "an accepted cache entry must hold the document that was written"
            );
        }
    }
});
