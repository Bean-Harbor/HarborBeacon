//! Shared retry scheduler aliases for durable detector controls.

pub use super::cat_detection_retry_scheduler::{
    CatDetectionRetryEnqueueResult as DetectionControlRetryEnqueueResult,
    CatDetectionRetryEntry as DetectionControlRetryEntry,
    CatDetectionRetryOutcome as DetectionControlRetryOutcome,
    CatDetectionRetryScheduler as DetectionControlRetryScheduler,
    CatDetectionRetrySchedulerConfig as DetectionControlRetrySchedulerConfig,
    CatDetectionRetryTask as DetectionControlRetryTask,
};
