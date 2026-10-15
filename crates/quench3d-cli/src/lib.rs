//! Quench3D 工具库：场景文件、无窗口渲染/仿真、大模型接入、MCP 服务器。
//!
//! 这个 crate 同时提供：
//!
//! * 二进制 `quench3d`（命令行为主，也有 `mcp` 子命令做 stdio 服务器）；
//! * 下面这些模块，方便把同样的能力嵌进别的 agent 宿主程序里。
//!
//! ```
//! use quench3d_cli::scene_doc::SceneDoc;
//!
//! let doc = SceneDoc::from_json(r#"{
//!     "ground": {"size": 10},
//!     "objects": [{"name":"球","shape":{"type":"sphere","radius":0.5},
//!                  "position":[0,4,0],"body":"dynamic"}]
//! }"#).unwrap();
//! let report = quench3d_cli::runner::run_simulate(&doc).unwrap();
//! assert_eq!(report.nan_bodies, 0);
//! assert_eq!(report.bodies, 2);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod agent;
pub mod bench;
pub mod generate;
pub mod llm;
pub mod mcp;
pub mod report;
pub mod runner;
pub mod scene_doc;
pub mod schema;
