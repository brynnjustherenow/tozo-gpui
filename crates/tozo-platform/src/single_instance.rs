use std::thread;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE,
    INFINITE,
};

const MUTEX_NAME: PCWSTR = w!("tozo.instance");
const ACTIVATION_EVENT_NAME: PCWSTR = w!("tozo.activation");

pub struct SingleInstance {
    mutex: HANDLE,
}

impl SingleInstance {
    /// 创建命名互斥体，已存在则视为已有实例
    pub fn acquire() -> Option<Self> {
        unsafe {
            let mutex = CreateMutexW(None, false, MUTEX_NAME).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(mutex);
                None
            } else {
                Some(SingleInstance { mutex })
            }
        }
    }
}

impl Drop for SingleInstance {
    /// 关闭互斥体句柄
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.mutex);
        }
    }
}

/// 后台线程等待激活事件并回调
pub fn listen_for_activation<F>(on_activate: F)
where
    F: Fn() + Send + 'static,
{
    thread::spawn(move || unsafe {
        let Ok(event) = CreateEventW(None, false, false, ACTIVATION_EVENT_NAME) else {
            return;
        };
        loop {
            if WaitForSingleObject(event, INFINITE) != WAIT_OBJECT_0 {
                break;
            }
            on_activate();
        }
    });
}

/// 置位激活事件唤醒首个实例
pub fn signal_activation() {
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, ACTIVATION_EVENT_NAME) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        }
    }
}
