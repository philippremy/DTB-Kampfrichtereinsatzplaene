//! Report transport — builds a crash report and sends it by e-mail.
//!
//! The SMTP credentials are **not** in the source or the binary as clear text:
//! `build.rs` reads them from `DTB_KE_SMTP_*` (maintainer build-host / CI
//! secrets only), encrypts them with a fresh random ChaCha20 key + nonce, and
//! emits ciphertext + an *obfuscated* key into `$OUT_DIR/smtp_secret.rs`. This
//! is obfuscation, not protection (a debugger on the running binary or a MITM of
//! the SMTP session still recovers them) — the real safety net is the dedicated
//! send-only mailbox + trivial rotation (rebuild with new env values).
//!
//! When the credentials were not configured at build time, [`available`] is
//! `false` and every [`send`] fails with [`MailError::NotConfigured`].

use chacha20::cipher::{KeyIvInit, StreamCipher};

mod secret {
    include!(concat!(env!("OUT_DIR"), "/smtp_secret.rs"));
}

/// The per-word offsets / mask `build.rs` applied to the key and nonce — public
/// on both sides, just an extra reversal step so the key does not sit in
/// `.rodata` as a bare 32-byte high-entropy run. **Keep identical to `build.rs`.**
const KEY_SALTS: [u64; 4] = [
    0x9E37_79B9_7F4A_7C15,
    0xC2B2_AE3D_27D4_EB4F,
    0x1656_67B1_9E37_79F9,
    0xF58C_4C24_D442_1D1D,
];
const NONCE_MASK: [u8; 12] = [
    0x5A, 0xC3, 0x1F, 0x88, 0x24, 0x9D, 0x71, 0xE6, 0x4B, 0x0A, 0xB2, 0x3C,
];

/// Whether report transmission is possible in this build.
pub fn available() -> bool {
    !secret::CIPHERTEXT.is_empty()
}

struct SmtpConfig {
    host: String,
    port: u16,
    user: String,
    pass: String,
    from: String,
    to: String,
}

/// Decrypt the embedded credentials. `None` when unconfigured or the blob is
/// corrupt.
fn config() -> Option<SmtpConfig> {
    if secret::CIPHERTEXT.is_empty() {
        return None;
    }

    let mut key = [0u8; 32];
    for (i, word) in secret::KEY_WORDS.iter().enumerate() {
        let plain = word.wrapping_sub(KEY_SALTS[i]).to_le_bytes();
        key[i * 8..i * 8 + 8].copy_from_slice(&plain);
    }
    let mut nonce = [0u8; 12];
    for i in 0..12 {
        nonce[i] = secret::NONCE_MASKED[i] ^ NONCE_MASK[i];
    }

    let mut buf = secret::CIPHERTEXT.to_vec();
    chacha20::ChaCha20::new((&key).into(), (&nonce).into()).apply_keystream(&mut buf);
    key.fill(0);

    let rest = buf.strip_prefix(b"DTBKEM01")?;
    let text = std::str::from_utf8(rest).ok()?;
    let mut fields = text.split('\n');
    let cfg = SmtpConfig {
        host: fields.next()?.to_owned(),
        port: fields.next()?.parse().ok()?,
        user: fields.next()?.to_owned(),
        pass: fields.next()?.to_owned(),
        from: fields.next()?.to_owned(),
        to: fields.next()?.to_owned(),
    };
    (!cfg.host.is_empty() && !cfg.user.is_empty()).then_some(cfg)
}

/// A report ready to send.
pub struct Report {
    pub subject: String,
    /// The plain-text body (crash summary + stack).
    pub body: String,
    /// Files to attach — `(filename, mime, bytes)`. Empty for a body-only mail.
    pub attachments: Vec<(String, &'static str, Vec<u8>)>,
}

#[derive(Debug)]
pub enum MailError {
    /// No credentials were embedded at build time.
    NotConfigured,
    /// The message could not be assembled (bad address, …).
    Build(String),
    /// The SMTP conversation failed (connect / auth / send).
    Send(String),
}

impl std::fmt::Display for MailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MailError::NotConfigured => {
                write!(
                    f,
                    "Der Versand ist in dieser Programmversion nicht verfügbar."
                )
            }
            MailError::Build(e) => write!(f, "Der Bericht konnte nicht erstellt werden: {e}"),
            MailError::Send(e) => write!(f, "Der Bericht konnte nicht gesendet werden: {e}"),
        }
    }
}
impl std::error::Error for MailError {}

/// Build and send `report` over SMTP. **Blocking** (connect + TLS + send) —
/// call it off the main thread.
pub fn send(report: Report) -> Result<(), MailError> {
    use lettre::message::header::ContentType;
    use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart};
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{Message, SmtpTransport, Transport};

    // Test hook: assemble the message but don't actually connect — exercises the
    // reporter's spinner / "Gesendet!" flow without a live SMTP server.
    match std::env::var("DTB_KE_MAIL_FAKE").as_deref() {
        Ok("ok") => {
            std::thread::sleep(std::time::Duration::from_millis(700));
            return Ok(());
        }
        Ok("err") => {
            std::thread::sleep(std::time::Duration::from_millis(700));
            return Err(MailError::Send(
                "simulierter Fehler (DTB_KE_MAIL_FAKE=err)".into(),
            ));
        }
        _ => {}
    }

    let cfg = config().ok_or(MailError::NotConfigured)?;

    let from: Mailbox = cfg
        .from
        .parse()
        .map_err(|e| MailError::Build(format!("Absender {:?}: {e}", cfg.from)))?;
    let to: Mailbox = cfg
        .to
        .parse()
        .map_err(|e| MailError::Build(format!("Empfänger {:?}: {e}", cfg.to)))?;

    let builder = Message::builder().from(from).to(to).subject(report.subject);

    let message = if report.attachments.is_empty() {
        builder.singlepart(SinglePart::plain(report.body))
    } else {
        let mut multi = MultiPart::mixed().singlepart(SinglePart::plain(report.body));
        for (name, mime, bytes) in report.attachments {
            let ct = ContentType::parse(mime).unwrap_or(ContentType::TEXT_PLAIN);
            multi = multi.singlepart(Attachment::new(name).body(bytes, ct));
        }
        builder.multipart(multi)
    }
    .map_err(|e| MailError::Build(e.to_string()))?;

    // 587 → STARTTLS, everything else (465 …) → implicit TLS.
    let relay = if cfg.port == 587 {
        SmtpTransport::starttls_relay(&cfg.host)
    } else {
        SmtpTransport::relay(&cfg.host)
    }
    .map_err(|e| MailError::Send(e.to_string()))?;

    let transport = relay
        .port(cfg.port)
        .credentials(Credentials::new(cfg.user, cfg.pass))
        .build();

    transport
        .send(&message)
        .map(|_| ())
        .map_err(|e| MailError::Send(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The obfuscation + ChaCha20 round-trip `build.rs` writes and `config`
    /// reverses — self-consistent, and the salts/masks must match `build.rs`.
    #[test]
    fn secret_obfuscation_round_trips() {
        let key = [0x2Bu8; 32];
        let nonce = [0x71u8; 12];
        let fields = [
            "mail.example",
            "465",
            "u@example",
            "p4$$w0rd",
            "f@example",
            "t@example",
        ];

        // ── build.rs side ──
        let mut blob = Vec::from(*b"DTBKEM01");
        blob.extend_from_slice(fields.join("\n").as_bytes());
        chacha20::ChaCha20::new((&key).into(), (&nonce).into()).apply_keystream(&mut blob);
        let key_words: [u64; 4] = std::array::from_fn(|i| {
            u64::from_le_bytes(key[i * 8..i * 8 + 8].try_into().unwrap()).wrapping_add(KEY_SALTS[i])
        });
        let nonce_masked: [u8; 12] = std::array::from_fn(|i| nonce[i] ^ NONCE_MASK[i]);

        // ── mail.rs side (mirrors `config`) ──
        let mut rkey = [0u8; 32];
        for (i, w) in key_words.iter().enumerate() {
            rkey[i * 8..i * 8 + 8].copy_from_slice(&w.wrapping_sub(KEY_SALTS[i]).to_le_bytes());
        }
        let rnonce: [u8; 12] = std::array::from_fn(|i| nonce_masked[i] ^ NONCE_MASK[i]);
        assert_eq!(rkey, key);
        assert_eq!(rnonce, nonce);

        let mut plain = blob;
        chacha20::ChaCha20::new((&rkey).into(), (&rnonce).into()).apply_keystream(&mut plain);
        let text = std::str::from_utf8(plain.strip_prefix(b"DTBKEM01").unwrap()).unwrap();
        assert_eq!(text.split('\n').collect::<Vec<_>>(), fields);
    }

    /// A build without credentials disables transport rather than panicking.
    #[test]
    fn absent_credentials_disable_transport() {
        if secret::CIPHERTEXT.is_empty() {
            assert!(!available());
            assert!(config().is_none());
            assert!(matches!(
                send(Report {
                    subject: String::new(),
                    body: String::new(),
                    attachments: Vec::new(),
                }),
                Err(MailError::NotConfigured)
            ));
        }
    }
}
