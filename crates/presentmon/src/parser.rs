//! PresentMon CSV parsing — header-driven, column-order-independent.
//!
//! PresentMon's console CSV output starts with a header line naming
//! the columns; the set and order vary across PresentMon versions and
//! flags. We resolve the three columns we need by name at header time
//! and ignore everything else, so a PresentMon upgrade that adds
//! columns doesn't break the parser (same forward-compatibility
//! stance as the recorder's jsonl reader).
//!
//! Columns used:
//! * `Application` — exe name, used to filter to the target process.
//! * `ProcessID` — same, by PID (preferred filter when known).
//! * `msBetweenPresents` — the frame time. This is the load-bearing
//!   number; a header without it is a hard error.
//! * `Dropped` — 1 if this present was dropped (composed away / not
//!   displayed), 0 otherwise. Optional: older PresentMon builds or
//!   trimmed column sets may omit it, in which case drops read as
//!   honest absence (0), never a fabricated claim.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("PresentMon header missing required column msBetweenPresents")]
    MissingFrameTimeColumn,
    #[error("data line before header")]
    DataBeforeHeader,
}

/// One parsed present (frame) from a PresentMon data line.
#[derive(Debug, Clone, PartialEq)]
pub struct PresentRow {
    pub application: Option<String>,
    pub process_id: Option<u32>,
    /// Frame time in microseconds (converted from the CSV's
    /// milliseconds-as-float).
    pub frame_time_us: u64,
    /// This present was dropped (PresentMon `Dropped == 1`). `false`
    /// when the column is absent or unparseable — honest absence, not
    /// a claim the frame displayed.
    pub dropped: bool,
}

/// Streaming line parser. Feed lines as they arrive from the child's
/// stdout; the header line configures the column mapping.
#[derive(Debug, Default)]
pub struct CsvParser {
    columns: Option<Columns>,
    /// Malformed data lines skipped since construction (parse
    /// resilience mirror of the recorder's skipped-line counter).
    pub skipped_lines: u64,
}

#[derive(Debug, Clone, Copy)]
struct Columns {
    application: Option<usize>,
    process_id: Option<usize>,
    ms_between_presents: usize,
    dropped: Option<usize>,
}

impl CsvParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one line. Returns `Ok(Some(row))` for a parsed data line,
    /// `Ok(None)` for the header / empty / skipped lines.
    pub fn feed_line(&mut self, line: &str) -> Result<Option<PresentRow>, ParseError> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        let Some(cols) = self.columns else {
            // First non-empty line must be the header.
            let names: Vec<&str> = line.split(',').map(str::trim).collect();
            let find = |name: &str| names.iter().position(|c| c.eq_ignore_ascii_case(name));
            let Some(ms_between_presents) = find("msBetweenPresents") else {
                return Err(ParseError::MissingFrameTimeColumn);
            };
            self.columns = Some(Columns {
                application: find("Application"),
                process_id: find("ProcessID"),
                ms_between_presents,
                dropped: find("Dropped"),
            });
            return Ok(None);
        };

        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(ms_str) = fields.get(cols.ms_between_presents) else {
            self.skipped_lines += 1;
            return Ok(None);
        };
        let Ok(ms) = ms_str.parse::<f64>() else {
            self.skipped_lines += 1;
            return Ok(None);
        };
        if !ms.is_finite() || ms < 0.0 {
            self.skipped_lines += 1;
            return Ok(None);
        }
        Ok(Some(PresentRow {
            application: cols
                .application
                .and_then(|i| fields.get(i))
                .map(|s| s.to_string()),
            process_id: cols
                .process_id
                .and_then(|i| fields.get(i))
                .and_then(|s| s.parse().ok()),
            frame_time_us: (ms * 1000.0).round() as u64,
            // PresentMon writes the flag as 0/1; some builds emit
            // "True"/"False". Accept both; anything else → not dropped.
            dropped: cols
                .dropped
                .and_then(|i| fields.get(i))
                .map(|s| matches!(s.trim(), "1" | "true" | "True" | "TRUE"))
                .unwrap_or(false),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "Application,ProcessID,SwapChainAddress,Runtime,SyncInterval,PresentFlags,\
         AllowsTearing,PresentMode,Dropped,TimeInSeconds,msInPresentAPI,\
         msBetweenPresents,msUntilRenderComplete,msUntilDisplayed";

    #[test]
    fn header_then_rows_parse_by_column_name() {
        let mut p = CsvParser::new();
        assert_eq!(p.feed_line(HEADER).unwrap(), None);
        let row = p
            .feed_line("Attila.exe,16444,0x1,DXGI,1,0,0,Composed,0,12.5,0.2,16.667,14.1,18.0")
            .unwrap()
            .expect("data row parses");
        assert_eq!(row.application.as_deref(), Some("Attila.exe"));
        assert_eq!(row.process_id, Some(16444));
        assert_eq!(row.frame_time_us, 16_667);
        assert!(!row.dropped, "Dropped=0 present is not dropped");
    }

    #[test]
    fn dropped_column_is_parsed_by_name() {
        let mut p = CsvParser::new();
        p.feed_line(HEADER).unwrap();
        // Same row but Dropped=1 (the 9th field, index 8).
        let row = p
            .feed_line("Attila.exe,16444,0x1,DXGI,1,0,0,Composed,1,12.5,0.2,16.667,14.1,18.0")
            .unwrap()
            .expect("data row parses");
        assert!(row.dropped, "Dropped=1 present is dropped");
    }

    #[test]
    fn dropped_absent_from_header_reads_as_not_dropped() {
        let mut p = CsvParser::new();
        // A trimmed header without a Dropped column is valid.
        p.feed_line("ProcessID,Application,msBetweenPresents")
            .unwrap();
        let row = p.feed_line("42,game.exe,16.0").unwrap().unwrap();
        assert!(!row.dropped, "no Dropped column → honest absence (false)");
    }

    #[test]
    fn column_order_does_not_matter() {
        let mut p = CsvParser::new();
        p.feed_line("msBetweenPresents,Dropped,ProcessID,Application")
            .unwrap();
        let row = p.feed_line("8.333,1,42,game.exe").unwrap().unwrap();
        assert_eq!(row.frame_time_us, 8_333);
        assert_eq!(row.process_id, Some(42));
        assert!(row.dropped, "Dropped resolves by name regardless of order");
    }

    #[test]
    fn header_without_frame_time_is_a_hard_error() {
        let mut p = CsvParser::new();
        assert_eq!(
            p.feed_line("Application,ProcessID"),
            Err(ParseError::MissingFrameTimeColumn)
        );
    }

    #[test]
    fn malformed_data_lines_are_skipped_not_fatal() {
        let mut p = CsvParser::new();
        p.feed_line(HEADER).unwrap();
        assert_eq!(
            p.feed_line("garbage line with,not,enough,columns").unwrap(),
            None
        );
        assert_eq!(
            p.feed_line("a,1,x,x,x,x,x,x,x,x,x,NaN,x,x").unwrap(),
            None,
            "NaN frame time is skipped"
        );
        assert_eq!(
            p.feed_line("a,1,x,x,x,x,x,x,x,x,x,-5.0,x,x").unwrap(),
            None,
            "negative frame time is skipped"
        );
        assert_eq!(p.skipped_lines, 3);
        // Stream recovers on the next good line.
        assert!(p
            .feed_line("a,1,x,x,x,x,x,x,x,x,x,16.0,x,x")
            .unwrap()
            .is_some());
    }
}
