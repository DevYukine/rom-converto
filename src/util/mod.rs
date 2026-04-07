pub mod http;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub const BYTES_PER_MB: f64 = 1_000_000.0;

pub const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})";

/// Creates a styled progress bar and adds it to the given `MultiProgress`.
pub fn create_progress_bar(
    pb: &MultiProgress,
    total: u64,
    msg: impl Into<String>,
) -> Result<ProgressBar, indicatif::style::TemplateError> {
    let pg = pb.add(ProgressBar::new(total));
    pg.set_style(
        ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)?
            .progress_chars("#>-"),
    );
    pg.set_message(msg.into());
    Ok(pg)
}

/// Creates a standalone styled progress bar (not attached to a MultiProgress).
pub fn create_standalone_progress_bar(
    total: u64,
    msg: impl Into<String>,
) -> Result<ProgressBar, indicatif::style::TemplateError> {
    let pg = ProgressBar::new(total);
    pg.set_style(
        ProgressStyle::default_bar()
            .template(PROGRESS_TEMPLATE)?
            .progress_chars("#>-"),
    );
    pg.set_message(msg.into());
    Ok(pg)
}
