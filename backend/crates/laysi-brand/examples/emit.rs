//! Write the wordmark and monogram to files for visual inspection.
//!
//! ```sh
//! cargo run -p laysi-brand --example emit -- /tmp/preview
//! ```

use laysi_brand::{monogram, wordmark, MarkOptions};

fn main() -> std::io::Result<()> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let opts = MarkOptions {
        height: 96.0,
        color: "#201f1d".into(),
        build: laysi_brand::integrity::build_fingerprint(),
    };
    std::fs::create_dir_all(&dir)?;
    std::fs::write(format!("{dir}/wordmark.svg"), wordmark(&opts))?;
    std::fs::write(format!("{dir}/monogram.svg"), monogram(&opts))?;
    println!("已寫入 {dir}/wordmark.svg 與 {dir}/monogram.svg");
    Ok(())
}
