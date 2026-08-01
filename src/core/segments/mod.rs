pub mod context_window;
pub mod cost;
pub mod directory;
pub mod git;
pub mod model;
pub mod output_style;
pub mod session;
pub mod time;
pub mod update;
pub mod usage;

use crate::config::{InputData, SegmentId};
use std::collections::HashMap;

// New Segment trait for data collection only
pub trait Segment {
    fn collect(&self, input: &InputData) -> Option<SegmentData>;
    fn id(&self) -> SegmentId;
}

#[derive(Debug, Clone)]
pub struct SegmentData {
    pub primary: String,
    pub secondary: String,
    pub metadata: HashMap<String, String>,
}

// Re-export all segment types
pub use context_window::ContextWindowSegment;
pub use cost::CostSegment;
pub use directory::DirectorySegment;
pub use git::GitSegment;
pub use model::ModelSegment;
pub use output_style::OutputStyleSegment;
pub use session::SessionSegment;
pub use time::TimeSegment;
pub use update::UpdateSegment;
pub use usage::UsageSegment;

/// Chinese weekday name, e.g. 周一 / 周日.
pub fn weekday_zh(weekday: chrono::Weekday) -> &'static str {
    match weekday {
        chrono::Weekday::Mon => "周一",
        chrono::Weekday::Tue => "周二",
        chrono::Weekday::Wed => "周三",
        chrono::Weekday::Thu => "周四",
        chrono::Weekday::Fri => "周五",
        chrono::Weekday::Sat => "周六",
        chrono::Weekday::Sun => "周日",
    }
}

/// 256-color state tone for a 0-100 utilization percentage, shared by the
/// ctx and usage segments (OMC thresholds): <70% sage green, 70-85% amber,
/// ≥85% soft red. The single source for both thresholds and palette.
pub fn util_color_256(percent: f64) -> u8 {
    if percent >= 85.0 {
        167 // soft red
    } else if percent >= 70.0 {
        179 // amber
    } else {
        108 // sage green
    }
}

/// Nerd Font circle_slice glyph for a 0-100 percentage: an 8-step pie gauge
/// in a single fixed-width (PUA, unambiguous) character. Coarse by design —
/// the exact value is always rendered as a number next to it.
pub fn circle_gauge(percent: f64) -> &'static str {
    match percent.clamp(0.0, 100.0) as u8 {
        0..=12 => "\u{f0a9e}",  // circle_slice_1
        13..=25 => "\u{f0a9f}", // circle_slice_2
        26..=37 => "\u{f0aa0}", // circle_slice_3
        38..=50 => "\u{f0aa1}", // circle_slice_4
        51..=62 => "\u{f0aa2}", // circle_slice_5
        63..=75 => "\u{f0aa3}", // circle_slice_6
        76..=87 => "\u{f0aa4}", // circle_slice_7
        _ => "\u{f0aa5}",       // circle_slice_8
    }
}
