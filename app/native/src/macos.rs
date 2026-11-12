//! macOS 专属的进程级设置。

/// 关掉本应用的智能引号 / 智能破折号 / 文本替换。
///
/// 系统设置里开着「智能引号」时,编辑器(NSTextView)会把敲的 ' " 换成弯引号,SQL 因此报错
/// (`'sad'` 成了 `’sad‘`,被当成列名)。这里只写 Datum 自己的偏好域,覆盖全局设置,
/// 不影响其他应用;要在任何文本视图创建之前调用(原生模块注册时)。
pub fn disable_smart_substitutions() {
    use objc2_core_foundation::{CFPreferencesSetAppValue, CFString, kCFBooleanFalse, kCFPreferencesCurrentApplication};
    let keys = [
        "NSAutomaticQuoteSubstitutionEnabled",
        "NSAutomaticDashSubstitutionEnabled",
        "NSAutomaticTextReplacementEnabled",
    ];
    // SAFETY:常量由 CoreFoundation 提供;值是 CFBoolean,属于属性列表类型
    unsafe {
        let Some(no) = kCFBooleanFalse else { return };
        for key in keys {
            CFPreferencesSetAppValue(&CFString::from_static_str(key), Some(no), kCFPreferencesCurrentApplication);
        }
    }
}
