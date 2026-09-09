#![no_main]

use docsight_render::trace::ProofBundle;
use libfuzzer_sys::fuzz_target;

mod support;

fuzz_target!(|data: &[u8]| {
    let _ = ProofBundle::from_bytes(support::bounded(data));
    if let Some(bundle) = support::proof_bundle_with_manifest(data) {
        let _ = ProofBundle::from_bytes(&bundle);
    }
});
