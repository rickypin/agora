//! API 版本与兼容判定（MISSION §7.3；docs/spec/api.md「api_version 兼容规则」）。
//!
//! 每个节点各自原地升级，N 个节点的 `api_version` 会不一致（MISSION §2.3 规则 10）。
//! 两种调用方——浏览器页面（`web/src/health.ts`）与 peer 客户端（agora-7ku.5）——在读任何
//! 业务数据之前先 `GET /api/system` 过这里的判定；结论是枚举，按类型分类，不做字符串匹配。
//!
//! 同 major 兼容、minor 只增字段：minor 的高低只用于日志与提示，**不得**据此拒绝对话，
//! 否则滚动升级期间整个集群互相看不见。

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `GET /api/system` 的 `api_version`：`{ "major": u32, "minor": u32 }`。
///
/// 刻意是对象而不是 `"1.0"` 字符串或裸整数：两端都直接反序列化成结构，没有解析这一步
/// 可以出错；旧二进制回的裸整数 `1` 在 [`ApiVersion::from_json`] 里归为读不出（api.md）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ApiVersion {
    pub major: u32,
    pub minor: u32,
}

impl ApiVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        ApiVersion { major, minor }
    }

    /// 从 `/api/system` 响应里的 `api_version` 值读出版本；缺失、不是对象、major / minor
    /// 不是非负整数都算读不出。
    pub fn from_json(value: &Value) -> Option<ApiVersion> {
        let obj = value.as_object()?;
        let field = |k: &str| obj.get(k)?.as_u64()?.try_into().ok();
        Some(ApiVersion {
            major: field("major")?,
            minor: field("minor")?,
        })
    }

    /// 本方是 `self`，对方是 `theirs`：同 major 兼容，否则不兼容。
    pub fn check(self, theirs: ApiVersion) -> Result<Compatibility, Incompatible> {
        check(self, theirs)
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// 本二进制实现的 API 版本。改形态时按 api.md 的递增规则改这里，并同步
/// `web/src/health.ts` 的 `API_VERSION`——页面按哪一版构建就按哪一版比。
pub const API_VERSION: ApiVersion = ApiVersion::new(1, 1);

/// `GET /api/system` 的响应体。peer 客户端用同一个类型反序列化，字段名只在这里出现一次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub api_version: ApiVersion,
    /// 二进制（crate）版本，只给人看；程序不据此判断。
    pub version: String,
    pub node: String,
}

/// 同 major 时的三档，只区分 minor 的高低（日志 / 提示用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    /// 同 major 同 minor。
    Same,
    /// 对方 minor 更大：多出来的字段本方看不懂、忽略即可。
    PeerNewer,
    /// 对方 minor 更小：本方要的字段可能缺，按缺省处理。
    PeerOlder,
}

/// 不兼容的两种情形；两种都意味着不读对方任何业务数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Incompatible {
    /// major 不同：形态可能已经变了，读了就是错读。
    #[error("API 版本不兼容：本方 {ours}，对方 {theirs}（major 不同）")]
    MajorMismatch {
        ours: ApiVersion,
        theirs: ApiVersion,
    },
    /// 对方的 `api_version` 缺失或不是 `{ major, minor }`：不是本协议的节点，或早于这条规则的二进制。
    #[error("对方没有报告可识别的 API 版本")]
    Unreadable,
}

/// 兼容判定：major 相同即兼容，minor 只决定三档里的哪一档。
pub fn check(ours: ApiVersion, theirs: ApiVersion) -> Result<Compatibility, Incompatible> {
    if ours.major != theirs.major {
        return Err(Incompatible::MajorMismatch { ours, theirs });
    }
    Ok(match theirs.minor.cmp(&ours.minor) {
        std::cmp::Ordering::Equal => Compatibility::Same,
        std::cmp::Ordering::Greater => Compatibility::PeerNewer,
        std::cmp::Ordering::Less => Compatibility::PeerOlder,
    })
}

/// 从 `GET /api/system` 的原始响应体一步得到结论：读不出 `api_version` 与 major 不同
/// 都是 `Incompatible`。peer 客户端连上的第一步就是它（agora-7ku.5）。
pub fn negotiate(ours: ApiVersion, system_body: &Value) -> Result<Compatibility, Incompatible> {
    let theirs = system_body
        .get("api_version")
        .and_then(ApiVersion::from_json)
        .ok_or(Incompatible::Unreadable)?;
    check(ours, theirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V1_0: ApiVersion = ApiVersion::new(1, 0);

    #[test]
    fn same_major_is_compatible_whatever_the_minor() {
        assert_eq!(check(V1_0, ApiVersion::new(1, 0)), Ok(Compatibility::Same));
        assert_eq!(
            check(V1_0, ApiVersion::new(1, 7)),
            Ok(Compatibility::PeerNewer)
        );
        assert_eq!(
            check(ApiVersion::new(1, 7), V1_0),
            Ok(Compatibility::PeerOlder)
        );
        // 方法形态与自由函数是同一个判定。
        assert_eq!(
            V1_0.check(ApiVersion::new(1, 3)),
            Ok(Compatibility::PeerNewer)
        );
    }

    #[test]
    fn different_major_is_incompatible_in_both_directions() {
        let v2 = ApiVersion::new(2, 0);
        assert_eq!(
            check(V1_0, v2),
            Err(Incompatible::MajorMismatch {
                ours: V1_0,
                theirs: v2
            })
        );
        assert_eq!(
            check(v2, V1_0),
            Err(Incompatible::MajorMismatch {
                ours: v2,
                theirs: V1_0
            })
        );
        // minor 再大也救不了 major：2.0 与 1.99 不兼容。
        assert!(check(ApiVersion::new(1, 99), v2).is_err());
    }

    #[test]
    fn negotiate_reads_the_raw_system_body() {
        let body =
            json!({ "api_version": { "major": 1, "minor": 2 }, "version": "0.1.0", "node": "mac" });
        assert_eq!(negotiate(V1_0, &body), Ok(Compatibility::PeerNewer));
        let body =
            json!({ "api_version": { "major": 2, "minor": 0 }, "version": "9.9.9", "node": "mac" });
        assert!(matches!(
            negotiate(V1_0, &body),
            Err(Incompatible::MajorMismatch { .. })
        ));
    }

    #[test]
    fn unreadable_api_version_is_incompatible_not_a_guess() {
        // 缺失、旧二进制的裸整数、字符串、负数、缺 minor：一律读不出，不猜成 1.0。
        for body in [
            json!({ "version": "0.1.0", "node": "mac" }),
            json!({ "api_version": 1 }),
            json!({ "api_version": "1.0" }),
            json!({ "api_version": { "major": -1, "minor": 0 } }),
            json!({ "api_version": { "major": 1 } }),
            json!({ "api_version": { "major": 1.5, "minor": 0 } }),
            json!(null),
        ] {
            assert_eq!(
                negotiate(V1_0, &body),
                Err(Incompatible::Unreadable),
                "body = {body}"
            );
        }
    }

    #[test]
    fn system_info_round_trips_and_serializes_as_an_object() {
        let info = SystemInfo {
            api_version: API_VERSION,
            version: "0.1.0".into(),
            node: "mac".into(),
        };
        let v = serde_json::to_value(&info).unwrap();
        // 用常量推导而不是写死数字：minor 每合入一批只增字段的分支就 +1（api.md「api_version 兼容规则」），
        // 2026-09-06 第一次 bump 就被写死的 { 1, 0 } 绊了一下。
        assert_eq!(
            v["api_version"],
            json!({ "major": API_VERSION.major, "minor": API_VERSION.minor })
        );
        let back: SystemInfo = serde_json::from_value(v).unwrap();
        assert_eq!(back, info);
        // 旧形态（裸整数）反序列化必须失败，而不是悄悄变成某个版本。
        assert!(serde_json::from_value::<SystemInfo>(
            json!({ "api_version": 1, "version": "0.1.0", "node": "mac" })
        )
        .is_err());
    }

    /// 页面按哪一版构建就按哪一版比：`web/src/health.ts` 的 `API_VERSION` 字面量必须与这里一致，
    /// 否则每个浏览器都会对着自己的节点报"不兼容"。守卫读的是源码文本（同 tests/arch_boundary.rs），
    /// 不是运行时靠文本判断——那条禁令（规则 10）管的是程序逻辑，不管构建期的一致性检查。
    #[test]
    fn page_is_built_against_the_same_api_version() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/health.ts"),
        )
        .expect("web/src/health.ts 应存在");
        let line = src
            .lines()
            .find(|l| l.starts_with("export const API_VERSION"))
            .expect("web/src/health.ts 应有 `export const API_VERSION: ApiVersion = { major: N, minor: M };`");
        let number = |key: &str| -> u32 {
            let at = line
                .find(key)
                .unwrap_or_else(|| panic!("{line:?} 里没有 {key}"));
            line[at + key.len()..]
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or_else(|_| panic!("{line:?} 里 {key} 后面不是整数"))
        };
        let page = ApiVersion::new(number("major:"), number("minor:"));
        assert_eq!(
            page, API_VERSION,
            "web/src/health.ts 的 API_VERSION（{page}）与 src/api/version.rs 的（{API_VERSION}）不一致：两边要同一个 commit 里一起改"
        );
    }

    #[test]
    fn display_is_major_dot_minor() {
        assert_eq!(ApiVersion::new(1, 0).to_string(), "1.0");
        assert_eq!(ApiVersion::new(12, 3).to_string(), "12.3");
        assert_eq!(
            Incompatible::MajorMismatch {
                ours: V1_0,
                theirs: ApiVersion::new(2, 1)
            }
            .to_string(),
            "API 版本不兼容：本方 1.0，对方 2.1（major 不同）"
        );
    }
}
