use url::Url;

pub mod bilibili;
pub mod danmu_cache;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtcSourceMeta {
    pub domain: &'static str,
    pub identifier: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Danmu {
    pub id: i64,
    pub progress_ms: u32,
    pub mode: u8,
    pub fontsize: u8,
    pub color: u32,
    pub content: String,
}

pub fn try_resolve_path(path: &str) -> Option<SmtcSourceMeta> {
    let parsed_url = Url::parse(path).ok()?;
    let domain = parsed_url.domain();
    if let Some(domain) = domain {
        return resolve_path(domain, &parsed_url);
    } else {
        return None;
    }
}
pub fn resolve_path(domain: &str, parsed_url: &Url) -> Option<SmtcSourceMeta> {
    let domain = domain.strip_prefix("www.").unwrap_or(domain);
    if domain.ends_with(".bilibili.com") || domain == "bilibili.com" {
        return bilibili::resolve_path_bilibili(parsed_url);
    }
    None
}
