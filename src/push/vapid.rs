//! VAPID（RFC 8292）：一对 P-256 密钥 + 每条推送一枚 ES256 JWT。
//!
//! 密钥落 `<AGORA_HOME>/push/vapid.json`（0600）：`private` 是 32 字节标量的 base64url，
//! `public` 是 65 字节未压缩点的 base64url。浏览器订阅时要知道公钥（`GET /api/system`
//! 的 `push.vapid_public_key`），服务端每次推送签一枚 ≤ 24 小时的 JWT——
//! `Authorization: vapid t=<jwt>, k=<public>`。

use std::path::{Path, PathBuf};

use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::SecretKey;
use serde::{Deserialize, Serialize};

use super::crypto::{b64decode, b64encode, CryptoError};

/// JWT 有效期：RFC 8292 §2 上限 24 小时。取 12 小时，时钟偏差与跨日都够。
pub const JWT_LIFETIME_SECS: u64 = 12 * 60 * 60;

#[derive(Debug, thiserror::Error)]
pub enum VapidError {
    #[error("读写 VAPID 密钥失败 {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("VAPID 密钥文件 {0} 不是合法 JSON: {1}")]
    Json(PathBuf, serde_json::Error),
    #[error("VAPID 私钥非法: {0}")]
    Key(String),
    #[error("VAPID 公钥与私钥不一致：文件里存的公钥对不上推导值")]
    PublicKeyMismatch,
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VapidFile {
    /// 32 字节标量，base64url 无填充。
    private: String,
    /// 65 字节未压缩点，base64url 无填充。
    public: String,
}

/// 一份可用的 VAPID 身份。
#[derive(Debug, Clone)]
pub struct Vapid {
    public_b64: String,
    signing: SigningKey,
}

impl Vapid {
    /// 私钥字节 → 身份；公钥由私钥推导，与传入的期望值（文件里存的）比对，防止文件被改坏后
    /// 浏览器订到一把用不了的公钥、直到收到推送才发现。
    pub fn from_private(private: &[u8], expected_public: &[u8]) -> Result<Self, VapidError> {
        let signing =
            SigningKey::from_slice(private).map_err(|e| VapidError::Key(e.to_string()))?;
        let public_der = signing
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        if !expected_public.is_empty() && expected_public != public_der {
            return Err(VapidError::PublicKeyMismatch);
        }
        Ok(Vapid {
            public_b64: b64encode(&public_der),
            signing,
        })
    }

    /// 从 base64url 形态的私钥构造（测试与工具用）。
    pub fn from_private_b64(private_b64: &str) -> Result<Self, VapidError> {
        Self::from_private(&b64decode(private_b64)?, &[])
    }

    /// 现取一把新密钥（首次启用推送时）。
    pub fn generate() -> Self {
        let sk = loop {
            let mut buf = [0u8; 32];
            getrandom::fill(&mut buf).expect("系统随机源不可用");
            if let Ok(sk) = SecretKey::from_slice(&buf) {
                break sk;
            }
        };
        let signing = SigningKey::from(&sk);
        let public_der = signing
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        Vapid {
            public_b64: b64encode(&public_der),
            signing,
        }
    }

    /// 浏览器 / 客户端要的公钥（base64url）。
    pub fn public_key(&self) -> &str {
        &self.public_b64
    }

    /// 签一枚发给 `audience`（推送端点的 origin，如 `https://web.push.apple.com`）的 JWT。
    /// `sub` 是联系方式（RFC 8292 要求 mailto 或 https）。
    pub fn jwt(&self, audience: &str, sub: &str, now_unix: u64) -> String {
        let header = serde_json::json!({ "typ": "JWT", "alg": "ES256" });
        let claims = serde_json::json!({
            "aud": audience,
            "exp": now_unix + JWT_LIFETIME_SECS,
            "sub": sub,
        });
        let signing_input = format!(
            "{}.{}",
            b64encode(&serde_json::to_vec(&header).expect("JWT header 可序列化")),
            b64encode(&serde_json::to_vec(&claims).expect("JWT claims 可序列化")),
        );
        let signature: Signature = self.signing.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", b64encode(&signature.to_bytes()))
    }

    /// 验签（自己签的 JWT 对得上自己的公钥）。生产路径不用——测试与排障。
    pub fn verify(&self, jwt: &str) -> bool {
        let parts: Vec<&str> = jwt.split('.').collect();
        if parts.len() != 3 {
            return false;
        }
        let Ok(sig_bytes) = b64decode(parts[2]) else {
            return false;
        };
        let Ok(sig) = Signature::from_slice(&sig_bytes) else {
            return false;
        };
        let verifying = VerifyingKey::from(&self.signing);
        verifying
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig)
            .is_ok()
    }
}

/// 加载或首次生成 `<home>/push/vapid.json`。生成时 0600、目录 0700——私钥能冒充服务端
/// 给所有订阅推消息，权限与 peer token 同级（ADR-003 D3 的落盘规矩）。
pub fn load_or_generate(home: &Path) -> Result<Vapid, VapidError> {
    let dir = home.join("push");
    let path = dir.join("vapid.json");
    if let Ok(raw) = std::fs::read(&path) {
        let file: VapidFile =
            serde_json::from_slice(&raw).map_err(|e| VapidError::Json(path.clone(), e))?;
        return Vapid::from_private(&b64decode(&file.private)?, &b64decode(&file.public)?);
    }
    let vapid = Vapid::generate();
    std::fs::create_dir_all(&dir).map_err(|source| VapidError::Io {
        path: dir.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let file = VapidFile {
        private: b64encode(&vapid.private_bytes()),
        public: vapid.public_b64.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&file).expect("VAPID 文件可序列化");
    write_private(&path, &bytes).map_err(|source| VapidError::Io {
        path: path.clone(),
        source,
    })?;
    tracing::info!(component = "push", "生成 VAPID 密钥对");
    Ok(vapid)
}

/// 私钥字节；只给落盘用。
impl Vapid {
    fn private_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.signing.to_bytes());
        out
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip_and_verify_their_jwt() {
        let v = Vapid::generate();
        let jwt = v.jwt(
            "https://web.push.apple.com",
            "mailto:agora@localhost",
            1_700_000_000,
        );
        assert!(v.verify(&jwt), "自己签的 JWT 必须能被自己验过");
        // 改负载（换掉 aud）签名就不再匹配；JWT 段是 base64，得真正重建那一段而不是替换明文。
        let parts: Vec<&str> = jwt.split('.').collect();
        let mut claims: serde_json::Value =
            serde_json::from_slice(&b64decode(parts[1]).unwrap()).unwrap();
        claims["aud"] = serde_json::json!("https://evil.example");
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            b64encode(&serde_json::to_vec(&claims).unwrap()),
            parts[2]
        );
        assert!(!v.verify(&tampered), "改过负载就必须验不过");
        // 改签名同样验不过。
        let mut sig = parts[2].to_owned();
        sig.push('A');
        assert!(!v.verify(&format!("{}.{}.{}", parts[0], parts[1], sig)));
    }

    #[test]
    fn file_round_trips_and_public_must_match_private() {
        let dir = std::env::temp_dir().join(format!("agora-push-vapid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = load_or_generate(&dir).unwrap();
        let b = load_or_generate(&dir).unwrap();
        assert_eq!(a.public_key(), b.public_key(), "第二次加载该复用同一把");
        // 写坏 public 字段：加载必须报不一致，而不是静默用推导值。
        let path = dir.join("push/vapid.json");
        let mut file: VapidFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        file.public = b64encode(&[4u8; 65]);
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert!(matches!(
            load_or_generate(&dir),
            Err(VapidError::PublicKeyMismatch)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
