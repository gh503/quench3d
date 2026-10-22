//! 纯 egui 的文件选择器（桌面端）。
//!
//! 为什么不用系统对话框：仓库要求离线构建，`rfd` 这类 crate 不在锁文件里；
//! 自己画一个够用的选择器，Windows/macOS/Linux 都能跑，也不依赖 zenity。
//!
//! 只做两件事：在目录里挑一个文件打开、或者输入文件名另存。过滤按扩展名做，
//! 目录永远显示。Web 端不用它（浏览器有原生 `<input type=file>` / 下载）。

use std::path::{Path, PathBuf};

/// 选择器模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickMode {
    /// 打开已有文件。
    Open,
    /// 另存为新文件。
    Save,
}

/// 选择器这次是替哪个动作选路径。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickAction {
    /// 打开场景。
    #[default]
    OpenScene,
    /// 场景另存为。
    SaveScene,
    /// 导出 glTF。
    ExportGltf,
    /// 导出 GLB。
    ExportGlb,
}

/// 选择结果。
#[derive(Clone, Debug)]
pub struct PickResult {
    /// 打开还是保存。
    pub mode: PickMode,
    /// 绝对/相对路径（由调用方决定怎么用）。
    pub path: PathBuf,
}

/// 单帧结果。
pub enum Outcome {
    /// 还开着。
    Pending,
    /// 用户取消/关窗。
    Cancelled,
    /// 选好了。
    Picked(PickResult),
}

/// 一个文件选择器窗口的状态。
pub struct FilePicker {
    mode: PickMode,
    /// 当前目录。
    dir: PathBuf,
    /// 文件名（保存时可直接编辑；打开时随列表点击更新）。
    filename: String,
    /// 允许的扩展名（小写，不带点）。
    extensions: Vec<String>,
    /// 标题。
    title: String,
    /// 读目录/覆盖确认的提示。
    error: Option<String>,
    /// 已存在，等待第二次确认。
    confirm_overwrite: bool,
    /// 窗口是否还开着。
    open: bool,
}

impl FilePicker {
    /// 打开模式：从 `start`（文件或目录）开始，只列 `extensions`。
    pub fn for_open(start: &Path, extensions: &[&str], title: &str) -> Self {
        let (dir, filename) = split_start(start);
        Self {
            mode: PickMode::Open,
            dir,
            filename,
            extensions: normalize_extensions(extensions),
            title: title.to_string(),
            error: None,
            confirm_overwrite: false,
            open: true,
        }
    }

    /// 保存模式：默认文件名 `default_name`。
    pub fn for_save(start: &Path, default_name: &str, extensions: &[&str], title: &str) -> Self {
        let (mut dir, mut filename) = split_start(start);
        if filename.is_empty() {
            filename = default_name.to_string();
        }
        if dir.as_os_str().is_empty() {
            dir = PathBuf::from(".");
        }
        Self {
            mode: PickMode::Save,
            dir,
            filename,
            extensions: normalize_extensions(extensions),
            title: title.to_string(),
            error: None,
            confirm_overwrite: false,
            open: true,
        }
    }

    /// 当前目录（测试/状态栏用）。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 当前文件名。
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// 画窗口；返回本帧结果。
    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        if !self.open {
            return Outcome::Cancelled;
        }
        let mut open = self.open;
        let mut picked = None;
        egui::Window::new(&self.title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("↑ 上级").clicked() {
                        if let Some(parent) = self.dir.parent() {
                            self.dir = parent.to_path_buf();
                        }
                    }
                    ui.label(
                        egui::RichText::new(self.dir.display().to_string())
                            .monospace()
                            .small(),
                    );
                });
                ui.separator();
                if let Some(error) = &self.error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| match read_entries(&self.dir, &self.extensions) {
                        Ok(entries) if entries.is_empty() => {
                            ui.label(egui::RichText::new("这个目录里没有匹配的文件").weak());
                        }
                        Ok(entries) => {
                            for entry in entries {
                                if entry.is_dir {
                                    if ui.button(format!("📁 {}", entry.name)).clicked() {
                                        self.dir = entry.path;
                                        self.error = None;
                                    }
                                } else if ui
                                    .selectable_label(
                                        self.filename == entry.name,
                                        format!("📄 {}", entry.name),
                                    )
                                    .clicked()
                                {
                                    self.filename = entry.name;
                                    self.confirm_overwrite = false;
                                }
                            }
                        }
                        Err(e) => {
                            ui.colored_label(egui::Color32::LIGHT_RED, e);
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("文件名");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filename)
                            .desired_width(320.0)
                            .hint_text("scene.json"),
                    );
                });
                ui.horizontal(|ui| {
                    let label = match self.mode {
                        PickMode::Open => "打开",
                        PickMode::Save => "保存",
                    };
                    if ui.button(label).clicked() {
                        match self.finish() {
                            Ok(Some(result)) => picked = Some(result),
                            Ok(None) => {}
                            Err(e) => self.error = Some(e),
                        }
                    }
                    if ui.button("取消").clicked() {
                        self.open = false;
                    }
                    if self.confirm_overwrite {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "文件已存在；再点一次「保存」覆盖。",
                        );
                    }
                });
            });
        self.open = open;
        if let Some(result) = picked {
            self.open = false;
            return Outcome::Picked(result);
        }
        if !self.open {
            Outcome::Cancelled
        } else {
            Outcome::Pending
        }
    }

    /// 校验文件名/扩展名/覆盖确认；返回 `Ok(None)` 表示只设置了覆盖确认。
    fn finish(&mut self) -> Result<Option<PickResult>, String> {
        let name = self.filename.trim();
        if name.is_empty() {
            return Err("文件名不能为空".to_string());
        }
        let mut path = self.dir.join(name);
        if path.is_dir() {
            return Err(format!("{} 是目录", path.display()));
        }
        if self.mode == PickMode::Save {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !self.extensions.is_empty()
                && !self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
            {
                let first = &self.extensions[0];
                path.set_extension(first);
            }
        } else if !path.exists() {
            return Err(format!("文件不存在：{}", path.display()));
        }
        if self.mode == PickMode::Save && path.exists() && !self.confirm_overwrite {
            self.confirm_overwrite = true;
            return Ok(None);
        }
        self.error = None;
        Ok(Some(PickResult {
            mode: self.mode,
            path,
        }))
    }
}

fn normalize_extensions(extensions: &[&str]) -> Vec<String> {
    extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

fn split_start(start: &Path) -> (PathBuf, String) {
    if start.is_dir() {
        return (start.to_path_buf(), String::new());
    }
    let dir = start
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let name = start
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    (dir, name)
}

#[derive(Debug)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

fn read_entries(dir: &Path, extensions: &[String]) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    let read = std::fs::read_dir(dir).map_err(|e| format!("读目录 {} 失败：{e}", dir.display()))?;
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if meta.is_dir() {
            out.push(Entry {
                name,
                path,
                is_dir: true,
            });
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if extensions.is_empty() || extensions.contains(&ext) {
            out.push(Entry {
                name,
                path,
                is_dir: false,
            });
        }
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("quench3d-picker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn entries_filter_dirs_and_extensions() {
        let dir = temp_dir("entries");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.json"), "{}").unwrap();
        std::fs::write(dir.join("b.txt"), "x").unwrap();
        let entries = read_entries(&dir, &normalize_extensions(&["json"])).unwrap();
        assert!(entries[0].is_dir, "目录必须排在文件前面");
        assert_eq!(entries[0].name, "sub");
        assert_eq!(entries.len(), 2, "只应留下目录 + a.json：{entries:?}");
        assert_eq!(entries[1].name, "a.json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_adds_missing_extension_and_confirms_overwrite() {
        let dir = temp_dir("save");
        let mut picker = FilePicker::for_save(&dir.join("scene.json"), "out", &["json"], "保存");
        picker.filename = "new".to_string();
        let result = picker.finish().unwrap().expect("新文件应直接通过");
        assert_eq!(result.path, dir.join("new.json"));
        std::fs::write(dir.join("new.json"), "{}").unwrap();
        picker.confirm_overwrite = false;
        assert!(picker.finish().unwrap().is_none(), "已存在文件必须先确认");
        assert!(picker.confirm_overwrite);
        let result = picker.finish().unwrap().expect("确认后应通过");
        assert_eq!(result.path, dir.join("new.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn contains_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        fn walk(shape: &egui::Shape, needle: &str) -> bool {
            match shape {
                egui::Shape::Text(text) => text.galley.text().contains(needle),
                egui::Shape::Vec(shapes) => shapes.iter().any(|s| walk(s, needle)),
                _ => false,
            }
        }
        shapes.iter().any(|clipped| walk(&clipped.shape, needle))
    }

    #[test]
    fn window_renders_title_and_filename_field() {
        let dir = temp_dir("window");
        std::fs::write(dir.join("a.json"), "{}").unwrap();
        let mut picker = FilePicker::for_open(&dir, &["json"], "打开场景");
        let ctx = egui::Context::default();
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                let _ = picker.show(ui.ctx());
            });
            output.textures_delta.clear();
            shapes = output.shapes;
        }
        assert!(contains_text(&shapes, "打开场景"), "窗口标题没画出来");
        assert!(contains_text(&shapes, "文件名"), "文件名输入框没画出来");
        assert!(contains_text(&shapes, "a.json"), "文件列表没画出来");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_rejects_missing_file() {
        let dir = temp_dir("open");
        let mut picker = FilePicker::for_open(&dir.join("scene.json"), &["json"], "打开");
        picker.filename = "nope.json".to_string();
        assert!(picker.finish().is_err());
        std::fs::write(dir.join("yes.json"), "{}").unwrap();
        picker.filename = "yes.json".to_string();
        assert!(picker.finish().unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
