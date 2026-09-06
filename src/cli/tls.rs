//! `agora tls fingerprint | rotate-key`（ADR-003 D4；agora-7ku.10）。
//!
//! 在**被访问**的节点上运行。`fingerprint` 打印本节点 TLS 证书的 SPKI 指纹（`sha256:<64 hex>`），
//! 别的节点把本节点配成 peer 时 `peers[].cert_fingerprint` 填的就是它；self-signed 模式下证书还没
//! 生成就先生成——daemon 之后复用同一对文件，所以装好节点不必先起 daemon 就能把指纹发给对方。
//! `rotate-key` 换一把新钥重签自签证书（external 的密钥归外部工具管，这里拒绝）；daemon 在跑的话
//! 靠热加载换上、不用重启，但每个把本节点配成 peer 的节点都得改指纹。
//!
//! stdout 只有指纹一行（`agora tls fingerprint` 的输出能直接粘进对方的 YAML），提示全走 stderr——
//! 与 `agora peer token create` 同一约定。

use std::path::Path;

use crate::config::TlsSection;
use crate::tls::{self, Fingerprint, Identity, Mode, TlsError, TlsFiles, SELF_SIGNED_DAYS};

const USAGE: &str = "用法: agora tls fingerprint | agora tls rotate-key";

/// 退出码：0 成功、1 操作失败、2 用法错误（与 `agora hooks` / `agora peer` 一致）。
/// `home` 与 `section` 由 `main.rs` 用 daemon 同一套解析给出，CLI 与 daemon 看到的永远是同一对文件。
pub fn run(args: &[&str], home: &Path, section: &TlsSection) -> i32 {
    let now = crate::clock::now_secs();
    let result = match args {
        ["fingerprint"] => fingerprint(home, section, now).map(|(mode, files, fp)| {
            eprintln!(
                "tls.mode = {}；证书 {}。别的节点把本节点配成 peer 时 peers[].cert_fingerprint 填这个值",
                mode.as_str(),
                files.cert_file.display()
            );
            fp
        }),
        ["rotate-key"] => rotate_key(home, section, now).map(|(files, fp)| {
            eprintln!(
                "已换钥重签 {}（{} 天）。每个把本节点配成 peer 的节点都要更新 peers[].cert_fingerprint；\
                 daemon 在跑的话 {:?} 内热加载，不必重启",
                files.cert_file.display(),
                SELF_SIGNED_DAYS,
                tls::reload::DEFAULT_INTERVAL
            );
            fp
        }),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    match result {
        Ok(fp) => {
            println!("{fp}");
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

/// 当前证书的指纹。self-signed 缺文件就生成——与 daemon 的 `bind_tls` 走同一个
/// [`tls::load_or_generate_self_signed`]，所以 CLI 先跑还是 daemon 先跑，得到的是同一对文件。
pub fn fingerprint(
    home: &Path,
    section: &TlsSection,
    now_secs: i64,
) -> Result<(Mode, TlsFiles, Fingerprint), TlsError> {
    let (mode, files) = TlsFiles::from_config(home, section)?;
    let identity = match mode {
        Mode::SelfSigned => tls::load_or_generate_self_signed(home, now_secs)?,
        Mode::External => Identity::from_files(&files)?,
    };
    Ok((mode, files, identity.fingerprint()))
}

/// 换钥重签；只对 self-signed 有意义（[`TlsError::ExternalRotate`]）。
pub fn rotate_key(
    home: &Path,
    section: &TlsSection,
    now_secs: i64,
) -> Result<(TlsFiles, Fingerprint), TlsError> {
    let (mode, files) = TlsFiles::from_config(home, section)?;
    if mode != Mode::SelfSigned {
        return Err(TlsError::ExternalRotate);
    }
    let identity = tls::generate_self_signed(&files, now_secs, SELF_SIGNED_DAYS)?;
    Ok((files, identity.fingerprint()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TlsExternal;
    use crate::tls::x509;
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::CertificateDer;
    use sha2::{Digest, Sha256};

    const NOW: i64 = 1_788_652_800; // 2026-09-06T00:00:00Z

    /// 不经 `Fingerprint`：直接从磁盘上的 PEM 取叶子证书的 SubjectPublicKeyInfo 做 SHA-256。
    fn spki_sha256_of(cert_file: &Path) -> String {
        let pem = std::fs::read(cert_file).unwrap();
        let leaf = CertificateDer::pem_slice_iter(&pem)
            .next()
            .unwrap()
            .unwrap();
        let spki = x509::spki(&leaf).unwrap();
        let hex: String = Sha256::digest(spki)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("sha256:{hex}")
    }

    #[test]
    fn fingerprint_output_equals_cert_spki_sha256() {
        // 验收：`agora tls fingerprint` 输出等于证书 SPKI 的 SHA-256。self-signed 首跑生成，
        // 再跑不变；external 指向同一对文件得到同一个值。
        let home = tempfile::tempdir().unwrap();
        let section = TlsSection::default();
        let (mode, files, fp) = fingerprint(home.path(), &section, NOW).unwrap();
        assert_eq!(mode, Mode::SelfSigned);
        assert_eq!(files, TlsFiles::self_signed(home.path()));
        assert_eq!(fp.to_string(), spki_sha256_of(&files.cert_file));
        assert_eq!(fingerprint(home.path(), &section, NOW + 1).unwrap().2, fp);

        let external = TlsSection {
            mode: "external".into(),
            external: TlsExternal {
                cert_file: Some(files.cert_file.clone()),
                key_file: Some(files.key_file.clone()),
                renew_command: None,
                renew_before: "720h".into(),
            },
        };
        let (mode, _, fp2) = fingerprint(home.path(), &external, NOW).unwrap();
        assert_eq!(mode, Mode::External);
        assert_eq!(fp2, fp);
        // external 缺文件是错误，不是生成。
        let missing = TlsSection {
            external: TlsExternal {
                cert_file: Some(home.path().join("nope.crt")),
                ..external.external.clone()
            },
            ..external.clone()
        };
        assert!(matches!(
            fingerprint(home.path(), &missing, NOW),
            Err(TlsError::Read { .. })
        ));
        assert!(matches!(
            rotate_key(home.path(), &external, NOW),
            Err(TlsError::ExternalRotate)
        ));

        // rotate-key：新钥、新指纹，fingerprint 从此报新值，且仍等于磁盘上证书的 SPKI。
        let (_, rotated) = rotate_key(home.path(), &section, NOW).unwrap();
        assert_ne!(rotated, fp);
        assert_eq!(rotated.to_string(), spki_sha256_of(&files.cert_file));
        assert_eq!(fingerprint(home.path(), &section, NOW).unwrap().2, rotated);
    }

    #[test]
    fn run_exit_codes() {
        let home = tempfile::tempdir().unwrap();
        let section = TlsSection::default();
        assert_eq!(run(&[], home.path(), &section), 2);
        assert_eq!(run(&["frobnicate"], home.path(), &section), 2);
        assert_eq!(run(&["fingerprint"], home.path(), &section), 0);
        assert_eq!(run(&["rotate-key"], home.path(), &section), 0);
        let bad = TlsSection {
            mode: "self-ca".into(),
            ..TlsSection::default()
        };
        assert_eq!(run(&["fingerprint"], home.path(), &bad), 1);
    }
}
