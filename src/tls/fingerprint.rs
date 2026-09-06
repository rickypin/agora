//! 证书指纹 = 证书里 SubjectPublicKeyInfo（DER 整段）的 SHA-256，文本形态 `sha256:<64 位小写十六进制>`
//! （ADR-003 D4）。钉 SPKI 而不是整张证书，是让"换证书不换密钥"（续期）不打断 peer；
//! 这也是 HPKP / `openssl x509 -pubkey | openssl pkey -pubin -outform der | sha256sum` 的约定，
//! 用户能拿别的工具核对。

use std::fmt;
use std::str::FromStr;

use sha2::{Digest, Sha256};

use super::x509;

pub const PREFIX: &str = "sha256:";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum FingerprintError {
    /// 配置里写的不是 `sha256:<64 位十六进制>`——包括空串。没有指纹就没有信任锚（无 TOFU）。
    #[error("指纹必须是 sha256:<64 位十六进制>，得到 {0:?}")]
    Format(String),
    #[error("证书不是合法的 X.509 DER: {0}")]
    Cert(#[from] x509::X509Error),
}

impl Fingerprint {
    pub fn of_spki(spki_der: &[u8]) -> Self {
        Fingerprint(Sha256::digest(spki_der).into())
    }

    pub fn of_cert(cert_der: &[u8]) -> Result<Self, FingerprintError> {
        Ok(Self::of_spki(x509::spki(cert_der)?))
    }

    /// 解析配置里的写法。十六进制大小写都收（人会从别的工具粘过来），前后空白忽略。
    pub fn parse(s: &str) -> Result<Self, FingerprintError> {
        let bad = || FingerprintError::Format(s.to_owned());
        let hex = s.trim().strip_prefix(PREFIX).ok_or_else(bad)?;
        if hex.len() != 64 {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
            let hi = (pair[0] as char).to_digit(16).ok_or_else(bad)?;
            let lo = (pair[1] as char).to_digit(16).ok_or_else(bad)?;
            out[i] = (hi as u8) << 4 | lo as u8;
        }
        Ok(Fingerprint(out))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(PREFIX)?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Fingerprint {
    type Err = FingerprintError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Fingerprint::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_form_round_trips_and_accepts_uppercase() {
        let fp = Fingerprint::of_spki(b"spki");
        let text = fp.to_string();
        assert!(
            text.starts_with("sha256:") && text.len() == 7 + 64,
            "{text}"
        );
        assert_eq!(Fingerprint::parse(&text).unwrap(), fp);
        // 十六进制部分大写（openssl 的输出）也收；前缀本身固定小写。
        let upper_hex = format!("{PREFIX}{}", text[PREFIX.len()..].to_uppercase());
        assert_eq!(Fingerprint::parse(&upper_hex).unwrap(), fp);
        assert_eq!(format!(" {text} ").parse::<Fingerprint>().unwrap(), fp);
    }

    #[test]
    fn empty_short_and_unprefixed_are_format_errors() {
        for bad in ["", "sha256:", "sha256:00", "sha1:00", &"0".repeat(64)] {
            assert!(
                matches!(Fingerprint::parse(bad), Err(FingerprintError::Format(_))),
                "{bad:?}"
            );
        }
        let odd = format!("sha256:{}zz", "0".repeat(62));
        assert!(matches!(
            Fingerprint::parse(&odd),
            Err(FingerprintError::Format(_))
        ));
        assert!(matches!(
            Fingerprint::of_cert(b"not a cert"),
            Err(FingerprintError::Cert(_))
        ));
    }
}
