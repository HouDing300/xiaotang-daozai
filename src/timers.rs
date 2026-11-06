//! 自定义定时提醒：间隔循环 / 每日固定时刻，可设标题、正文、生效时段与开关。
//!
//! 提醒列表保存在 `config_dir()/timers.json`，与设置、待办等数据一起随数据目录迁移。
//! 触发判定集中在 `TimerList::take_due`，不依赖界面，便于回归测试。

use crate::app::{App, Env};
use crate::model::{atomic_write, config_dir};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

const FILE: &str = "timers.json";
/// 内置「站立提醒」的固定 id：设置里是独立开关，不算自定义提醒。
pub const STAND_ID: &str = "stand-preset";
/// 「专注与定时」分组下与专注(2)、饮水(3)并列的两个子页（0..11 是原有子页）。
pub const STAND_TAB: usize = 12;
pub const CUSTOM_TAB: usize = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerMode {
    /// 每隔 N 分钟提醒一次。
    Interval { minutes: u32 },
    /// 每天固定时刻提醒一次。
    Daily { hour: u32, minute: u32 },
}

#[derive(Clone, Debug)]
pub struct TimerItem {
    pub id: String,
    pub title: String,
    pub body: String,
    pub enabled: bool,
    pub mode: TimerMode,
    /// 生效时段（小时）；start == end 表示全天。
    pub start_hour: u32,
    pub end_hour: u32,
    /// 上次触发的 Unix 秒；0 表示新增后先等满一个间隔。
    pub last_fired: i64,
    /// 每日提醒已触发的日期 `YYYY-MM-DD`。
    pub fired_on: String,
    /// 「稍后提醒」到期时间（Unix 秒），0 表示没有。
    pub snooze_until: i64,
}

impl TimerItem {
    /// 与喝水提醒相同的时段语义：相等为全天，反向为跨夜。
    pub fn in_active_hours(&self, hour: u32) -> bool {
        let (a, b) = (self.start_hour.min(24), self.end_hour.min(24));
        if a == b {
            true
        } else if a < b {
            hour >= a && hour < b
        } else {
            hour >= a || hour < b
        }
    }

    pub fn summary(&self) -> String {
        let window = if self.start_hour == self.end_hour {
            "全天".to_string()
        } else {
            format!(
                "{:02}:00–{:02}:00",
                self.start_hour.min(24),
                self.end_hour.min(24)
            )
        };
        match self.mode {
            TimerMode::Interval { minutes } => format!("每 {minutes} 分钟 · {window}"),
            TimerMode::Daily { hour, minute } => format!("每天 {hour:02}:{minute:02} · {window}"),
        }
    }

    /// 岛上页面用：距离下一次触发还有多久（秒）。`None` 表示当前不会触发。
    pub fn remaining(&self, now: i64, hour: u32, minute: u32, today: &str) -> Option<i64> {
        if !self.enabled {
            return None;
        }
        if self.snooze_until > now {
            return Some(self.snooze_until - now);
        }
        match self.mode {
            TimerMode::Interval { minutes } => {
                let period = minutes.max(1) as i64 * 60;
                if self.last_fired <= 0 {
                    // 还没开始计时：下一次就是「现在 + 一个间隔」。
                    Some(period)
                } else {
                    Some((period - (now - self.last_fired)).max(0))
                }
            }
            TimerMode::Daily { hour: h, minute: m } => {
                let now_secs = hour as i64 * 3600 + minute as i64 * 60;
                let target = h.min(23) as i64 * 3600 + m.min(59) as i64 * 60;
                if self.fired_on == today {
                    Some(86_400 - now_secs + target)
                } else if target > now_secs {
                    Some(target - now_secs)
                } else {
                    Some(0)
                }
            }
        }
    }

    /// 岛上页面用：简短的下次触发说明。
    pub fn next_label(&self, now: i64, hour: u32, minute: u32, today: &str) -> String {
        if !self.enabled {
            return "已关闭".into();
        }
        match self.mode {
            TimerMode::Daily { hour: h, minute: m } if self.fired_on == today => {
                format!("明天 {h:02}:{m:02}")
            }
            TimerMode::Daily { hour: h, minute: m } => format!("今天 {h:02}:{m:02}"),
            TimerMode::Interval { minutes } => match self.remaining(now, hour, minute, today) {
                Some(0) => "即将提醒".into(),
                Some(secs) => format!("{} 分钟后", (secs + 59) / 60),
                None => format!("每 {minutes} 分钟"),
            },
        }
    }
}

/// 岛上「定时」页要展示的下一条提醒。
#[derive(Clone, Debug)]
pub struct Upcoming {
    pub title: String,
    pub when: String,
    pub secs: i64,
}

/// `take_due` 返回的一条待弹提醒。
#[derive(Clone, Debug)]
pub struct TimerDue {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Debug, Default)]
pub struct TimerList {
    pub items: Vec<TimerItem>,
}

fn num(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(default)
}
fn uint(v: &Value, key: &str, default: u32, min: u32, max: u32) -> u32 {
    num(v, key, default as i64).clamp(min as i64, max as i64) as u32
}
fn text(v: &Value, key: &str, default: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

impl TimerList {
    pub fn path() -> PathBuf {
        config_dir().join(FILE)
    }

    pub fn load() -> Self {
        let Ok(raw) = fs::read(Self::path()) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
            return Self::default();
        };
        Self::from_json(&value)
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_vec_pretty(&self.to_json()) {
            let _ = atomic_write(&path, &data);
        }
    }

    pub fn to_json(&self) -> Value {
        Value::Array(
            self.items
                .iter()
                .map(|i| {
                    let (mode, minutes, hour, minute) = match i.mode {
                        TimerMode::Interval { minutes } => ("interval", minutes, 9, 0),
                        TimerMode::Daily { hour, minute } => ("daily", 45, hour, minute),
                    };
                    json!({
                        "id": i.id,
                        "title": i.title,
                        "body": i.body,
                        "enabled": i.enabled,
                        "mode": mode,
                        "minutes": minutes,
                        "hour": hour,
                        "minute": minute,
                        "start_hour": i.start_hour,
                        "end_hour": i.end_hour,
                        "last_fired": i.last_fired,
                        "fired_on": i.fired_on,
                        "snooze_until": i.snooze_until,
                    })
                })
                .collect(),
        )
    }

    pub fn from_json(value: &Value) -> Self {
        let items = value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| {
                let id = v.get("id")?.as_str()?.to_owned();
                let mode = match v.get("mode").and_then(Value::as_str).unwrap_or("interval") {
                    "daily" => TimerMode::Daily {
                        hour: uint(v, "hour", 9, 0, 23),
                        minute: uint(v, "minute", 0, 0, 59),
                    },
                    _ => TimerMode::Interval {
                        minutes: uint(v, "minutes", 45, 1, 720),
                    },
                };
                Some(TimerItem {
                    id,
                    title: text(v, "title", "定时提醒"),
                    body: text(v, "body", ""),
                    enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                    mode,
                    start_hour: uint(v, "start_hour", 0, 0, 24),
                    end_hour: uint(v, "end_hour", 0, 0, 24),
                    last_fired: num(v, "last_fired", 0).max(0),
                    fired_on: text(v, "fired_on", ""),
                    snooze_until: num(v, "snooze_until", 0).max(0),
                })
            })
            .collect();
        Self { items }
    }

    pub fn remove(&mut self, id: &str) {
        self.items.retain(|i| i.id != id);
    }

    pub fn replace(&mut self, item: TimerItem) {
        if let Some(slot) = self.items.iter_mut().find(|i| i.id == item.id) {
            *slot = item;
        } else {
            self.items.push(item);
        }
    }

    pub fn find(&self, id: &str) -> Option<&TimerItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// 最近的一条待触发提醒，用于岛上「定时」页的主展示。
    pub fn upcoming(&self, now: i64, hour: u32, minute: u32, today: &str) -> Option<Upcoming> {
        self.items
            .iter()
            .filter_map(|item| {
                let secs = item.remaining(now, hour, minute, today)?;
                Some(Upcoming { title: item.title.clone(), when: item.next_label(now, hour, minute, today), secs })
            })
            .min_by_key(|u| u.secs)
    }

    pub fn enabled_count(&self) -> usize {
        self.items.iter().filter(|i| i.enabled).count()
    }

    /// 内置「站立提醒」，尚未开启时返回 None。
    pub fn stand(&self) -> Option<&TimerItem> {
        self.find(STAND_ID)
    }

    /// 创建或更新内置站立提醒。只在设置里真的改动时调用，避免每帧重置计时基准。
    pub fn set_stand(
        &mut self,
        enabled: bool,
        minutes: u32,
        start_hour: u32,
        end_hour: u32,
        now: i64,
    ) {
        self.replace(TimerItem {
            id: STAND_ID.into(),
            title: "站立提醒".into(),
            body: "坐太久了，起来走动一下，看看远处。".into(),
            enabled,
            mode: TimerMode::Interval {
                minutes: minutes.clamp(1, 720),
            },
            start_hour: start_hour.min(24),
            end_hour: end_hour.min(24),
            last_fired: now,
            fired_on: String::new(),
            snooze_until: 0,
        });
    }

    /// 推进所有提醒，取出本次该弹的条目并就地更新触发状态。
    /// `now` 为 Unix 秒，`hour`/`minute`/`today` 由外壳的 `Env` 提供，便于测试注入。
    pub fn take_due(&mut self, now: i64, hour: u32, minute: u32, today: &str) -> Vec<TimerDue> {
        let mut due = Vec::new();
        for item in self.items.iter_mut() {
            if !item.enabled || item.snooze_until > now {
                continue;
            }
            // 稍后提醒到期后无条件弹一次，不再受间隔或当日去重限制。
            let snooze_due = item.snooze_until > 0;
            if !snooze_due && !item.in_active_hours(hour) {
                continue;
            }
            let fires = if snooze_due {
                true
            } else {
                match item.mode {
                    TimerMode::Interval { minutes } => {
                        if item.last_fired == 0 {
                            // 新增或旧文件缺字段：先记基准，下一轮才开始计时。
                            item.last_fired = now;
                            false
                        } else {
                            now.saturating_sub(item.last_fired) >= minutes.max(1) as i64 * 60
                        }
                    }
                    TimerMode::Daily { hour: h, minute: m } => {
                        item.fired_on != today && (hour, minute) >= (h, m)
                    }
                }
            };
            if !fires {
                continue;
            }
            item.last_fired = now;
            item.fired_on = today.to_owned();
            item.snooze_until = 0;
            due.push(TimerDue {
                id: item.id.clone(),
                title: item.title.clone(),
                body: item.body.clone(),
            });
        }
        due
    }
}

/// 设置页「新增 / 编辑」表单。表单状态放在 `App` 上，跨帧保留。
#[derive(Clone, Debug)]
pub struct TimerForm {
    pub editing: Option<String>,
    pub title: String,
    pub body: String,
    /// 0 = 间隔提醒，1 = 每日时刻。
    pub mode_index: usize,
    pub interval_minutes: u32,
    pub hour: u32,
    pub minute: u32,
    pub start_hour: u32,
    pub end_hour: u32,
    pub enabled: bool,
    pub status: String,
}

impl Default for TimerForm {
    fn default() -> Self {
        Self {
            editing: None,
            title: String::new(),
            body: String::new(),
            mode_index: 0,
            interval_minutes: 45,
            hour: 9,
            minute: 0,
            start_hour: 0,
            end_hour: 0,
            enabled: true,
            status: String::new(),
        }
    }
}

impl TimerForm {
    pub fn preset_stand() -> Self {
        Self {
            title: "站立提醒".into(),
            body: "坐太久了，起来走动一下，看看远处。".into(),
            interval_minutes: 45,
            start_hour: 9,
            end_hour: 22,
            ..Self::default()
        }
    }

    pub fn preset_smoke() -> Self {
        Self {
            title: "抽烟提醒".into(),
            body: "到时间了，先缓一缓再决定。".into(),
            interval_minutes: 90,
            start_hour: 9,
            end_hour: 22,
            ..Self::default()
        }
    }

    pub fn from_item(item: &TimerItem) -> Self {
        let (mode_index, interval_minutes, hour, minute) = match item.mode {
            TimerMode::Interval { minutes } => (0, minutes, 9, 0),
            TimerMode::Daily { hour, minute } => (1, 45, hour, minute),
        };
        Self {
            editing: Some(item.id.clone()),
            title: item.title.clone(),
            body: item.body.clone(),
            mode_index,
            interval_minutes,
            hour,
            minute,
            start_hour: item.start_hour,
            end_hour: item.end_hour,
            enabled: item.enabled,
            status: String::new(),
        }
    }

    pub fn to_item(&self, id: String, now: i64) -> Option<TimerItem> {
        let title = self.title.trim();
        if title.is_empty() {
            return None;
        }
        let mode = if self.mode_index == 1 {
            TimerMode::Daily {
                hour: self.hour.min(23),
                minute: self.minute.min(59),
            }
        } else {
            TimerMode::Interval {
                minutes: self.interval_minutes.clamp(1, 720),
            }
        };
        Some(TimerItem {
            id,
            title: title.to_owned(),
            body: self.body.trim().to_owned(),
            enabled: self.enabled,
            mode,
            start_hour: self.start_hour.min(24),
            end_hour: self.end_hour.min(24),
            last_fired: now,
            fired_on: String::new(),
            snooze_until: 0,
        })
    }
}

impl App {
    /// 每帧由 `App::tick` 调用：取出到期提醒并入队通知。
    pub(crate) fn tick_timers(&mut self, env: &Env) {
        if self.timers.items.is_empty() {
            return;
        }
        let now = chrono::Local::now().timestamp();
        let due = self
            .timers
            .take_due(now, env.hour() as u32, env.minute(), &env.today);
        if due.is_empty() {
            return;
        }
        self.timers.save();
        for entry in due {
            self.timer_alert(entry.id, entry.title, entry.body);
        }
    }

    /// 通知卡片上的「稍后」：10 分钟后重新弹一次。
    pub(crate) fn snooze_timer(&mut self, id: &str) {
        let now = chrono::Local::now().timestamp();
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if let Some(item) = self.timers.items.iter_mut().find(|i| i.id == id) {
            item.snooze_until = now + 600;
            item.last_fired = now;
            item.fired_on = today;
        }
        self.timers.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, mode: TimerMode) -> TimerItem {
        TimerItem {
            id: id.into(),
            title: format!("提醒 {id}"),
            body: "内容".into(),
            enabled: true,
            mode,
            start_hour: 0,
            end_hour: 0,
            last_fired: 1000,
            fired_on: String::new(),
            snooze_until: 0,
        }
    }

    #[test]
    fn json_round_trip_preserves_both_modes() {
        let mut list = TimerList::default();
        list.items
            .push(item("interval", TimerMode::Interval { minutes: 30 }));
        let mut daily = item(
            "daily",
            TimerMode::Daily {
                hour: 14,
                minute: 30,
            },
        );
        daily.start_hour = 9;
        daily.end_hour = 22;
        daily.enabled = false;
        list.items.push(daily);
        let restored = TimerList::from_json(&list.to_json());
        assert_eq!(restored.items.len(), 2);
        assert_eq!(restored.items[0].mode, TimerMode::Interval { minutes: 30 });
        assert_eq!(
            restored.items[1].mode,
            TimerMode::Daily {
                hour: 14,
                minute: 30
            }
        );
        assert_eq!(restored.items[1].start_hour, 9);
        assert!(!restored.items[1].enabled);
        assert_eq!(restored.items[0].body, "内容");
    }

    #[test]
    fn interval_fires_only_after_the_configured_minutes() {
        let mut list = TimerList::default();
        list.items
            .push(item("a", TimerMode::Interval { minutes: 30 }));
        assert!(list
            .take_due(1000 + 29 * 60, 10, 0, "2026-11-05")
            .is_empty());
        let due = list.take_due(1000 + 30 * 60, 10, 0, "2026-11-05");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "a");
        assert!(list
            .take_due(1000 + 31 * 60, 10, 0, "2026-11-05")
            .is_empty());
    }

    #[test]
    fn newly_added_timer_waits_for_a_full_interval() {
        let mut list = TimerList::default();
        let mut fresh = item("fresh", TimerMode::Interval { minutes: 45 });
        fresh.last_fired = 0;
        list.items.push(fresh);
        assert!(list.take_due(5_000_000, 10, 0, "2026-11-05").is_empty());
        assert_eq!(list.items[0].last_fired, 5_000_000);
        assert!(
            list.take_due(5_000_000 + 45 * 60, 10, 0, "2026-11-05")
                .len()
                == 1
        );
    }

    #[test]
    fn daily_timer_fires_once_per_day_at_or_after_the_time() {
        let mut list = TimerList::default();
        list.items.push(item(
            "d",
            TimerMode::Daily {
                hour: 9,
                minute: 30,
            },
        ));
        assert!(list.take_due(2000, 9, 29, "2026-11-05").is_empty());
        assert_eq!(list.take_due(2000, 9, 41, "2026-11-05").len(), 1);
        assert!(
            list.take_due(2100, 10, 0, "2026-11-05").is_empty(),
            "同一天不重复"
        );
        assert_eq!(
            list.take_due(2200, 9, 31, "2026-11-06").len(),
            1,
            "次日重新触发"
        );
    }

    #[test]
    fn active_hours_cover_all_day_and_overnight() {
        let mut all_day = item("a", TimerMode::Interval { minutes: 1 });
        assert!(all_day.in_active_hours(3) && all_day.in_active_hours(23));
        all_day.start_hour = 22;
        all_day.end_hour = 6;
        assert!(all_day.in_active_hours(23) && all_day.in_active_hours(2));
        assert!(!all_day.in_active_hours(12));
        all_day.start_hour = 9;
        all_day.end_hour = 22;
        assert!(all_day.in_active_hours(9) && !all_day.in_active_hours(22));
    }

    #[test]
    fn disabled_and_out_of_window_timers_do_not_fire() {
        let mut list = TimerList::default();
        let mut off = item("off", TimerMode::Interval { minutes: 1 });
        off.enabled = false;
        off.last_fired = 0;
        let mut windowed = item("windowed", TimerMode::Interval { minutes: 1 });
        windowed.start_hour = 9;
        windowed.end_hour = 18;
        list.items.push(off);
        list.items.push(windowed);
        assert!(list.take_due(2000, 8, 0, "2026-11-05").is_empty());
        assert_eq!(list.take_due(2000, 10, 0, "2026-11-05").len(), 1);
        assert_eq!(list.items[1].last_fired, 2000, "触发后重置计时基准");
        assert_eq!(list.items[0].last_fired, 0, "关闭的提醒不推进状态");
    }

    #[test]
    fn snooze_delays_the_next_fire_by_ten_minutes() {
        let mut list = TimerList::default();
        list.items
            .push(item("s", TimerMode::Daily { hour: 9, minute: 0 }));
        list.items[0].snooze_until = 5000;
        assert!(list.take_due(4900, 10, 0, "2026-11-05").is_empty());
        let due = list.take_due(5000, 10, 0, "2026-11-05");
        assert_eq!(due.len(), 1, "稍后到期后无条件再弹一次");
        assert_eq!(list.items[0].snooze_until, 0);
    }

    #[test]
    fn form_requires_a_title_and_builds_the_selected_mode() {
        let mut form = TimerForm::default();
        assert!(form.to_item("x".into(), 100).is_none());
        form.title = "  站立提醒  ".into();
        form.mode_index = 1;
        form.hour = 14;
        form.minute = 5;
        let item = form.to_item("x".into(), 100).unwrap();
        assert_eq!(item.title, "站立提醒");
        assert_eq!(
            item.mode,
            TimerMode::Daily {
                hour: 14,
                minute: 5
            }
        );
        assert_eq!(item.last_fired, 100);
        let back = TimerForm::from_item(&item);
        assert_eq!(back.mode_index, 1);
        assert_eq!(back.hour, 14);
    }
}
