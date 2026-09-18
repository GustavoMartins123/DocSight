mod classes;
mod compare;
mod engine;
mod ground_truth;
mod measure;

pub use classes::{
    COMPLEXITIES, DocumentClass, SIGNAL_METRICS, Signal, Taxonomy, load_taxonomy, read_taxonomy,
    validate_taxonomy,
};
pub use compare::{COMPARISON_SCHEMA, Candidate, Change, Comparison, compare};
pub use engine::{
    DiffSummary, EngineSession, Geometry, Observation, RENDER_DPI, RENDER_PAGE, RenderSummary,
    Structure, TextSummary, render_covers_page, signal_values,
};
pub use ground_truth::{
    DiffExpectation, GROUND_TRUTH_SCHEMA, GroundTruth, Proposal, REVIEW_STATUSES, Review,
    load_register, prepare_with, validate_record,
};
pub use measure::{
    BASES, ClassSummary, DocumentResult, EXTRAPOLATION, METRICS, MeasureInputs, MetricOutcome,
    QUALITY_REPORT_SCHEMA, QualityReport, STATUSES, Tally, measure_with, validate_report,
};
