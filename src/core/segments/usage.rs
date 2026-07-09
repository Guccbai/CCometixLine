use super::{progress_bar, Segment, SegmentData};
use crate::config::{InputData, RateLimits, SegmentId};
use crate::utils::credentials;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Subset of /api/oauth/usage we render. serde ignores all other windows
// (seven_day_sonnet, seven_day_opus, extra_usage, ...) automatically.
#[derive(Debug, Deserialize)]
struct ApiUsageResponse {
    five_hour: UsagePeriod,
    seven_day: UsagePeriod,
}

// `utilization` is a 0-100 percentage (e.g. 15.0 == 15%), not a 0-1 fraction.
#[derive(Debug, Deserialize)]
struct UsagePeriod {
    utilization: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ApiUsageCache {
    five_hour_utilization: f64,
    #[serde(default)]
    five_hour_resets_at: Option<String>,
    seven_day_utilization: f64,
    // Legacy field (7d reset time) kept for backward compatibility with old caches.
    resets_at: Option<String>,
    cached_at: String,
}

/// Normalized usage windows from either data source (stdin or API).
struct UsageData {
    five_hour_util: f64,
    five_hour_reset: Option<String>,
    seven_day_util: f64,
    seven_day_reset: Option<String>,
    /// 7d Fable-only window; None until the data source provides it.
    seven_day_fable: Option<(f64, Option<String>)>,
}

#[derive(Default)]
pub struct UsageSegment;

impl UsageSegment {
    pub fn new() -> Self {
        Self
    }

    fn get_circle_icon(utilization: f64) -> String {
        let percent = (utilization * 100.0) as u8;
        match percent {
            0..=12 => "\u{f0a9e}".to_string(),  // circle_slice_1
            13..=25 => "\u{f0a9f}".to_string(), // circle_slice_2
            26..=37 => "\u{f0aa0}".to_string(), // circle_slice_3
            38..=50 => "\u{f0aa1}".to_string(), // circle_slice_4
            51..=62 => "\u{f0aa2}".to_string(), // circle_slice_5
            63..=75 => "\u{f0aa3}".to_string(), // circle_slice_6
            76..=87 => "\u{f0aa4}".to_string(), // circle_slice_7
            _ => "\u{f0aa5}".to_string(),       // circle_slice_8
        }
    }

    /// Unix epoch seconds (stdin `resets_at`) → RFC3339, so both data sources
    /// feed the same rendering path.
    fn epoch_to_rfc3339(epoch: i64) -> Option<String> {
        DateTime::<Utc>::from_timestamp(epoch, 0).map(|dt| dt.to_rfc3339())
    }

    /// Absolute local reset time: "周二 21:08" for the 5h window,
    /// "07/12 周六 18:00" (with date) for the 7d windows.
    fn format_reset_absolute(reset_time_str: Option<&str>, with_date: bool) -> String {
        if let Some(time_str) = reset_time_str {
            if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                let local = dt.with_timezone(&chrono::Local);
                let weekday = super::weekday_zh(chrono::Datelike::weekday(&local));
                return if with_date {
                    format!(
                        "{} {} {}",
                        local.format("%m/%d"),
                        weekday,
                        local.format("%H:%M")
                    )
                } else {
                    format!("{} {}", weekday, local.format("%H:%M"))
                };
            }
        }
        "?".to_string()
    }

    /// Usage data from Claude Code's stdin JSON (>= 2.1.80): real-time, zero
    /// network. Also refreshes the on-disk cache so a new session's first
    /// renders (before rate_limits appears) don't fall back to the API.
    fn collect_from_stdin(&self, limits: &RateLimits) -> UsageData {
        let window = |w: &crate::config::RateLimitWindow| {
            (
                w.used_percentage.unwrap_or(0.0),
                w.resets_at.and_then(Self::epoch_to_rfc3339),
            )
        };
        let (five_util, five_reset) = limits.five_hour.as_ref().map(window).unwrap_or((0.0, None));
        let (seven_util, seven_reset) =
            limits.seven_day.as_ref().map(window).unwrap_or((0.0, None));
        let seven_day_fable = limits.seven_day_fable.as_ref().map(window);

        let cache_fresh = self
            .load_cache()
            .map(|c| self.is_cache_valid(&c, 60))
            .unwrap_or(false);
        if !cache_fresh {
            self.save_cache(&ApiUsageCache {
                five_hour_utilization: five_util,
                five_hour_resets_at: five_reset.clone(),
                seven_day_utilization: seven_util,
                resets_at: seven_reset.clone(),
                cached_at: Utc::now().to_rfc3339(),
            });
        }

        UsageData {
            five_hour_util: five_util,
            five_hour_reset: five_reset,
            seven_day_util: seven_util,
            seven_day_reset: seven_reset,
            seven_day_fable,
        }
    }

    fn get_cache_path() -> Option<std::path::PathBuf> {
        let home = dirs::home_dir()?;
        Some(
            home.join(".claude")
                .join("ccline")
                .join(".api_usage_cache.json"),
        )
    }

    fn load_cache(&self) -> Option<ApiUsageCache> {
        let cache_path = Self::get_cache_path()?;
        if !cache_path.exists() {
            return None;
        }

        let content = std::fs::read_to_string(&cache_path).ok()?;
        serde_json::from_str(&content).ok()
    }

    fn save_cache(&self, cache: &ApiUsageCache) {
        if let Some(cache_path) = Self::get_cache_path() {
            if let Some(parent) = cache_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(cache) {
                let _ = std::fs::write(&cache_path, json);
            }
        }
    }

    fn is_cache_valid(&self, cache: &ApiUsageCache, cache_duration: u64) -> bool {
        if let Ok(cached_at) = DateTime::parse_from_rfc3339(&cache.cached_at) {
            let now = Utc::now();
            let elapsed = now.signed_duration_since(cached_at.with_timezone(&Utc));
            elapsed.num_seconds() < cache_duration as i64
        } else {
            false
        }
    }

    fn get_claude_code_version() -> String {
        use std::process::Command;

        let output = Command::new("npm")
            .args(["view", "@anthropic-ai/claude-code", "version"])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !version.is_empty() {
                    return format!("claude-code/{version}");
                }
            }
            _ => {}
        }

        "claude-code".to_string()
    }

    fn get_proxy_from_settings() -> Option<String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok()?;
        let settings_path = format!("{home}/.claude/settings.json");

        let content = std::fs::read_to_string(&settings_path).ok()?;
        let settings: serde_json::Value = serde_json::from_str(&content).ok()?;

        // Try HTTPS_PROXY first, then HTTP_PROXY
        settings
            .get("env")?
            .get("HTTPS_PROXY")
            .or_else(|| settings.get("env")?.get("HTTP_PROXY"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn fetch_api_usage(
        &self,
        api_base_url: &str,
        token: &str,
        timeout_secs: u64,
    ) -> Option<ApiUsageResponse> {
        let url = format!("{api_base_url}/api/oauth/usage");
        let user_agent = Self::get_claude_code_version();

        let agent = if let Some(proxy_url) = Self::get_proxy_from_settings() {
            if let Ok(proxy) = ureq::Proxy::new(&proxy_url) {
                ureq::Agent::config_builder()
                    .proxy(Some(proxy))
                    .build()
                    .new_agent()
            } else {
                ureq::Agent::new_with_defaults()
            }
        } else {
            ureq::Agent::new_with_defaults()
        };

        let response = agent
            .get(&url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("anthropic-beta", "oauth-2025-04-20")
            .header("User-Agent", &user_agent)
            .config()
            .timeout_global(Some(std::time::Duration::from_secs(timeout_secs)))
            .build()
            .call()
            .ok()?;

        response.into_body().read_json().ok()
    }
}

impl UsageSegment {
    /// Fallback for Claude Code versions without stdin rate_limits: cached
    /// /api/oauth/usage polling. That endpoint aggressively rate limits
    /// (429s persisting for hours), hence the failure backoff below.
    fn fetch_with_cache(&self) -> Option<UsageData> {
        let token = credentials::get_oauth_token()?;

        // Load config from file to get segment options
        let config = crate::config::Config::load().ok()?;
        let segment_config = config.segments.iter().find(|s| s.id == SegmentId::Usage);

        let api_base_url = segment_config
            .and_then(|sc| sc.options.get("api_base_url"))
            .and_then(|v| v.as_str())
            .unwrap_or("https://api.anthropic.com");

        let cache_duration = segment_config
            .and_then(|sc| sc.options.get("cache_duration"))
            .and_then(|v| v.as_u64())
            .unwrap_or(300);

        let timeout = segment_config
            .and_then(|sc| sc.options.get("timeout"))
            .and_then(|v| v.as_u64())
            .unwrap_or(2);

        let cached_data = self.load_cache();
        let use_cached = cached_data
            .as_ref()
            .map(|cache| self.is_cache_valid(cache, cache_duration))
            .unwrap_or(false);

        let from_cache = |c: ApiUsageCache| UsageData {
            five_hour_util: c.five_hour_utilization,
            five_hour_reset: c.five_hour_resets_at,
            seven_day_util: c.seven_day_utilization,
            seven_day_reset: c.resets_at,
            seven_day_fable: None,
        };

        if use_cached {
            return Some(from_cache(cached_data.unwrap()));
        }
        match self.fetch_api_usage(api_base_url, &token, timeout) {
            Some(response) => {
                let cache = ApiUsageCache {
                    five_hour_utilization: response.five_hour.utilization,
                    five_hour_resets_at: response.five_hour.resets_at.clone(),
                    seven_day_utilization: response.seven_day.utilization,
                    resets_at: response.seven_day.resets_at.clone(),
                    cached_at: Utc::now().to_rfc3339(),
                };
                self.save_cache(&cache);
                Some(UsageData {
                    five_hour_util: response.five_hour.utilization,
                    five_hour_reset: response.five_hour.resets_at,
                    seven_day_util: response.seven_day.utilization,
                    seven_day_reset: response.seven_day.resets_at,
                    seven_day_fable: None,
                })
            }
            None => match cached_data {
                Some(mut cache) => {
                    // Failed fetch (e.g. 429): refresh cached_at so the next
                    // cache_duration renders from cache instead of hitting the
                    // endpoint on every statusline refresh, which compounds
                    // the rate limit.
                    cache.cached_at = Utc::now().to_rfc3339();
                    self.save_cache(&cache);
                    Some(from_cache(cache))
                }
                None => None,
            },
        }
    }
}

impl Segment for UsageSegment {
    fn collect(&self, input: &InputData) -> Option<SegmentData> {
        // Prefer rate_limits from stdin (Claude Code >= 2.1.80); the API
        // path only serves older versions and early-session renders.
        let data = match &input.rate_limits {
            Some(limits) => self.collect_from_stdin(limits),
            None => self.fetch_with_cache()?,
        };

        // Icon reflects the highest-watermark window; color stays fixed (config).
        let fable_util = data.seven_day_fable.as_ref().map(|(u, _)| *u);
        let max_util = data
            .five_hour_util
            .max(data.seven_day_util)
            .max(fable_util.unwrap_or(0.0));
        let dynamic_icon = Self::get_circle_icon(max_util / 100.0);

        // Each window: bar + percentage + relative reset countdown. Windows
        // get distinct embedded colors (5h cyan, 7d magenta, fable blue) so
        // they read apart at a glance.
        let five_bar = progress_bar(data.five_hour_util, 5);
        let seven_bar = progress_bar(data.seven_day_util, 5);
        let mut primary = format!(
            "\x1b[96m5h {five_bar} {}% {}\x1b[0m\x1b[90m · \x1b[0m\x1b[95m7d {seven_bar} {}% {}\x1b[0m",
            data.five_hour_util.round() as u8,
            Self::format_reset_absolute(data.five_hour_reset.as_deref(), false),
            data.seven_day_util.round() as u8,
            Self::format_reset_absolute(data.seven_day_reset.as_deref(), true),
        );
        if let Some((util, reset)) = &data.seven_day_fable {
            let bar = progress_bar(*util, 5);
            primary.push_str(&format!(
                "\x1b[90m · \x1b[0m\x1b[94mfable {bar} {}% {}\x1b[0m",
                util.round() as u8,
                Self::format_reset_absolute(reset.as_deref(), true),
            ));
        }

        let mut metadata = HashMap::new();
        metadata.insert("dynamic_icon".to_string(), dynamic_icon);

        Some(SegmentData {
            primary,
            secondary: String::new(),
            metadata,
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Usage
    }
}
