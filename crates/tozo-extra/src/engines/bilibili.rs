use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use std::path::PathBuf;

use crate::engines::Danmu;
use prost::Message;
use serde_json::Value;
use url::Url;

use crate::engines::SmtcSourceMeta;

static WARMED: AtomicBool = AtomicBool::new(false);

pub fn resolve_path_bilibili(parsed_url: &Url) -> Option<SmtcSourceMeta> {
    let segs = parsed_url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();

    match segs.last() {
        Some(bv) if bv.starts_with("BV") => Some(SmtcSourceMeta {
            domain: "bilibili.com",
            identifier: bv.to_string(),
        }),
        _ => None,
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DmSegMobileReply {
    #[prost(message, repeated, tag = "1")]
    pub elems: Vec<DanmakuElem>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DanmakuElem {
    #[prost(int64, tag = "1")]
    pub id: i64,
    #[prost(int32, tag = "2")]
    pub progress: i32,
    #[prost(int32, tag = "3")]
    pub mode: i32,
    #[prost(int32, tag = "4")]
    pub fontsize: i32,
    #[prost(uint32, tag = "5")]
    pub color: u32,
    #[prost(string, tag = "7")]
    pub content: String,
    #[prost(int32, tag = "11")]
    pub pool: i32,
}

const COOKIE_FILES: [&str; 2] = ["bilibili.tcookie", "bilibili.cookie"];

fn parse_cookie_file(raw: &str) -> Option<String> {
    let mut pairs: Vec<String> = Vec::new();
    if raw.contains('\t') {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 7 {
                let domain = parts[0].trim_start_matches('.');
                let (name, value) = (parts[5], parts[6]);
                if domain.ends_with("bilibili.com") && !name.is_empty() {
                    pairs.push(format!("{name}={value}"));
                }
            }
        }
    } else {
        for line in raw.lines() {
            let line = line.trim().trim_end_matches(';').trim();
            if line.is_empty() || line.starts_with('#') || !line.contains('=') {
                continue;
            }
            pairs.push(line.to_string());
        }
    }
    (!pairs.is_empty()).then(|| pairs.join("; "))
}

fn load_cookie_header() -> Option<String> {
    // 依次尝试当前目录与 exe 同目录（gui 子系统下 CWD 不可靠）
    let mut candidates = COOKIE_FILES.map(PathBuf::from).to_vec();
    if let Ok(dir) = std::env::current_exe() {
        if let Some(dir) = dir.parent() {
            for name in COOKIE_FILES {
                candidates.push(dir.join(name));
            }
        }
    }
    for path in candidates {
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Some(header) = parse_cookie_file(&raw) {
                tracing::info!("[danmu] cookie file `{}` loaded", path.display());
                return Some(header);
            }
        }
    }
    None
}

fn cookie_jar(header: &Option<String>) -> reqwest::cookie::Jar {
    let jar = reqwest::cookie::Jar::default();
    if let Some(header) = header {
        let url = "https://www.bilibili.com".parse().unwrap();
        for pair in header.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            jar.add_cookie_str(&format!("{pair}; Domain=.bilibili.com; Path=/"), &url);
        }
    }
    jar
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        let jar = cookie_jar(&load_cookie_header());
        reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
            .cookie_provider(std::sync::Arc::new(jar))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

async fn warmup() {
    if WARMED.load(Ordering::Relaxed) {
        return;
    }
    let _ = client().get("https://www.bilibili.com/").send().await;
    let nav = client()
        .get("https://api.bilibili.com/x/web-interface/nav")
        .send()
        .await;
    if let Ok(resp) = nav {
        if let Ok(value) = resp.json::<Value>().await {
            let logged = value["data"]["isLogin"].as_bool().unwrap_or(false);
            let name = value["data"]["uname"].as_str().unwrap_or("");
            tracing::info!("[danmu] bilibili login: {logged} {name}");
        }
    }
    WARMED.store(true, Ordering::Relaxed);
}

pub async fn fetch_cid(bvid: &str) -> Result<(i64, i64), String> {
    warmup().await;
    let url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
    let body = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    if value["code"].as_i64() != Some(0) {
        return Err(format!("view api code={}", value["code"]));
    }
    let aid = value["data"]["aid"].as_i64().ok_or("missing aid")?;
    let cid = value["data"]["cid"].as_i64().ok_or("missing cid")?;
    Ok((aid, cid))
}

pub async fn fetch_segment(aid: i64, cid: i64, segment_index: u32) -> Result<Vec<Danmu>, String> {
    warmup().await;
    let url = format!(
        "https://api.bilibili.com/x/v2/dm/web/seg.so?type=1&oid={cid}&pid={aid}&segment_index={segment_index}"
    );
    let bytes = client()
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .bytes()
        .await
        .map_err(|e| e.to_string())?;
    let reply = DmSegMobileReply::decode(bytes.as_ref()).map_err(|e| e.to_string())?;
    let mut items: Vec<Danmu> = reply
        .elems
        .into_iter()
        .filter(|e| e.pool == 0 && (1..=3).contains(&e.mode) && e.progress >= 0)
        .map(|e| Danmu {
            id: e.id,
            progress_ms: e.progress as u32,
            mode: e.mode as u8,
            fontsize: e.fontsize.clamp(0, 255) as u8,
            color: e.color,
            content: e.content,
        })
        .collect();
    items.sort_by_key(|d| d.progress_ms);
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::parse_cookie_file;

    #[test]
    fn parses_netscape_format() {
        let raw = "# Netscape HTTP Cookie File\n.bilibili.com\tTRUE\t/\tTRUE\t1893456000\tSESSDATA\tabc%2Fdef\n.bilibili.com\tTRUE\t/\tTRUE\t1893456000\tbuvid3\txyz\n.example.com\tTRUE\t/\tTRUE\t1893456000\tother\t1\n";
        assert_eq!(
            parse_cookie_file(raw).as_deref(),
            Some("SESSDATA=abc%2Fdef; buvid3=xyz")
        );
    }

    #[test]
    fn parses_header_string() {
        assert_eq!(
            parse_cookie_file("SESSDATA=abc; buvid3=xyz;").as_deref(),
            Some("SESSDATA=abc; buvid3=xyz")
        );
    }

    #[test]
    fn empty_file_yields_none() {
        assert_eq!(parse_cookie_file(""), None);
        assert_eq!(parse_cookie_file("# only comments"), None);
    }
}
