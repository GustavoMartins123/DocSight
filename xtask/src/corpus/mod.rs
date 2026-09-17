mod manifest;
mod runner;

pub use manifest::{Case, Expectation, Input, Manifest, load_manifest, validate_manifest};
pub use runner::{CaseOutcome, Report, evaluate, json_pointer, run_corpus, run_corpus_with};

pub const OPERATIONS: [&str; 4] = ["inspect", "text", "render", "diff"];
pub const FORMATS: [&str; 3] = ["docx", "pdf", "invalid"];
pub const ORIGINS: [&str; 2] = ["synthetic", "consented-real"];
