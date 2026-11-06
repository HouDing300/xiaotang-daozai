//! 抽烟记录与统计。
//!
//! 每点一次「抽了一根」就往 `config_dir()/smoke.json` 里追加一个 Unix 秒时间戳，
//! 设置页与岛上页面都从这里算今日根数、距上一根的间隔和近 7 天趋势。
//! 只保留最近 180 天，避免文件无限增长。

use crate::model::{atomic_write, config_dir};
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

const FILE: &str = "smoke.json";
/// 保留天数：够看半年趋势，又不至于让文件一直长。
const KEEP_DAYS: i64 = 180;

#[derive(Clone, Debug, Default)]
pub struct SmokeLog {
    /// 每次抽烟的 Unix 秒时间戳，按时间升序。
    pub events: Vec<i64>,
}

fn path() -> PathBuf {
    config_dir().join(FILE)
}

fn day_of(ts: i64) -> Option<NaiveDate> {
    Local.timestamp_opt(ts, 0).single().map(|t| t.date_naive())
}

impl SmokeLog {
    pub fn load() -> Self {
        let Ok(raw) = fs::read(path()) else {
            return Self::default();
        };
        Self::from_json(&serde_json::from_slice::<Value>(&raw).unwrap_or(Value::Null))
    }

    pub fn from_json(value: &Value) -> Self {
        let mut events: Vec<i64> = value
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .filter(|ts| *ts > 0)
            .collect();
        events.sort_unstable();
        events.dedup();
        Self { events }
    }

    pub fn to_json(&self) -> Value {
        json!({ "events": self.events })
    }

    pub fn save(&self) {
        let path = path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_vec_pretty(&self.to_json()) {
            let _ = atomic_write(&path, &data);
        }
    }

    /// 记一根，并顺手丢掉过期记录。
    pub fn add(&mut self, now: i64) {
        if now <= 0 {
            return;
        }
        self.events.push(now);
        self.events.sort_unstable();
        let cutoff = now - KEEP_DAYS * 86_400;
        self.events.retain(|ts| *ts >= cutoff);
    }

    /// 撤销最近一次记录，返回是否真的删掉了。
    pub fn undo(&mut self) -> bool {
        self.events.pop().is_some()
    }

    pub fn last_at(&self) -> Option<i64> {
        self.events.last().copied()
    }

    pub fn count_on(&self, day: NaiveDate) -> usize {
        self.events.iter().filter(|ts| day_of(**ts) == Some(day)).count()
    }

    pub fn today(&self, today: &str) -> usize {
        NaiveDate::parse_from_str(today, "%Y-%m-%d")
            .map(|day| self.count_on(day))
            .unwrap_or(0)
    }

    /// 距上一根多少分钟；没有记录时返回 None。
    pub fn minutes_since_last(&self, now: i64) -> Option<i64> {
        self.last_at().map(|last| ((now - last).max(0)) / 60)
    }

    /// 今天第一根到现在，平均每根间隔多少分钟（不足两根时为 None）。
    pub fn today_average_gap(&self, today: &str) -> Option<i64> {
        let day = NaiveDate::parse_from_str(today, "%Y-%m-%d").ok()?;
        let times: Vec<i64> = self.events.iter().copied().filter(|ts| day_of(*ts) == Some(day)).collect();
        if times.len() < 2 {
            return None;
        }
        let span = times.last()? - times.first()?;
        (span > 0).then(|| span / (times.len() as i64 - 1) / 60)
    }

    /// 最近 `days` 天（含今天）的每日根数，从早到晚。
    /// 岛面每帧都会画这张图，所以只扫一遍记录，不按天重复遍历整个日志。
    pub fn recent_days(&self, today: &str, days: usize) -> Vec<(String, usize)> {
        let Ok(end) = NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
            return Vec::new();
        };
        let list: Vec<NaiveDate> = (0..days)
            .rev()
            .filter_map(|back| end.checked_sub_signed(chrono::Duration::days(back as i64)))
            .collect();
        let mut counts = vec![0usize; list.len()];
        for ts in &self.events {
            if let Some(day) = day_of(*ts) {
                if let Ok(index) = list.binary_search(&day) {
                    counts[index] += 1;
                }
            }
        }
        list.into_iter()
            .zip(counts)
            .map(|(day, count)| (day.format("%m-%d").to_string(), count))
            .collect()
    }

    pub fn total(&self) -> usize {
        self.events.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: &str, hour: u32, minute: u32) -> i64 {
        let date = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
        Local
            .with_ymd_and_hms(date.year(), date.month(), date.day(), hour, minute, 0)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn counts_today_and_average_gap() {
        let mut log = SmokeLog::default();
        log.add(at("2026-11-06", 9, 0));
        log.add(at("2026-11-06", 11, 0));
        log.add(at("2026-11-06", 13, 0));
        log.add(at("2026-11-05", 20, 0));
        assert_eq!(log.today("2026-11-06"), 3);
        assert_eq!(log.today("2026-11-05"), 1);
        assert_eq!(log.today_average_gap("2026-11-06"), Some(120));
        assert_eq!(log.minutes_since_last(at("2026-11-06", 14, 0)), Some(60));
    }

    #[test]
    fn recent_days_are_ordered_and_padded() {
        let mut log = SmokeLog::default();
        log.add(at("2026-11-06", 9, 0));
        log.add(at("2026-11-04", 9, 0));
        let days = log.recent_days("2026-11-06", 7);
        assert_eq!(days.len(), 7);
        assert_eq!(days.last().unwrap(), &("11-06".to_string(), 1));
        assert_eq!(days[4], ("11-04".to_string(), 1));
        assert_eq!(days[5], ("11-05".to_string(), 0));
    }

    #[test]
    fn undo_removes_only_the_last_entry() {
        let mut log = SmokeLog::default();
        log.add(100);
        log.add(200);
        assert!(log.undo());
        assert_eq!(log.events, vec![100]);
        assert!(log.undo());
        assert!(!log.undo());
    }

    #[test]
    fn json_roundtrip_drops_invalid_entries() {
        let log = SmokeLog::from_json(&json!({"events":[3,-1,0,"x",3,5]}));
        assert_eq!(log.events, vec![3, 5]);
        assert_eq!(log.to_json()["events"].as_array().unwrap().len(), 2);
    }
}
