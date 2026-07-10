//! Markdown results-table rendering matching the layout of the committed
//! `benchmark/*.md` files.

use crate::stats::{WarmStats, delta_ratio, fmt_seconds, fmt_size_delta, human_size};

/// One row of a platform's results table.
pub struct Row {
    pub operation: String,
    pub ext: Option<WarmStats>,
    pub rc: WarmStats,
    /// `(rom-converto output size, reference output size)` when a size
    /// delta against the reference tool is meaningful.
    pub size: Option<(u64, u64)>,
    /// rom-converto output size, shown in the Output column of a
    /// rom-converto-only table.
    pub output_bytes: Option<u64>,
}

impl Row {
    pub fn compared(operation: impl Into<String>, ext: WarmStats, rc: WarmStats) -> Row {
        Row {
            operation: operation.into(),
            ext: Some(ext),
            rc,
            size: None,
            output_bytes: None,
        }
    }

    pub fn rc_only(operation: impl Into<String>, rc: WarmStats) -> Row {
        Row {
            operation: operation.into(),
            ext: None,
            rc,
            size: None,
            output_bytes: None,
        }
    }

    pub fn with_size(mut self, rc_size: u64, ref_size: u64) -> Row {
        self.size = Some((rc_size, ref_size));
        self
    }

    pub fn with_output(mut self, bytes: u64) -> Row {
        self.output_bytes = Some(bytes);
        self
    }
}

/// A platform's results, printed as a Markdown table matching the layout
/// of the committed `benchmark/*.md` files. In rom-converto-only mode the
/// reference columns are replaced by a single Output column.
pub struct Table {
    pub title: String,
    pub ext_label: String,
    pub rom_converto_only: bool,
    pub rows: Vec<Row>,
}

impl Table {
    pub fn new(title: impl Into<String>, ext_label: impl Into<String>) -> Table {
        Table {
            title: title.into(),
            ext_label: ext_label.into(),
            rom_converto_only: false,
            rows: Vec::new(),
        }
    }

    pub fn print(&self) {
        println!();
        println!("### {}", self.title);
        println!();
        if self.rom_converto_only {
            self.print_rom_converto_only();
        } else {
            self.print_head_to_head();
        }
    }

    fn print_head_to_head(&self) {
        println!(
            "| Operation | {} warm mean | rom-converto warm mean | Delta | Size delta |",
            self.ext_label
        );
        println!("|---|---:|---:|---:|---:|");
        for row in &self.rows {
            let ext_cell = match &row.ext {
                Some(s) => format!("{} (sigma = {:.3})", fmt_seconds(s.mean), s.sigma),
                None => "-".to_string(),
            };
            let delta_cell = match &row.ext {
                Some(s) => delta_ratio(row.rc.mean, s.mean),
                None => "-".to_string(),
            };
            let size_cell = match row.size {
                Some((rc, reference)) => fmt_size_delta(rc, reference),
                None => "-".to_string(),
            };
            println!(
                "| {} | {ext_cell} | {} | {delta_cell} | {size_cell} |",
                row.operation,
                rc_cell(&row.rc)
            );
        }
    }

    fn print_rom_converto_only(&self) {
        println!("| Operation | rom-converto warm mean | Output |");
        println!("|---|---:|---:|");
        for row in &self.rows {
            let output = row
                .output_bytes
                .map(human_size)
                .unwrap_or_else(|| "-".to_string());
            println!("| {} | {} | {output} |", row.operation, rc_cell(&row.rc));
        }
    }
}

fn rc_cell(rc: &WarmStats) -> String {
    format!("**{} (sigma = {:.3})**", fmt_seconds(rc.mean), rc.sigma)
}
