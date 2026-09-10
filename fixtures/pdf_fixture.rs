#![allow(dead_code)]

pub fn sample_pdf() -> Vec<u8> {
    build_pdf(
        "0.9 0.9 1 rg 10 10 180 80 re f 0 0 0 rg BT /F1 12 Tf 20 70 Td (Hello DOCSIGHT) Tj ET",
        "[0 0 200 100]",
        "",
    )
}

pub fn build_pdf(content: &str, media_box: &str, stream_entries: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox {media_box} /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!(
            "<< /Length {} {} >>\nstream\n{}\nendstream",
            content.len(),
            stream_entries,
            content
        ),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, object).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

pub struct ModernPdfOptions {
    pub compress_objects: bool,
    pub predictor: bool,
    pub incremental_update: bool,
}

impl Default for ModernPdfOptions {
    fn default() -> Self {
        Self {
            compress_objects: true,
            predictor: true,
            incremental_update: false,
        }
    }
}

pub fn build_modern_pdf(content: &str, options: &ModernPdfOptions) -> Vec<u8> {
    let catalog = "<< /Type /Catalog /Pages 2 0 R >>";
    let pages = "<< /Type /Pages /Kids [3 0 R] /Count 1 >>";
    let page = "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>";
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

    let mut pdf = b"%PDF-1.5\n".to_vec();
    let mut located: Vec<(u32, usize)> = Vec::new();
    let mut compressed: Vec<(u32, usize)> = Vec::new();

    if options.compress_objects {
        let members: [(u32, &str); 4] = [(1, catalog), (2, pages), (3, page), (4, font)];
        let mut header = String::new();
        let mut body = String::new();
        for (number, object) in members {
            header.push_str(&format!("{number} {} ", body.len()));
            body.push_str(object);
            body.push('\n');
        }
        let first = header.len();
        let payload = format!("{header}{body}");
        let deflated = deflate(payload.as_bytes());
        located.push((6, pdf.len()));
        pdf.extend_from_slice(
            format!(
                "6 0 obj\n<< /Type /ObjStm /N {} /First {first} /Filter /FlateDecode /Length {} >>\nstream\n",
                members.len(),
                deflated.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&deflated);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        for (index, (number, _)) in members.iter().enumerate() {
            compressed.push((*number, index));
        }
    } else {
        for (number, object) in [(1, catalog), (2, pages), (3, page), (4, font)] {
            located.push((number, pdf.len()));
            pdf.extend_from_slice(format!("{number} 0 obj\n{object}\nendobj\n").as_bytes());
        }
    }

    located.push((5, pdf.len()));
    pdf.extend_from_slice(
        format!(
            "5 0 obj\n<< /Length {} >>\nstream\n{content}\nendstream\nendobj\n",
            content.len()
        )
        .as_bytes(),
    );

    let size = 8;
    let xref_offset = pdf.len();
    let records = xref_stream_records(&located, &compressed, xref_offset, size);
    let payload = if options.predictor {
        deflate(&png_up_encode(&records, 5))
    } else {
        deflate(&records)
    };
    let parms = if options.predictor {
        " /DecodeParms << /Predictor 12 /Columns 5 >>"
    } else {
        ""
    };
    pdf.extend_from_slice(
        format!(
            "7 0 obj\n<< /Type /XRef /Size {size} /W [1 3 1] /Root 1 0 R /Filter /FlateDecode{parms} /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    pdf.extend_from_slice(&payload);
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    pdf.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF\n").as_bytes());

    if options.incremental_update {
        let previous = xref_offset;
        let update_offset = pdf.len();
        let records = xref_stream_records(&[(8, update_offset)], &[], update_offset, 9);
        let payload = deflate(&png_up_encode(&records, 5));
        pdf.extend_from_slice(
            format!(
                "8 0 obj\n<< /Type /XRef /Size 9 /Index [8 1] /Prev {previous} /W [1 3 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 5 >> /Length {} >>\nstream\n",
                payload.len()
            )
            .as_bytes(),
        );
        pdf.extend_from_slice(&payload);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");
        pdf.extend_from_slice(format!("startxref\n{update_offset}\n%%EOF\n").as_bytes());
    }

    pdf
}

fn xref_stream_records(
    located: &[(u32, usize)],
    compressed: &[(u32, usize)],
    self_offset: usize,
    size: u32,
) -> Vec<u8> {
    let mut rows = vec![[0_u8; 5]; size as usize];
    rows[0] = [0, 0, 0, 0, 255];
    for (number, offset) in located {
        rows[*number as usize] = offset_record(*offset);
    }
    for (number, index) in compressed {
        rows[*number as usize] = [2, 0, 0, 6, *index as u8];
    }
    if (7_u32) < size {
        rows[7] = offset_record(self_offset);
    }
    rows.into_iter().flatten().collect()
}

fn offset_record(offset: usize) -> [u8; 5] {
    let bytes = (offset as u32).to_be_bytes();
    [1, bytes[1], bytes[2], bytes[3], 0]
}

fn png_up_encode(data: &[u8], columns: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut previous = vec![0_u8; columns];
    for row in data.chunks(columns) {
        out.push(2);
        for (index, byte) in row.iter().enumerate() {
            out.push(byte.wrapping_sub(previous[index]));
        }
        previous = row.to_vec();
    }
    out
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(0xffff).peekable();
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(chunk) = chunks.next() {
        let final_block = u8::from(chunks.peek().is_none());
        let length = chunk.len() as u16;
        out.push(final_block);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut low = 1_u32;
    let mut high = 0_u32;
    for byte in data {
        low = (low + u32::from(*byte)) % 65521;
        high = (high + low) % 65521;
    }
    (high << 16) | low
}
