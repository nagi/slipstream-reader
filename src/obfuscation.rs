//! Fonts a book has scrambled so they can't simply be copied out of it, put back as they were.
//!
//! `META-INF/encryption.xml` names each such font and how it was scrambled: the start of the file
//! XORed with a key made from the book's identifier. That isn't DRM, and every reading system
//! undoes it; without that the page would get a broken font and fall back to another.
//!
//! - IDPF (`http://www.idpf.org/2008/embedding`): the first 1040 bytes, with the SHA-1 of the
//!   book's unique identifier, whitespace taken out.
//! - Adobe (`http://ns.adobe.com/pdf/enc#RC`): the first 1024 bytes, with the 16 bytes of the
//!   book's `urn:uuid:` identifier.

use std::collections::HashMap;

const IDPF: &str = "http://www.idpf.org/2008/embedding";
const ADOBE: &str = "http://ns.adobe.com/pdf/enc#RC";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Idpf,
    Adobe,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Idpf => "IDPF",
            Scheme::Adobe => "Adobe",
        }
    }
}

/// The book's scrambled files, by their absolute decoded path (`/OEBPS/fonts/a.ttf`), and how to
/// put each back.
#[derive(Debug, Default)]
pub struct Obfuscated {
    files: HashMap<String, Scheme>,
    idpf_key: Option<[u8; 20]>,
    adobe_key: Option<[u8; 16]>,
}

impl Obfuscated {
    /// From `encryption.xml`, with the book's unique identifier and all its identifiers (the Adobe
    /// key is whichever of them is a UUID).
    pub fn read(xml: &str, unique_identifier: Option<&str>, identifiers: &[String]) -> Obfuscated {
        let files = scrambled_files(xml);
        if files.is_empty() {
            return Obfuscated::default();
        }
        let idpf_key = unique_identifier.map(|id| {
            let stripped: String = id
                .chars()
                .filter(|c| !matches!(c, ' ' | '\t' | '\r' | '\n'))
                .collect();
            sha1(stripped.as_bytes())
        });
        let adobe_key = unique_identifier
            .into_iter()
            .chain(identifiers.iter().map(String::as_str))
            .find_map(uuid_bytes);
        Obfuscated {
            files,
            idpf_key,
            adobe_key,
        }
    }

    /// How the file at `path` was scrambled, if it was.
    pub fn scheme(&self, path: &str) -> Option<Scheme> {
        self.files.get(path).copied()
    }

    /// The file at `path` as it was before it was scrambled. Others are left alone.
    pub fn restore(&self, path: &str, bytes: &mut [u8]) {
        let (key, length): (&[u8], usize) = match self.scheme(path) {
            Some(Scheme::Idpf) => match &self.idpf_key {
                Some(key) => (key, 1040),
                None => return,
            },
            Some(Scheme::Adobe) => match &self.adobe_key {
                Some(key) => (key, 1024),
                None => return,
            },
            None => return,
        };
        for (at, byte) in bytes.iter_mut().take(length).enumerate() {
            *byte ^= key[at % key.len()];
        }
    }
}

/// Each `CipherReference` under an obfuscation algorithm this knows, made absolute. Paths in
/// `encryption.xml` are from the root of the book.
fn scrambled_files(xml: &str) -> HashMap<String, Scheme> {
    use quick_xml::events::Event;
    let mut files = HashMap::new();
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut scheme = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag) | Event::Empty(tag)) => {
                let local = tag.local_name();
                let attribute = |name: &[u8]| {
                    tag.attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == name)
                        .and_then(|a| {
                            a.normalized_value(quick_xml::XmlVersion::default())
                                .ok()
                                .map(|v| v.into_owned())
                        })
                };
                match local.as_ref() {
                    b"EncryptedData" => scheme = None,
                    b"EncryptionMethod" => {
                        scheme = match attribute(b"Algorithm").as_deref() {
                            Some(IDPF) => Some(Scheme::Idpf),
                            Some(ADOBE) => Some(Scheme::Adobe),
                            _ => None,
                        }
                    }
                    b"CipherReference" => {
                        if let (Some(scheme), Some(uri)) = (scheme, attribute(b"URI")) {
                            let path = crate::book::percent_decoded(uri.trim_start_matches('/'));
                            files.insert(format!("/{path}"), scheme);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    files
}

/// The 16 bytes of a UUID, as `urn:uuid:…` or bare.
fn uuid_bytes(id: &str) -> Option<[u8; 16]> {
    let id = id.trim();
    let id = id
        .strip_prefix("urn:uuid:")
        .or_else(|| id.strip_prefix("uuid:"))
        .unwrap_or(id);
    let hex: String = id.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (at, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(at * 2..at * 2 + 2)?, 16).ok()?;
    }
    Some(bytes)
}

/// SHA-1, only for the IDPF key: a short identifier, once per book.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(value);
        }
    }
    let mut out = [0u8; 20];
    for (at, word) in h.iter().enumerate() {
        out[at * 4..at * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha1_matches_the_known_answers() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(hex(&sha1(long)), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    }

    const XML: &str = r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container"
        xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
      <enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://ns.adobe.com/pdf/enc#RC"/>
        <enc:CipherData><enc:CipherReference URI="fonts/00001.ttf"/></enc:CipherData>
      </enc:EncryptedData>
      <enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
        <enc:CipherData><enc:CipherReference URI="OEBPS/Fonts/My%20Font.otf"/></enc:CipherData>
      </enc:EncryptedData>
      <enc:EncryptedData>
        <enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
        <enc:CipherData><enc:CipherReference URI="OEBPS/locked.xhtml"/></enc:CipherData>
      </enc:EncryptedData>
    </encryption>"#;

    #[test]
    fn the_scrambled_fonts_are_found_by_their_paths() {
        let found = Obfuscated::read(XML, Some("x"), &[]);
        assert_eq!(found.scheme("/fonts/00001.ttf"), Some(Scheme::Adobe));
        assert_eq!(found.scheme("/OEBPS/Fonts/My Font.otf"), Some(Scheme::Idpf));
        // Real encryption is DRM, and left alone.
        assert_eq!(found.scheme("/OEBPS/locked.xhtml"), None);
    }

    #[test]
    fn scrambling_twice_gives_the_font_back() {
        let id = "urn:uuid:0f6a3d2c-9a7b-4c1e-8f00-123456789abc";
        let found = Obfuscated::read(XML, Some(id), &[]);
        let font: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        for path in ["/fonts/00001.ttf", "/OEBPS/Fonts/My Font.otf"] {
            let mut bytes = font.clone();
            found.restore(path, &mut bytes);
            assert_ne!(bytes, font);
            assert_eq!(bytes[1040..], font[1040..]);
            found.restore(path, &mut bytes);
            assert_eq!(bytes, font);
        }
    }

    #[test]
    fn the_adobe_key_is_the_uuid() {
        let key = uuid_bytes("urn:uuid:0f6a3d2c-9a7b-4c1e-8f00-123456789abc").unwrap();
        assert_eq!(hex(&key), "0f6a3d2c9a7b4c1e8f00123456789abc");
        assert_eq!(uuid_bytes("isbn:9780000000000"), None);
    }

    #[test]
    fn the_adobe_key_comes_from_any_identifier_that_is_a_uuid() {
        let ids = vec!["urn:uuid:0f6a3d2c-9a7b-4c1e-8f00-123456789abc".to_string()];
        let found = Obfuscated::read(XML, Some("isbn:123"), &ids);
        let mut bytes = vec![0u8; 16];
        found.restore("/fonts/00001.ttf", &mut bytes);
        assert_eq!(hex(&bytes), "0f6a3d2c9a7b4c1e8f00123456789abc");
    }
}
