#![no_main]

use docsight_core::Rect;
use docsight_tables::{RulingSegment, TextSpanItem, detect_tables};
use libfuzzer_sys::fuzz_target;

mod support;

fuzz_target!(|data: &[u8]| {
    let mut spans = Vec::new();
    let mut rulings = Vec::new();
    for (index, chunk) in support::bounded(data).chunks(8).take(128).enumerate() {
        let [
            x,
            y,
            width_byte,
            height_byte,
            text,
            font_size,
            extent,
            orientation,
        ] = chunk
        else {
            continue;
        };
        let x0 = f32::from(*x) * 2.0;
        let y0 = f32::from(*y) * 2.0;
        let width = f32::from(*width_byte % 40) + 1.0;
        let height = f32::from(*height_byte % 30) + 1.0;
        if let Ok(bbox) = Rect::new(x0, y0, x0 + width, y0 + height) {
            spans.push(TextSpanItem {
                text: format!("{index}:{text}"),
                bbox,
                font_size: f32::from(*font_size % 48) + 1.0,
                bold: *extent & 1 == 1,
            });
        }
        let rx = f32::from(*text) * 2.0;
        let ry = f32::from(*font_size) * 2.0;
        if *orientation & 1 == 0 {
            rulings.push(RulingSegment {
                x0: rx,
                y0: ry,
                x1: rx + f32::from(*extent % 80) + 1.0,
                y1: ry,
            });
        } else {
            rulings.push(RulingSegment {
                x0: rx,
                y0: ry,
                x1: rx,
                y1: ry + f32::from(*extent % 80) + 1.0,
            });
        }
    }
    let first = detect_tables(1, &spans, &rulings, 612.0, 792.0);
    let second = detect_tables(1, &spans, &rulings, 612.0, 792.0);
    assert_eq!(first, second);
});
