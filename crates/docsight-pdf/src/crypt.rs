use crate::syntax::{ObjectRef, Value, malformed};
use aes::cipher::{
    Array, BlockCipherDecrypt, BlockCipherEncrypt, BlockModeDecrypt, KeyInit, KeyIvInit,
    block_padding::NoPadding,
};
use docsight_core::DocsightError;
use md5::Md5;
use rc4::{Rc4, StreamCipher};
use sha2::{Digest, Sha256, Sha384, Sha512};
use std::collections::BTreeMap;

const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

const MAX_KEY_BYTES: usize = 32;
const MAX_HARDENED_ROUNDS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Cipher {
    Rc4,
    Aes,
    None,
}

#[derive(Clone, Debug)]
pub(crate) struct Decryptor {
    key: Vec<u8>,
    stream_cipher: Cipher,
    string_cipher: Cipher,
    revision: u8,
}

impl Decryptor {
    pub fn from_encrypt_dictionary(
        dictionary: &BTreeMap<String, Value>,
        first_id: &[u8],
        password: &[u8],
    ) -> Result<Self, DocsightError> {
        match dictionary.get("Filter") {
            Some(Value::Name(name)) if name == "Standard" => {}
            Some(Value::Name(name)) => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF security handler {name}"),
                });
            }
            _ => return Err(malformed("encryption dictionary has no Filter name")),
        }

        let version = integer(dictionary, "V")?.unwrap_or(0);
        let revision = integer(dictionary, "R")?
            .ok_or_else(|| malformed("encryption dictionary has no R revision"))?;
        let permissions = integer(dictionary, "P")?
            .ok_or_else(|| malformed("encryption dictionary has no P permissions"))?;
        let owner = byte_string(dictionary, "O")?;
        let user = byte_string(dictionary, "U")?;
        let encrypt_metadata = match dictionary.get("EncryptMetadata") {
            Some(Value::Bool(value)) => *value,
            None => true,
            _ => return Err(malformed("EncryptMetadata must be a boolean")),
        };

        let revision = u8::try_from(revision)
            .map_err(|_| malformed("encryption revision is outside the supported range"))?;

        if version == 5 || revision >= 5 {
            let key = aes256_key(
                dictionary,
                password,
                &owner,
                &user,
                permissions,
                encrypt_metadata,
                revision,
            )?;
            return Ok(Self {
                key,
                stream_cipher: Cipher::Aes,
                string_cipher: Cipher::Aes,
                revision,
            });
        }

        let length_bits = integer(dictionary, "Length")?.unwrap_or(40);
        let key_bytes = usize::try_from(length_bits / 8)
            .map_err(|_| malformed("encryption key length is outside the supported range"))?;
        if !(5..=16).contains(&key_bytes) {
            return Err(malformed("encryption key length must be 40 to 128 bits"));
        }

        let (stream_cipher, string_cipher, effective_bytes) = match version {
            1 | 2 => (Cipher::Rc4, Cipher::Rc4, key_bytes),
            4 => crypt_filters(dictionary, key_bytes)?,
            other => {
                return Err(DocsightError::UnsupportedFeature {
                    feature: format!("PDF encryption version {other}"),
                });
            }
        };

        let key = legacy_key(
            password,
            &owner,
            permissions,
            first_id,
            revision,
            effective_bytes,
            encrypt_metadata,
        );
        let expected = expected_user_value(&key, first_id, revision)?;
        let matches = match revision {
            2 => user.len() >= 32 && expected[..32] == user[..32],
            _ => user.len() >= 16 && expected[..16] == user[..16],
        };
        if !matches {
            return Err(DocsightError::EncryptedDocument);
        }

        Ok(Self {
            key,
            stream_cipher,
            string_cipher,
            revision,
        })
    }

    pub fn decrypt_stream(
        &self,
        reference: ObjectRef,
        data: &[u8],
    ) -> Result<Vec<u8>, DocsightError> {
        self.decrypt(self.stream_cipher, reference, data)
    }

    pub fn decrypt_string(
        &self,
        reference: ObjectRef,
        data: &[u8],
    ) -> Result<Vec<u8>, DocsightError> {
        self.decrypt(self.string_cipher, reference, data)
    }

    fn decrypt(
        &self,
        cipher: Cipher,
        reference: ObjectRef,
        data: &[u8],
    ) -> Result<Vec<u8>, DocsightError> {
        match cipher {
            Cipher::None => Ok(data.to_vec()),
            Cipher::Rc4 => {
                let key = self.object_key(cipher, reference)?;
                let mut output = data.to_vec();
                let mut rc4 = Rc4::new_from_slice(&key)
                    .map_err(|_| malformed("RC4 key length is not supported"))?;
                rc4.apply_keystream(&mut output);
                Ok(output)
            }
            Cipher::Aes => {
                let key = self.object_key(cipher, reference)?;
                decrypt_aes_cbc(&key, data)
            }
        }
    }

    fn object_key(&self, cipher: Cipher, reference: ObjectRef) -> Result<Vec<u8>, DocsightError> {
        if self.revision >= 5 {
            return Ok(self.key.clone());
        }
        let mut hasher = Md5::new();
        hasher.update(&self.key);
        hasher.update(&reference.number.to_le_bytes()[..3]);
        hasher.update(&reference.generation.to_le_bytes()[..2]);
        if cipher == Cipher::Aes {
            hasher.update([0x73, 0x41, 0x6C, 0x54]);
        }
        let digest = hasher.finalize();
        let length = (self.key.len() + 5).min(16);
        Ok(digest[..length].to_vec())
    }
}

fn crypt_filters(
    dictionary: &BTreeMap<String, Value>,
    key_bytes: usize,
) -> Result<(Cipher, Cipher, usize), DocsightError> {
    let filters = match dictionary.get("CF") {
        Some(Value::Dict(filters)) => filters.clone(),
        None => BTreeMap::new(),
        _ => return Err(malformed("CF must be a dictionary of crypt filters")),
    };
    let named = |name: &str| -> Result<(Cipher, usize), DocsightError> {
        if name == "Identity" {
            return Ok((Cipher::None, key_bytes));
        }
        let Some(Value::Dict(filter)) = filters.get(name) else {
            return Err(malformed("crypt filter name is not declared in CF"));
        };
        let method = match filter.get("CFM") {
            Some(Value::Name(method)) => method.as_str(),
            _ => return Err(malformed("crypt filter has no CFM method")),
        };
        let length = match integer(filter, "Length")? {
            Some(value) if value > 40 => usize::try_from(value / 8)
                .map_err(|_| malformed("crypt filter length is outside the supported range"))?,
            Some(value) if value > 0 => usize::try_from(value)
                .map_err(|_| malformed("crypt filter length is outside the supported range"))?,
            _ => key_bytes,
        };
        match method {
            "V2" => Ok((Cipher::Rc4, length)),
            "AESV2" => Ok((Cipher::Aes, 16)),
            "None" => Ok((Cipher::None, length)),
            other => Err(DocsightError::UnsupportedFeature {
                feature: format!("PDF crypt filter method {other}"),
            }),
        }
    };
    let stream_name = match dictionary.get("StmF") {
        Some(Value::Name(name)) => name.clone(),
        None => "Identity".to_owned(),
        _ => return Err(malformed("StmF must be a name")),
    };
    let string_name = match dictionary.get("StrF") {
        Some(Value::Name(name)) => name.clone(),
        None => "Identity".to_owned(),
        _ => return Err(malformed("StrF must be a name")),
    };
    let (stream_cipher, stream_length) = named(&stream_name)?;
    let (string_cipher, _) = named(&string_name)?;
    Ok((stream_cipher, string_cipher, stream_length))
}

fn legacy_key(
    password: &[u8],
    owner: &[u8],
    permissions: i64,
    first_id: &[u8],
    revision: u8,
    key_bytes: usize,
    encrypt_metadata: bool,
) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(padded_password(password));
    let mut owner_value = [0_u8; 32];
    let copied = owner.len().min(32);
    owner_value[..copied].copy_from_slice(&owner[..copied]);
    hasher.update(owner_value);
    hasher.update((permissions as i32).to_le_bytes());
    hasher.update(first_id);
    if revision >= 4 && !encrypt_metadata {
        hasher.update([0xFF, 0xFF, 0xFF, 0xFF]);
    }
    let mut digest = hasher.finalize();
    if revision >= 3 {
        for _ in 0..50 {
            let mut round = Md5::new();
            round.update(&digest[..key_bytes.min(16)]);
            digest = round.finalize();
        }
    }
    digest[..key_bytes.min(16)].to_vec()
}

fn expected_user_value(
    key: &[u8],
    first_id: &[u8],
    revision: u8,
) -> Result<Vec<u8>, DocsightError> {
    if revision == 2 {
        let mut value = PADDING.to_vec();
        let mut rc4 =
            Rc4::new_from_slice(key).map_err(|_| malformed("RC4 key length is not supported"))?;
        rc4.apply_keystream(&mut value);
        return Ok(value);
    }
    let mut hasher = Md5::new();
    hasher.update(PADDING);
    hasher.update(first_id);
    let mut value = hasher.finalize().to_vec();
    let mut rc4 =
        Rc4::new_from_slice(key).map_err(|_| malformed("RC4 key length is not supported"))?;
    rc4.apply_keystream(&mut value);
    for round in 1_u8..=19 {
        let rotated: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
        let mut cipher = Rc4::new_from_slice(&rotated)
            .map_err(|_| malformed("RC4 key length is not supported"))?;
        cipher.apply_keystream(&mut value);
    }
    value.resize(32, 0);
    Ok(value)
}

fn aes256_key(
    dictionary: &BTreeMap<String, Value>,
    password: &[u8],
    owner: &[u8],
    user: &[u8],
    permissions: i64,
    encrypt_metadata: bool,
    revision: u8,
) -> Result<Vec<u8>, DocsightError> {
    if user.len() < 48 {
        return Err(malformed("AES-256 encryption requires a 48 byte U value"));
    }
    let validation_salt = &user[32..40];
    let key_salt = &user[40..48];
    let key = if hardened_hash(password, validation_salt, &[], revision)? != user[..32] {
        if owner.len() >= 48
            && hardened_hash(password, &owner[32..40], &user[..48], revision)? == owner[..32]
        {
            owner_file_key(dictionary, password, owner, user, revision)?
        } else {
            return Err(DocsightError::EncryptedDocument);
        }
    } else {
        let intermediate = hardened_hash(password, key_salt, &[], revision)?;
        let encrypted = byte_string(dictionary, "UE")?;
        if encrypted.len() != 32 {
            return Err(malformed("AES-256 encryption requires a 32 byte UE value"));
        }
        decrypt_aes_cbc_no_iv(&intermediate, &encrypted)?
    };
    verify_perms(dictionary, &key, permissions, encrypt_metadata)?;
    Ok(key)
}

fn verify_perms(
    dictionary: &BTreeMap<String, Value>,
    key: &[u8],
    permissions: i64,
    encrypt_metadata: bool,
) -> Result<(), DocsightError> {
    let encrypted = byte_string(dictionary, "Perms")?;
    if encrypted.len() != 16 {
        return Err(malformed(
            "AES-256 encryption requires a 16 byte Perms value",
        ));
    }
    let block: [u8; 16] = encrypted
        .try_into()
        .map_err(|_| malformed("AES-256 Perms value has an invalid length"))?;
    let mut block = Array::from(block);
    let cipher = aes::Aes256::new_from_slice(key)
        .map_err(|_| malformed("AES-256 key length is not supported"))?;
    cipher.decrypt_block(&mut block);
    let permission_bits = if let Ok(value) = i32::try_from(permissions) {
        value as u32
    } else if let Ok(value) = u32::try_from(permissions) {
        value
    } else {
        return Err(malformed(
            "AES-256 permissions value is outside the supported range",
        ));
    };
    let mut expected = [0xff_u8; 16];
    expected[..4].copy_from_slice(&permission_bits.to_le_bytes());
    expected[4..8].copy_from_slice(&[0xff; 4]);
    expected[8] = if encrypt_metadata { b'T' } else { b'F' };
    expected[9..12].copy_from_slice(b"adb");
    if block[..12] != expected[..12] {
        return Err(malformed("AES-256 Perms validation failed"));
    }
    Ok(())
}

fn owner_file_key(
    dictionary: &BTreeMap<String, Value>,
    password: &[u8],
    owner: &[u8],
    user: &[u8],
    revision: u8,
) -> Result<Vec<u8>, DocsightError> {
    let intermediate = hardened_hash(password, &owner[40..48], &user[..48], revision)?;
    let encrypted = byte_string(dictionary, "OE")?;
    if encrypted.len() != 32 {
        return Err(malformed("AES-256 encryption requires a 32 byte OE value"));
    }
    decrypt_aes_cbc_no_iv(&intermediate, &encrypted)
}

fn hardened_hash(
    password: &[u8],
    salt: &[u8],
    extra: &[u8],
    revision: u8,
) -> Result<Vec<u8>, DocsightError> {
    let mut hasher = Sha256::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(extra);
    let mut digest = hasher.finalize().to_vec();
    if revision < 6 {
        return Ok(digest);
    }

    for round in 0..MAX_HARDENED_ROUNDS {
        if digest.len() < 32 {
            return Err(malformed("AES-256 hardened hash produced a short digest"));
        }
        let block_len = (password.len() + digest.len() + extra.len())
            .checked_mul(64)
            .ok_or_else(|| malformed("AES-256 hardened hash input overflowed"))?;
        let mut block = Vec::with_capacity(block_len);
        for _ in 0..64 {
            block.extend_from_slice(password);
            block.extend_from_slice(&digest);
            block.extend_from_slice(extra);
        }
        let encrypted = encrypt_aes_cbc_no_padding(&digest[..16], &digest[16..32], &block)?;
        let modulo: u32 = encrypted[..16].iter().map(|byte| u32::from(*byte)).sum();
        digest = match modulo % 3 {
            0 => Sha256::digest(&encrypted).to_vec(),
            1 => Sha384::digest(&encrypted).to_vec(),
            _ => Sha512::digest(&encrypted).to_vec(),
        };
        let last = usize::from(*encrypted.last().unwrap_or(&0));
        if round >= 63 && last <= round - 32 {
            break;
        }
    }
    digest.truncate(32);
    Ok(digest)
}

type Aes128CbcDecryptor = cbc::Decryptor<aes::Aes128>;
type Aes256CbcDecryptor = cbc::Decryptor<aes::Aes256>;

fn decrypt_aes_cbc(key: &[u8], data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    if data.len() < 16 {
        return Err(malformed("AES encrypted data is shorter than its own IV"));
    }
    let (iv, body) = data.split_at(16);
    if !body.len().is_multiple_of(16) {
        return Err(malformed(
            "AES encrypted data is not a multiple of the block size",
        ));
    }
    let mut output = body.to_vec();
    match key.len() {
        16 => Aes128CbcDecryptor::new_from_slices(key, iv)
            .map_err(|_| malformed("AES-128 key or IV length is not supported"))?
            .decrypt_padded::<NoPadding>(&mut output)
            .map_err(|_| malformed("AES-128 decryption failed"))?,
        32 => Aes256CbcDecryptor::new_from_slices(key, iv)
            .map_err(|_| malformed("AES-256 key or IV length is not supported"))?
            .decrypt_padded::<NoPadding>(&mut output)
            .map_err(|_| malformed("AES-256 decryption failed"))?,
        _ => return Err(malformed("AES key length is not supported")),
    };
    strip_pkcs7(output)
}

fn decrypt_aes_cbc_no_iv(key: &[u8], data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let mut output = data.to_vec();
    Aes256CbcDecryptor::new_from_slices(key, &[0_u8; 16])
        .map_err(|_| malformed("AES-256 key length is not supported"))?
        .decrypt_padded::<NoPadding>(&mut output)
        .map_err(|_| malformed("AES-256 decryption failed"))?;
    Ok(output)
}

fn encrypt_aes_cbc_no_padding(
    key: &[u8],
    iv: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, DocsightError> {
    if !data.len().is_multiple_of(16) {
        return Err(malformed("AES input is not a multiple of the block size"));
    }
    let cipher = aes::Aes128::new_from_slice(key)
        .map_err(|_| malformed("AES-128 key length is not supported"))?;
    let mut previous = [0_u8; 16];
    previous.copy_from_slice(iv);
    let mut output = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut block = [0_u8; 16];
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = chunk[index] ^ previous[index];
        }
        let mut array = Array::from(block);
        cipher.encrypt_block(&mut array);
        previous.copy_from_slice(&array);
        output.extend_from_slice(&array);
    }
    Ok(output)
}

fn strip_pkcs7(mut data: Vec<u8>) -> Result<Vec<u8>, DocsightError> {
    let Some(padding) = data.last().copied() else {
        return Ok(data);
    };
    let padding = usize::from(padding);
    if padding == 0 || padding > 16 || padding > data.len() {
        return Err(malformed("AES decrypted data has invalid padding"));
    }
    let start = data.len() - padding;
    if data[start..]
        .iter()
        .any(|byte| usize::from(*byte) != padding)
    {
        return Err(malformed("AES decrypted data has invalid padding"));
    }
    data.truncate(start);
    Ok(data)
}

fn padded_password(password: &[u8]) -> [u8; 32] {
    let mut padded = [0_u8; 32];
    let copied = password.len().min(32);
    padded[..copied].copy_from_slice(&password[..copied]);
    padded[copied..].copy_from_slice(&PADDING[..32 - copied]);
    padded
}

fn integer(dictionary: &BTreeMap<String, Value>, key: &str) -> Result<Option<i64>, DocsightError> {
    match dictionary.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Int(value)) => Ok(Some(*value)),
        Some(_) => Err(malformed("encryption dictionary entry must be an integer")),
    }
}

fn byte_string(dictionary: &BTreeMap<String, Value>, key: &str) -> Result<Vec<u8>, DocsightError> {
    match dictionary.get(key) {
        Some(Value::String(bytes)) => {
            if bytes.len() > MAX_KEY_BYTES * 8 {
                return Err(malformed("encryption dictionary string is too long"));
            }
            Ok(bytes.clone())
        }
        None => Ok(Vec::new()),
        Some(_) => Err(malformed("encryption dictionary entry must be a string")),
    }
}

#[cfg(test)]
mod tests {
    use super::hardened_hash;

    #[test]
    fn r6_hardened_hash_matches_the_standard_vector() -> Result<(), Box<dyn std::error::Error>> {
        let digest = hardened_hash(b"test-only-password", &[0, 1, 2, 3, 4, 5, 6, 7], &[], 6)?;
        let encoded = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            encoded,
            "68e0a08a44140219b584ea2cd51ac4b522b08feca3613e5101e9167ce8266840"
        );
        Ok(())
    }
}
