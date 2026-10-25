# assets/ —— 编辑器内嵌的界面字体

## `ui-symbols.ttf`（22 KB，子集化）

* **来源**：[DejaVu Sans](https://dejavu-fonts.github.io/)（本机取自
  `/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf`）。
* **许可**：Bitstream Vera / DejaVu（见同目录 `ui-symbols.LICENSE.txt`）。
* **作用**：egui 自带的 Ubuntu-Light 有弯引号、`≈`/`≠`/`≥`，但**没有箭头**
  （`→ ← ↑ ↓ ↔`）；中文子集也不含这些符号。这里只取源码里出现的 17 个非汉字
  符号，补在字体栈最后，避免界面出现第二类豆腐块。

## `ui-font.ttf`（563 KB，子集化）

* **来源**：[Droid Sans Fallback](https://android.googlesource.com/platform/frameworks/base/+/master/data/fonts/DroidSansFallbackFull.ttf)
  （本机取自 `/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf`）。
* **许可**：Apache License 2.0（见 https://www.apache.org/licenses/LICENSE-2.0 ）。
  字体本身**未修改字形数据**，只是做了子集化（subsetting）后重新打包。
* **为什么要内嵌**：egui 自带的字体（Ubuntu-Light + emoji 图标字体）**一个中文字形都没有**，
  而引擎的界面、日志、场景格式里全是中文。不内嵌的话界面会变成一排"豆腐块"（缺字方框）。
* **怎么生成的**（可复现）：

  ```bash
  python3 - <<'EOF'
  from fontTools import subset
  # 字符集 = crates/quench3d-editor/src 下所有 .rs 源码里出现的字符
  #         + GB2312 一级汉字 + 常用中文标点（共 3905 字）
  # 不要只扫字符串字面量：注释里的汉字也在源码里，统一收进去最省事。
  subset.main(["/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
               "--text-file=chars.txt",
               "--output-file=crates/quench3d-editor/assets/ui-font.ttf",
               "--layout-features=*", "--no-hinting", "--desubroutinize"])
  EOF
  ```

  需要 `fonttools`（`pip install fonttools`，本机用 4.29.1 生成）。
* **注意**：子集里**只有**上面的字符集。界面文案里新出现的汉字如果不在 GB2312 一级字内，
  必须重新生成字体，否则那一处就是豆腐块（菜单「渲染」曾经显示成「x 染」，就是因为
  子集生成早于菜单项，漏了「渲」）。现在有一条单元测试
  `app::font_tests::ui_font_covers_every_cjk_character_in_editor_sources`
  逐字检查源码里的汉字，漏字会直接让 CI 变红；编辑器启动时仍保留日志自检：
  `编辑器字体：中文子集可用 = true`。
* 该字体在 `crates/quench3d-editor/src/app.rs::install_fonts` 里被挂到
  `Proportional` 与 `Monospace` 两个字体族的**末尾**：Latin 仍由 egui 自带的
  Ubuntu-Light 渲染（更好看），只有它缺的字才回退到这份子集。
