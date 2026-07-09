use super::{weekday_zh, Segment, SegmentData};
use crate::config::{InputData, SegmentId};
use chrono::{Datelike, Local};
use std::collections::HashMap;

#[derive(Default)]
pub struct TimeSegment;

impl TimeSegment {
    pub fn new() -> Self {
        Self
    }
}

impl Segment for TimeSegment {
    fn collect(&self, _input: &InputData) -> Option<SegmentData> {
        let now = Local::now();
        Some(SegmentData {
            primary: format!(
                "{} {} {}",
                now.format("%m/%d"),
                weekday_zh(now.weekday()),
                now.format("%H:%M")
            ),
            secondary: String::new(),
            metadata: HashMap::new(),
        })
    }

    fn id(&self) -> SegmentId {
        SegmentId::Time
    }
}
