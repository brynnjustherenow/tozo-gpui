use std::ffi::c_void;
use std::sync::mpsc as std_mpsc;
use std::sync::OnceLock;
use std::thread;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use windows::core::{implement, GUID, PCWSTR, Result};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, AUDIO_VOLUME_NOTIFICATION_DATA,
    DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};

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
        /// 获取设备混音格式
        fn GetMixFormat(&self, device: PCWSTR, format: *mut *mut c_void) -> Result<()>;
        /// 获取设备音频格式
        fn GetDeviceFormat(
            &self,
            device: PCWSTR,
            default_format: BOOL,
            format: *mut *mut c_void,
        ) -> Result<()>;
        /// 重置设备格式
        fn ResetDeviceFormat(&self, device: PCWSTR) -> Result<()>;
        /// 设置设备音频格式
        fn SetDeviceFormat(
            &self,
            device: PCWSTR,
            endpoint_format: *mut c_void,
            mix_format: *mut c_void,
        ) -> Result<()>;
        /// 获取处理周期
        fn GetProcessingPeriod(
            &self,
            device: PCWSTR,
            default_period: *mut i64,
            min_period: *mut i64,
        ) -> Result<()>;
        /// 设置处理周期
        fn SetProcessingPeriod(&self, device: PCWSTR, period: i64) -> Result<()>;
        /// 获取共享模式
        fn GetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> Result<()>;
        /// 设置共享模式
        fn SetShareMode(&self, device: PCWSTR, mode: *mut c_void) -> Result<()>;
        /// 获取设备属性值
        fn GetPropertyValue(
            &self,
            device: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> Result<()>;
        /// 设置设备属性值
        fn SetPropertyValue(
            &self,
            device: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut c_void,
        ) -> Result<()>;
        /// 将设备设为指定角色的默认端点
        fn SetDefaultEndpoint(&self, device: PCWSTR, role: ERole) -> Result<()>;
        /// 设置端点可见性
        fn SetEndpointVisibility(&self, device: PCWSTR, visible: BOOL) -> Result<()>;
    }

    /// 创建 IPolicyConfig COM 实例
    pub fn create() -> windows::core::Result<IPolicyConfig> {
        unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }
    }

    /// 将设备设为 console 与 multimedia 角色的默认端点
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

pub enum Command {
    SetVolume(f32),
    SetMute(bool),
    SetDefault(String),
}

static CMD_TX: OnceLock<std_mpsc::Sender<Msg>> = OnceLock::new();

/// 发送音频控制命令到 worker 线程
pub fn submit(cmd: Command) {
    if let Some(tx) = CMD_TX.get() {
        let _ = tx.send(Msg::Cmd(cmd));
    }
}

enum Msg {
    Cmd(Command),
    DevicesChanged,
    VolumeChanged,
}

#[implement(IMMNotificationClient)]
struct DeviceNotify {
    tx: std_mpsc::Sender<Msg>,
}

impl IMMNotificationClient_Impl for DeviceNotify_Impl {
    /// 设备状态变化回调，通知刷新
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        let _ = self.tx.send(Msg::DevicesChanged);
        Ok(())
    }

    /// 设备新增回调，通知刷新
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        let _ = self.tx.send(Msg::DevicesChanged);
        Ok(())
    }

    /// 设备移除回调，通知刷新
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        let _ = self.tx.send(Msg::DevicesChanged);
        Ok(())
    }

    /// 默认设备变化回调，通知刷新
    fn OnDefaultDeviceChanged(
        &self,
        _flow: EDataFlow,
        _role: ERole,
        _id: &PCWSTR,
    ) -> Result<()> {
        let _ = self.tx.send(Msg::DevicesChanged);
        Ok(())
    }

    /// 属性值变化回调，当前忽略
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeNotify {
    tx: std_mpsc::Sender<Msg>,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeNotify_Impl {
    /// 端点音量变化回调，通知重读
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        let _ = self.tx.send(Msg::VolumeChanged);
        Ok(())
    }
}

pub struct AudioStore {
    pub state: AudioState,
}

impl AudioStore {
    /// 启动音频 worker 线程并转发状态更新到 UI
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (state_tx, mut state_rx) = unbounded::<AudioState>();
        let (msg_tx, msg_rx) = std_mpsc::channel::<Msg>();
        let _ = CMD_TX.set(msg_tx.clone());
        thread::Builder::new()
            .name("tozo-audio-worker".into())
            .spawn(move || worker_main(msg_tx, msg_rx, state_tx))
            .ok();
        cx.spawn(async move |this, cx| {
            while let Some(state) = state_rx.next().await {
                let _ = this.update(cx, |store, cx| {
                    store.state = state;
                    cx.notify();
                });
            }
        })
        .detach();
        Self {
            state: AudioState::default(),
        }
    }
}

impl Render for AudioStore {
    /// 渲染音量图标岛，点击打开音频弹窗
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = self.state.muted;
        let volume = self.state.volume;
        let icon = if muted {
            IconName::VolumeX
        } else if volume < 0.5 {
            IconName::Volume1
        } else {
            IconName::Volume2
        };
        let color = if muted {
            rgba(0xFFFFFF6E)
        } else {
            rgba(0xFFFFFFEB)
        };
        crate::sources::clock::pill()
            .id("audio-island")
            .px_2()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.defer(|cx| crate::audio_popup::toggle(cx));
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(2.))
                    .text_size(px(14.))
                    .text_color(color)
                    .child(icon),
            )
    }
}

/// 创建 MMDeviceEnumerator COM 实例
fn make_enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&CLSID_MM_DEVICE_ENUMERATOR, None, CLSCTX_ALL) }
}

/// 音频 worker 线程：注册设备与音量通知，处理命令并刷新状态
fn worker_main(
    msg_tx: std_mpsc::Sender<Msg>,
    msg_rx: std_mpsc::Receiver<Msg>,
    state_tx: futures::channel::mpsc::UnboundedSender<AudioState>,
) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let Ok(enumerator) = make_enumerator() else {
        return;
    };
    let device_notify: IMMNotificationClient = DeviceNotify {
        tx: msg_tx.clone(),
    }
    .into();
    unsafe {
        let _ = enumerator.RegisterEndpointNotificationCallback(&device_notify);
    }
    let volume_notify: IAudioEndpointVolumeCallback =
        VolumeNotify { tx: msg_tx.clone() }.into();

    let mut endpoint_volume: Option<IAudioEndpointVolume> = None;
    let mut bound_endpoint: Option<String> = None;
    let mut last = snapshot(&enumerator);

    let bind = |endpoint_volume: &mut Option<IAudioEndpointVolume>,
                bound_endpoint: &mut Option<String>| {
        let def = default_id(&enumerator, eRender);
        if def == *bound_endpoint {
            return;
        }
        if let Some(ep) = &*endpoint_volume {
            unsafe { let _ = ep.UnregisterControlChangeNotify(&volume_notify); }
        }
        *endpoint_volume = None;
        if let Some(ep) = endpoint_volume_of(&enumerator) {
            unsafe { let _ = ep.RegisterControlChangeNotify(&volume_notify); }
            *endpoint_volume = Some(ep);
        }
        *bound_endpoint = def;
    };
    bind(&mut endpoint_volume, &mut bound_endpoint);
    let _ = state_tx.unbounded_send(last.clone());

    while let Ok(msg) = msg_rx.recv() {
        match msg {
            Msg::Cmd(Command::SetVolume(v)) => {
                if let Some(ep) = &endpoint_volume {
                    unsafe {
                        let _ =
                            ep.SetMasterVolumeLevelScalar(v.clamp(0., 1.), std::ptr::null());
                    }
                }
            }
            Msg::Cmd(Command::SetMute(m)) => {
                if let Some(ep) = &endpoint_volume {
                    unsafe { let _ = ep.SetMute(m, std::ptr::null()); }
                }
            }
            Msg::Cmd(Command::SetDefault(id)) => {
                policy::set_default(&id);
            }
            Msg::DevicesChanged => {
                bind(&mut endpoint_volume, &mut bound_endpoint);
                last = snapshot(&enumerator);
                let _ = state_tx.unbounded_send(last.clone());
            }
            Msg::VolumeChanged => {
                if let Some(ep) = &endpoint_volume {
                    let (volume, muted) = unsafe {
                        (
                            ep.GetMasterVolumeLevelScalar().unwrap_or(last.volume),
                            ep.GetMute().map(|m| m.as_bool()).unwrap_or(last.muted),
                        )
                    };
                    last.volume = volume;
                    last.muted = muted;
                    let _ = state_tx.unbounded_send(last.clone());
                }
            }
        }
    }
}

/// 一次性读取音量、静音与活跃设备列表
fn snapshot(enumerator: &IMMDeviceEnumerator) -> AudioState {
    let default_out = default_id(enumerator, eRender);
    let default_in = default_id(enumerator, eCapture);
    let (volume, muted) = endpoint_volume_of(enumerator)
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

/// 取默认播放设备的音量控制接口
fn endpoint_volume_of(enumerator: &IMMDeviceEnumerator) -> Option<IAudioEndpointVolume> {
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()? };
    unsafe { device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok() }
}

/// 取指定数据流向的默认设备 id
fn default_id(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Option<String> {
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole).ok()? };
    device_id(&device)
}

/// 取设备 id 字符串并释放原生内存
fn device_id(device: &IMMDevice) -> Option<String> {
    let id = unsafe { device.GetId().ok()? };
    let text = unsafe { id.to_string().ok() };
    unsafe { CoTaskMemFree(Some(id.as_ptr() as *const c_void)) };
    text
}

/// 从属性存储读设备友好名称
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
