pub mod http;

pub const BYTES_PER_MB: f64 = 1_000_000.0;

pub const PROGRESS_TEMPLATE: &str = "{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})";
