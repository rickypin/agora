//! X.509 证书 DER 的最小解析：只取 SubjectPublicKeyInfo 与 Validity。
//!
//! 不引 x509-parser（连带 der-parser / nom / asn1-rs 一串依赖）：需要的只是沿着 TBSCertificate
//! 的固定字段顺序（RFC 5280 §4.1：[version] serial signature issuer validity subject spki …）
//! 跳过前几项，几十行 TLV 读取就够。对面要么是 rcgen 生成的自签证书、要么是用户从 CA 拿来的
//! 真证书，两者都是合法 DER；守卫测试拿 rcgen 的 `subject_public_key_info()` 与设定的有效期对照。

use crate::clock::parse_utc_secs;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum X509Error {
    /// 剩 `0` 字节时还想读：DER 被截断或长度字段不合法。
    #[error("DER 截断或长度不合法（剩余 {0} 字节）")]
    Truncated(usize),
    #[error("期望 tag {expected:#04x}，得到 {actual:#04x}")]
    Tag { expected: u8, actual: u8 },
    #[error("时间字段 {0:?} 不是 UTCTime / GeneralizedTime")]
    Time(String),
}

const SEQUENCE: u8 = 0x30;
const INTEGER: u8 = 0x02;
/// tbsCertificate 里 EXPLICIT [0] 的 version；v1 证书没有这一项。
const CONTEXT_0: u8 = 0xA0;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;

struct Tlv<'a> {
    tag: u8,
    content: &'a [u8],
    /// tag + length + content 整段：SPKI 的指纹算的是整个 TLV 而不只是内容。
    whole: &'a [u8],
}

fn tlv(input: &[u8]) -> Result<(Tlv<'_>, &[u8]), X509Error> {
    let trunc = || X509Error::Truncated(input.len());
    let (&tag, rest) = input.split_first().ok_or_else(trunc)?;
    let (&first, rest) = rest.split_first().ok_or_else(trunc)?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return Err(trunc());
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | *b as usize);
        (len, &rest[n..])
    };
    if rest.len() < len {
        return Err(trunc());
    }
    let (content, after) = rest.split_at(len);
    let header = input.len() - rest.len();
    Ok((
        Tlv {
            tag,
            content,
            whole: &input[..header + len],
        },
        after,
    ))
}

fn expect(input: &[u8], tag: u8) -> Result<(Tlv<'_>, &[u8]), X509Error> {
    let (t, rest) = tlv(input)?;
    if t.tag != tag {
        return Err(X509Error::Tag {
            expected: tag,
            actual: t.tag,
        });
    }
    Ok((t, rest))
}

struct Tbs<'a> {
    validity: &'a [u8],
    spki: &'a [u8],
}

fn tbs(cert: &[u8]) -> Result<Tbs<'_>, X509Error> {
    let (cert_seq, _) = expect(cert, SEQUENCE)?;
    let (tbs, _) = expect(cert_seq.content, SEQUENCE)?;
    let mut rest = tbs.content;
    if rest.first() == Some(&CONTEXT_0) {
        rest = tlv(rest)?.1;
    }
    rest = expect(rest, INTEGER)?.1; // serialNumber
    rest = expect(rest, SEQUENCE)?.1; // signature AlgorithmIdentifier
    rest = expect(rest, SEQUENCE)?.1; // issuer Name
    let (validity, rest) = expect(rest, SEQUENCE)?;
    let rest = expect(rest, SEQUENCE)?.1; // subject Name
    let (spki, _) = expect(rest, SEQUENCE)?;
    Ok(Tbs {
        validity: validity.content,
        spki: spki.whole,
    })
}

/// 证书里 SubjectPublicKeyInfo 的整段 DER（tag + length + content）。指纹算的就是它的 SHA-256。
pub fn spki(cert_der: &[u8]) -> Result<&[u8], X509Error> {
    Ok(tbs(cert_der)?.spki)
}

/// `(notBefore, notAfter)`，unix 秒。
pub fn validity(cert_der: &[u8]) -> Result<(i64, i64), X509Error> {
    let t = tbs(cert_der)?;
    let (not_before, rest) = tlv(t.validity)?;
    let (not_after, _) = tlv(rest)?;
    Ok((time(&not_before)?, time(&not_after)?))
}

/// UTCTime `YYMMDDHHMMSSZ`（YY < 50 → 20YY，否则 19YY；RFC 5280 §4.1.2.5.1）或
/// GeneralizedTime `YYYYMMDDHHMMSSZ`。证书里两者都必须以 Z 结尾、不带小数秒。
fn time(t: &Tlv<'_>) -> Result<i64, X509Error> {
    let s = std::str::from_utf8(t.content)
        .map_err(|_| X509Error::Time(format!("{:02x?}", t.content)))?;
    let bad = || X509Error::Time(s.to_owned());
    let digits = |a: usize, b: usize| -> Result<i64, X509Error> {
        s.get(a..b)
            .filter(|d| d.bytes().all(|c| c.is_ascii_digit()))
            .and_then(|d| d.parse::<i64>().ok())
            .ok_or_else(bad)
    };
    let (year, off) = match t.tag {
        UTC_TIME => {
            let yy = digits(0, 2)?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, 2)
        }
        GENERALIZED_TIME => (digits(0, 4)?, 4),
        _ => return Err(bad()),
    };
    if s.len() != off + 11 || !s.ends_with('Z') {
        return Err(bad());
    }
    let (mo, d, h, mi, sec) = (
        digits(off, off + 2)?,
        digits(off + 2, off + 4)?,
        digits(off + 4, off + 6)?,
        digits(off + 6, off + 8)?,
        digits(off + 8, off + 10)?,
    );
    parse_utc_secs(&format!(
        "{year:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{sec:02}Z"
    ))
    .ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertificateParams, KeyPair, PublicKeyData};

    fn cert(not_before: (i32, u8, u8), not_after: (i32, u8, u8)) -> (Vec<u8>, KeyPair) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.not_before = rcgen::date_time_ymd(not_before.0, not_before.1, not_before.2);
        params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
        let c = params.self_signed(&key).unwrap();
        (c.der().to_vec(), key)
    }

    #[test]
    fn spki_matches_the_generator_and_validity_reads_both_time_forms() {
        // 2049 以前 rcgen 写 UTCTime，2050 起写 GeneralizedTime（RFC 5280 §4.1.2.5）：一张证书两种都覆盖。
        let (der, key) = cert((2026, 9, 6), (2060, 1, 2));
        assert_eq!(
            spki(&der).unwrap(),
            key.subject_public_key_info().as_slice()
        );
        let (nb, na) = validity(&der).unwrap();
        assert_eq!(nb, parse_utc_secs("2026-09-06T00:00:00Z").unwrap());
        assert_eq!(na, parse_utc_secs("2060-01-02T00:00:00Z").unwrap());
        // 两个都在 UTCTime 范围内的也对。
        let (der, _) = cert((2026, 9, 6), (2036, 9, 6));
        assert_eq!(
            validity(&der).unwrap().1,
            parse_utc_secs("2036-09-06T00:00:00Z").unwrap()
        );
    }

    #[test]
    fn garbage_and_truncation_are_typed_errors_not_panics() {
        let (der, _) = cert((2026, 9, 6), (2036, 9, 6));
        for cut in [0, 1, 2, 5, 40, der.len() / 2, der.len() - 1] {
            let err = spki(&der[..cut]).unwrap_err();
            assert!(
                matches!(err, X509Error::Truncated(_) | X509Error::Tag { .. }),
                "cut={cut}: {err:?}"
            );
        }
        assert!(matches!(
            spki(&[0x02, 0x01, 0x00]),
            Err(X509Error::Tag { .. })
        ));
        // 长度字段声称 5 字节长度、只给 4：截断，不越界读。
        assert!(matches!(
            spki(&[0x30, 0x85, 0, 0, 0, 0]),
            Err(X509Error::Truncated(_))
        ));
    }

    #[test]
    fn time_rejects_non_utc_and_short_forms() {
        let t = |tag: u8, s: &'static str| {
            let bytes = s.as_bytes();
            time(&Tlv {
                tag,
                content: bytes,
                whole: bytes,
            })
        };
        assert!(t(UTC_TIME, "260906000000+0800").is_err());
        assert!(t(UTC_TIME, "2609060000Z").is_err());
        assert!(t(GENERALIZED_TIME, "20260906000000.5Z").is_err());
        assert!(t(INTEGER, "260906000000Z").is_err());
        assert_eq!(
            t(UTC_TIME, "990906000000Z").unwrap(),
            parse_utc_secs("1999-09-06T00:00:00Z").unwrap()
        );
    }
}
