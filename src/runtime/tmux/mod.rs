//! tmux 运行时实现（ADR-001 D2/D3/D6/D7；agora-xqa.7）。
//!
//! 专用 socket `-L agora`；对默认 socket 只读；没有"杀整个 server"的方法（ADR-001 D3）。
