//! TLS：证书来源、SPKI 指纹、监听器的 acceptor 与 peer 客户端的指纹钉住（ADR-003 D4 / D5；
//! agora-7ku.10）。
//!
//! 信任模型只有一个数：对端证书 SubjectPublicKeyInfo 的 SHA-256（[`fingerprint`]）。
//! - 服务端（本节点被 peer 访问，[`server`]）：`tls.mode` 决定证书从哪来——`self-signed`（默认）
//!   首次开 TLS 监听器时生成到 `<AGORA_HOME>/tls/`，之后复用；`external` 读用户给的
//!   `cert_file` / `key_file`。两种最后都是一对 PEM 文件（[`TlsFiles`]），文件变化由 [`reload`]
//!   热加载、SPKI 变了警告 peer 需更新指纹、external 到期前调 `renew_command`。
//! - 客户端（本节点作为 peer 客户端，[`client`]）：只比 `peers[].cert_fingerprint`，不看 CA /
//!   主机名 / 有效期，无 TOFU；不匹配是独立的错误类型，绝不并进"离线"。
//!
//! 加密后端 ring：交叉编译到 x86_64 Linux 不需要 cmake（Cargo.toml 的注释）。
//! `self-ca` 未实现（ADR-003 D4：`external` 不可用时才评估，agora-thc.2）。

pub mod client;
pub mod fingerprint;
pub mod reload;
pub mod server;
pub mod x509;

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;

use crate::config::TlsSection;

pub use fingerprint::{Fingerprint, FingerprintError};

/// `<AGORA_HOME>/tls/`：self-signed 模式的证书目录。
pub const DIR: &str = "tls";
pub const CERT_FILE: &str = "cert.pem";
pub const KEY_FILE: &str = "key.pem";
/// 自签证书的有效期（ADR-003 D4："10 年自签证书"）。peer 不看有效期，这个数只影响浏览器直连自签的场景。
pub const SELF_SIGNED_DAYS: u64 = 3650;

#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("tls.mode {0:?} 不支持：只有 self-signed 与 external（self-ca 未实现，ADR-003 D4）")]
    Mode(String),
    #[error("tls.mode = external 需要 tls.external.cert_file 与 key_file 都给出")]
    ExternalIncomplete,
    /// `agora tls rotate-key` 只对 self-signed 有意义：external 的密钥是外面签的，agora 换不了。
    #[error("tls.mode = external 的密钥由外部工具管理（换钥后 peer 自行 `agora tls fingerprint` 取新指纹）")]
    ExternalRotate,
    #[error("读取 {} 失败: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("写入 {} 失败: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} 不是 PEM 证书 / 私钥: {source}", path.display())]
    Pem {
        path: PathBuf,
        #[source]
        source: rustls::pki_types::pem::Error,
    },
    #[error("{} 里没有证书", path.display())]
    NoCert { path: PathBuf },
    #[error("生成自签证书失败: {0}")]
    Generate(#[from] rcgen::Error),
    #[error("TLS 配置失败: {0}")]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    X509(#[from] x509::X509Error),
}

/// `tls.mode` 的两个取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    SelfSigned,
    External,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode, TlsError> {
        match s {
            "self-signed" => Ok(Mode::SelfSigned),
            "external" => Ok(Mode::External),
            other => Err(TlsError::Mode(other.to_owned())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::SelfSigned => "self-signed",
            Mode::External => "external",
        }
    }
}

/// 证书与私钥文件的位置。两种模式最后都归到一对 PEM 文件，热加载只盯这一对。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

impl TlsFiles {
    pub fn self_signed(home: &Path) -> Self {
        TlsFiles {
            cert_file: home.join(DIR).join(CERT_FILE),
            key_file: home.join(DIR).join(KEY_FILE),
        }
    }

    /// 按 `tls` 段决定位置：external 两个路径都要给；self-signed 落在 `<home>/tls/`。只算路径，
    /// 不读文件、不生成。
    pub fn from_config(home: &Path, tls: &TlsSection) -> Result<(Mode, TlsFiles), TlsError> {
        let mode = Mode::parse(&tls.mode)?;
        let files = match mode {
            Mode::SelfSigned => TlsFiles::self_signed(home),
            Mode::External => match (&tls.external.cert_file, &tls.external.key_file) {
                (Some(cert), Some(key)) => TlsFiles {
                    cert_file: cert.clone(),
                    key_file: key.clone(),
                },
                _ => return Err(TlsError::ExternalIncomplete),
            },
        };
        Ok((mode, files))
    }
}

/// 一张证书链 + 私钥（DER）以及叶证书的 SPKI 指纹。
pub struct Identity {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    fingerprint: Fingerprint,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Identity({}, {} certs)",
            self.fingerprint,
            self.certs.len()
        )
    }
}

impl Identity {
    /// 读一对 PEM 文件。证书文件可以是整条链（第一张是叶）；私钥 PKCS#8 / SEC1 / PKCS#1 都收。
    pub fn from_files(files: &TlsFiles) -> Result<Identity, TlsError> {
        let cert_pem = std::fs::read(&files.cert_file).map_err(|source| TlsError::Read {
            path: files.cert_file.clone(),
            source,
        })?;
        let key_pem = std::fs::read(&files.key_file).map_err(|source| TlsError::Read {
            path: files.key_file.clone(),
            source,
        })?;
        Self::from_pem(&cert_pem, &key_pem, files)
    }

    fn from_pem(cert_pem: &[u8], key_pem: &[u8], files: &TlsFiles) -> Result<Identity, TlsError> {
        let certs = CertificateDer::pem_slice_iter(cert_pem)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| TlsError::Pem {
                path: files.cert_file.clone(),
                source,
            })?;
        let leaf = certs.first().ok_or_else(|| TlsError::NoCert {
            path: files.cert_file.clone(),
        })?;
        let fingerprint = Fingerprint::of_cert(leaf).map_err(|e| match e {
            FingerprintError::Cert(x) => TlsError::X509(x),
            FingerprintError::Format(s) => TlsError::Mode(s), // 不会发生：of_cert 不解析文本
        })?;
        let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|source| TlsError::Pem {
            path: files.key_file.clone(),
            source,
        })?;
        Ok(Identity {
            certs,
            key,
            fingerprint,
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    pub fn leaf(&self) -> &CertificateDer<'static> {
        &self.certs[0]
    }

    /// 叶证书的 `(notBefore, notAfter)`，unix 秒。
    pub fn validity(&self) -> Result<(i64, i64), TlsError> {
        Ok(x509::validity(self.leaf())?)
    }

    /// rustls 用的形态；私钥装不进 provider（算法不支持、与证书不配）在这里报出来，不等到握手。
    pub fn certified_key(&self, provider: &CryptoProvider) -> Result<CertifiedKey, TlsError> {
        Ok(CertifiedKey::from_der(
            self.certs.clone(),
            self.key.clone_key(),
            provider,
        )?)
    }
}

// ---------- self-signed ----------

/// self-signed 模式的入口：文件在就读，不在就生成。**一次生成、之后复用**——指纹稳定是 peer
/// 配置得以成立的前提（守卫：`tests/peer_tls.rs::self_signed_generated_once_and_reused`）。
/// 只有证书或只有私钥（半写、手动删了一个）当作没有，重新生成一对。
pub fn load_or_generate_self_signed(home: &Path, now_secs: i64) -> Result<Identity, TlsError> {
    let files = TlsFiles::self_signed(home);
    if files.cert_file.is_file() && files.key_file.is_file() {
        return Identity::from_files(&files);
    }
    generate_self_signed(&files, now_secs, SELF_SIGNED_DAYS)
}

/// 生成新密钥 + 自签证书写到 `files`（覆盖已有的：`agora tls rotate-key` 就是这个）。
/// notBefore 取 `now_secs` 当天 0 点 UTC（时钟略偏的机器也不会拿到"还没生效"的证书），
/// 有效 `valid_days` 天。密钥 ECDSA P-256（rcgen 默认）。文件 0600、目录 0700。
pub fn generate_self_signed(
    files: &TlsFiles,
    now_secs: i64,
    valid_days: u64,
) -> Result<Identity, TlsError> {
    let key = KeyPair::generate()?;
    let cert_pem = issue_self_signed(&key, now_secs, valid_days)?;
    write_pair(files, &cert_pem, &key.serialize_pem())?;
    Identity::from_files(files)
}

/// 同一把私钥重新签一张证书（续期而不换钥）：SPKI 不变，peer 的指纹继续有效。
pub fn reissue_self_signed(
    files: &TlsFiles,
    now_secs: i64,
    valid_days: u64,
) -> Result<Identity, TlsError> {
    let key_pem = std::fs::read_to_string(&files.key_file).map_err(|source| TlsError::Read {
        path: files.key_file.clone(),
        source,
    })?;
    let key = KeyPair::from_pem(&key_pem)?;
    let cert_pem = issue_self_signed(&key, now_secs, valid_days)?;
    write_file(&files.cert_file, cert_pem.as_bytes())?;
    Identity::from_files(files)
}

fn issue_self_signed(key: &KeyPair, now_secs: i64, valid_days: u64) -> Result<String, TlsError> {
    let today = crate::clock::format_utc_secs(now_secs);
    let num = |a: usize, b: usize| today[a..b].parse::<i32>().unwrap_or(1970);
    let not_before = rcgen::date_time_ymd(num(0, 4), num(5, 7) as u8, num(8, 10) as u8);
    let mut params = CertificateParams::new(Vec::<String>::new())?;
    params.not_before = not_before;
    params.not_after = not_before + std::time::Duration::from_secs(valid_days * 86_400);
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "agora");
    params.distinguished_name = dn;
    Ok(params.self_signed(key)?.pem())
}

fn write_pair(files: &TlsFiles, cert_pem: &str, key_pem: &str) -> Result<(), TlsError> {
    if let Some(dir) = files.key_file.parent() {
        std::fs::create_dir_all(dir).map_err(|source| TlsError::Write {
            path: dir.to_path_buf(),
            source,
        })?;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    // 先写私钥再写证书：热加载的 watcher 若恰好在两次写之间读到"新钥 + 旧证"，会因为钥证不配而
    // 拒绝并保留旧证书，下一轮读到成对的再换（tls::reload）；反过来"旧钥 + 新证"也一样。
    write_file(&files.key_file, key_pem.as_bytes())?;
    write_file(&files.cert_file, cert_pem.as_bytes())
}

/// 0600 落盘：先写 `.tmp` 再 rename，读的一方永远看不到半个文件。
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), TlsError> {
    let err = |source| TlsError::Write {
        path: path.to_path_buf(),
        source,
    };
    let tmp = path.with_extension("tmp");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(err)?;
    f.write_all(bytes).map_err(err)?;
    f.sync_all().map_err(err)?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(err)
}

// ---------- 零凭据警告 ----------

/// TLS 监听器已开、却没有任何机器 token 也没有任何已配对设备：没有人能连进来（ADR-003 D5）。
/// 这是**警告**不是拒绝——D1 没有直通路由，零凭据状态下监听在公网上什么也做不了；返回类型
/// 里结构性地没有"拒绝"这一档（守卫：`tests/listen.rs::tls_listen_without_credentials_warns_not_refuses`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroCredentials;

impl fmt::Display for ZeroCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "TLS 监听器已开但没有任何机器 token 与已配对设备，没有人能连进来；\
             `agora peer token create <name>` 或 `agora pair`",
        )
    }
}

pub fn zero_credentials_warning(
    paired_devices: usize,
    machine_tokens: usize,
) -> Option<ZeroCredentials> {
    (paired_devices == 0 && machine_tokens == 0).then_some(ZeroCredentials)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_788_652_800; // 2026-09-06T00:00:00Z

    #[test]
    fn self_signed_is_ten_years_from_today_and_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let id = load_or_generate_self_signed(dir.path(), NOW + 3600).unwrap();
        let (nb, na) = id.validity().unwrap();
        assert_eq!(nb, NOW, "notBefore 是当天 0 点 UTC");
        assert_eq!(na - nb, SELF_SIGNED_DAYS as i64 * 86_400);
        let files = TlsFiles::self_signed(dir.path());
        for p in [&files.cert_file, &files.key_file] {
            let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", p.display());
        }
        let dir_mode = std::fs::metadata(dir.path().join(DIR))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert!(!files.cert_file.with_extension("tmp").exists());
    }

    #[test]
    fn reissue_keeps_the_key_and_rotate_changes_it() {
        let dir = tempfile::tempdir().unwrap();
        let files = TlsFiles::self_signed(dir.path());
        let a = generate_self_signed(&files, NOW, 10).unwrap();
        let b = reissue_self_signed(&files, NOW + 86_400, 30).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint(), "续期不换钥");
        assert_ne!(a.leaf(), b.leaf(), "证书本身换了");
        assert_eq!(
            b.validity().unwrap().1 - b.validity().unwrap().0,
            30 * 86_400
        );
        let c = generate_self_signed(&files, NOW, 10).unwrap();
        assert_ne!(a.fingerprint(), c.fingerprint(), "rotate 换钥");
    }

    #[test]
    fn mode_and_files_follow_the_config_section() {
        let home = Path::new("/h");
        let mut tls = TlsSection::default();
        let (mode, files) = TlsFiles::from_config(home, &tls).unwrap();
        assert_eq!(mode, Mode::SelfSigned);
        assert_eq!(files, TlsFiles::self_signed(home));
        assert_eq!(files.cert_file, PathBuf::from("/h/tls/cert.pem"));

        tls.mode = "external".into();
        assert!(matches!(
            TlsFiles::from_config(home, &tls),
            Err(TlsError::ExternalIncomplete)
        ));
        tls.external.cert_file = Some("/c.pem".into());
        tls.external.key_file = Some("/k.pem".into());
        let (mode, files) = TlsFiles::from_config(home, &tls).unwrap();
        assert_eq!(mode, Mode::External);
        assert_eq!(files.key_file, PathBuf::from("/k.pem"));

        tls.mode = "self-ca".into();
        assert!(matches!(
            TlsFiles::from_config(home, &tls),
            Err(TlsError::Mode(m)) if m == "self-ca"
        ));
    }

    #[test]
    fn broken_or_missing_files_are_typed_errors() {
        let dir = tempfile::tempdir().unwrap();
        let files = TlsFiles::self_signed(dir.path());
        assert!(matches!(
            Identity::from_files(&files),
            Err(TlsError::Read { .. })
        ));
        generate_self_signed(&files, NOW, 10).unwrap();
        std::fs::write(&files.cert_file, "not pem").unwrap();
        assert!(matches!(
            Identity::from_files(&files),
            Err(TlsError::Pem { .. } | TlsError::NoCert { .. })
        ));
        // 证书与私钥不配：CertifiedKey 建不出来，在 acceptor 建立时就报。
        let other = tempfile::tempdir().unwrap();
        let other_files = TlsFiles::self_signed(other.path());
        generate_self_signed(&other_files, NOW, 10).unwrap();
        std::fs::copy(&other_files.cert_file, &files.cert_file).unwrap();
        let mismatched = Identity::from_files(&files).unwrap();
        assert!(matches!(
            server::Acceptor::new(&mismatched),
            Err(TlsError::Rustls(_))
        ));
    }

    #[test]
    fn zero_credentials_only_warns_when_both_are_missing() {
        assert_eq!(zero_credentials_warning(0, 0), Some(ZeroCredentials));
        assert_eq!(zero_credentials_warning(1, 0), None);
        assert_eq!(zero_credentials_warning(0, 1), None);
        assert!(ZeroCredentials.to_string().contains("agora pair"));
    }
}
