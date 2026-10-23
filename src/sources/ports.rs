use std::collections::HashSet;
use std::time::Duration;

use gpui_kit::prelude::*;
use gpui_kit::*;
use super::clock::pill;
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6TABLE_OWNER_MODULE, MIB_TCPTABLE_OWNER_MODULE,
    TCP_TABLE_OWNER_MODULE_ALL,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

#[derive(Clone, Debug, PartialEq)]
pub struct PortEntry {
    pub port: u16,
    pub addr: String,
    pub pid: u32,
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
                        tracing::info!("[ports] + {}:{} pid={}", e.addr, e.port, e.pid);
                    }
                    for e in prev.iter().filter(|e| !entries.contains(e)) {
                        tracing::info!("[ports] - {}:{} pid={}", e.addr, e.port, e.pid);
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

/// 扫描 IPv4/IPv6 的 TCP 监听表，按端口排序并按 (地址, 端口) 去重
fn scan() -> Vec<PortEntry> {
    let mut out = Vec::new();
    unsafe {
        for family in [AF_INET.0 as u32, AF_INET6.0 as u32] {
            let mut size = 0u32;
            let _ =
                GetExtendedTcpTable(None, &mut size, false, family, TCP_TABLE_OWNER_MODULE_ALL, 0);
            if size == 0 {
                continue;
            }
            let mut buf = vec![0u8; size as usize];
            // The table can grow between the size probe and the fill call
            // (ERROR_INSUFFICIENT_BUFFER); regrow and retry instead of
            // dropping the whole family for this beat.
            let mut ok = false;
            for _ in 0..4 {
                let rc = GetExtendedTcpTable(
                    Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                    &mut size,
                    false,
                    family,
                    TCP_TABLE_OWNER_MODULE_ALL,
                    0,
                );
                if rc == 0 {
                    ok = true;
                    break;
                }
                if rc == 122 && size as usize > buf.len() {
                    buf.resize(size as usize, 0);
                    continue;
                }
                break;
            }
            if !ok {
                continue;
            }
            if family == AF_INET.0 as u32 {
                let table = &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_MODULE);
                let n = table.dwNumEntries as usize;
                if n == 0 {
                    continue;
                }
                let rows = std::slice::from_raw_parts(&table.table[0], n);
                for row in rows {
                    if row.dwState != 2 {
                        continue;
                    }
                    out.push(PortEntry {
                        port: flip_port(row.dwLocalPort),
                        addr: format_v4(row.dwLocalAddr),
                        pid: row.dwOwningPid,
                    });
                }
            } else {
                let table = &*(buf.as_ptr() as *const MIB_TCP6TABLE_OWNER_MODULE);
                let n = table.dwNumEntries as usize;
                if n == 0 {
                    continue;
                }
                let rows = std::slice::from_raw_parts(&table.table[0], n);
                for row in rows {
                    if row.dwState != 2 {
                        continue;
                    }
                    out.push(PortEntry {
                        port: flip_port(row.dwLocalPort),
                        addr: format_v6(&row.ucLocalAddr),
                        pid: row.dwOwningPid,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.port.cmp(&b.port).then_with(|| a.addr.cmp(&b.addr)));
    // A tearing-down listener can coexist with its replacement on the same
    // addr:port, with OwningPid already zeroed; keep the live row.
    let mut deduped: Vec<PortEntry> = Vec::with_capacity(out.len());
    for entry in out {
        match deduped
            .iter_mut()
            .find(|e| e.addr == entry.addr && e.port == entry.port)
        {
            Some(existing) => {
                if existing.pid == 0 && entry.pid != 0 {
                    *existing = entry;
                }
            }
            None => deduped.push(entry),
        }
    }
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
