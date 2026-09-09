#![no_main]

use docsight_search::execute_spatial_query;
use libfuzzer_sys::fuzz_target;

mod support;

fuzz_target!(|data: &[u8]| {
    let Some(document) = support::laid_out_fixed_document() else {
        return;
    };
    let query = String::from_utf8_lossy(support::bounded(data));
    let _ = execute_spatial_query(&document, &query);
});
