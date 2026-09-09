#![no_main]

use docsight_core::Rect;
use docsight_render::{HitQuery, hit_test};
use libfuzzer_sys::fuzz_target;

mod support;

fn byte(data: &[u8], index: usize) -> u8 {
    match data.get(index) {
        Some(value) => *value,
        None => 0,
    }
}

fuzz_target!(|data: &[u8]| {
    let Some(document) = support::laid_out_fixed_document() else {
        return;
    };
    let x0 = f32::from(byte(data, 0)) * 3.0;
    let y0 = f32::from(byte(data, 1)) * 4.0;
    let query = if byte(data, 2) & 1 == 0 {
        HitQuery::Point(x0, y0)
    } else {
        let width = f32::from(byte(data, 3) % 80) + 1.0;
        let height = f32::from(byte(data, 4) % 80) + 1.0;
        let Ok(bbox) = Rect::new(x0, y0, x0 + width, y0 + height) else {
            return;
        };
        HitQuery::BBox(bbox)
    };
    let _ = hit_test(&document, 1, &query);
});
