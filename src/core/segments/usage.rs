use super::{progress_bar, Segment, SegmentData};
use crate::config::{InputData, SegmentId};
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

    /// Time until the window resets, as a compact relative string (e.g. "2d6h", "5h30m", "45m").
    fn format_reset_relative(reset_time_str: Option<&str>) -> String {
        if let Some(time_str) = reset_time_str {
            if let Ok(dt) = DateTime::parse_from_rfc3339(time_str) {
                let secs = dt
                    .with_timezone(&Utc)
                    .signed_duration_since(Utc::now())
                    .num_seconds();
                if secs <= 0 {
                    return "now".to_string();
                }
                let days = secs / 86_400;
                let hours = (secs % 86_400) / 3_600;
                let mins = (secs % 3_600) / 60;
                return if days > 0 {
                    format!("{days}d{hours}h")
                } else if hours > 0 {
                    format!("{hours}h{mins}m")
                } else {
                    format!("{mins}m")
                };
            }
        }
        "?".to_string()
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

impl Segment for UsageSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
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

        // Each tuple element: 5h util, 5h reset time, 7d-all util, 7d reset time.
        let from_cache = |c: ApiUsageCache| {
            (
                c.five_hour_utilization,
                c.five_hour_resets_at,
                c.seven_day_utilization,
                c.resets_at,
            )
        };

        let (five_hour_util, five_hour_reset, seven_day_util, seven_day_reset) = if use_cached {
            from_cache(cached_data.unwrap())
        } else {
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
                    (
                        response.five_hour.utilization,
                        response.five_hour.resets_at,
                        response.seven_day.utilization,
                        response.seven_day.resets_at,
                    )
                }
                None => match cached_data {
                    Some(cache) => from_cache(cache),
                    None => return None,
                },
            }
        };

        // Icon reflects the highest-watermark window; color stays fixed (config).
        let max_util = five_hour_util.max(seven_day_util);
        let dynamic_icon = Self::get_circle_icon(max_util / 100.0);

        // Each window: bar + percentage + relative reset countdown.
        let five_bar = progress_bar(five_hour_util, 5);
        let seven_bar = progress_bar(seven_day_util, 5);
        let primary = format!(
            "5h {five_bar} {}% {}  7d {seven_bar} {}% {}",
            five_hour_util.round() as u8,
            Self::format_reset_relative(five_hour_reset.as_deref()),
            seven_day_util.round() as u8,
            Self::format_reset_relative(seven_day_reset.as_deref()),
        );

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
