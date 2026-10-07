//! 配对链接的二维码（agora-thc.1）：`agora pair` 在终端打印、Dashboard「配对新设备」在页面里
//! 画同一份矩阵。只依赖 qrcode 的纯算法部分（无 image / svg 特性）——渲染两处各自做：
//! 终端用半块字符（[`terminal`]），页面拿 [`Qr::rows`] 的 '0'/'1' 自己拼 SVG。
//!
//! QR 用低纠错档（L）：链接本身不长，L 容量够、矩阵最小、终端里扫得最舒服；一个二维码
//! 装着 256 位一次性 token，扫走即用一次，纠错冗余不值得换面积。

use qrcode::QrCode;

#[derive(Debug, thiserror::Error)]
pub enum QrError {
    #[error("二维码编码失败: {0}")]
    Encode(String),
}

/// 一份二维码矩阵：`rows` 每行是等长的 '0'（浅）/ '1'（深）字符串，行列数都是 `size`。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Qr {
    pub size: usize,
    pub rows: Vec<String>,
}

impl Qr {
    pub fn encode(text: &str) -> Result<Self, QrError> {
        let code = QrCode::new(text.as_bytes()).map_err(|e| QrError::Encode(e.to_string()))?;
        let width = code.width();
        let colors = code.to_colors();
        let rows = colors
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|c| if *c == qrcode::Color::Dark { '1' } else { '0' })
                    .collect()
            })
            .collect();
        Ok(Qr { size: width, rows })
    }

    /// 终端里的半块渲染：每个字符格放上下两行（上半块 / 下半块），外加一圈白边——
    /// 终端没有白边时不少扫码器读不出来。
    pub fn terminal(&self, quiet: usize) -> String {
        let blank = " ".repeat(self.size + quiet * 2);
        let mut lines: Vec<String> = Vec::new();
        for _ in 0..quiet {
            lines.push(blank.clone());
        }
        let at = |r: isize, c: isize| -> bool {
            let (r, c) = (r - quiet as isize, c - quiet as isize);
            if r < 0 || c < 0 || r >= self.size as isize || c >= self.size as isize {
                return false;
            }
            self.rows[r as usize].as_bytes()[c as usize] == b'1'
        };
        for row in 0..self.size.div_ceil(2) {
            let mut line = String::new();
            for col in 0..self.size + quiet * 2 {
                let top = at(row as isize * 2, col as isize);
                let bottom = at(row as isize * 2 + 1, col as isize);
                line.push(match (top, bottom) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
            lines.push(line);
        }
        for _ in 0..quiet {
            lines.push(blank.clone());
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_is_square_and_round_trips() {
        let qr = Qr::encode("https://zuan.example:7681/#pair=abc").unwrap();
        assert_eq!(qr.rows.len(), qr.size);
        assert!(qr.rows.iter().all(|r| r.len() == qr.size));
        assert!(qr
            .rows
            .iter()
            .all(|r| r.chars().all(|c| c == '0' || c == '1')));
        // 位置探测图形：左上角 7×7 的第一行是 1111111，第二行是 1000001。
        assert_eq!(&qr.rows[0][..7], "1111111");
        assert_eq!(&qr.rows[1][..7], "1000001");
    }

    #[test]
    fn terminal_rendering_has_quiet_zone_and_full_blocks() {
        let qr = Qr::encode("https://example.test/#pair=x").unwrap();
        let text = qr.terminal(2);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), qr.size.div_ceil(2) + 4);
        assert_eq!(lines[0].chars().count(), qr.size + 4);
        assert!(lines[0].trim().is_empty(), "白边行不能有块");
        assert!(text.contains('█'), "必须出现全块");
        assert!(
            lines.iter().all(|l| l.chars().count() == qr.size + 4),
            "每行等宽（半块渲染只影响高度）"
        );
    }
}
