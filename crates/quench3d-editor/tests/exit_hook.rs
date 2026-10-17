//! `QUENCH3D_EDITOR_EXIT_AFTER_MS` 自动退出钩子的**纯逻辑守卫**（不需要显卡、不开窗口）。
//!
//! 为什么要单测这个看起来无关紧要的解析：这个钩子是"关窗口 → 退出"这条路**唯一**
//! 能被无人值守复现的入口（`gdb -batch` 里没人能点关闭按钮，CI 上没有显示器）。
//! 解析一旦错，复现就整个失去意义 ——
//!
//! * 该退不退：跑满 `timeout`，退出码 124，看起来"没崩"，其实是**没测到**退出路径；
//! * 起表过早/过晚：第一帧还没画完就退、或等到超时，同样测不到真正的退出析构。
//!
//! 所以这里把两件事钉死：**默认必须是关的**（不设变量绝不自动退出），
//! 以及**只有正整数毫秒数才打开**。
#![cfg(not(target_arch = "wasm32"))]

use quench3d_editor::app::{parse_exit_after_ms, EXIT_AFTER_MS_ENV};

#[test]
fn env_var_name_is_the_documented_one() {
    // 这个名字出现在文档与复现配方里，改了就得同时改文档 —— 钉住它。
    assert_eq!(EXIT_AFTER_MS_ENV, "QUENCH3D_EDITOR_EXIT_AFTER_MS");
}

#[test]
fn unset_or_blank_means_off() {
    assert_eq!(parse_exit_after_ms(None), None, "不设变量 = 永不自动退出");
    assert_eq!(parse_exit_after_ms(Some("")), None);
    assert_eq!(parse_exit_after_ms(Some("   ")), None);
    assert_eq!(parse_exit_after_ms(Some("\t\n")), None);
}

#[test]
fn zero_means_off() {
    // `0` 是"显式要求不自动退出"，不是错误 —— 所以它必须和"没设"一样是 None。
    assert_eq!(parse_exit_after_ms(Some("0")), None);
    assert_eq!(parse_exit_after_ms(Some("  0 ")), None);
}

#[test]
fn garbage_means_off() {
    for raw in [
        "abc", "-1", "-3000", "3.5", "1e3", "3000ms", "ms3000", "+", "0x10", "٣", " ",
    ] {
        assert_eq!(
            parse_exit_after_ms(Some(raw)),
            None,
            "{raw:?} 不是非负整数，必须当作「不自动退出」"
        );
    }
}

#[test]
fn positive_integers_are_milliseconds() {
    assert_eq!(parse_exit_after_ms(Some("1")), Some(1));
    assert_eq!(parse_exit_after_ms(Some("3000")), Some(3000));
    assert_eq!(
        parse_exit_after_ms(Some("  3000  ")),
        Some(3000),
        "两侧空白可容忍"
    );
    assert_eq!(
        parse_exit_after_ms(Some("18446744073709551615")),
        Some(u64::MAX)
    );
}

/// 解析是**纯函数**：同一个输入反复问，答案必须一样（不能读环境、不能有内部状态）。
///
/// 这条看着多余，但它是"钩子默认关闭"这个承诺的守卫 —— 一旦有人把实现改成
/// 直接读 `std::env` 或塞个 `OnceLock` 缓存，这里就会红。
#[test]
fn parsing_is_pure_and_repeatable() {
    for _ in 0..3 {
        assert_eq!(parse_exit_after_ms(Some("250")), Some(250));
        assert_eq!(parse_exit_after_ms(Some("0")), None);
        assert_eq!(parse_exit_after_ms(None), None);
    }
}
