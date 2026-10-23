use windows::core::Result;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, 
    COINIT_APARTMENTTHREADED,
};
pub struct ComGuard {}

impl ComGuard {
    /// 以 STA 模式初始化 COM
    pub fn new() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }

        Ok(Self {})
    }
}
impl Drop for ComGuard {
    /// 调用 CoUninitialize 反初始化 COM
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
