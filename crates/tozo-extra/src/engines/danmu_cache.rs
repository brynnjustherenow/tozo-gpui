use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use crate::engines::Danmu;

pub const SEGMENT_MS: u64 = 360_000;
const PREFETCH_WINDOW_MS: u64 = 30_000;
pub const FETCH_COOLDOWN_SECS: u64 = 30;
const SEEK_BACK_THRESHOLD_MS: u64 = 2_000;

pub struct DanmuCache {
    aid: i64,
    cid: i64,
    segments: BTreeMap<u32, Vec<Danmu>>,
    cursors: HashMap<u32, usize>,
    failed: HashMap<u32, Instant>,
    last_t: u64,
}

impl DanmuCache {
    pub fn new(aid: i64, cid: i64) -> Self {
        Self {
            aid,
            cid,
            segments: BTreeMap::new(),
            cursors: HashMap::new(),
            failed: HashMap::new(),
            last_t: 0,
        }
    }

    pub fn ids(&self) -> (i64, i64) {
        (self.aid, self.cid)
    }

    pub fn segment_index(t_ms: u64) -> u32 {
        (t_ms / SEGMENT_MS) as u32 + 1
    }

    fn missing(&self, index: u32) -> bool {
        if self.segments.contains_key(&index) {
            return false;
        }
        self.failed
            .get(&index)
            .is_none_or(|at| at.elapsed().as_secs() >= FETCH_COOLDOWN_SECS)
    }

    pub fn wanted(&self, t_ms: u64) -> Vec<u32> {
        let cur = Self::segment_index(t_ms);
        let mut wanted = vec![cur];
        if t_ms % SEGMENT_MS + PREFETCH_WINDOW_MS >= SEGMENT_MS {
            wanted.push(cur + 1);
        }
        wanted.retain(|&index| index != 0 && self.missing(index));
        wanted
    }

    pub fn insert(&mut self, index: u32, list: Vec<Danmu>) {
        self.segments.insert(index, list);
        self.failed.remove(&index);
    }

    pub fn fail(&mut self, index: u32) {
        self.failed.insert(index, Instant::now());
    }

    fn reset_cursors(&mut self, t_ms: u64) {
        for (seg, list) in &self.segments {
            let cursor = list.partition_point(|d| d.progress_ms as u64 <= t_ms);
            self.cursors.insert(*seg, cursor);
        }
    }

    fn evict_before(&mut self, floor: u32) {
        self.segments.retain(|&seg, _| seg >= floor);
        self.cursors.retain(|&seg, _| seg >= floor);
        self.failed.retain(|&seg, _| seg >= floor);
    }

    pub fn advance(&mut self, t_ms: u64) -> Vec<Danmu> {
        if t_ms + SEEK_BACK_THRESHOLD_MS < self.last_t {
            self.reset_cursors(t_ms);
        }
        self.last_t = t_ms;

        let cur = Self::segment_index(t_ms);
        let floor = cur.saturating_sub(1);
        self.evict_before(floor);

        let mut spawned = Vec::new();
        for seg in [floor, cur] {
            if seg == 0 {
                continue;
            }
            let Some(list) = self.segments.get(&seg) else {
                continue;
            };
            let cursor = self.cursors.entry(seg).or_insert(0);
            while *cursor < list.len() && list[*cursor].progress_ms as u64 <= t_ms {
                spawned.push(list[*cursor].clone());
                *cursor += 1;
            }
        }
        spawned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn danmu(id: i64, progress_ms: u32) -> Danmu {
        Danmu {
            id,
            progress_ms,
            mode: 1,
            fontsize: 25,
            color: 0xFFFFFF,
            content: format!("d{id}"),
        }
    }

    fn cache_with(segments: Vec<(u32, Vec<Danmu>)>) -> DanmuCache {
        let mut cache = DanmuCache::new(1, 2);
        for (idx, list) in segments {
            cache.insert(idx, list);
        }
        cache
    }

    fn ids(items: &[Danmu]) -> Vec<i64> {
        items.iter().map(|d| d.id).collect()
    }

    #[test]
    fn spawns_in_order_without_repeats() {
        let mut cache =
            cache_with(vec![(1, vec![danmu(1, 1000), danmu(2, 2500), danmu(3, 4000)])]);
        assert_eq!(ids(&cache.advance(1500)), vec![1]);
        assert_eq!(ids(&cache.advance(3000)), vec![2]);
        assert_eq!(ids(&cache.advance(3000)), Vec::<i64>::new());
        assert_eq!(ids(&cache.advance(5000)), vec![3]);
    }

    #[test]
    fn spans_segment_boundary() {
        let mut cache = cache_with(vec![
            (1, vec![danmu(1, 359_000)]),
            (2, vec![danmu(2, 360_500), danmu(3, 361_000)]),
            (3, vec![]),
        ]);
        let spawned = cache.advance(360_800);
        assert_eq!(ids(&spawned), vec![1, 2]);
        assert_eq!(ids(&cache.advance(361_500)), vec![3]);
    }

    #[test]
    fn seek_back_skips_landing_burst_then_replays() {
        let mut cache =
            cache_with(vec![(1, vec![danmu(1, 1000), danmu(2, 5000), danmu(3, 9000)])]);
        assert_eq!(ids(&cache.advance(6000)), vec![1, 2]);
        cache.advance(12_000);
        let spawned = cache.advance(2000);
        assert!(spawned.is_empty());
        assert_eq!(ids(&cache.advance(9500)), vec![2, 3]);
    }

    #[test]
    fn wanted_reports_missing_with_prefetch() {
        let cache = cache_with(vec![(1, vec![])]);
        assert_eq!(cache.wanted(1000), Vec::<u32>::new());
        assert_eq!(cache.wanted(340_000), vec![2]);
        let mut cache = DanmuCache::new(1, 2);
        assert_eq!(cache.wanted(1000), vec![1]);
        cache.fail(1);
        assert_eq!(cache.wanted(1000), Vec::<u32>::new());
    }
}
