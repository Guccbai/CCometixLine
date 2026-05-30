pub mod context_window;
pub mod cost;
pub mod directory;
pub mod git;
pub mod model;
pub mod output_style;
pub mod session;
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
pub use update::UpdateSegment;
pub use usage::UsageSegment;

/// Render a fixed-width block-character progress bar, e.g. "[██████░░░░]".
/// `percent` is 0-100; out-of-range values are clamped.
pub fn progress_bar(percent: f64, width: usize) -> String {
    let ratio = (percent / 100.0).clamp(0.0, 1.0);
    let filled = ((ratio * width as f64).round() as usize).min(width);
    format!(
        "[{}{}]",
        "\u{2588}".repeat(filled),         // █ full block
        "\u{2591}".repeat(width - filled)  // ░ light shade
    )
}
