use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;
use super::clock::pill;
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6TABLE_OWNER_MODULE, MIB_TCPTABLE_OWNER_MODULE,
    MIB_UDP6TABLE_OWNER_MODULE, MIB_UDPTABLE_OWNER_MODULE, TCP_TABLE_OWNER_MODULE_ALL,
    UDP_TABLE_OWNER_MODULE,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn as_str(self) -> &'static str {
        match self {
            Proto::Tcp => "TCP",
            Proto::Udp => "UDP",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PortEntry {
    pub port: u16,
    pub addr: String,
    pub pid: u32,
    pub proto: Proto,
}

pub struct PortStore {
    pub entries: Vec<PortEntry>,
    pub count: usize,
}

impl PortStore {
    /// 每 3s 后台扫描 TCP 监听表，变化时打增删日志并更新岛数据
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            let mut prev: Vec<PortEntry> = Vec::new();
            loop {
                let entries = cx.background_executor().spawn(async move { scan() }).await;
                if entries != prev {
                    for e in entries.iter().filter(|e| !prev.contains(e)) {
                        tracing::debug!("[ports] + {} {}:{} pid={}", e.proto.as_str(), e.addr, e.port, e.pid);
                    }
                    for e in prev.iter().filter(|e| !entries.contains(e)) {
                        tracing::debug!("[ports] - {} {}:{} pid={}", e.proto.as_str(), e.addr, e.port, e.pid);
                    }
                }
                prev = entries.clone();
                let count = entries
                    .iter()
                    .map(|e| e.port)
                    .collect::<HashSet<_>>()
                    .len();
                if this
                    .update(cx, |store, cx| {
                        store.entries = entries;
                        store.count = count;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(3))
                    .await;
            }
        })
        .detach();
        Self {
            entries: Vec::new(),
            count: 0,
        }
    }
}

impl Render for PortStore {
    /// 渲染端口岛：绿点加监听端口数，点击开端口弹窗
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.count == 0 {
            return div().id("ports-empty").into_any_element();
        }
        pill()
            .id("ports-island")
            .gap(px(4.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.defer(|cx| crate::ports_popup::toggle(cx));
                }),
            )
            .child(div().size(px(6.)).rounded_full().bg(rgba(0x4ADE80FF)))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgba(0xFFFFFFEB))
                    .child(self.count.to_string()),
            )
            .into_any_element()
    }
}

/// 两段式查询扩展表：先探 size 再填充；表在两次调用间变大（rc=122）时
/// 按新 size 扩容重试，避免竞态丢整族数据
fn query_table(
    mut call: impl FnMut(Option<*mut core::ffi::c_void>, &mut u32) -> u32,
) -> Option<Vec<u8>> {
    let mut size = 0u32;
    let _ = call(None, &mut size);
    if size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    for _ in 0..4 {
        let rc = call(Some(buf.as_mut_ptr() as *mut core::ffi::c_void), &mut size);
        if rc == 0 {
            return Some(buf);
        }
        if rc == 122 && size as usize > buf.len() {
            buf.resize(size as usize, 0);
            continue;
        }
        return None;
    }
    None
}

/// 扫描 IPv4/IPv6 的 TCP（仅 LISTEN）与 UDP 监听表，按端口排序、
/// (地址, 端口, 协议) 去重并过滤系统侧监听
fn scan() -> Vec<PortEntry> {
    let mut out = Vec::new();
    for family in [AF_INET.0 as u32, AF_INET6.0 as u32] {
        let is_v4 = family == AF_INET.0 as u32;

        // TCP：仅取 LISTEN（dwState == 2）行
        let tcp = query_table(|ptr, size| unsafe {
            GetExtendedTcpTable(ptr, size, false, family, TCP_TABLE_OWNER_MODULE_ALL, 0)
        });
        if let Some(buf) = tcp {
            unsafe {
                if is_v4 {
                    let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_MODULE);
                    let rows = std::slice::from_raw_parts(&table.table[0], table.dwNumEntries as usize);
                    for row in rows {
                        if row.dwState != 2 {
                            continue;
                        }
                        out.push(PortEntry {
                            port: flip_port(row.dwLocalPort),
                            addr: format_v4(row.dwLocalAddr),
                            pid: row.dwOwningPid,
                            proto: Proto::Tcp,
                        });
                    }
                } else {
                    let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_MODULE);
                    let rows = std::slice::from_raw_parts(&table.table[0], table.dwNumEntries as usize);
                    for row in rows {
                        if row.dwState != 2 {
                            continue;
                        }
                        out.push(PortEntry {
                            port: flip_port(row.dwLocalPort),
                            addr: format_v6(&row.ucLocalAddr),
                            pid: row.dwOwningPid,
                            proto: Proto::Tcp,
                        });
                    }
                }
            }
        }

        // UDP：无状态位，全部行都是"监听"
        let udp = query_table(|ptr, size| unsafe {
            GetExtendedUdpTable(ptr, size, false, family, UDP_TABLE_OWNER_MODULE, 0)
        });
        if let Some(buf) = udp {
            unsafe {
                if is_v4 {
                    let table = &*(buf.as_ptr() as *const MIB_UDPTABLE_OWNER_MODULE);
                    let rows = std::slice::from_raw_parts(&table.table[0], table.dwNumEntries as usize);
                    for row in rows {
                        out.push(PortEntry {
                            port: flip_port(row.dwLocalPort),
                            addr: format_v4(row.dwLocalAddr),
                            pid: row.dwOwningPid,
                            proto: Proto::Udp,
                        });
                    }
                } else {
                    let table = &*(buf.as_ptr() as *const MIB_UDP6TABLE_OWNER_MODULE);
                    let rows = std::slice::from_raw_parts(&table.table[0], table.dwNumEntries as usize);
                    for row in rows {
                        out.push(PortEntry {
                            port: flip_port(row.dwLocalPort),
                            addr: format_v6(&row.ucLocalAddr),
                            pid: row.dwOwningPid,
                            proto: Proto::Udp,
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.port
            .cmp(&b.port)
            .then_with(|| (a.proto == Proto::Udp).cmp(&(b.proto == Proto::Udp)))
            .then_with(|| a.addr.cmp(&b.addr))
    });
    // A tearing-down listener can coexist with its replacement on the same
    // addr:port, with OwningPid already zeroed; keep the live row.
    let mut deduped: Vec<PortEntry> = Vec::with_capacity(out.len());
    for entry in out {
        match deduped
            .iter_mut()
            .find(|e| e.addr == entry.addr && e.port == entry.port && e.proto == entry.proto)
        {
            Some(existing) => {
                if existing.pid == 0 && entry.pid != 0 {
                    *existing = entry;
                }
            }
            None => deduped.push(entry),
        }
    }
    // [tozo] 过滤系统侧监听噪音：pid 0 = 销毁中的僵尸行（无 pid!=0 孪生活行时
    // 会残留在表里），pid 4 = System 进程（135/445 等 OS 固有服务）。
    // 两类都显示为 "System"，对用户无信息量。
    deduped.retain(|e| e.pid != 0 && e.pid != 4);
    deduped
}

/// 网络序 16 位端口转主机序
fn flip_port(v: u32) -> u16 {
    (((v & 0xff) << 8) | ((v >> 8) & 0xff)) as u16
}

/// u32 地址（小端序）转点分十进制字符串
fn format_v4(addr: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        addr & 0xff,
        (addr >> 8) & 0xff,
        (addr >> 16) & 0xff,
        (addr >> 24) & 0xff
    )
}

/// 16 字节地址转冒号分隔十六进制（不压缩零段）
fn format_v6(bytes: &[u8; 16]) -> String {
    let groups: Vec<String> = bytes
        .chunks(2)
        .map(|c| format!("{:02x}{:02x}", c[0], c[1]))
        .collect();
    groups.join(":")
}
