use super::{circle_gauge, util_color_256, Segment, SegmentData};
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

// Utilizations are Option so a window the data source omitted survives a
// cache round trip as "absent" instead of resurfacing as a fake 0%.
#[derive(Debug, Serialize, Deserialize)]
struct ApiUsageCache {
    five_hour_utilization: Option<f64>,
    #[serde(default)]
    five_hour_resets_at: Option<String>,
    seven_day_utilization: Option<f64>,
    // Legacy field (7d reset time) kept for backward compatibility with old caches.
    resets_at: Option<String>,
    cached_at: String,
}

/// One usage window: utilization (0-100) and reset time (RFC3339).
struct UsageWindow {
    util: f64,
    reset: Option<String>,
}

// Embedded window tones: dim gray labels (matching theme_default's muted
// labels) and neutral light values.
const DIM: &str = "\x1b[38;5;245m";
const LIGHT: &str = "\x1b[38;5;252m";
const RESET: &str = "\x1b[0m";

/// Normalized usage windows from either data source (stdin or API).
/// A `None` window means the source didn't provide it (rendered as "-"),
/// which is distinct from a freshly-reset 0% window.
struct UsageData {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    /// 7d Fable-only window; only rendered when the data source provides it.
    seven_day_fable: Option<UsageWindow>,
}

#[derive(Default)]
pub struct UsageSegment;

impl UsageSegment {
    pub fn new() -> Self {
        Self
    }

    /// Unix epoch seconds (stdin `resets_at`) → RFC3339, so both data sources
    /// feed the same rendering path.
    fn epoch_to_rfc3339(epoch: i64) -> Option<String> {
        DateTime::<Utc>::from_timestamp(epoch, 0).map(|dt| dt.to_rfc3339())
    }

    /// Humanized reset info: ("4h12m", "04:49")-style (countdown, absolute)
    /// pair. Countdown scales with the horizon: "38m" / "4h12m" / "3d15h".
    /// The absolute part carries a relative day word only when the horizon
    /// needs one: the 5h window shows bare "04:49" (unambiguous within 5h),
    /// the 7d windows "明天 18:00" / "周六 18:00" / "07/12 周六 18:00".
    fn format_reset(
        reset_time_str: Option<&str>,
        with_day_word: bool,
        now: DateTime<chrono::Local>,
    ) -> Option<(String, String)> {
        let dt = DateTime::parse_from_rfc3339(reset_time_str?).ok()?;
        let local = dt.with_timezone(&chrono::Local);
        let mins = (local - now).num_minutes().max(0);
        let (d, h, m) = (mins / 1440, (mins % 1440) / 60, mins % 60);
        let countdown = if d > 0 {
            format!("{d}d{h}h")
        } else if h > 0 {
            format!("{h}h{m:02}m")
        } else {
            format!("{m}m")
        };
        let hm = local.format("%H:%M");
        let absolute = if !with_day_word {
            hm.to_string()
        } else {
            let days = (local.date_naive() - now.date_naive()).num_days();
            let weekday = super::weekday_zh(chrono::Datelike::weekday(&local));
            match days {
                0 => format!("今天 {hm}"),
                1 => format!("明天 {hm}"),
                2..=6 => format!("{weekday} {hm}"),
                _ => format!("{} {weekday} {hm}", local.format("%m/%d")),
            }
        };
        Some((countdown, absolute))
    }

    /// Usage data from Claude Code's stdin JSON (>= 2.1.80): real-time, zero
    /// network. Also refreshes the on-disk cache so a new session's first
    /// renders (before rate_limits appears) don't fall back to the API.
    fn collect_from_stdin(&self, limits: &RateLimits) -> UsageData {
        let window = |w: &crate::config::RateLimitWindow| UsageWindow {
            util: w.used_percentage.unwrap_or(0.0),
            reset: w.resets_at.and_then(Self::epoch_to_rfc3339),
        };
        let five_hour = limits.five_hour.as_ref().map(window);
        let seven_day = limits.seven_day.as_ref().map(window);
        let seven_day_fable = limits.seven_day_fable.as_ref().map(window);

        let cache_fresh = self
            .load_cache()
            .map(|c| self.is_cache_valid(&c, 60))
            .unwrap_or(false);
        if !cache_fresh {
            self.save_cache(&ApiUsageCache {
                five_hour_utilization: five_hour.as_ref().map(|w| w.util),
                five_hour_resets_at: five_hour.as_ref().and_then(|w| w.reset.clone()),
                seven_day_utilization: seven_day.as_ref().map(|w| w.util),
                resets_at: seven_day.as_ref().and_then(|w| w.reset.clone()),
                cached_at: Utc::now().to_rfc3339(),
            });
        }

        UsageData {
            five_hour,
            seven_day,
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
            five_hour: c.five_hour_utilization.map(|util| UsageWindow {
                util,
                reset: c.five_hour_resets_at,
            }),
            seven_day: c.seven_day_utilization.map(|util| UsageWindow {
                util,
                reset: c.resets_at,
            }),
            seven_day_fable: None,
        };

        if use_cached {
            return Some(from_cache(cached_data.unwrap()));
        }
        match self.fetch_api_usage(api_base_url, &token, timeout) {
            Some(response) => {
                let cache = ApiUsageCache {
                    five_hour_utilization: Some(response.five_hour.utilization),
                    five_hour_resets_at: response.five_hour.resets_at.clone(),
                    seven_day_utilization: Some(response.seven_day.utilization),
                    resets_at: response.seven_day.resets_at.clone(),
                    cached_at: Utc::now().to_rfc3339(),
                };
                self.save_cache(&cache);
                Some(UsageData {
                    five_hour: Some(UsageWindow {
                        util: response.five_hour.utilization,
                        reset: response.five_hour.resets_at,
                    }),
                    seven_day: Some(UsageWindow {
                        util: response.seven_day.utilization,
                        reset: response.seven_day.resets_at,
                    }),
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
        let max_util = [&data.five_hour, &data.seven_day, &data.seven_day_fable]
            .into_iter()
            .flatten()
            .map(|w| w.util)
            .fold(0.0f64, f64::max);
        let dynamic_icon = circle_gauge(max_util).to_string();

        // Each window: dim label, gauge + percentage in its state color
        // (utilization thresholds, not per-window decoration), then the
        // countdown as a primary value with the absolute reset time in
        // parentheses as secondary; "-" when the data source omits the
        // window (distinct from a real 0%).
        let now = chrono::Local::now();
        let window_text = |label: &str, w: &Option<UsageWindow>, with_day_word: bool| match w {
            Some(w) => {
                let reset_part = Self::format_reset(w.reset.as_deref(), with_day_word, now)
                    .map(|(cd, abs)| format!("{LIGHT}{cd}{RESET} {DIM}({abs}){RESET}"))
                    .unwrap_or_else(|| format!("{DIM}?{RESET}"));
                format!(
                    "{DIM}{label} \x1b[38;5;{}m{} {}%{RESET} {reset_part}",
                    util_color_256(w.util),
                    circle_gauge(w.util),
                    w.util.round() as u8,
                )
            }
            None => format!("{DIM}{label} -{RESET}"),
        };
        let mut primary = format!(
            "{}\x1b[90m · \x1b[0m{}",
            window_text("5h", &data.five_hour, false),
            window_text("7d", &data.seven_day, true),
        );
        if data.seven_day_fable.is_some() {
            use std::fmt::Write as _;
            let _ = write!(
                primary,
                "\x1b[90m · \x1b[0m{}",
                window_text("fable", &data.seven_day_fable, true),
            );
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
