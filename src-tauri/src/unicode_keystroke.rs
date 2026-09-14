//! Secure Event Input 探测（macOS）。
//!
//! 密码框、sudo 提示、1Password 等会打开 Secure Event Input。`host_document` 的
//! 光标 / 屏幕 OCR 读取路径用它做第一道硬拦 —— 这个信号一亮就说明屏幕上正在
//! 输入凭据，一个字都不该读。（原流式逐字上屏合成键盘事件的部分已随流式输入
//! 功能一起删除。）
mod macos_impl {
    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn IsSecureEventInputEnabled() -> i32;
    }

    /// Secure Event Input 是否开启（密码框、sudo 提示、1Password 等会打开它）。
    pub fn is_secure_input_enabled() -> bool {
        unsafe { IsSecureEventInputEnabled() != 0 }
    }
}
pub use macos_impl::is_secure_input_enabled;
