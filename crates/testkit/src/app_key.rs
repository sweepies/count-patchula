//! Throwaway GitHub App signing keys for tests, generated with aws-lc-rs,
//! the backend that signs the App's JWTs.
use aws_lc_rs::{
    encoding::AsDer,
    rsa::{KeyPair, KeySize},
    signature::KeyPair as _,
};
use base64::Engine as _;

/// A fresh 2048-bit RSA key in the encodings an App key arrives in.
pub struct AppKey {
    pkcs8: Vec<u8>,
    public: Vec<u8>,
}

impl AppKey {
    pub fn generate() -> Self {
        let key = KeyPair::generate(KeySize::Rsa2048).expect("generate an RSA key");
        Self {
            pkcs8: key.as_der().expect("PKCS#8 encoding").as_ref().to_vec(),
            public: key.public_key().as_ref().to_vec(),
        }
    }

    /// The private key as PKCS#1 PEM (`BEGIN RSA PRIVATE KEY`), the form
    /// GitHub hands out App keys in.
    pub fn pkcs1_pem(&self) -> String {
        pem("RSA PRIVATE KEY", pkcs1(&self.pkcs8))
    }

    /// The private key as PKCS#8 PEM (`BEGIN PRIVATE KEY`).
    pub fn pkcs8_pem(&self) -> String {
        pem("PRIVATE KEY", &self.pkcs8)
    }

    /// The public key as a DER PKCS#1 `RSAPublicKey`.
    pub fn public_der(&self) -> &[u8] {
        &self.public
    }
}

/// The PKCS#1 `RSAPrivateKey` inside a PKCS#8 `PrivateKeyInfo`:
/// `SEQUENCE { INTEGER version, SEQUENCE algorithm, OCTET STRING key, … }`.
fn pkcs1(pkcs8: &[u8]) -> &[u8] {
    let (0x30, info, _) = tlv(pkcs8) else {
        panic!("PKCS#8 is not a SEQUENCE")
    };
    let (0x02, _, rest) = tlv(info) else {
        panic!("PKCS#8 has no version")
    };
    let (0x30, _, rest) = tlv(rest) else {
        panic!("PKCS#8 has no algorithm")
    };
    let (0x04, key, _) = tlv(rest) else {
        panic!("PKCS#8 has no private key")
    };
    key
}

/// One DER element: its tag, its contents and what follows it.
fn tlv(input: &[u8]) -> (u8, &[u8], &[u8]) {
    let (tag, first) = (input[0], input[1]);
    let (length, header) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 0x7f);
        let length = input[2..2 + count]
            .iter()
            .fold(0, |length, byte| (length << 8) | usize::from(*byte));
        (length, 2 + count)
    };
    let (contents, rest) = input[header..].split_at(length);
    (tag, contents, rest)
}

fn pem(label: &str, der: &[u8]) -> String {
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).expect("base64 is ASCII"))
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}
