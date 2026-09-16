use md5::{Digest, Md5};
use rc4::{KeyInit, Rc4, StreamCipher};

const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

const OWNER_VALUE: [u8; 32] = [
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00,
    0x0F, 0x1E, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0x87, 0x96, 0xA5, 0xB4, 0xC3, 0xD2, 0xE1, 0xF0,
];

const FILE_ID: [u8; 16] = [
    0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF,
];

const PERMISSIONS: i32 = -60;

fn padded(password: &[u8]) -> [u8; 32] {
    let mut value = [0_u8; 32];
    let copied = password.len().min(32);
    value[..copied].copy_from_slice(&password[..copied]);
    value[copied..].copy_from_slice(&PADDING[..32 - copied]);
    value
}

fn file_key(password: &[u8]) -> Vec<u8> {
    let key_bytes = 16;
    let mut hasher = Md5::new();
    hasher.update(padded(password));
    hasher.update(OWNER_VALUE);
    hasher.update(PERMISSIONS.to_le_bytes());
    hasher.update(FILE_ID);
    let mut digest = hasher.finalize();
    for _ in 0..50 {
        let mut round = Md5::new();
        round.update(&digest[..key_bytes]);
        digest = round.finalize();
    }
    digest[..key_bytes].to_vec()
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut output = data.to_vec();
    let Ok(mut cipher) = Rc4::new_from_slice(key) else {
        unreachable!("test key length is supported")
    };
    cipher.apply_keystream(&mut output);
    output
}

fn user_value(key: &[u8]) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(PADDING);
    hasher.update(FILE_ID);
    let mut value = rc4(key, &hasher.finalize());
    for round in 1_u8..=19 {
        let rotated: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
        value = rc4(&rotated, &value);
    }
    value.resize(32, 0);
    value
}

fn object_key(key: &[u8], number: u32) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(key);
    hasher.update(&number.to_le_bytes()[..3]);
    hasher.update([0_u8; 2]);
    let digest = hasher.finalize();
    digest[..16].to_vec()
}

fn escape(bytes: &[u8]) -> String {
    let mut escaped = String::new();
    for byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                escaped.push('\\');
                escaped.push(char::from(*byte));
            }
            _ => escaped.push_str(&format!("\\{byte:03o}")),
        }
    }
    escaped
}

pub fn build(password: &[u8]) -> Vec<u8> {
    let key = file_key(password);
    let content = b"BT /F1 12 Tf 20 70 Td (Confidential) Tj ET";
    let encrypted_content = rc4(&object_key(&key, 5), content);
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        {
            let mut stream =
                format!("<< /Length {} >>\nstream\n", encrypted_content.len()).into_bytes();
            stream.extend_from_slice(&encrypted_content);
            stream.extend_from_slice(b"\nendstream");
            stream
        },
        format!(
            "<< /Filter /Standard /V 2 /R 3 /Length 128 /P {PERMISSIONS} /O ({}) /U ({}) >>",
            escape(&OWNER_VALUE),
            escape(&user_value(&key))
        )
        .into_bytes(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    let id = FILE_ID
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Encrypt 6 0 R /ID [<{id}> <{id}>] >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}
