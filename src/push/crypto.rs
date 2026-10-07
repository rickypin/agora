//! Web Push 的密码学原语：RFC 8291 的 `aes128gcm` 内容加密与 RFC 8292 的 VAPID
//! （ES256 JWT）。两个都在这里、不引第三方 webpush crate：整条链只要 p256 / aes-gcm /
//! hkdf / sha2，与本仓"只用 RustCrypto + ring，避开 aws-lc / openssl"的交叉编译约束一致
//! （Cargo.toml 顶部；agora-thc.6）。
//!
//! 附录 A 的测试向量在 `tests/push.rs::rfc8291_appendix_a_vector`：加密（用固定的临时私钥与
//! salt）必须逐字节等于 RFC 的 body，解密 RFC 的 body 必须得到那句 watermelon——自己加自己解
//! 只能证明自洽，向量才能证明用的是对的字节序、对的信息串。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes128Gcm;
use hkdf::Hkdf;
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// RFC 8188 的 `rs`：单条记录上限。推送的 payload 很小，永远一条记录。
pub const RECORD_SIZE: u32 = 4096;

/// 最后一条记录的定界符（RFC 8188 §2）；恰好一条记录时就是它。
const LAST_RECORD: u8 = 0x02;

const KEY_INFO_PREFIX: &[u8] = b"WebPush: info\0";
const CEK_INFO: &[u8] = b"Content-Encoding: aes128gcm\0";
const NONCE_INFO: &[u8] = b"Content-Encoding: nonce\0";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("base64url 解码失败: {0}")]
    Base64(String),
    #[error("P-256 公钥非法（要 65 字节未压缩点）: {0}")]
    PublicKey(String),
    #[error("P-256 私钥非法: {0}")]
    PrivateKey(String),
    #[error("HKDF 输出超界")]
    Hkdf,
    #[error("AEAD 加解密失败: {0}")]
    Aead(String),
    #[error("密文太短：{0} 字节")]
    Truncated(usize),
    #[error("密文头非法: {0}")]
    Header(String),
}

pub fn b64decode(s: &str) -> Result<Vec<u8>, CryptoError> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim())
        .map_err(|e| CryptoError::Base64(e.to_string()))
}

pub fn b64encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 随机 32 字节标量（P-256 的私钥）。取样失败宁可 panic 也不发弱密钥——与 `auth::random_token`
/// 同一条规矩。拒绝零与 ≥ 阶的值（概率 ~2⁻³²，重试即可）。
fn random_secret_key() -> SecretKey {
    loop {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("系统随机源不可用");
        if let Ok(sk) = SecretKey::from_slice(&buf) {
            return sk;
        }
    }
}

fn random_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("系统随机源不可用");
    salt
}

/// 用订阅里的 `p256dh` / `auth`（base64url）加密一条推送载荷，产出 RFC 8188 的完整 body
/// （salt ‖ rs ‖ idlen ‖ as_public ‖ 密文）。临时私钥与 salt 每次现取：同一订阅的两条推送
/// 不该长得像。
pub fn encrypt(
    ua_public_b64: &str,
    auth_secret_b64: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let ua_public = b64decode(ua_public_b64)?;
    let auth = b64decode(auth_secret_b64)?;
    let sk = random_secret_key();
    let mut private = [0u8; 32];
    private.copy_from_slice(&sk.to_bytes());
    encrypt_with(&private, &random_salt(), &ua_public, &auth, plaintext)
}

/// [`encrypt`] 的确定性版本：固定临时私钥与 salt。只该给测试与向量用——线上每次现取。
pub fn encrypt_with(
    as_private: &[u8; 32],
    salt: &[u8; 16],
    ua_public: &[u8],
    auth_secret: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let sk =
        SecretKey::from_slice(as_private).map_err(|e| CryptoError::PrivateKey(e.to_string()))?;
    let ua =
        PublicKey::from_sec1_bytes(ua_public).map_err(|e| CryptoError::PublicKey(e.to_string()))?;
    let as_public = sk.public_key().to_encoded_point(false);
    let ua_encoded = ua.to_encoded_point(false);
    let (cek, nonce) = derive(
        &sk,
        &ua,
        ua_encoded.as_bytes(),
        as_public.as_bytes(),
        salt,
        auth_secret,
    )?;
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|e| CryptoError::Aead(e.to_string()))?;
    let mut record = plaintext.to_vec();
    record.push(LAST_RECORD);
    let ciphertext = cipher
        .encrypt(
            &nonce.into(),
            Payload {
                msg: &record,
                aad: &[],
            },
        )
        .map_err(|e| CryptoError::Aead(e.to_string()))?;

    let mut body = Vec::with_capacity(16 + 4 + 1 + as_public.len() + ciphertext.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&ciphertext);
    Ok(body)
}

/// RFC 8291 §3.4 的密钥推导：ECDH → HKDF(auth_secret, "WebPush: info") → HKDF(salt, cek/nonce)。
///
/// 视角无关：`peer_public` 是另一方的公钥（加密时是订阅者、解密时是服务端临时公钥），
/// `receiver_public` / `sender_public` 只用来拼信息串——RFC 的 `key_info` 固定是
/// `"WebPush: info" || 0x00 || ua_public || as_public`，两个方向的顺序不能跟着视角换。
fn derive(
    private: &SecretKey,
    peer_public: &PublicKey,
    receiver_public: &[u8],
    sender_public: &[u8],
    salt: &[u8; 16],
    auth_secret: &[u8],
) -> Result<([u8; 16], [u8; 12]), CryptoError> {
    let shared = diffie_hellman(private.to_nonzero_scalar(), peer_public.as_affine());

    // RFC 8291 §3.3：auth secret 作 salt 先把 ECDH 秘密压成 IKM，信息串里两个公钥都要。
    let mut info =
        Vec::with_capacity(KEY_INFO_PREFIX.len() + receiver_public.len() + sender_public.len());
    info.extend_from_slice(KEY_INFO_PREFIX);
    info.extend_from_slice(receiver_public);
    info.extend_from_slice(sender_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth_secret), shared.raw_secret_bytes())
        .expand(&info, &mut ikm)
        .map_err(|_| CryptoError::Hkdf)?;

    let (_, hkdf) = Hkdf::<Sha256>::extract(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    hkdf.expand(CEK_INFO, &mut cek)
        .map_err(|_| CryptoError::Hkdf)?;
    let mut nonce = [0u8; 12];
    hkdf.expand(NONCE_INFO, &mut nonce)
        .map_err(|_| CryptoError::Hkdf)?;
    Ok((cek, nonce))
}

/// 解密一条 RFC 8188 `aes128gcm` body（只支持单条记录）。生产路径不需要它——它在测试里
/// 扮演"浏览器"：拿 ua_private 验证服务端真的按规范加密了；也供排障时人工核对。
pub fn decrypt(
    body: &[u8],
    ua_private: &[u8; 32],
    auth_secret: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if body.len() < 16 + 4 + 1 {
        return Err(CryptoError::Truncated(body.len()));
    }
    let (salt, rest) = body.split_at(16);
    let mut salt16 = [0u8; 16];
    salt16.copy_from_slice(salt);
    let (_rs, rest) = rest.split_at(4);
    let idlen = rest[0] as usize;
    if idlen == 0 || rest.len() < 1 + idlen {
        return Err(CryptoError::Header(format!("idlen={idlen}")));
    }
    let as_public = &rest[1..1 + idlen];
    let ciphertext = &rest[1 + idlen..];
    let sk =
        SecretKey::from_slice(ua_private).map_err(|e| CryptoError::PrivateKey(e.to_string()))?;
    let as_key =
        PublicKey::from_sec1_bytes(as_public).map_err(|e| CryptoError::PublicKey(e.to_string()))?;
    let ua_encoded = sk.public_key().to_encoded_point(false);
    let (cek, nonce) = derive(
        &sk,
        &as_key,
        ua_encoded.as_bytes(),
        as_public,
        &salt16,
        auth_secret,
    )?;
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|e| CryptoError::Aead(e.to_string()))?;
    let record = cipher
        .decrypt(
            &nonce.into(),
            Payload {
                msg: ciphertext,
                aad: &[],
            },
        )
        .map_err(|e| CryptoError::Aead(e.to_string()))?;
    // 去掉 RFC 8188 的定界符；单条记录时最后一个字节必是 0x02（乱序 / 截断会在这里露出来）。
    match record.split_last() {
        Some((&LAST_RECORD, body)) => Ok(body.to_vec()),
        _ => Err(CryptoError::Header("缺少最后一条记录定界符".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_round_trip() {
        let bytes = [0u8, 1, 2, 250, 255];
        assert_eq!(b64decode(&b64encode(&bytes)).unwrap(), bytes);
        assert!(b64decode("not*base64").is_err());
    }
}
