//! m115 crypto: the request/response cipher used by 115 Netdisk's download
//! APIs. Transplanted byte-for-byte from SheltonZhu/115driver
//! `pkg/crypto/m115` (public.go / rsa.go / xor.go / util.go, 194 lines of Go).
//!
//! Layout of an encoded payload:
//!   buf = key(16B) || plaintext
//!   xorTransform(buf[16:], xorDeriveKey(key, 4))
//!   reverseBytes(buf[16:])
//!   xorTransform(buf[16:], xorClientKey)
//!   base64(rsaEncrypt(buf))            // fixed 1024-bit public key, custom padding
//!
//! Decoding is the exact inverse with a second derived key (size 12).

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use num_bigint::BigUint;
use rand::RngCore;

pub type M115Key = [u8; 16];

const XOR_KEY_SEED: [u8; 144] = [
    0xf0, 0xe5, 0x69, 0xae, 0xbf, 0xdc, 0xbf, 0x8a, 0x1a, 0x45, 0xe8, 0xbe, 0x7d, 0xa6, 0x73,
    0xb8, 0xde, 0x8f, 0xe7, 0xc4, 0x45, 0xda, 0x86, 0xc4, 0x9b, 0x64, 0x8b, 0x14, 0x6a, 0xb4,
    0xf1, 0xaa, 0x38, 0x01, 0x35, 0x9e, 0x26, 0x69, 0x2c, 0x86, 0x00, 0x6b, 0x4f, 0xa5, 0x36,
    0x34, 0x62, 0xa6, 0x2a, 0x96, 0x68, 0x18, 0xf2, 0x4a, 0xfd, 0xbd, 0x6b, 0x97, 0x8f, 0x4d,
    0x8f, 0x89, 0x13, 0xb7, 0x6c, 0x8e, 0x93, 0xed, 0x0e, 0x0d, 0x48, 0x3e, 0xd7, 0x2f, 0x88,
    0xd8, 0xfe, 0xfe, 0x7e, 0x86, 0x50, 0x95, 0x4f, 0xd1, 0xeb, 0x83, 0x26, 0x34, 0xdb, 0x66,
    0x7b, 0x9c, 0x7e, 0x9d, 0x7a, 0x81, 0x32, 0xea, 0xb6, 0x33, 0xde, 0x3a, 0xa9, 0x59, 0x34,
    0x66, 0x3b, 0xaa, 0xba, 0x81, 0x60, 0x48, 0xb9, 0xd5, 0x81, 0x9c, 0xf8, 0x6c, 0x84, 0x77,
    0xff, 0x54, 0x78, 0x26, 0x5f, 0xbe, 0xe8, 0x1e, 0x36, 0x9f, 0x34, 0x80, 0x5c, 0x45, 0x2c,
    0x9b, 0x76, 0xd5, 0x1b, 0x8f, 0xcc, 0xc3, 0xb8, 0xf5,
];

const XOR_CLIENT_KEY: [u8; 12] = [
    0x78, 0x06, 0xad, 0x4c, 0x33, 0x86, 0x5d, 0x18, 0x4c, 0x01, 0x3f, 0x46,
];

/// 512-bit RSA modulus (hex) hard-coded by the 115 client.
const RSA_N_HEX: &str = "8686980c0f5a24c4b9d43020cd2c22703ff3f450756529058b1cf88f09b86021\
36477198a6e2683149659bd122c33592fdb5ad47944ad1ea4d36c6b172aad633\
8c3bb6ac6227502d010993ac967d1aef00f0c8e038de2e4d3bc2ec368af2e9f1\
0a6f1eda4f7262f136420c07c331b871bf139f74f3010e3c4fe57df3afb71683";
const RSA_E: u64 = 0x10001;

/// Lazily-parsed 1024-bit RSA modulus.
fn rsa_n() -> &'static BigUint {
    use std::sync::OnceLock;
    static N: OnceLock<BigUint> = OnceLock::new();
    N.get_or_init(|| BigUint::parse_bytes(RSA_N_HEX.as_bytes(), 16).expect("115 RSA N"))
}

fn rsa_key_length() -> usize {
    rsa_n().bits() as usize / 8
}

fn xor_derive_key(seed: &[u8], size: usize) -> Vec<u8> {
    let mut key = vec![0u8; size];
    for i in 0..size {
        key[i] = (seed[i].wrapping_add(XOR_KEY_SEED[size * i])) ^ XOR_KEY_SEED[size * (size - i - 1)];
    }
    key
}

fn xor_transform(data: &mut [u8], key: &[u8]) {
    let (data_size, key_size) = (data.len(), key.len());
    let mod_ = data_size % 4;
    if mod_ > 0 {
        for i in 0..mod_ {
            data[i] ^= key[i % key_size];
        }
    }
    for i in mod_..data_size {
        data[i] ^= key[(i - mod_) % key_size];
    }
}

fn reverse_bytes(data: &mut [u8]) {
    data.reverse();
}

fn rsa_encrypt(input: &[u8]) -> Vec<u8> {
    let key_len = rsa_key_length();
    let mut out = Vec::with_capacity(input.len() + input.len() / (key_len - 11) + 1);
    let mut remain = input;
    while !remain.is_empty() {
        let slice_size = (key_len - 11).min(remain.len());
        rsa_encrypt_slice(&remain[..slice_size], &mut out, key_len);
        remain = &remain[slice_size..];
    }
    out
}

fn rsa_encrypt_slice(input: &[u8], out: &mut Vec<u8>, key_len: usize) {
    // Custom PKCS#1 v1.5-style padding: 0x00 0x02 <random non-zero> 0x00 <data>.
    let pad_size = key_len - input.len() - 3;
    let mut buf = vec![0u8; key_len];
    buf[0] = 0;
    buf[1] = 2;
    let mut pad = vec![0u8; pad_size];
    rand::thread_rng().fill_bytes(&mut pad);
    for (i, b) in pad.iter().enumerate() {
        buf[2 + i] = b % 0xff + 0x01;
    }
    buf[pad_size + 2] = 0;
    buf[pad_size + 3..].copy_from_slice(input);

    let msg = BigUint::from_bytes_be(&buf);
    let e = BigUint::from(RSA_E);
    let ret = msg.modpow(&e, rsa_n()).to_bytes_be();
    out.extend(std::iter::repeat(0u8).take(key_len.saturating_sub(ret.len())));
    out.extend_from_slice(&ret);
}

fn rsa_decrypt(input: &[u8]) -> Vec<u8> {
    let key_len = rsa_key_length();
    let mut out = Vec::with_capacity(input.len());
    let mut remain = input;
    while !remain.is_empty() {
        let slice_size = key_len.min(remain.len());
        rsa_decrypt_slice(&remain[..slice_size], &mut out);
        remain = &remain[slice_size..];
    }
    out
}

fn rsa_decrypt_slice(input: &[u8], out: &mut Vec<u8>) {
    let msg = BigUint::from_bytes_be(input);
    let e = BigUint::from(RSA_E);
    let ret = msg.modpow(&e, rsa_n()).to_bytes_be();
    // Find the first 0x00 (past index 0): plaintext follows it.
    for (i, b) in ret.iter().enumerate() {
        if *b == 0 && i != 0 {
            out.extend_from_slice(&ret[i + 1..]);
            break;
        }
    }
}

/// Generate a fresh random session key.
pub fn generate_key() -> M115Key {
    let mut key = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key);
    key
}

/// Encode a plaintext payload for the 115 API.
pub fn encode(input: &[u8], key: &M115Key) -> String {
    let mut buf = Vec::with_capacity(16 + input.len());
    buf.extend_from_slice(key);
    buf.extend_from_slice(input);

    let derived = xor_derive_key(key, 4);
    xor_transform(&mut buf[16..], &derived);
    reverse_bytes(&mut buf[16..]);
    xor_transform(&mut buf[16..], &XOR_CLIENT_KEY);

    BASE64.encode(rsa_encrypt(&buf))
}

/// Decode a base64 m115 response back into plaintext.
pub fn decode(input: &str, key: &M115Key) -> Result<Vec<u8>, String> {
    let data = BASE64.decode(input).map_err(|e| format!("base64 解码失败: {e}"))?;
    let data = rsa_decrypt(&data);
    let mut out = data[16..].to_vec();

    let derived12 = xor_derive_key(&data[..16], 12);
    xor_transform(&mut out, &derived12);
    reverse_bytes(&mut out);
    let derived4 = xor_derive_key(key, 4);
    xor_transform(&mut out, &derived4);
    Ok(out)
}
