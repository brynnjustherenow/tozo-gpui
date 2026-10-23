use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::watch;
use windows::core::GUID;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IMMDevice, IMMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

use crate::scheduler::{scheduler, Schedule};

mod policy {
    #![allow(dead_code, non_snake_case)]

    use std::ffi::c_void;

    use windows::core::{interface, BOOL, GUID, PCWSTR, Result};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{eConsole, eMultimedia, ERole};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

    const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: windows::core::IUnknown {
        /// 查询设备的混音格式
        fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut c_void) -> Result<()>;
        /// 查询设备格式，可请求默认格式
        fn GetDeviceFormat(
            &self,
            device: PCWSTR,
            default_format: BOOL,
            format: *mut *mut c_void,
        ) -> Result<()>;
        /// 重置设备格式为默认值
        fn ResetDeviceFormat(&self, device: PCWSTR) -> Result<()>;
        /// 设置设备格式
        fn SetDeviceFormat(
            &self,
            device: PCWSTR,
            endpoint_format: *mut c_void,
            mix_format: *mut c_void,
        ) -> Result<()>;
        /// 查询设备的处理周期
        fn GetProcessingPeriod(
            &self,
            device: PCWSTR,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> Result<()>;
        /// 设置设备的处理周期
        fn SetProcessingPeriod(&self, device: PCWSTR, period: i64) -> Result<()>;
        /// 查询设备的共享模式
        fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> Result<()>;
        /// 设置设备的共享模式
        fn SetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> Result<()>;
        /// 读取设备的指定属性值
        fn GetPropertyValue(
            &self,
            device: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> Result<()>;
        /// 写入设备的指定属性值
        fn SetPropertyValue(
            &self,
            device: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> Result<()>;
        /// 将设备设为指定角色的默认端点
        fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> Result<()>;
        /// 设置端点是否可见
        fn SetEndpointVisibility(&self, device: PCWSTR, visible: BOOL) -> Result<()>;
    }

    /// 创建 IPolicyConfig COM 实例
    pub fn create() -> windows::core::Result<IPolicyConfig> {
        unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
    }

    /// 将设备设为 Console 与 Multimedia 角色的默认端点
    pub fn set_default(id: &str) {
        use windows::core::HSTRING;
        let Ok(policy) = create() else {
            return;
        };
        let wide = HSTRING::from(id);
        let device = PCWSTR::from_raw(wide.as_ptr());
        unsafe {
            let _ = policy.SetDefaultEndpoint(device, eConsole);
            let _ = policy.SetDefaultEndpoint(device, eMultimedia);
        }
    }
}

const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xbcde0395_e52f_467c_8e3d_c4579291692e);

const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

#[derive(Clone, Debug, PartialEq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub input: bool,
    pub is_default: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioState {
    pub volume: f32,
    pub muted: bool,
    pub devices: Vec<AudioDevice>,
}

static AUDIO_RX: OnceLock<watch::Receiver<AudioState>> = OnceLock::new();

/// 返回音频状态 watch 流，250ms 轮询，首次调用时初始化
pub fn audio_stream() -> watch::Receiver<AudioState> {
    AUDIO_RX
        .get_or_init(|| {
            scheduler().poll(
                "audio",
                Schedule::Interval(Duration::from_millis(250)),
                AudioState::default(),
                snapshot,
            )
        })
        .clone()
}

/// 设置默认播放端点的主音量，钳制到 0 到 1
pub fn set_volume(volume: f32) {
    if let Some(endpoint) = endpoint_of(eRender) {
        unsafe {
            let _ = endpoint.SetMasterVolumeLevelScalar(volume.clamp(0., 1.), std::ptr::null());
        }
    }
}

/// 设置默认播放端点的静音状态
pub fn set_mute(muted: bool) {
    if let Some(endpoint) = endpoint_of(eRender) {
        unsafe {
            let _ = endpoint.SetMute(muted, std::ptr::null());
        }
    }
}

/// 将指定设备设为默认音频端点
pub fn set_default_device(id: &str) {
    ensure_mta();
    policy::set_default(id);
}

/// 采集音量、静音与全部活跃设备列表的快照
fn snapshot() -> AudioState {
    ensure_mta();
    let Ok(enumerator) = enumerator() else {
        return AudioState::default();
    };
    let default_out = default_id(&enumerator, eRender);
    let default_in = default_id(&enumerator, eCapture);
    let (volume, muted) = endpoint_of(eRender)
        .map(|endpoint| unsafe {
            (
                endpoint.GetMasterVolumeLevelScalar().unwrap_or(0.),
                endpoint.GetMute().map(|m| m.as_bool()).unwrap_or(false),
            )
        })
        .unwrap_or((0., false));
    let mut devices = Vec::new();
    for (flow, input, default) in [
        (eRender, false, default_out.as_deref()),
        (eCapture, true, default_in.as_deref()),
    ] {
        let Ok(collection) = (unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) })
        else {
            continue;
        };
        let count = unsafe { collection.GetCount() }.unwrap_or(0);
        for i in 0..count {
            let Ok(device) = (unsafe { collection.Item(i) }) else {
                continue;
            };
            let Some(id) = device_id(&device) else {
                continue;
            };
            devices.push(AudioDevice {
                is_default: Some(id.as_str()) == default,
                name: device_name(&device),
                input,
                id,
            });
        }
    }
    AudioState {
        volume,
        muted,
        devices,
    }
}

/// 在当前线程初始化 COM 多线程单元，仅执行一次
fn ensure_mta() {
    thread_local! {
        static INIT: () = unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); };
    }
    INIT.with(|_| ());
}

/// 创建 MMDevice 枚举器 COM 实例
fn enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    ensure_mta();
    unsafe { CoCreateInstance(&CLSID_MM_DEVICE_ENUMERATOR, None, CLSCTX_ALL) }
}

/// 获取指定流向默认设备的端点音量控制接口
fn endpoint_of(flow: EDataFlow) -> Option<IAudioEndpointVolume> {
    let enumerator = enumerator().ok()?;
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole).ok()? };
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
}

/// 查询指定流向的默认设备 id
fn default_id(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Option<String> {
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole).ok()? };
    device_id(&device)
}

/// 读取设备 id 字符串并释放 COM 分配的内存
fn device_id(device: &IMMDevice) -> Option<String> {
    let id = unsafe { device.GetId().ok()? };
    let text = unsafe { id.to_string().ok() };
    unsafe { CoTaskMemFree(Some(id.as_ptr() as *const c_void)) };
    text
}

/// 从设备属性库读取友好名称，失败返回空串
fn device_name(device: &IMMDevice) -> String {
    let Ok(store) = (unsafe { device.OpenPropertyStore(STGM_READ) }) else {
        return String::new();
    };
    let mut value = match unsafe { store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) } {
        Ok(value) => value,
        Err(_) => return String::new(),
    };
    let name = match unsafe { PropVariantToStringAlloc(&value) } {
        Ok(text) => {
            let string = unsafe { text.to_string() }.unwrap_or_default();
            unsafe { CoTaskMemFree(Some(text.as_ptr() as *const c_void)) };
            string
        }
        Err(_) => String::new(),
    };
    unsafe {
        let _ = PropVariantClear(&mut value);
    }
    name
}
