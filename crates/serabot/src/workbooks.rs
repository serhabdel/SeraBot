//! Bounded XLSX creation, inspection, editing and round trip.
//!
//! The model is deliberately literal about *values*: serde denies unknown fields, so a larger
//! JSON document cannot smuggle macros, external links, or embedded objects into a workbook.
//! Every string written is length- and control-character-checked here, mirroring `deck.rs`.
//!
//! Three invariants shape the module:
//!
//! * Formulas are never evaluated. `gridcraft-calc` is not a dependency; the crate that is
//!   (`gridcraft-xlsx`) only reads and writes the file format. A formula edit stores the text in
//!   the cell's formula slot and leaves the cached result empty, so Excel recalculates on open
//!   and SeraBot never computes anything.
//! * The owner's document survives a round trip. Reading and re-writing keeps cell styles,
//!   number formats, column widths, row heights, merged cells, charts and floating images,
//!   because `gridcraft-model` models all of them and the writer emits them again.
//! * Input is bounded before it reaches the parser. The crate caps decompressed parts at
//!   512 MB, which is far too generous for a single-operator agent, so the package's own
//!   central directory is read here first: oversize input, macros, external workbook links,
//!   and embedded objects are rejected by name rather than silently stripped. Images are
//!   content, not code, so `xl/media/` is accepted.
//!
//! All entry points take byte slices. Files arrive through the inbox or the `ArtifactStore`;
//! no model-chosen path is ever opened here.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use gridcraft_core::date::{datetime_from_serial, days_in_month, serial_from_ymd};
use gridcraft_core::{CellRef, DateSystem, RangeRef, Value, letters_to_col};
use gridcraft_model::{
    Cell, Formula, NumFmt, Preserved, Sheet, Style, StyleId, Workbook as GridWorkbook,
};
use gridcraft_xlsx::{IoError, ReadReport, read_xlsx, write_xlsx};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};

use crate::domain::Language;
use crate::error::{Result, SeraError};
use crate::gateway::{Tool, ToolContext, ToolOutput, ToolSpec};
use crate::maker::{ArtifactMeta, ArtifactStore};
use crate::store::Store;

/// Largest workbook accepted for import or edit. The crate's own cap (512 MB decompressed) is
/// far too generous, so the bound is enforced here before any parsing happens.
pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
/// Bound on the sum of declared uncompressed part sizes, checked from the zip central directory
/// before the crate inflates anything.
pub const MAX_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
/// Bound on the number of parts in a package.
pub const MAX_PACKAGE_PARTS: usize = 4096;

pub const MAX_TITLE_CHARS: usize = 140;
pub const MAX_SHEET_NAME_CHARS: usize = 31;
/// Sheets a *spec* may ask `xlsx_build` to create.
pub const MAX_SHEETS: usize = 20;
pub const MAX_COLUMNS: usize = 64;
pub const MAX_ROWS_PER_SHEET: usize = 5000;
/// Cells a *spec* may ask `xlsx_build` to create.
pub const MAX_CELLS: usize = 50_000;
pub const MAX_TEXT_CHARS: usize = 500;
pub const MAX_EDITS: usize = 500;
pub const MAX_WARNINGS: usize = 20;

/// Sheets accepted from a workbook the owner supplies. A real workbook is larger than anything
/// SeraBot builds, so import, inspect, read and edit use these wider bounds; `xlsx_build` keeps
/// the spec bounds above.
pub const MAX_IMPORT_SHEETS: usize = 100;
/// Cells accepted from a workbook the owner supplies.
pub const MAX_IMPORT_CELLS: usize = 500_000;
/// Cells one `xlsx_read` call may cover. A larger range is refused, never truncated.
pub const MAX_READ_CELLS: usize = 2_000;
/// Characters of a single value `xlsx_read` returns before it clips and flags it.
pub const MAX_READ_VALUE_CHARS: usize = 500;
/// Bound on a formula written into a cell. Long enough for a real expression, short enough that
/// a model cannot paste a document into one cell.
pub const MAX_FORMULA_CHARS: usize = 512;
/// Bound on a number-format code. Excel's own limit for a custom format is 255 characters.
pub const MAX_NUMBER_FORMAT_CHARS: usize = 128;
/// Column width bounds, in characters as the Excel Column Width dialog shows them.
pub const MIN_COLUMN_WIDTH_CHARS: f64 = 0.5;
pub const MAX_COLUMN_WIDTH_CHARS: f64 = 255.0;

/// Number format used for literal dates: they are stored as serial numbers, as Excel does.
pub const DATE_NUMBER_FORMAT: &str = "yyyy-mm-dd";

/// Functions that reach outside the workbook when Excel recalculates. SeraBot writes formula
/// text but refuses these, because they are remote or executable effects rather than content —
/// the same reason macros and external links are rejected.
const REMOTE_FORMULA_FUNCTIONS: &[&str] = &[
    "WEBSERVICE",
    "FILTERXML",
    "RTD",
    "DDE",
    "CALL",
    "EXEC",
    "REGISTER.ID",
    "IMPORTDATA",
    "IMPORTHTML",
    "IMPORTXML",
    "IMPORTFEED",
    "IMPORTRANGE",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkbookSpec {
    pub title: String,
    /// Optional document language, recorded with the artifact. Defaults to English.
    #[serde(default)]
    pub language: Option<Language>,
    pub sheets: Vec<SheetSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetSpec {
    pub name: String,
    /// Optional header row, written on row 1.
    #[serde(default)]
    pub header: Vec<String>,
    /// Data rows, written under the header. A short row leaves the remaining cells blank.
    #[serde(default)]
    pub rows: Vec<Vec<CellValue>>,
}

/// A literal cell value. There is no formula variant on purpose: formulas are set through
/// [`Edit::SetFormula`], which never evaluates them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CellValue {
    /// Free text. An empty string leaves the cell blank.
    Text(String),
    /// A finite number.
    Number(f64),
    /// A calendar date written as `YYYY-MM-DD`.
    Date(String),
}

/// A bounded literal cell edit against an imported workbook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellEdit {
    pub sheet: String,
    /// A single `A1`-style address (`B7`, `$B$7`). Ranges are rejected.
    pub address: String,
    pub value: CellValue,
}

/// One bounded edit against an imported workbook.
///
/// The literal form `{sheet, address, value}` is still accepted for compatibility; the explicit
/// form adds `op` and is what the tool documents.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "RawEdit")]
pub enum Edit {
    /// Replace a cell with a literal value.
    SetValue {
        sheet: String,
        address: String,
        value: CellValue,
    },
    /// Store formula text in a cell. The text is written as given and never evaluated; the
    /// cached result is left empty so Excel recalculates when the owner opens the file.
    SetFormula {
        sheet: String,
        address: String,
        formula: String,
    },
    /// Give a cell a number-format code, keeping the rest of its style.
    SetNumberFormat {
        sheet: String,
        address: String,
        format: String,
    },
    /// Set a column width in characters, as the Column Width dialog shows it.
    SetColumnWidth {
        sheet: String,
        column: String,
        width: f64,
    },
    /// Merge a rectangular range that is not already merged.
    MergeCells { sheet: String, range: String },
}

impl Edit {
    /// The operation name, for messages and audit summaries.
    pub fn op(&self) -> &'static str {
        match self {
            Edit::SetValue { .. } => "set_value",
            Edit::SetFormula { .. } => "set_formula",
            Edit::SetNumberFormat { .. } => "set_number_format",
            Edit::SetColumnWidth { .. } => "set_column_width",
            Edit::MergeCells { .. } => "merge_cells",
        }
    }

    fn apply(&self, workbook: &mut GridWorkbook, index: usize) -> Result<()> {
        let label = format!("edits[{index}]");
        match self {
            Edit::SetValue {
                sheet,
                address,
                value,
            } => {
                value.validate(&format!("{label}.value"))?;
                let sheet_index = resolve_sheet(workbook, sheet, &label)?;
                let cell = resolve_cell(address, &label)?;
                // A date keeps the cell's other formatting and only gains the date format.
                let style = match value {
                    CellValue::Date(_) => {
                        let current = workbook
                            .sheet(sheet_index)
                            .map_or(StyleId::DEFAULT, |sheet| sheet.style_id(cell));
                        workbook.styles.derive(current, |style| {
                            style.num_fmt = NumFmt::new(DATE_NUMBER_FORMAT);
                        })
                    }
                    _ => StyleId::DEFAULT,
                };
                let sheet = workbook.sheet_mut(sheet_index).ok_or_else(missing_sheet)?;
                write_literal(sheet, cell, value, style)
            }
            Edit::SetFormula {
                sheet,
                address,
                formula,
            } => {
                check_formula(&format!("{label}.formula"), formula)?;
                let sheet_index = resolve_sheet(workbook, sheet, &label)?;
                let cell = resolve_cell(address, &label)?;
                let text = formula
                    .strip_prefix('=')
                    .unwrap_or(formula)
                    .trim()
                    .to_string();
                let style = workbook
                    .sheet(sheet_index)
                    .map_or(StyleId::DEFAULT, |sheet| sheet.style_id(cell));
                let sheet = workbook.sheet_mut(sheet_index).ok_or_else(missing_sheet)?;
                sheet.set_cell(
                    cell,
                    Cell {
                        value: Value::Empty,
                        // `expr: None` keeps the text verbatim: no parse, no calculation.
                        formula: Some(Arc::new(Formula {
                            text,
                            expr: None,
                            array: None,
                        })),
                        style,
                    },
                );
                Ok(())
            }
            Edit::SetNumberFormat {
                sheet,
                address,
                format,
            } => {
                check_number_format(&format!("{label}.format"), format)?;
                let sheet_index = resolve_sheet(workbook, sheet, &label)?;
                let cell = resolve_cell(address, &label)?;
                let current = workbook
                    .sheet(sheet_index)
                    .map_or(StyleId::DEFAULT, |sheet| sheet.style_id(cell));
                let style = workbook.styles.derive(current, |style| {
                    style.num_fmt = NumFmt::new(format);
                });
                let sheet = workbook.sheet_mut(sheet_index).ok_or_else(missing_sheet)?;
                sheet.set_style(cell, style);
                Ok(())
            }
            Edit::SetColumnWidth {
                sheet,
                column,
                width,
            } => {
                let column = check_column(&format!("{label}.column"), column)?;
                let width = column_width_px(&format!("{label}.width"), *width)?;
                let sheet_index = resolve_sheet(workbook, sheet, &label)?;
                let sheet = workbook.sheet_mut(sheet_index).ok_or_else(missing_sheet)?;
                let info = sheet.cols.entry(column).or_default();
                info.size = Some(width);
                Ok(())
            }
            Edit::MergeCells { sheet, range } => {
                let range = check_merge_range(&format!("{label}.range"), range)?;
                let sheet_index = resolve_sheet(workbook, sheet, &label)?;
                let sheet = workbook.sheet_mut(sheet_index).ok_or_else(missing_sheet)?;
                if sheet
                    .merges
                    .iter()
                    .any(|existing| existing.intersects(&range))
                {
                    return Err(SeraError::Validation(format!(
                        "{label}.range overlaps a range that is already merged in sheet '{sheet_name}'",
                        sheet_name = sheet.name
                    )));
                }
                sheet.merges.push(range);
                Ok(())
            }
        }
    }
}

/// What an imported workbook looks like: sheet names, used ranges, cell and object counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workbook {
    pub sheets: Vec<SheetSummary>,
    /// Floating pictures across every sheet.
    pub images: usize,
    /// Charts across every sheet.
    pub charts: usize,
    /// Cells holding a formula. The text is never evaluated.
    pub formulas: usize,
    /// Whether any cell holds a formula.
    pub has_formulas: bool,
    /// Bounded notes from the lenient reader (skipped optional parts, repairs).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetSummary {
    pub name: String,
    /// Smallest range containing a stored cell, e.g. `A1:D10`; `None` for an empty sheet.
    pub used_range: Option<String>,
    pub cells: usize,
    pub formulas: usize,
    pub images: usize,
    pub charts: usize,
    pub merged_ranges: usize,
}

impl Workbook {
    pub fn sheet_names(&self) -> Vec<&str> {
        self.sheets
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect()
    }

    pub fn cell_count(&self) -> usize {
        self.sheets.iter().map(|sheet| sheet.cells).sum()
    }
}

/// What a cell holds, as `xlsx_read` reports it. A formula cell is never evaluated: its text and
/// the result Excel last cached are reported separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CellKind {
    Text,
    Number,
    /// A number whose cell format is a date or time format, reported as `YYYY-MM-DD[ HH:MM:SS]`.
    Date,
    Boolean,
    /// An Excel error value such as `#DIV/0!`, reported as its text.
    Error,
    Formula,
    /// A cell that is stored (it carries formatting) but holds no value.
    Blank,
}

/// One cell of a read range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellRecord {
    /// `A1`-style address within the sheet.
    pub address: String,
    pub kind: CellKind,
    /// The value: a string for text, dates and errors, a number for numbers, a boolean for
    /// booleans. `None` for a formula cell (see [`CellRecord::cached`]) and for a blank cell.
    pub value: Option<JsonValue>,
    /// Formula text without the leading `=`, for a formula cell.
    pub formula: Option<String>,
    /// The result Excel last cached for the formula. SeraBot never computes it, so it can be
    /// absent or stale.
    pub cached: Option<JsonValue>,
    /// The value was longer than the per-value limit and was clipped.
    pub truncated: bool,
}

/// A bounded read of one worksheet range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeRead {
    pub sheet: String,
    /// The range read, in `A1` form; `None` when the sheet is empty.
    pub range: Option<String>,
    /// Stored cells in the range, in row-major order.
    pub cells: Vec<CellRecord>,
    /// Cells the requested range covers, blank or not. Bounded by [`MAX_READ_CELLS`].
    pub requested_cells: u64,
    /// How many values were clipped at [`MAX_READ_VALUE_CHARS`].
    pub truncated_values: usize,
}

/// Validate a spec without producing bytes.
pub fn validate_spec(spec: &WorkbookSpec) -> Result<()> {
    check_line("workbook.title", &spec.title, MAX_TITLE_CHARS)?;
    if spec.sheets.is_empty() {
        return Err(SeraError::Validation(
            "workbook must contain at least one sheet".into(),
        ));
    }
    if spec.sheets.len() > MAX_SHEETS {
        return Err(SeraError::Validation(format!(
            "workbook.sheets must contain at most {MAX_SHEETS} sheets"
        )));
    }

    let mut seen: Vec<String> = Vec::with_capacity(spec.sheets.len());
    let mut cells = 0usize;
    for (sheet_index, sheet) in spec.sheets.iter().enumerate() {
        check_sheet_name(&format!("sheets[{sheet_index}].name"), &sheet.name)?;
        if seen
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&sheet.name))
        {
            return Err(SeraError::Validation(format!(
                "sheets[{sheet_index}].name duplicates an earlier sheet name"
            )));
        }
        seen.push(sheet.name.clone());

        if sheet.header.len() > MAX_COLUMNS {
            return Err(SeraError::Validation(format!(
                "sheets[{sheet_index}].header must contain at most {MAX_COLUMNS} cells"
            )));
        }
        if sheet.rows.len() > MAX_ROWS_PER_SHEET {
            return Err(SeraError::Validation(format!(
                "sheets[{sheet_index}].rows must contain at most {MAX_ROWS_PER_SHEET} rows"
            )));
        }
        cells = cells.saturating_add(sheet.header.len());
        for (cell_index, text) in sheet.header.iter().enumerate() {
            check_cell_text(&format!("sheets[{sheet_index}].header[{cell_index}]"), text)?;
        }

        for (row_index, row) in sheet.rows.iter().enumerate() {
            if row.len() > MAX_COLUMNS {
                return Err(SeraError::Validation(format!(
                    "sheets[{sheet_index}].rows[{row_index}] must contain at most {MAX_COLUMNS} cells"
                )));
            }
            cells = cells.saturating_add(row.len());
            for (cell_index, value) in row.iter().enumerate() {
                value.validate(&format!(
                    "sheets[{sheet_index}].rows[{row_index}][{cell_index}]"
                ))?;
            }
        }
    }

    if cells > MAX_CELLS {
        return Err(SeraError::Validation(format!(
            "workbook must contain at most {MAX_CELLS} cells"
        )));
    }
    Ok(())
}

/// Build a valid `.xlsx` package from a bounded spec.
pub fn build(spec: &WorkbookSpec) -> Result<Vec<u8>> {
    validate_spec(spec)?;

    let mut workbook = GridWorkbook::new();
    workbook.sheets.clear();
    workbook.props.title = spec.title.clone();
    let date_style = workbook.styles.intern(Style {
        num_fmt: NumFmt::new(DATE_NUMBER_FORMAT),
        ..Style::default()
    });

    for sheet_spec in &spec.sheets {
        let mut sheet = Sheet::new(sheet_spec.name.clone());
        for (column, text) in sheet_spec.header.iter().enumerate() {
            let cell = CellRef::new(0, column as u32);
            write_text(&mut sheet, cell, text);
        }
        for (row_index, row) in sheet_spec.rows.iter().enumerate() {
            let row_index = row_index as u32 + 1;
            for (column, value) in row.iter().enumerate() {
                write_literal(
                    &mut sheet,
                    CellRef::new(row_index, column as u32),
                    value,
                    date_style,
                )?;
            }
        }
        workbook.sheets.push(Arc::new(sheet));
    }

    write_xlsx(&workbook).map_err(write_error)
}

/// Parse an existing workbook into a bounded summary, rejecting unsafe packages.
pub fn import(bytes: &[u8]) -> Result<Workbook> {
    let (workbook, report) = load(bytes)?;
    Ok(summarize(&workbook, &report))
}

/// Read the values of one worksheet range, so a caller can see what is in a cell before editing
/// it. Read-only: nothing is written and no artifact is produced.
///
/// `range` is `Sheet1!A1:D20`, `'My Sheet'!A1:D20`, or a bare sheet name for its whole used
/// range. The range must sit inside the sheet's used range and cover at most
/// [`MAX_READ_CELLS`] cells; a larger range is refused rather than truncated.
pub fn read_range(bytes: &[u8], range: &str) -> Result<RangeRead> {
    let (workbook, _) = load(bytes)?;
    read_range_in(&workbook, range)
}

fn read_range_in(workbook: &GridWorkbook, spec: &str) -> Result<RangeRead> {
    let (sheet_index, requested) = resolve_read_range(workbook, spec)?;
    let sheet = workbook.sheet(sheet_index).ok_or_else(missing_sheet)?;
    let used = sheet.cells.used_range();

    let target = match requested {
        Some(range) => {
            let used = used.ok_or_else(|| {
                SeraError::Validation(format!(
                    "sheet '{}' is empty; there is no used range to read",
                    sheet.name
                ))
            })?;
            if !used.contains_range(&range) {
                return Err(SeraError::Validation(format!(
                    "range {} is outside the used range {} of sheet '{}'; read inside it",
                    range.a1(),
                    used.a1(),
                    sheet.name
                )));
            }
            range
        }
        None => match used {
            Some(range) => range,
            None => {
                return Ok(RangeRead {
                    sheet: sheet.name.clone(),
                    range: None,
                    cells: Vec::new(),
                    requested_cells: 0,
                    truncated_values: 0,
                });
            }
        },
    };

    let requested_cells = target.count();
    if requested_cells > MAX_READ_CELLS as u64 {
        return Err(SeraError::Validation(format!(
            "range {} covers {requested_cells} cells; a read is limited to {MAX_READ_CELLS}. \
             Read a narrower range.",
            target.a1()
        )));
    }

    let mut cells = Vec::new();
    let mut truncated_values = 0usize;
    for (cell_ref, cell) in sheet.cells.iter_range(target) {
        let record = cell_record(workbook, sheet, cell_ref, cell);
        if record.truncated {
            truncated_values = truncated_values.saturating_add(1);
        }
        cells.push(record);
    }

    Ok(RangeRead {
        sheet: sheet.name.clone(),
        range: Some(target.a1()),
        cells,
        requested_cells,
        truncated_values,
    })
}

/// Build one record for a stored cell. Nothing is evaluated: a formula cell reports its text and
/// whatever result the file cached.
fn cell_record(
    workbook: &GridWorkbook,
    sheet: &Sheet,
    cell_ref: CellRef,
    cell: &Cell,
) -> CellRecord {
    let address = cell_ref.a1();
    if let Some(formula) = cell.formula.as_deref() {
        let cached = value_json(workbook, sheet, cell_ref, &cell.value);
        return CellRecord {
            address,
            kind: CellKind::Formula,
            value: None,
            formula: Some(formula.text.clone()),
            cached: cached.map(|(_, json)| json),
            truncated: false,
        };
    }
    match value_json(workbook, sheet, cell_ref, &cell.value) {
        Some((kind, value)) => {
            let (value, truncated) = clip_json(value);
            CellRecord {
                address,
                kind,
                value: Some(value),
                formula: None,
                cached: None,
                truncated,
            }
        }
        None => CellRecord {
            address,
            kind: CellKind::Blank,
            value: None,
            formula: None,
            cached: None,
            truncated: false,
        },
    }
}

/// The kind and JSON value of a scalar cell value, or `None` when the cell is empty.
fn value_json(
    workbook: &GridWorkbook,
    sheet: &Sheet,
    cell_ref: CellRef,
    value: &Value,
) -> Option<(CellKind, JsonValue)> {
    match value.scalar() {
        Value::Empty => None,
        Value::Text(text) => Some((CellKind::Text, JsonValue::String(text.to_string()))),
        Value::Bool(flag) => Some((CellKind::Boolean, JsonValue::Bool(flag))),
        Value::Error(error) => Some((
            CellKind::Error,
            JsonValue::String(error.as_str().to_string()),
        )),
        Value::Number(number) => {
            let style = sheet.style_id(cell_ref);
            let format = workbook.styles.get(style).num_fmt.as_str();
            if number_format_is_date(format) {
                if let Some(text) = format_serial(workbook.date_system, number) {
                    return Some((CellKind::Date, JsonValue::String(text)));
                }
            }
            Some((CellKind::Number, json!(number)))
        }
        // `scalar()` already unwrapped arrays; this arm keeps the match exhaustive.
        Value::Array(_) => None,
    }
}

/// Whether a number-format code shows a date or time, so a numeric cell should be reported as a
/// date. Quoted literals, `\`-escapes, `_`/`*` padding and `[...]` sections are skipped first, so
/// `"$"#,##0_);[Red]\("$"#,##0\)` is not a date.
fn number_format_is_date(code: &str) -> bool {
    let mut chars = code.chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => {
                for inner in chars.by_ref() {
                    if inner == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => {
                for inner in chars.by_ref() {
                    if inner == ']' {
                        break;
                    }
                }
            }
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' => return true,
            _ => {}
        }
    }
    false
}

/// A serial number as `YYYY-MM-DD`, with ` HH:MM:SS` when it carries a time of day.
fn format_serial(system: DateSystem, serial: f64) -> Option<String> {
    let moment = datetime_from_serial(system, serial)?;
    if moment.hour == 0 && moment.minute == 0 && moment.second == 0 && moment.milli == 0 {
        Some(format!(
            "{:04}-{:02}-{:02}",
            moment.year, moment.month, moment.day
        ))
    } else {
        Some(format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            moment.year, moment.month, moment.day, moment.hour, moment.minute, moment.second
        ))
    }
}

/// Clip a value to [`MAX_READ_VALUE_CHARS`], reporting whether it was clipped.
fn clip_json(value: JsonValue) -> (JsonValue, bool) {
    match value {
        JsonValue::String(text) => {
            if text.chars().count() <= MAX_READ_VALUE_CHARS {
                (JsonValue::String(text), false)
            } else {
                let clipped: String = text.chars().take(MAX_READ_VALUE_CHARS).collect();
                (JsonValue::String(clipped), true)
            }
        }
        other => (other, false),
    }
}

/// Resolve `Sheet1!A1:D20`, `'My Sheet'!A1:D20` or a bare sheet name to a sheet and an optional
/// range. The workbook decides the split, so a sheet name containing `!` still resolves.
fn resolve_read_range(workbook: &GridWorkbook, spec: &str) -> Result<(usize, Option<RangeRef>)> {
    let text = spec.trim();
    if text.is_empty() {
        return Err(SeraError::Validation(
            "range must not be empty; use Sheet1!A1:D20 or a sheet name".into(),
        ));
    }

    if let Some(rest) = text.strip_prefix('\'') {
        let (name, rest) = split_quoted(rest)?;
        let index = workbook
            .sheet_index(&name)
            .ok_or_else(|| unknown_sheet(workbook, &name))?;
        let rest = rest.trim();
        if rest.is_empty() {
            return Ok((index, None));
        }
        let rest = rest.strip_prefix('!').ok_or_else(|| {
            SeraError::Validation(format!(
                "expected '!' after the quoted sheet name '{}'",
                name
            ))
        })?;
        let range = parse_read_range(rest)?;
        return Ok((index, Some(range)));
    }

    for (position, _) in text.match_indices('!') {
        let (name, rest) = text.split_at(position);
        let Some(index) = workbook.sheet_index(name.trim()) else {
            continue;
        };
        let rest = rest.get(1..).unwrap_or("").trim();
        if rest.is_empty() {
            return Ok((index, None));
        }
        let range = parse_read_range(rest)?;
        return Ok((index, Some(range)));
    }

    let index = workbook
        .sheet_index(text)
        .ok_or_else(|| unknown_sheet(workbook, text))?;
    Ok((index, None))
}

/// Split a quoted sheet name: the text after the opening `'`, with `''` unescaped. Returns the
/// name and the remaining text.
fn split_quoted(text: &str) -> Result<(String, &str)> {
    let mut name = String::new();
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if character == '\'' {
            if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                chars.next();
                name.push('\'');
                continue;
            }
            let rest = text.get(index + 1..).unwrap_or("");
            return Ok((name, rest));
        }
        name.push(character);
    }
    Err(SeraError::Validation(
        "range has an unterminated quoted sheet name".into(),
    ))
}

fn parse_read_range(text: &str) -> Result<RangeRef> {
    RangeRef::parse(text).ok_or_else(|| {
        SeraError::Validation(format!("'{text}' is not an A1-style range such as A1:D20"))
    })
}

fn unknown_sheet(workbook: &GridWorkbook, name: &str) -> SeraError {
    let sheets: Vec<&str> = workbook
        .sheets
        .iter()
        .map(|sheet| sheet.name.as_str())
        .collect();
    SeraError::Validation(format!(
        "no sheet named '{name}' in this workbook; sheets are: {}",
        sheets.join(", ")
    ))
}

/// Apply a bounded set of literal cell edits to an imported workbook and re-export it.
pub fn edit(bytes: &[u8], edits: &[CellEdit]) -> Result<Vec<u8>> {
    let converted: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit::SetValue {
            sheet: edit.sheet.clone(),
            address: edit.address.clone(),
            value: edit.value.clone(),
        })
        .collect();
    apply(bytes, &converted)
}

/// Apply a bounded set of edits to an imported workbook and re-export it.
///
/// Everything the model does not touch is written back: styles, number formats, column widths,
/// row heights, merged cells, charts and images.
pub fn apply_edits(bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>> {
    apply(bytes, edits)
}

fn apply(bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>> {
    if edits.is_empty() {
        return Err(SeraError::Validation(
            "at least one edit is required".into(),
        ));
    }
    if edits.len() > MAX_EDITS {
        return Err(SeraError::Validation(format!(
            "edits must contain at most {MAX_EDITS} entries"
        )));
    }

    let (mut workbook, _) = load(bytes)?;
    for (index, edit) in edits.iter().enumerate() {
        edit.apply(&mut workbook, index)?;
    }
    // Edits can add cells, so the bound is checked again before the package is written.
    check_import_limits(&workbook)?;
    write_xlsx(&workbook).map_err(write_error)
}

impl CellValue {
    fn validate(&self, label: &str) -> Result<()> {
        match self {
            CellValue::Text(text) => check_cell_text(label, text),
            CellValue::Number(number) => {
                if !number.is_finite() {
                    return Err(SeraError::Validation(format!(
                        "{label} must be a finite number"
                    )));
                }
                if number.abs() >= 1e308 {
                    return Err(SeraError::Validation(format!(
                        "{label} is outside the range a spreadsheet can store"
                    )));
                }
                Ok(())
            }
            CellValue::Date(text) => {
                if parse_date(text).is_none() {
                    return Err(SeraError::Validation(format!(
                        "{label} must be a real calendar date written as YYYY-MM-DD"
                    )));
                }
                Ok(())
            }
        }
    }
}

fn write_text(sheet: &mut Sheet, cell: CellRef, text: &str) {
    if text.is_empty() {
        sheet.remove_cell(cell);
    } else {
        sheet.set_value(cell, Value::text(text));
    }
}

fn write_literal(
    sheet: &mut Sheet,
    cell: CellRef,
    value: &CellValue,
    date_style: StyleId,
) -> Result<()> {
    match value {
        CellValue::Text(text) => {
            write_text(sheet, cell, text);
            Ok(())
        }
        CellValue::Number(number) => {
            sheet.set_value(cell, Value::number(*number));
            Ok(())
        }
        CellValue::Date(text) => {
            let serial = parse_date(text).ok_or_else(|| {
                SeraError::Validation(format!(
                    "{}: must be a real calendar date written as YYYY-MM-DD",
                    cell.a1()
                ))
            })?;
            sheet.set_cell(
                cell,
                Cell {
                    value: Value::Number(serial),
                    formula: None,
                    style: date_style,
                },
            );
            Ok(())
        }
    }
}

/// Strict `YYYY-MM-DD` → Excel serial in the 1900 date system.
fn parse_date(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = value.get(0..4)?.parse().ok()?;
    let month: u32 = value.get(5..7)?.parse().ok()?;
    let day: u32 = value.get(8..10)?.parse().ok()?;
    if !(1900..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    if day == 0 || day > days_in_month(year as i32, month) {
        return None;
    }
    serial_from_ymd(DateSystem::D1900, year, i64::from(month), i64::from(day))
}

fn load(bytes: &[u8]) -> Result<(GridWorkbook, ReadReport)> {
    if bytes.is_empty() {
        return Err(SeraError::Validation("workbook is empty".into()));
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(SeraError::Validation(format!(
            "workbook is {} bytes; the limit is {MAX_INPUT_BYTES} bytes (32 MiB)",
            bytes.len()
        )));
    }

    let index = index_package(bytes)?;
    reject_unsafe_parts(&index.names)?;

    let (workbook, report) = read_xlsx(bytes).map_err(read_error)?;

    if workbook.vba_project.is_some() {
        return Err(SeraError::Validation(
            "workbook contains a VBA project (macros); SeraBot does not accept macro-enabled workbooks"
                .into(),
        ));
    }
    if let Some(preserved) = workbook.preserved.as_deref() {
        check_preserved(preserved)?;
    }
    for sheet in &workbook.sheets {
        if let Some(preserved) = sheet.preserved.as_deref() {
            check_preserved(preserved)?;
        }
    }
    check_import_limits(&workbook)?;

    Ok((workbook, report))
}

/// The sheet and cell bounds for a workbook the owner supplies. Applied on import and again
/// after an edit, because an edit can add cells.
fn check_import_limits(workbook: &GridWorkbook) -> Result<()> {
    if workbook.sheets.len() > MAX_IMPORT_SHEETS {
        return Err(SeraError::Validation(format!(
            "workbook has {} sheets; SeraBot handles at most {MAX_IMPORT_SHEETS}",
            workbook.sheets.len()
        )));
    }
    let cells = workbook.sheets.iter().fold(0usize, |total, sheet| {
        total.saturating_add(sheet.cells.len())
    });
    if cells > MAX_IMPORT_CELLS {
        return Err(SeraError::Validation(format!(
            "workbook stores {cells} cells; SeraBot handles at most {MAX_IMPORT_CELLS}"
        )));
    }
    Ok(())
}

fn summarize(workbook: &GridWorkbook, report: &ReadReport) -> Workbook {
    let sheets: Vec<SheetSummary> = workbook
        .sheets
        .iter()
        .map(|sheet| SheetSummary {
            name: sheet.name.clone(),
            used_range: sheet.cells.used_range().map(|range| range.a1()),
            cells: sheet.cells.len(),
            formulas: sheet
                .cells
                .iter()
                .filter(|(_, cell)| cell.formula.is_some())
                .count(),
            images: sheet.images.len(),
            charts: sheet.charts.len(),
            merged_ranges: sheet.merges.len(),
        })
        .collect();

    let formulas: usize = sheets.iter().map(|sheet| sheet.formulas).sum();
    Workbook {
        images: sheets.iter().map(|sheet| sheet.images).sum(),
        charts: sheets.iter().map(|sheet| sheet.charts).sum(),
        formulas,
        has_formulas: formulas > 0,
        sheets,
        warnings: report
            .warnings
            .iter()
            .take(MAX_WARNINGS)
            .map(|warning| warning.chars().take(200).collect())
            .collect(),
    }
}

fn read_error(error: IoError) -> SeraError {
    match error {
        IoError::TooLarge(message) => {
            SeraError::Validation(format!("workbook part is too large for SeraBot: {message}"))
        }
        other => SeraError::Validation(format!("invalid workbook: {other}")),
    }
}

fn write_error(error: IoError) -> SeraError {
    SeraError::Internal(format!("could not write workbook: {error}"))
}

/// Names of the parts in a zip package, read from its central directory.
struct PackageIndex {
    names: Vec<String>,
}

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// The end-of-central-directory record plus its maximum comment.
const EOCD_SEARCH_WINDOW: usize = 66_000;

fn index_package(bytes: &[u8]) -> Result<PackageIndex> {
    let search_start = bytes.len().saturating_sub(EOCD_SEARCH_WINDOW);
    let eocd = (search_start..bytes.len().saturating_sub(3))
        .rev()
        .find(|offset| u32_at(bytes, *offset) == Some(EOCD_SIGNATURE))
        .ok_or_else(|| {
            SeraError::Validation("workbook is not a zip package (no central directory)".into())
        })?;

    let declared_parts = usize::from(u16_at(bytes, eocd + 10).ok_or_else(truncated_package)?);
    let directory_size = u32_at(bytes, eocd + 12).ok_or_else(truncated_package)? as usize;
    let directory_offset = u32_at(bytes, eocd + 16).ok_or_else(truncated_package)? as usize;
    if declared_parts == usize::from(u16::MAX) || directory_offset == u32::MAX as usize {
        return Err(SeraError::Validation(
            "zip64 packages are not accepted".into(),
        ));
    }
    if declared_parts > MAX_PACKAGE_PARTS {
        return Err(SeraError::Validation(format!(
            "workbook package declares {declared_parts} parts; the limit is {MAX_PACKAGE_PARTS}"
        )));
    }
    let directory_end = directory_offset
        .checked_add(directory_size)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(truncated_package)?;

    let mut names = Vec::with_capacity(declared_parts);
    let mut uncompressed_bytes = 0u64;
    let mut cursor = directory_offset;
    for _ in 0..declared_parts {
        if u32_at(bytes, cursor) != Some(CENTRAL_SIGNATURE) {
            return Err(SeraError::Validation(
                "workbook package has a malformed central directory".into(),
            ));
        }
        let uncompressed = u32_at(bytes, cursor + 24).ok_or_else(truncated_package)?;
        let name_length = usize::from(u16_at(bytes, cursor + 28).ok_or_else(truncated_package)?);
        let extra_length = usize::from(u16_at(bytes, cursor + 30).ok_or_else(truncated_package)?);
        let comment_length = usize::from(u16_at(bytes, cursor + 32).ok_or_else(truncated_package)?);
        let name_start = cursor + 46;
        let name_end = name_start
            .checked_add(name_length)
            .filter(|end| *end <= directory_end)
            .ok_or_else(truncated_package)?;
        let name = bytes
            .get(name_start..name_end)
            .ok_or_else(truncated_package)?;
        names.push(String::from_utf8_lossy(name).into_owned());
        if uncompressed != u32::MAX {
            uncompressed_bytes = uncompressed_bytes.saturating_add(u64::from(uncompressed));
        }
        cursor = name_end
            .checked_add(extra_length)
            .and_then(|next| next.checked_add(comment_length))
            .ok_or_else(truncated_package)?;
    }

    if uncompressed_bytes > MAX_UNCOMPRESSED_BYTES {
        return Err(SeraError::Validation(format!(
            "workbook declares {uncompressed_bytes} uncompressed bytes; the limit is {MAX_UNCOMPRESSED_BYTES}"
        )));
    }
    Ok(PackageIndex { names })
}

/// Reject parts that are executable or remote rather than content.
///
/// `xl/media/` is deliberately absent: pictures are the owner's content and are carried through.
/// `xl/embeddings/` holds OLE payloads and `xl/activeX/`/`xl/ctrlProps/` hold controls, which are
/// embedded objects, not images.
fn reject_unsafe_parts(names: &[String]) -> Result<()> {
    for name in names {
        let lowered = name.to_ascii_lowercase();
        if lowered.contains("vbaproject") {
            return Err(SeraError::Validation(
                "workbook contains a VBA project (macros); SeraBot does not accept macro-enabled workbooks"
                    .into(),
            ));
        }
        if lowered.starts_with("xl/externallinks/") || lowered.contains("externallink") {
            return Err(SeraError::Validation(
                "workbook contains external workbook links; SeraBot does not accept linked workbooks"
                    .into(),
            ));
        }
        if lowered.starts_with("xl/embeddings/")
            || lowered.starts_with("xl/activex/")
            || lowered.starts_with("xl/ctrlprops/")
        {
            return Err(SeraError::Validation(
                "workbook contains embedded objects or controls; SeraBot does not accept OLE payloads"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// Reject kept package content that would re-export an external link, embedded object, or
/// macro. The reader preserves unmodelled parts, so anything dangerous must fail here rather
/// than be written back out.
fn check_preserved(preserved: &Preserved) -> Result<()> {
    let has_external_link = |rel: &gridcraft_model::PreservedRel| {
        rel.rel_type.to_ascii_lowercase().contains("externallink")
            || rel.target.to_ascii_lowercase().contains("externallink")
    };
    if preserved.rels.iter().any(&has_external_link)
        || preserved.package_rels.iter().any(&has_external_link)
        || preserved
            .parts
            .iter()
            .any(|part| part.name.to_ascii_lowercase().contains("externallink"))
        || preserved
            .elements
            .iter()
            .any(|element| element.slot == "externalReferences")
    {
        return Err(SeraError::Validation(
            "workbook contains external workbook links; SeraBot does not accept linked workbooks"
                .into(),
        ));
    }
    // VML is only kept for form controls and OLE objects (notes are modelled), so its presence
    // means the sheet carries an embedded object rather than content.
    if preserved.vml.is_some()
        || preserved
            .elements
            .iter()
            .any(|element| matches!(element.slot.as_str(), "oleObjects" | "controls"))
    {
        return Err(SeraError::Validation(
            "workbook contains embedded objects or form controls; SeraBot does not accept OLE payloads"
                .into(),
        ));
    }
    Ok(())
}

fn truncated_package() -> SeraError {
    SeraError::Validation("workbook package is truncated or malformed".into())
}

fn missing_sheet() -> SeraError {
    SeraError::Internal("workbook sheet index disappeared during edit".into())
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn resolve_sheet(workbook: &GridWorkbook, name: &str, label: &str) -> Result<usize> {
    workbook.sheet_index(name).ok_or_else(|| {
        SeraError::Validation(format!(
            "{label}.sheet: no sheet named '{name}' in this workbook"
        ))
    })
}

fn resolve_cell(address: &str, label: &str) -> Result<CellRef> {
    CellRef::parse(address).ok_or_else(|| {
        SeraError::Validation(format!(
            "{label}.address: '{address}' is not a single A1-style cell address"
        ))
    })
}

/// A single column letter reference (`C`, `$C`).
fn check_column(label: &str, column: &str) -> Result<u32> {
    let trimmed = column.trim();
    let letters = trimmed.strip_prefix('$').unwrap_or(trimmed);
    if letters.is_empty() || letters.len() > 3 || !letters.bytes().all(|b| b.is_ascii_alphabetic())
    {
        return Err(SeraError::Validation(format!(
            "{label}: '{column}' is not a single column letter such as C or $C"
        )));
    }
    letters_to_col(letters).ok_or_else(|| {
        SeraError::Validation(format!(
            "{label}: '{column}' is outside the spreadsheet grid"
        ))
    })
}

/// Column width in characters (the Column Width dialog value) → the model's pixel width.
///
/// This is the inverse of `gridcraft_model::points_to_chars`, which is what the model exposes
/// for the same conversion in the other direction.
fn column_width_px(label: &str, width: f64) -> Result<f32> {
    if !width.is_finite() || !(MIN_COLUMN_WIDTH_CHARS..=MAX_COLUMN_WIDTH_CHARS).contains(&width) {
        return Err(SeraError::Validation(format!(
            "{label} must be between {MIN_COLUMN_WIDTH_CHARS} and {MAX_COLUMN_WIDTH_CHARS} characters"
        )));
    }
    let px = if width <= 1.0 {
        width * 12.0
    } else {
        width * 7.0 + 5.0
    };
    Ok(px.round() as f32)
}

fn check_merge_range(label: &str, value: &str) -> Result<RangeRef> {
    let range = RangeRef::parse(value).ok_or_else(|| {
        SeraError::Validation(format!(
            "{label}: '{value}' is not an A1-style range such as B2:D2"
        ))
    })?;
    if range.is_single() {
        return Err(SeraError::Validation(format!(
            "{label} must cover at least two cells"
        )));
    }
    if range.height() > MAX_ROWS_PER_SHEET as u32 || range.width() > MAX_COLUMNS as u32 {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {MAX_ROWS_PER_SHEET} rows by {MAX_COLUMNS} columns"
        )));
    }
    Ok(range)
}

fn check_number_format(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(SeraError::Validation(format!("{label} must not be empty")));
    }
    if value.chars().count() > MAX_NUMBER_FORMAT_CHARS {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {MAX_NUMBER_FORMAT_CHARS} characters"
        )));
    }
    if value
        .chars()
        .any(|character| character.is_control() || is_bidi_or_zero_width(character))
    {
        return Err(SeraError::Validation(format!(
            "{label} contains control or invisible formatting characters"
        )));
    }
    Ok(())
}

/// Formula text is stored verbatim, so it is checked here rather than parsed: no control
/// characters, no external or structured references, and no function that reaches outside the
/// workbook when Excel recalculates.
fn check_formula(label: &str, value: &str) -> Result<()> {
    let body = value.strip_prefix('=').unwrap_or(value);
    if body.trim().is_empty() {
        return Err(SeraError::Validation(format!("{label} must not be empty")));
    }
    if body.chars().count() > MAX_FORMULA_CHARS {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {MAX_FORMULA_CHARS} characters"
        )));
    }
    if body
        .chars()
        .any(|character| character.is_control() || is_bidi_or_zero_width(character))
    {
        return Err(SeraError::Validation(format!(
            "{label} contains control or invisible formatting characters"
        )));
    }
    if body.contains('[') || body.contains(']') {
        return Err(SeraError::Validation(format!(
            "{label} must not contain '[' or ']': external and structured references are not accepted"
        )));
    }
    for name in REMOTE_FORMULA_FUNCTIONS {
        if calls_function(body, name) {
            return Err(SeraError::Validation(format!(
                "{label} calls {name}(), which reaches outside the workbook; SeraBot does not write remote formulas"
            )));
        }
    }
    Ok(())
}

/// Whether `text` calls `name` as a function (case-insensitive, whole identifier).
fn calls_function(text: &str, name: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    let mut from = 0usize;
    while let Some(offset) = upper.get(from..).and_then(|rest| rest.find(name)) {
        let start = from + offset;
        let end = start + name.len();
        let boundary_before = upper
            .as_bytes()
            .get(start.wrapping_sub(1))
            .is_none_or(|byte| !is_identifier_byte(*byte));
        let boundary_after = upper
            .get(end..)
            .is_none_or(|rest| rest.trim_start().starts_with('('));
        if boundary_before && boundary_after {
            return true;
        }
        from = end;
    }
    false
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

fn check_line(label: &str, value: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty() {
        return Err(SeraError::Validation(format!("{label} must not be empty")));
    }
    if value.chars().count() > max_chars {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {max_chars} characters"
        )));
    }
    for character in value.chars() {
        if character.is_control() || is_bidi_or_zero_width(character) {
            return Err(SeraError::Validation(format!(
                "{label} contains control or invisible formatting characters"
            )));
        }
    }
    Ok(())
}

fn check_sheet_name(label: &str, name: &str) -> Result<()> {
    check_line(label, name, MAX_SHEET_NAME_CHARS)?;
    if name
        .chars()
        .any(|c| matches!(c, ':' | '\\' | '/' | '?' | '*' | '[' | ']'))
    {
        return Err(SeraError::Validation(format!(
            "{label} must not contain any of : \\ / ? * [ ]"
        )));
    }
    if name.starts_with('\'') || name.ends_with('\'') {
        return Err(SeraError::Validation(format!(
            "{label} must not begin or end with an apostrophe"
        )));
    }
    if name.eq_ignore_ascii_case("history") {
        return Err(SeraError::Validation(format!(
            "{label} must not be the reserved name 'History'"
        )));
    }
    Ok(())
}

fn check_cell_text(label: &str, value: &str) -> Result<()> {
    if value.chars().count() > MAX_TEXT_CHARS {
        return Err(SeraError::Validation(format!(
            "{label} must be at most {MAX_TEXT_CHARS} characters"
        )));
    }
    for character in value.chars() {
        if character.is_control() || is_bidi_or_zero_width(character) {
            return Err(SeraError::Validation(format!(
                "{label} contains control or invisible formatting characters"
            )));
        }
    }
    if value.starts_with('=') {
        return Err(SeraError::Validation(format!(
            "{label} looks like a formula; set a formula with op 'set_formula' instead"
        )));
    }
    Ok(())
}

fn is_bidi_or_zero_width(character: char) -> bool {
    matches!(
        character,
        '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'
            | '\u{2066}'..='\u{2069}'
            | '\u{00ad}'
            | '\u{feff}'
    )
}

/// A deterministic, ASCII-only artifact filename derived from the workbook title.
fn default_filename(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() {
        "workbook.xlsx".into()
    } else {
        format!("{}.xlsx", slug.trim_end_matches('-'))
    }
}

fn spec_from_args(args: &JsonValue) -> Result<WorkbookSpec> {
    serde_json::from_value(
        args.get("spec")
            .cloned()
            .ok_or_else(|| SeraError::Validation("spec is required".into()))?,
    )
    .map_err(|error| SeraError::Validation(format!("invalid workbook spec: {error}")))
}

fn optional_filename(args: &JsonValue) -> Result<Option<String>> {
    match args.get("filename").and_then(JsonValue::as_str) {
        Some(filename) => {
            check_line("filename", filename, 120)?;
            if !filename.to_ascii_lowercase().ends_with(".xlsx") {
                return Err(SeraError::Validation("filename must end with .xlsx".into()));
            }
            Ok(Some(filename.to_owned()))
        }
        None => Ok(None),
    }
}

fn spec_summary(spec: &WorkbookSpec, stored: &crate::maker::StoredArtifact) -> JsonValue {
    json!({
        "artifact_hash": stored.hash,
        "filename": stored.filename,
        "title": spec.title,
        "sheets": spec.sheets.iter().map(|sheet| json!({
            "name": sheet.name,
            "rows": sheet.rows.len(),
            "columns": sheet.header.len().max(
                sheet.rows.iter().map(Vec::len).max().unwrap_or(0)
            ),
        })).collect::<Vec<_>>(),
        "language": spec.language.unwrap_or(Language::En).code(),
    })
}

/// The JSON shape an edit arrives in, before it is resolved to one [`Edit`].
///
/// Every field is optional so that both the legacy literal form and the `op`-tagged form parse;
/// [`Edit::try_from`] then enforces exactly which fields the chosen operation takes.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEdit {
    #[serde(default)]
    op: Option<String>,
    sheet: String,
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    value: Option<CellValue>,
    #[serde(default)]
    formula: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    column: Option<String>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    range: Option<String>,
}

impl TryFrom<RawEdit> for Edit {
    type Error = String;

    fn try_from(raw: RawEdit) -> std::result::Result<Edit, String> {
        let op = match raw.op.as_deref() {
            Some(op) => op.trim().to_ascii_lowercase(),
            None => infer_op(&raw)?,
        };
        match op.as_str() {
            "set_value" => {
                check_fields("set_value", &["address", "value"], &raw)?;
                Ok(Edit::SetValue {
                    sheet: raw.sheet,
                    address: require(raw.address, "address", "set_value")?,
                    value: require(raw.value, "value", "set_value")?,
                })
            }
            "set_formula" => {
                check_fields("set_formula", &["address", "formula"], &raw)?;
                Ok(Edit::SetFormula {
                    sheet: raw.sheet,
                    address: require(raw.address, "address", "set_formula")?,
                    formula: require(raw.formula, "formula", "set_formula")?,
                })
            }
            "set_number_format" => {
                check_fields("set_number_format", &["address", "format"], &raw)?;
                Ok(Edit::SetNumberFormat {
                    sheet: raw.sheet,
                    address: require(raw.address, "address", "set_number_format")?,
                    format: require(raw.format, "format", "set_number_format")?,
                })
            }
            "set_column_width" => {
                check_fields("set_column_width", &["column", "width"], &raw)?;
                Ok(Edit::SetColumnWidth {
                    sheet: raw.sheet,
                    column: require(raw.column, "column", "set_column_width")?,
                    width: require(raw.width, "width", "set_column_width")?,
                })
            }
            "merge_cells" => {
                check_fields("merge_cells", &["range"], &raw)?;
                Ok(Edit::MergeCells {
                    sheet: raw.sheet,
                    range: require(raw.range, "range", "merge_cells")?,
                })
            }
            other => Err(format!(
                "unknown op '{other}'; use set_value, set_formula, set_number_format, set_column_width or merge_cells"
            )),
        }
    }
}

fn require<T>(field: Option<T>, name: &str, op: &str) -> std::result::Result<T, String> {
    field.ok_or_else(|| format!("{op} needs '{name}'"))
}

/// Which operation an edit without `op` means, judged by the field it carries.
fn infer_op(raw: &RawEdit) -> std::result::Result<String, String> {
    let mut found: Vec<&str> = Vec::new();
    if raw.value.is_some() {
        found.push("set_value");
    }
    if raw.formula.is_some() {
        found.push("set_formula");
    }
    if raw.format.is_some() {
        found.push("set_number_format");
    }
    if raw.width.is_some() || raw.column.is_some() {
        found.push("set_column_width");
    }
    if raw.range.is_some() {
        found.push("merge_cells");
    }
    match found.as_slice() {
        [only] => Ok((*only).to_string()),
        [] => Err(
            "edit needs an op (set_value, set_formula, set_number_format, set_column_width, merge_cells)"
                .into(),
        ),
        _ => Err("edit mixes several operations; set op to choose one".into()),
    }
}

fn check_fields(op: &str, allowed: &[&str], raw: &RawEdit) -> std::result::Result<(), String> {
    for (name, present) in [
        ("address", raw.address.is_some()),
        ("value", raw.value.is_some()),
        ("formula", raw.formula.is_some()),
        ("format", raw.format.is_some()),
        ("column", raw.column.is_some()),
        ("width", raw.width.is_some()),
        ("range", raw.range.is_some()),
    ] {
        if present && !allowed.contains(&name) {
            return Err(format!("{op} does not take '{name}'"));
        }
    }
    Ok(())
}

pub struct XlsxBuildTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl XlsxBuildTool {
    pub fn new(artifacts: Arc<ArtifactStore>, store: Store) -> Self {
        Self { artifacts, store }
    }
}

#[async_trait]
impl Tool for XlsxBuildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "xlsx_build".into(),
            description:
                "Build a deterministic .xlsx workbook from a bounded spec of literal values. \
                 Sheets carry a header row and rows of text, number, or date cells. The spec has \
                 no formula, macro, or link variant; nothing is ever calculated. Use xlsx_edit \
                 to add formulas, number formats, widths, or merges to an existing workbook."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "spec": {
                        "type": "object",
                        "description": "WorkbookSpec: title, optional language (it|fr|en), sheets[{name, header[text], rows[[{type: text|number|date, value}]]}]"
                    },
                    "filename": {"type": "string", "description": "Optional artifact filename ending in .xlsx"}
                },
                "required": ["spec"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: JsonValue, context: &ToolContext) -> Result<ToolOutput> {
        let spec = spec_from_args(&args)?;
        let filename = optional_filename(&args)?.unwrap_or_else(|| default_filename(&spec.title));
        let bytes = tokio::task::spawn_blocking({
            let spec = spec.clone();
            move || build(&spec)
        })
        .await
        .map_err(|error| SeraError::Internal(format!("workbook task failed: {error}")))??;

        let meta = ArtifactMeta {
            title: spec.title.clone(),
            filename: filename.clone(),
            language: spec.language.unwrap_or(Language::En),
            source_artifact: None,
            created_at: Utc::now(),
        };
        let stored = self.artifacts.save(&bytes, &filename, meta)?;
        self.store.record_audit(
            &context.actor,
            "workbook.built",
            Some("xlsx_build"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "sheets": spec.sheets.len(),
                "source_artifact": JsonValue::Null,
            })),
        )?;

        let data = spec_summary(&spec, &stored);
        Ok(ToolOutput {
            content: format!(
                "title: {}\nsheets: {}\nfilename: {}\nartifact: {}",
                spec.title,
                spec.sheets
                    .iter()
                    .map(|sheet| sheet.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                stored.filename,
                stored.hash
            ),
            data: Some(data),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
        })
    }
}

pub struct XlsxEditTool {
    artifacts: Arc<ArtifactStore>,
    store: Store,
}

impl XlsxEditTool {
    pub fn new(artifacts: Arc<ArtifactStore>, store: Store) -> Self {
        Self { artifacts, store }
    }
}

#[async_trait]
impl Tool for XlsxEditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "xlsx_edit".into(),
            description:
                "Apply a bounded set of edits to an existing workbook artifact and store the \
                 result as a new artifact that records its source. Edits set literal values, \
                 formula text (written as given, never evaluated), number formats, column \
                 widths, or merged ranges. Styles, number formats, widths, heights, merges, \
                 charts, and images are preserved; macros, external links, and embedded objects \
                 are rejected."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source_artifact": {"type": "string", "description": "Artifact hash of the workbook to edit"},
                    "edits": {
                        "type": "array",
                        "description": "Bounded edits. {op: set_value, sheet, address, value:{type,value}} | {op: set_formula, sheet, address, formula} | {op: set_number_format, sheet, address, format} | {op: set_column_width, sheet, column, width} | {op: merge_cells, sheet, range}. The literal form {sheet, address, value} is accepted too.",
                        "items": {"type": "object"}
                    },
                    "filename": {"type": "string", "description": "Optional artifact filename ending in .xlsx"}
                },
                "required": ["source_artifact", "edits"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: JsonValue, context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("source_artifact")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| SeraError::Validation("source_artifact is required".into()))?;
        let edits: Vec<Edit> = serde_json::from_value(
            args.get("edits")
                .cloned()
                .ok_or_else(|| SeraError::Validation("edits is required".into()))?,
        )
        .map_err(|error| SeraError::Validation(format!("invalid edits: {error}")))?;
        let source = self.artifacts.load(hash)?;
        if !source.filename.to_ascii_lowercase().ends_with(".xlsx") {
            return Err(SeraError::Validation(
                "source_artifact is not an .xlsx workbook".into(),
            ));
        }
        let filename = optional_filename(&args)?.unwrap_or_else(|| source.filename.clone());
        let bytes = source.bytes.clone();
        let edit_count = edits.len();
        let operations: Vec<&str> = edits.iter().map(Edit::op).collect();
        let edited = tokio::task::spawn_blocking(move || apply_edits(&bytes, &edits))
            .await
            .map_err(|error| SeraError::Internal(format!("workbook task failed: {error}")))??;

        let title: String = format!("{} (edited)", source.meta.title)
            .chars()
            .take(MAX_TITLE_CHARS)
            .collect();
        let meta = ArtifactMeta {
            title,
            filename: filename.clone(),
            language: source.meta.language,
            source_artifact: Some(source.hash.clone()),
            created_at: Utc::now(),
        };
        let stored = self.artifacts.save(&edited, &filename, meta)?;
        self.store.record_audit(
            &context.actor,
            "workbook.edited",
            Some("xlsx_edit"),
            context.task_id.as_deref(),
            None,
            None,
            None,
            Some(&json!({
                "artifact_hash": stored.hash,
                "source_artifact": source.hash,
                "edits": edit_count,
                "operations": operations,
            })),
        )?;

        let data = json!({
            "artifact_hash": stored.hash,
            "filename": stored.filename,
            "source_artifact": source.hash,
            "edits": edit_count,
            "operations": operations,
        });
        Ok(ToolOutput {
            content: format!(
                "applied {edit_count} edit(s) from {}\nfilename: {}\nartifact: {}",
                source.filename, stored.filename, stored.hash
            ),
            data: Some(data),
            artifact_path: Some(format!("artifacts/{}", stored.filename)),
            artifact_hash: Some(stored.hash),
            filename: Some(stored.filename),
            destination: None,
        })
    }
}

pub struct XlsxInspectTool {
    artifacts: Arc<ArtifactStore>,
}

impl XlsxInspectTool {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        Self { artifacts }
    }
}

#[async_trait]
impl Tool for XlsxInspectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "xlsx_inspect".into(),
            description:
                "Read-only summary of a workbook artifact: sheet names, used ranges, cell and \
                 formula counts, image and chart counts, and merged ranges — enough to reason \
                 about the file without reading every cell. Macros, external links, and embedded \
                 objects are rejected."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source_artifact": {"type": "string", "description": "Artifact hash of the workbook to inspect"}
                },
                "required": ["source_artifact"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: JsonValue, _context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("source_artifact")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| SeraError::Validation("source_artifact is required".into()))?;
        let source = self.artifacts.load(hash)?;
        if !source.filename.to_ascii_lowercase().ends_with(".xlsx") {
            return Err(SeraError::Validation(
                "source_artifact is not an .xlsx workbook".into(),
            ));
        }
        let bytes = source.bytes.clone();
        let summary = tokio::task::spawn_blocking(move || import(&bytes))
            .await
            .map_err(|error| SeraError::Internal(format!("workbook task failed: {error}")))??;
        let data = serde_json::to_value(&summary)?;
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&summary)?,
            data: Some(data),
            artifact_hash: Some(source.hash),
            filename: Some(source.filename),
            ..ToolOutput::default()
        })
    }
}

/// Read-only: shows what is in the cells of a range so an edit is not a blind overwrite.
pub struct XlsxReadTool {
    artifacts: Arc<ArtifactStore>,
}

impl XlsxReadTool {
    pub fn new(artifacts: Arc<ArtifactStore>) -> Self {
        Self { artifacts }
    }
}

#[async_trait]
impl Tool for XlsxReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "xlsx_read".into(),
            description:
                "Read the values of one worksheet range of a workbook artifact. Returns, for each \
                 stored cell, its address, kind (text, number, date, boolean, error, formula, \
                 blank) and value. A formula cell returns its text and the result Excel last \
                 cached, labelled separately; nothing is ever evaluated. The range must sit \
                 inside the sheet's used range and cover at most 2000 cells — a larger range is \
                 refused, not truncated. Read-only: no artifact is written."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source_artifact": {"type": "string", "description": "Artifact hash of the workbook to read"},
                    "range": {"type": "string", "description": "Sheet1!A1:D20, 'My Sheet'!A1:D20, or a bare sheet name for its whole used range"}
                },
                "required": ["source_artifact", "range"],
                "additionalProperties": false
            }),
            external_effect: false,
        }
    }

    async fn execute(&self, args: JsonValue, _context: &ToolContext) -> Result<ToolOutput> {
        let hash = args
            .get("source_artifact")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| SeraError::Validation("source_artifact is required".into()))?;
        let range = args
            .get("range")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| SeraError::Validation("range is required".into()))?
            .to_owned();
        let source = self.artifacts.load(hash)?;
        if !source.filename.to_ascii_lowercase().ends_with(".xlsx") {
            return Err(SeraError::Validation(
                "source_artifact is not an .xlsx workbook".into(),
            ));
        }
        let bytes = source.bytes.clone();
        let read = tokio::task::spawn_blocking(move || read_range(&bytes, &range))
            .await
            .map_err(|error| SeraError::Internal(format!("workbook task failed: {error}")))??;
        let content = render_read(&read);
        let data = serde_json::to_value(&read)?;
        Ok(ToolOutput {
            content,
            data: Some(data),
            ..ToolOutput::default()
        })
    }
}

/// A compact, one-line-per-cell rendering of a read, for the model to read as text.
fn render_read(read: &RangeRead) -> String {
    let range = read.range.as_deref().unwrap_or("(empty sheet)");
    let mut out = format!(
        "sheet: {}\nrange: {}\ncells: {} stored of {} requested (limit {MAX_READ_CELLS})\n",
        read.sheet,
        range,
        read.cells.len(),
        read.requested_cells
    );
    if read.truncated_values > 0 {
        out.push_str(&format!(
            "note: {} value(s) clipped at {MAX_READ_VALUE_CHARS} characters\n",
            read.truncated_values
        ));
    }
    for cell in &read.cells {
        out.push_str(&format!(
            "{}  {:<8} {}\n",
            cell.address,
            kind_label(cell.kind),
            render_cell_value(cell)
        ));
    }
    out
}

fn kind_label(kind: CellKind) -> &'static str {
    match kind {
        CellKind::Text => "text",
        CellKind::Number => "number",
        CellKind::Date => "date",
        CellKind::Boolean => "boolean",
        CellKind::Error => "error",
        CellKind::Formula => "formula",
        CellKind::Blank => "blank",
    }
}

fn render_cell_value(cell: &CellRecord) -> String {
    let mut text = match (&cell.formula, &cell.cached, &cell.value) {
        (Some(formula), cached, _) => {
            let cached = cached
                .as_ref()
                .map(render_json)
                .unwrap_or_else(|| "none".to_string());
            format!("={formula}  cached: {cached}")
        }
        (None, _, Some(value)) => render_json(value),
        (None, _, None) => String::new(),
    };
    if cell.truncated {
        text.push_str(" …(truncated)");
    }
    text
}

fn render_json(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use gridcraft_model::{
        Anchor, Chart, ChartKind, Font, Image, LegendPos, LineInfo, Series, points_to_chars,
    };

    use super::*;
    use crate::gateway::ToolRegistry;

    /// Synthetic image bytes: the PNG signature followed by a marker. Nothing in SeraBot or in
    /// `gridcraft-xlsx` decodes pictures, so a fixture never needs to be a real image.
    const SYNTHETIC_PNG: &[u8] = b"\x89PNG\r\n\x1a\nserabot-synthetic-image";

    fn spec() -> WorkbookSpec {
        WorkbookSpec {
            title: "Synthetic Quarterly Figures".into(),
            language: Some(Language::En),
            sheets: vec![SheetSpec {
                name: "Summary".into(),
                header: vec!["Region".into(), "Revenue".into(), "Closed".into()],
                rows: vec![
                    vec![
                        CellValue::Text("North".into()),
                        CellValue::Number(1250.5),
                        CellValue::Date("2026-01-31".into()),
                    ],
                    vec![
                        CellValue::Text("South".into()),
                        CellValue::Number(-40.0),
                        CellValue::Date("2026-02-28".into()),
                    ],
                ],
            }],
        }
    }

    fn context(tool: &str) -> ToolContext {
        ToolContext {
            actor: "owner".into(),
            bot: "maker".into(),
            task_id: None,
            turn_id: None,
            conversation_id: None,
            allowed_tools: [tool.to_string()].into_iter().collect(),
            require_approval: HashSet::new(),
        }
    }

    fn cell_text(workbook: &GridWorkbook, sheet: usize, address: &str) -> Option<String> {
        let cell = CellRef::parse(address)?;
        let value = workbook.sheet(sheet)?.cells.get(cell)?.value.clone();
        Some(match value {
            Value::Text(text) => text.to_string(),
            Value::Number(number) => format!("{number}"),
            other => format!("{other:?}"),
        })
    }

    fn cell_number(workbook: &GridWorkbook, sheet: usize, address: &str) -> Option<f64> {
        let cell = CellRef::parse(address)?;
        workbook.sheet(sheet)?.cells.get(cell)?.value.as_f64()
    }

    /// A workbook that exercises every feature the round trip must preserve.
    fn rich_workbook() -> Vec<u8> {
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        workbook.props.title = "Synthetic Rich Workbook".into();

        let heading = workbook.styles.intern(Style {
            font: Font {
                bold: true,
                ..Font::default()
            },
            ..Style::default()
        });
        let money = workbook.styles.intern(Style {
            num_fmt: NumFmt::new("#,##0.00"),
            ..Style::default()
        });

        let mut sheet = Sheet::new("Summary");
        for (column, text) in ["Region", "Revenue"].iter().enumerate() {
            sheet.set_cell(
                CellRef::new(0, column as u32),
                Cell {
                    value: Value::text(*text),
                    formula: None,
                    style: heading,
                },
            );
        }
        sheet.set_cell(
            CellRef::new(1, 0),
            Cell {
                value: Value::text("North"),
                formula: None,
                style: StyleId::DEFAULT,
            },
        );
        sheet.set_cell(
            CellRef::new(1, 1),
            Cell {
                value: Value::number(1250.5),
                formula: None,
                style: money,
            },
        );
        sheet.cols.insert(
            0,
            LineInfo {
                size: Some(145.0),
                ..LineInfo::default()
            },
        );
        sheet.rows.insert(
            0,
            LineInfo {
                size: Some(32.0),
                ..LineInfo::default()
            },
        );
        sheet
            .merges
            .push(RangeRef::parse("A4:C4").unwrap_or_else(|| RangeRef::cell(CellRef::new(3, 0))));
        sheet.charts.push(Chart {
            id: 1,
            kind: ChartKind::ColumnClustered,
            anchor: Anchor {
                cell: CellRef::new(6, 0),
                width: 320.0,
                height: 200.0,
                ..Anchor::default()
            },
            title: Some("Revenue".into()),
            series: vec![Series {
                name: Some("Summary!$B$1".into()),
                categories: Some("Summary!$A$2".into()),
                values: "Summary!$B$2".into(),
                ..Series::default()
            }],
            legend: LegendPos::Bottom,
            data_labels: false,
            gridlines: true,
            style: 2,
            x_title: None,
            y_title: None,
            source: None,
            by_rows: false,
            pivot: None,
        });
        sheet.images.push(Image {
            id: 2,
            anchor: Anchor {
                cell: CellRef::new(9, 0),
                width: 64.0,
                height: 64.0,
                ..Anchor::default()
            },
            data: SYNTHETIC_PNG.to_vec(),
            mime: "image/png".into(),
            alt: "Synthetic mark".into(),
        });
        workbook.sheets.push(Arc::new(sheet));

        write_xlsx(&workbook).expect("the synthetic fixture must be writable")
    }

    #[test]
    fn builds_and_reads_back_literal_values() {
        let bytes = build(&spec()).unwrap();
        assert!(bytes.starts_with(b"PK\x03\x04"));

        let (workbook, _) = load(&bytes).unwrap();
        assert_eq!(workbook.sheets.len(), 1);
        assert_eq!(workbook.sheets[0].name, "Summary");
        assert_eq!(cell_text(&workbook, 0, "A1").as_deref(), Some("Region"));
        assert_eq!(cell_text(&workbook, 0, "B1").as_deref(), Some("Revenue"));
        assert_eq!(cell_text(&workbook, 0, "A2").as_deref(), Some("North"));
        assert_eq!(cell_number(&workbook, 0, "B2"), Some(1250.5));
        // Dates are stored as serials in the 1900 system, as Excel does.
        assert_eq!(cell_number(&workbook, 0, "C2"), Some(46053.0));
        assert_eq!(cell_text(&workbook, 0, "A3").as_deref(), Some("South"));

        let summary = import(&bytes).unwrap();
        assert_eq!(summary.sheet_names(), vec!["Summary"]);
        assert_eq!(summary.sheets[0].used_range.as_deref(), Some("A1:C3"));
        assert_eq!(summary.cell_count(), 9);
        assert_eq!(summary.images, 0);
        assert_eq!(summary.charts, 0);
        assert!(!summary.has_formulas);
    }

    #[test]
    fn edits_an_imported_workbook() {
        let bytes = build(&spec()).unwrap();
        let edits = vec![
            CellEdit {
                sheet: "summary".into(),
                address: "B2".into(),
                value: CellValue::Number(999.25),
            },
            CellEdit {
                sheet: "Summary".into(),
                address: "$A$4".into(),
                value: CellValue::Text("Total".into()),
            },
            CellEdit {
                sheet: "Summary".into(),
                address: "C4".into(),
                value: CellValue::Date("2026-03-31".into()),
            },
        ];
        let edited = edit(&bytes, &edits).unwrap();
        let (workbook, _) = load(&edited).unwrap();
        assert_eq!(cell_number(&workbook, 0, "B2"), Some(999.25));
        assert_eq!(cell_text(&workbook, 0, "A4").as_deref(), Some("Total"));
        assert_eq!(cell_number(&workbook, 0, "C4"), Some(46112.0));
        // Untouched cells survive the round trip.
        assert_eq!(cell_text(&workbook, 0, "A2").as_deref(), Some("North"));
        assert_eq!(workbook.sheets[0].cells.len(), 11);
    }

    #[test]
    fn rejects_edits_that_are_not_literal_cells() {
        let bytes = build(&spec()).unwrap();
        let missing_sheet = vec![CellEdit {
            sheet: "Nope".into(),
            address: "A1".into(),
            value: CellValue::Text("x".into()),
        }];
        assert!(edit(&bytes, &missing_sheet).is_err());

        let range = vec![CellEdit {
            sheet: "Summary".into(),
            address: "A1:B2".into(),
            value: CellValue::Text("x".into()),
        }];
        assert!(edit(&bytes, &range).is_err());

        let formula = vec![CellEdit {
            sheet: "Summary".into(),
            address: "A1".into(),
            value: CellValue::Text("=SUM(B2:B3)".into()),
        }];
        assert!(edit(&bytes, &formula).is_err());

        assert!(edit(&bytes, &[]).is_err());
    }

    #[test]
    fn round_trip_preserves_styles_geometry_merges_charts_and_images() {
        let bytes = rich_workbook();
        let edited = apply_edits(
            &bytes,
            &[Edit::SetValue {
                sheet: "Summary".into(),
                address: "A2".into(),
                value: CellValue::Text("South".into()),
            }],
        )
        .unwrap();

        let (workbook, _) = load(&edited).unwrap();
        let sheet = workbook.sheet(0).unwrap();

        // The edit landed and the untouched neighbour kept its value.
        assert_eq!(cell_text(&workbook, 0, "A2").as_deref(), Some("South"));
        assert_eq!(cell_number(&workbook, 0, "B2"), Some(1250.5));

        // Cell styles and number formats.
        let heading = sheet.cells.get(CellRef::new(0, 0)).unwrap().style;
        assert!(workbook.styles.get(heading).font.bold);
        let money = sheet.cells.get(CellRef::new(1, 1)).unwrap().style;
        assert_eq!(workbook.styles.get(money).num_fmt.as_str(), "#,##0.00");

        // Column widths and row heights.
        assert_eq!(sheet.col_width(0), 145.0);
        assert_eq!(points_to_chars(145.0), 20.0);
        assert_eq!(sheet.row_height(0), 32.0);

        // Merged cells.
        let merges: Vec<String> = sheet.merges.iter().map(RangeRef::a1).collect();
        assert_eq!(merges, vec!["A4:C4".to_string()]);

        // Charts.
        assert_eq!(sheet.charts.len(), 1);
        assert_eq!(sheet.charts[0].kind, ChartKind::ColumnClustered);
        assert_eq!(sheet.charts[0].title.as_deref(), Some("Revenue"));
        assert_eq!(sheet.charts[0].series.len(), 1);

        // Images.
        assert_eq!(sheet.images.len(), 1);
        assert_eq!(sheet.images[0].alt, "Synthetic mark");
        assert_eq!(sheet.images[0].mime, "image/png");
        assert_eq!(sheet.images[0].data, SYNTHETIC_PNG);

        // The package itself carries the drawing, chart and media parts, not just the model.
        let names = package_names(&edited);
        for expected in [
            "xl/drawings/drawing1.xml",
            "xl/charts/chart1.xml",
            "xl/media/image1.png",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "missing {expected} in {names:?}"
            );
        }
    }

    /// Part names inside a package, for asserting that a round trip kept them.
    fn package_names(bytes: &[u8]) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        (0..archive.len())
            .filter_map(|index| {
                archive
                    .by_index(index)
                    .ok()
                    .map(|file| file.name().to_owned())
            })
            .collect()
    }

    #[test]
    fn inspect_reports_images_charts_formulas_and_merges() {
        let bytes = rich_workbook();
        let summary = import(&bytes).unwrap();
        assert_eq!(summary.sheets.len(), 1);
        assert_eq!(summary.images, 1);
        assert_eq!(summary.charts, 1);
        assert_eq!(summary.sheets[0].images, 1);
        assert_eq!(summary.sheets[0].charts, 1);
        assert_eq!(summary.sheets[0].merged_ranges, 1);
        assert_eq!(summary.sheets[0].used_range.as_deref(), Some("A1:B2"));
        assert!(!summary.has_formulas);

        let with_formula = apply_edits(
            &bytes,
            &[Edit::SetFormula {
                sheet: "Summary".into(),
                address: "B3".into(),
                formula: "=SUM(B2:B2)".into(),
            }],
        )
        .unwrap();
        let summary = import(&with_formula).unwrap();
        assert_eq!(summary.formulas, 1);
        assert_eq!(summary.sheets[0].formulas, 1);
        assert!(summary.has_formulas);
    }

    /// A workbook with one of every kind of cell a read must report.
    fn typed_workbook() -> Vec<u8> {
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        workbook.props.title = "Synthetic Typed".into();
        let date_style = workbook.styles.intern(Style {
            num_fmt: NumFmt::new(DATE_NUMBER_FORMAT),
            ..Style::default()
        });

        let mut sheet = Sheet::new("Data");
        sheet.set_value(CellRef::new(0, 0), Value::text("Region"));
        sheet.set_value(CellRef::new(0, 1), Value::number(1250.5));
        sheet.set_value(CellRef::new(0, 2), Value::Bool(true));
        sheet.set_cell(
            CellRef::new(0, 3),
            Cell {
                value: Value::Number(46053.0),
                formula: None,
                style: date_style,
            },
        );
        sheet.set_cell(
            CellRef::new(1, 0),
            Cell {
                value: Value::Error(gridcraft_core::CellError::Div0),
                formula: None,
                style: StyleId::DEFAULT,
            },
        );
        // A formula with a cached result, as a real file carries it.
        sheet.set_cell(
            CellRef::new(1, 1),
            Cell {
                value: Value::number(1250.5),
                formula: Some(Arc::new(Formula {
                    text: "SUM(B1:B1)".into(),
                    expr: None,
                    array: None,
                })),
                style: StyleId::DEFAULT,
            },
        );
        // A stored cell with formatting but no value.
        sheet.set_style(CellRef::new(1, 2), date_style);
        workbook.sheets.push(Arc::new(sheet));

        write_xlsx(&workbook).expect("the synthetic fixture must be writable")
    }

    #[test]
    fn reads_a_range_with_typed_values() {
        let bytes = typed_workbook();
        let read = read_range(&bytes, "Data!A1:D2").unwrap();
        assert_eq!(read.sheet, "Data");
        assert_eq!(read.range.as_deref(), Some("A1:D2"));
        assert_eq!(read.requested_cells, 8);
        assert_eq!(read.truncated_values, 0);

        let kinds: Vec<(String, CellKind)> = read
            .cells
            .iter()
            .map(|cell| (cell.address.clone(), cell.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("A1".to_string(), CellKind::Text),
                ("B1".to_string(), CellKind::Number),
                ("C1".to_string(), CellKind::Boolean),
                ("D1".to_string(), CellKind::Date),
                ("A2".to_string(), CellKind::Error),
                ("B2".to_string(), CellKind::Formula),
                ("C2".to_string(), CellKind::Blank),
            ]
        );

        assert_eq!(read.cells[0].value, Some(json!("Region")));
        assert_eq!(read.cells[1].value, Some(json!(1250.5)));
        assert_eq!(read.cells[2].value, Some(json!(true)));
        assert_eq!(read.cells[3].value, Some(json!("2026-01-31")));
        assert_eq!(read.cells[4].value, Some(json!("#DIV/0!")));
        assert_eq!(read.cells[6].value, None);

        // A bare sheet name reads the whole used range.
        let whole = read_range(&bytes, "Data").unwrap();
        assert_eq!(whole.range.as_deref(), Some("A1:D2"));
        assert_eq!(whole.cells.len(), read.cells.len());
    }

    #[test]
    fn reads_a_formula_cell_as_text_plus_cached_value() {
        let bytes = typed_workbook();
        let read = read_range(&bytes, "Data!B2").unwrap();
        assert_eq!(read.cells.len(), 1);
        let cell = &read.cells[0];
        assert_eq!(cell.kind, CellKind::Formula);
        assert_eq!(cell.formula.as_deref(), Some("SUM(B1:B1)"));
        assert_eq!(cell.cached, Some(json!(1250.5)));
        // The formula's own value slot stays empty: text and cached result are separate.
        assert_eq!(cell.value, None);

        // A formula set by an edit has no cached result at all.
        let edited = apply_edits(
            &bytes,
            &[Edit::SetFormula {
                sheet: "Data".into(),
                address: "A3".into(),
                formula: "=B1*2".into(),
            }],
        )
        .unwrap();
        let read = read_range(&edited, "Data!A3").unwrap();
        assert_eq!(read.cells[0].kind, CellKind::Formula);
        assert_eq!(read.cells[0].formula.as_deref(), Some("B1*2"));
        assert_eq!(read.cells[0].cached, None);
    }

    #[test]
    fn read_cap_rejects_rather_than_truncates() {
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        let mut sheet = Sheet::new("Wide");
        for row in 0..=(MAX_READ_CELLS as u32) {
            sheet.set_value(CellRef::new(row, 0), Value::number(row as f64));
        }
        workbook.sheets.push(Arc::new(sheet));
        let bytes = write_xlsx(&workbook).unwrap();

        let error = read_range(&bytes, "Wide!A1:A2001").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("2001 cells") && message.contains("narrower range"),
            "unexpected error: {message}"
        );

        // Exactly at the cap is fine and is not truncated.
        let read = read_range(&bytes, "Wide!A1:A2000").unwrap();
        assert_eq!(read.requested_cells, MAX_READ_CELLS as u64);
        assert_eq!(read.cells.len(), MAX_READ_CELLS);
        assert_eq!(read.cells[0].value, Some(json!(0.0)));
        assert_eq!(read.cells[MAX_READ_CELLS - 1].value, Some(json!(1999.0)));
    }

    #[test]
    fn read_rejects_bad_ranges_and_clips_long_values() {
        let bytes = typed_workbook();

        // Outside the used range, unknown sheet, malformed range.
        assert!(read_range(&bytes, "Data!A1:F9").is_err());
        assert!(read_range(&bytes, "Nope!A1").is_err());
        assert!(read_range(&bytes, "Data!not-a-range").is_err());
        assert!(read_range(&bytes, "Data!").is_ok());
        assert!(read_range(&bytes, "").is_err());

        // A quoted sheet name, including one with a space.
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        let mut sheet = Sheet::new("My Sheet");
        sheet.set_value(CellRef::new(0, 0), Value::text("x"));
        workbook.sheets.push(Arc::new(sheet));
        let quoted = write_xlsx(&workbook).unwrap();
        let read = read_range(&quoted, "'My Sheet'!A1:A1").unwrap();
        assert_eq!(read.sheet, "My Sheet");
        assert_eq!(read.cells[0].value, Some(json!("x")));

        // Values longer than the limit are clipped and flagged, never silently shortened.
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        let mut sheet = Sheet::new("Long");
        sheet.set_value(
            CellRef::new(0, 0),
            Value::text("y".repeat(MAX_READ_VALUE_CHARS + 50)),
        );
        workbook.sheets.push(Arc::new(sheet));
        let long = write_xlsx(&workbook).unwrap();
        let read = read_range(&long, "Long!A1").unwrap();
        assert_eq!(read.truncated_values, 1);
        assert!(read.cells[0].truncated);
        let text = read.cells[0]
            .value
            .as_ref()
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        assert_eq!(text.chars().count(), MAX_READ_VALUE_CHARS);
    }

    #[test]
    fn sets_formula_text_without_evaluating_it() {
        let bytes = build(&spec()).unwrap();
        let edited = apply_edits(
            &bytes,
            &[Edit::SetFormula {
                sheet: "Summary".into(),
                address: "B4".into(),
                formula: "=SUM(B2:B3)".into(),
            }],
        )
        .unwrap();

        let (workbook, _) = load(&edited).unwrap();
        let cell = workbook
            .sheet(0)
            .and_then(|sheet| sheet.cells.get(CellRef::new(3, 1)))
            .cloned()
            .expect("the formula cell must exist");
        let formula = cell.formula.expect("the cell must hold a formula");
        assert_eq!(formula.text, "SUM(B2:B3)");
        // No cached result: SeraBot never calculates, so Excel recalculates on open.
        assert!(cell.value.is_empty());
    }

    #[test]
    fn sets_number_format_and_column_width() {
        let bytes = build(&spec()).unwrap();
        let edited = apply_edits(
            &bytes,
            &[
                Edit::SetNumberFormat {
                    sheet: "Summary".into(),
                    address: "B2".into(),
                    format: "#,##0.00".into(),
                },
                Edit::SetColumnWidth {
                    sheet: "Summary".into(),
                    column: "B".into(),
                    width: 20.0,
                },
            ],
        )
        .unwrap();

        let (workbook, _) = load(&edited).unwrap();
        let sheet = workbook.sheet(0).unwrap();
        let style = sheet.cells.get(CellRef::new(1, 1)).unwrap().style;
        assert_eq!(workbook.styles.get(style).num_fmt.as_str(), "#,##0.00");
        // 20 characters is the Column Width dialog value the edit asked for.
        assert_eq!(points_to_chars(f64::from(sheet.col_width(1))), 20.0);
        // The value itself is untouched by a format change.
        assert_eq!(cell_number(&workbook, 0, "B2"), Some(1250.5));
    }

    #[test]
    fn merges_a_range_and_rejects_overlaps() {
        let bytes = build(&spec()).unwrap();
        let merged = apply_edits(
            &bytes,
            &[Edit::MergeCells {
                sheet: "Summary".into(),
                range: "A5:C5".into(),
            }],
        )
        .unwrap();
        let (workbook, _) = load(&merged).unwrap();
        assert_eq!(
            workbook.sheets[0]
                .merges
                .iter()
                .map(RangeRef::a1)
                .collect::<Vec<_>>(),
            vec!["A5:C5".to_string()]
        );

        let overlap = apply_edits(
            &merged,
            &[Edit::MergeCells {
                sheet: "Summary".into(),
                range: "B5:C6".into(),
            }],
        );
        assert!(overlap.is_err());

        let single = apply_edits(
            &merged,
            &[Edit::MergeCells {
                sheet: "Summary".into(),
                range: "B7".into(),
            }],
        );
        assert!(single.is_err());

        let huge = apply_edits(
            &merged,
            &[Edit::MergeCells {
                sheet: "Summary".into(),
                range: "A1:ZZ9999".into(),
            }],
        );
        assert!(huge.is_err());
    }

    #[test]
    fn rejects_unsafe_formula_text() {
        let bytes = build(&spec()).unwrap();
        let formula = |text: &str| {
            apply_edits(
                &bytes,
                &[Edit::SetFormula {
                    sheet: "Summary".into(),
                    address: "D1".into(),
                    formula: text.into(),
                }],
            )
        };
        assert!(formula("").is_err());
        assert!(formula("   ").is_err());
        assert!(formula("=[Book1.xlsx]Sheet1!A1").is_err());
        assert!(formula("=WEBSERVICE(\"https://example.invalid\")").is_err());
        assert!(formula("=webservice (\"https://example.invalid\")").is_err());
        assert!(formula("=RTD(\"prog\",,\"topic\")").is_err());
        assert!(formula("=SUM(A1:A2)\u{202e}").is_err());
        assert!(formula(&format!("=SUM({})", "A1,".repeat(MAX_FORMULA_CHARS))).is_err());
        // Ordinary formulas over plain ranges are accepted.
        assert!(formula("=SUM(B2:B3)").is_ok());
        assert!(formula("=B2*2").is_ok());
        // A name that merely contains a forbidden word is not a call.
        assert!(formula("=CALLED_OFF").is_ok());
    }

    #[test]
    fn rejects_bad_number_formats_and_column_widths() {
        let bytes = build(&spec()).unwrap();
        let format = |text: &str| {
            apply_edits(
                &bytes,
                &[Edit::SetNumberFormat {
                    sheet: "Summary".into(),
                    address: "B2".into(),
                    format: text.into(),
                }],
            )
        };
        assert!(format("").is_err());
        assert!(format("   ").is_err());
        assert!(format("0.00\u{0007}").is_err());
        assert!(format(&"0".repeat(MAX_NUMBER_FORMAT_CHARS + 1)).is_err());
        assert!(format("#,##0.00").is_ok());

        let width = |value: f64| {
            apply_edits(
                &bytes,
                &[Edit::SetColumnWidth {
                    sheet: "Summary".into(),
                    column: "B".into(),
                    width: value,
                }],
            )
        };
        assert!(width(0.0).is_err());
        assert!(width(-1.0).is_err());
        assert!(width(f64::INFINITY).is_err());
        assert!(width(MAX_COLUMN_WIDTH_CHARS + 1.0).is_err());
        assert!(width(8.43).is_ok());

        let column = |text: &str| {
            apply_edits(
                &bytes,
                &[Edit::SetColumnWidth {
                    sheet: "Summary".into(),
                    column: text.into(),
                    width: 10.0,
                }],
            )
        };
        assert!(column("").is_err());
        assert!(column("B1").is_err());
        assert!(column("1").is_err());
        assert!(column("$B").is_ok());
    }

    #[test]
    fn rejects_macro_external_link_and_embedded_object_parts() {
        let bytes = build(&spec()).unwrap();

        let macro_package = append_stored_part(&bytes, "xl/vbaProject.bin", b"macro");
        let error = import(&macro_package).unwrap_err();
        assert!(
            error.to_string().contains("VBA project"),
            "unexpected error: {error}"
        );

        let linked = append_stored_part(&bytes, "xl/externalLinks/externalLink1.xml", b"<x/>");
        let error = import(&linked).unwrap_err();
        assert!(
            error.to_string().contains("external workbook links"),
            "unexpected error: {error}"
        );

        let embedded = append_stored_part(&bytes, "xl/embeddings/oleObject1.bin", b"ole");
        let error = import(&embedded).unwrap_err();
        assert!(
            error.to_string().contains("embedded objects"),
            "unexpected error: {error}"
        );

        let activex = append_stored_part(&bytes, "xl/activeX/activeX1.bin", b"ctl");
        assert!(import(&activex).is_err());
    }

    #[test]
    fn accepts_image_parts() {
        let bytes = build(&spec()).unwrap();
        // An unreferenced media part is content, not code: it must not be rejected.
        let image = append_stored_part(&bytes, "xl/media/image1.png", SYNTHETIC_PNG);
        assert!(import(&image).is_ok());
    }

    #[test]
    fn accepts_legacy_and_op_tagged_edit_json() {
        let legacy: Edit = serde_json::from_str(
            r#"{"sheet":"Summary","address":"B2","value":{"type":"number","value":42}}"#,
        )
        .unwrap();
        assert_eq!(
            legacy,
            Edit::SetValue {
                sheet: "Summary".into(),
                address: "B2".into(),
                value: CellValue::Number(42.0),
            }
        );

        let tagged: Edit = serde_json::from_str(
            r#"{"op":"set_formula","sheet":"Summary","address":"B2","formula":"=B1*2"}"#,
        )
        .unwrap();
        assert_eq!(tagged.op(), "set_formula");

        let merge: Edit =
            serde_json::from_str(r#"{"op":"merge_cells","sheet":"Summary","range":"A1:C1"}"#)
                .unwrap();
        assert_eq!(merge.op(), "merge_cells");

        // Mixed or unknown shapes are refused with a reason.
        assert!(
            serde_json::from_str::<Edit>(
                r#"{"sheet":"Summary","address":"B2","value":{"type":"number","value":1},"format":"0"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Edit>(r#"{"op":"set_value","sheet":"Summary"}"#).is_err());
        assert!(
            serde_json::from_str::<Edit>(
                r#"{"op":"set_value","sheet":"Summary","address":"A1","value":{"type":"number","value":1},"range":"A1:B1"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Edit>(r#"{"op":"nope","sheet":"Summary","range":"A1:B1"}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<Edit>(r#"{"sheet":"Summary"}"#).is_err());
    }

    #[test]
    fn rejects_workbooks_over_the_import_bounds() {
        // 100 sheets are accepted; 101 are not.
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        for index in 0..MAX_IMPORT_SHEETS {
            workbook
                .sheets
                .push(Arc::new(Sheet::new(format!("Sheet{}", index + 1))));
        }
        assert!(check_import_limits(&workbook).is_ok());
        workbook.sheets.push(Arc::new(Sheet::new("OneTooMany")));
        let error = check_import_limits(&workbook).unwrap_err();
        assert!(
            error.to_string().contains("sheets"),
            "unexpected error: {error}"
        );

        // 500,000 cells are accepted; 500,001 are not.
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        let mut sheet = Sheet::new("Big");
        for row in 0..=MAX_IMPORT_CELLS as u32 / MAX_COLUMNS as u32 {
            for column in 0..MAX_COLUMNS as u32 {
                sheet.set_value(CellRef::new(row, column), Value::number(1.0));
            }
        }
        workbook.sheets.push(Arc::new(sheet));
        let error = check_import_limits(&workbook).unwrap_err();
        assert!(
            error.to_string().contains("cells"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn imports_a_workbook_above_the_old_cell_cap() {
        // 60,000 cells: over the 50,000 `xlsx_build` bound, inside the import bound.
        let mut workbook = GridWorkbook::new();
        workbook.sheets.clear();
        let mut sheet = Sheet::new("Model");
        for row in 0..1_000u32 {
            for column in 0..60u32 {
                sheet.set_value(CellRef::new(row, column), Value::number(row as f64));
            }
        }
        workbook.sheets.push(Arc::new(sheet));
        let bytes = write_xlsx(&workbook).unwrap();

        let summary = import(&bytes).unwrap();
        assert_eq!(summary.cell_count(), 60_000);
        assert_eq!(summary.sheets[0].used_range.as_deref(), Some("A1:BH1000"));

        // The read bound still applies to a single call.
        let error = read_range(&bytes, "Model!A1:BH1000").unwrap_err();
        assert!(
            error.to_string().contains("narrower range"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn rejects_oversize_input_before_parsing() {
        let oversize = vec![0u8; MAX_INPUT_BYTES + 1];
        let error = import(&oversize).unwrap_err();
        assert!(
            error.to_string().contains("32 MiB"),
            "unexpected error: {error}"
        );
        assert!(edit(&oversize, &[]).is_err());
    }

    #[test]
    fn rejects_non_zip_input() {
        assert!(import(b"not a workbook").is_err());
        assert!(import(b"").is_err());
    }

    #[test]
    fn rejects_control_characters_and_bad_sheet_names() {
        let mut bad = spec();
        bad.sheets[0].rows[0][0] = CellValue::Text("North\u{202e}".into());
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].name = "a/b".into();
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].name = "x".repeat(MAX_SHEET_NAME_CHARS + 1);
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].name = "History".into();
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets.push(SheetSpec {
            name: "summary".into(),
            header: vec![],
            rows: vec![],
        });
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].rows[0][0] = CellValue::Text("x".repeat(MAX_TEXT_CHARS + 1));
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].rows[0][1] = CellValue::Number(f64::INFINITY);
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].rows[0][2] = CellValue::Date("2026-02-30".into());
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].rows[0][2] = CellValue::Date("31/01/2026".into());
        assert!(build(&bad).is_err());
    }

    #[test]
    fn rejects_oversized_specs() {
        let mut bad = spec();
        bad.sheets = (0..MAX_SHEETS + 1)
            .map(|index| SheetSpec {
                name: format!("Sheet{index}"),
                header: vec![],
                rows: vec![],
            })
            .collect();
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].rows = vec![vec![CellValue::Number(1.0)]; MAX_ROWS_PER_SHEET + 1];
        assert!(build(&bad).is_err());

        let mut bad = spec();
        bad.sheets[0].rows = vec![vec![CellValue::Number(1.0); MAX_COLUMNS + 1]];
        assert!(build(&bad).is_err());
    }

    #[test]
    fn serde_rejects_unknown_fields_and_formula_variants() {
        let raw = r#"{
            "title": "Workbook",
            "sheets": [],
            "macros": ["payload.bin"]
        }"#;
        assert!(serde_json::from_str::<WorkbookSpec>(raw).is_err());

        let raw = r#"{
            "title": "Workbook",
            "sheets": [{
                "name": "Sheet1",
                "header": [],
                "rows": [[{"type": "formula", "value": "=SUM(A1:A2)"}]],
                "links": []
            }]
        }"#;
        assert!(serde_json::from_str::<WorkbookSpec>(raw).is_err());

        let raw = r#"{
            "title": "Workbook",
            "sheets": [{
                "name": "Sheet1",
                "rows": [[{"type": "formula", "value": "=SUM(A1:A2)"}]]
            }]
        }"#;
        assert!(serde_json::from_str::<WorkbookSpec>(raw).is_err());
    }

    #[test]
    fn derives_artifact_filenames() {
        assert_eq!(
            default_filename("Synthetic Quarterly Figures"),
            "synthetic-quarterly-figures.xlsx"
        );
        assert_eq!(default_filename("  "), "workbook.xlsx");
    }

    #[tokio::test]
    async fn build_tool_stores_an_artifact_and_edit_tool_records_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path()).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let mut registry = ToolRegistry::default();
        crate::tools::register_workbook_tools(&mut registry, artifacts.clone(), store.clone())
            .unwrap();
        assert!(registry.is_registered("xlsx_build"));
        assert!(registry.is_registered("xlsx_edit"));
        assert!(registry.is_registered("xlsx_inspect"));
        assert!(registry.is_registered("xlsx_read"));

        let build_tool = registry.get("xlsx_build").unwrap();
        let output = build_tool
            .execute(json!({"spec": spec()}), &context("xlsx_build"))
            .await
            .unwrap();
        let built_hash = output.artifact_hash.unwrap();
        assert_eq!(
            output.filename.as_deref(),
            Some("synthetic-quarterly-figures.xlsx")
        );

        let inspect_tool = registry.get("xlsx_inspect").unwrap();
        let inspected = inspect_tool
            .execute(
                json!({"source_artifact": built_hash}),
                &context("xlsx_inspect"),
            )
            .await
            .unwrap();
        assert!(inspected.content.contains("Summary"));
        assert!(inspected.content.contains("has_formulas"));

        // The read tool shows the model what is in the cells, and writes nothing.
        let read_tool = registry.get("xlsx_read").unwrap();
        let read = read_tool
            .execute(
                json!({"source_artifact": built_hash, "range": "Summary!A1:C2"}),
                &context("xlsx_read"),
            )
            .await
            .unwrap();
        assert!(read.artifact_hash.is_none());
        assert!(read.content.contains("Region"));
        assert!(read.content.contains("1250.5"));
        let read_data = read.data.unwrap();
        assert_eq!(read_data["sheet"], json!("Summary"));
        assert_eq!(read_data["cells"].as_array().map(Vec::len), Some(6));

        let edit_tool = registry.get("xlsx_edit").unwrap();
        let edited = edit_tool
            .execute(
                json!({
                    "source_artifact": built_hash,
                    "edits": [
                        {
                            "sheet": "Summary",
                            "address": "B2",
                            "value": {"type": "number", "value": 42.0}
                        },
                        {"op": "set_formula", "sheet": "Summary", "address": "B4", "formula": "=SUM(B2:B3)"},
                        {"op": "set_number_format", "sheet": "Summary", "address": "B2", "format": "#,##0.00"},
                        {"op": "set_column_width", "sheet": "Summary", "column": "B", "width": 18.0},
                        {"op": "merge_cells", "sheet": "Summary", "range": "A5:C5"}
                    ]
                }),
                &context("xlsx_edit"),
            )
            .await
            .unwrap();
        let edited_hash = edited.artifact_hash.unwrap();
        assert_ne!(edited_hash, built_hash);
        let stored = artifacts.load(&edited_hash).unwrap();
        assert_eq!(
            stored.meta.source_artifact.as_deref(),
            Some(built_hash.as_str())
        );

        let (workbook, _) = load(&stored.bytes).unwrap();
        assert_eq!(cell_number(&workbook, 0, "B2"), Some(42.0));
        let sheet = workbook.sheet(0).unwrap();
        assert!(sheet.cells.get(CellRef::new(3, 1)).is_some_and(|cell| {
            cell.formula
                .as_ref()
                .is_some_and(|f| f.text == "SUM(B2:B3)")
        }));
        assert_eq!(sheet.merges.len(), 1);
        assert_eq!(points_to_chars(f64::from(sheet.col_width(1))), 18.0);
    }

    #[tokio::test]
    async fn tools_reject_unknown_spec_fields() {
        let dir = tempfile::tempdir().unwrap();
        let artifacts = Arc::new(ArtifactStore::new(dir.path()).unwrap());
        let store = Store::open_in_memory(384).unwrap();
        let tool = XlsxBuildTool::new(artifacts, store);
        let args = json!({"spec": {"title": "x", "sheets": [], "attachments": ["a.bin"]}});
        assert!(tool.execute(args, &context("xlsx_build")).await.is_err());
    }

    /// Appends a stored (uncompressed) entry to a zip package, so tests can build a package
    /// that carries a macro, a link, or an embedded object without any extra dependency.
    fn append_stored_part(original: &[u8], name: &str, data: &[u8]) -> Vec<u8> {
        let eocd = (0..original.len().saturating_sub(3))
            .rev()
            .find(|offset| u32_at(original, *offset) == Some(EOCD_SIGNATURE))
            .unwrap();
        let parts = u32_at(original, eocd + 10).unwrap();
        let directory_size = u32_at(original, eocd + 12).unwrap();
        let directory_offset = u32_at(original, eocd + 16).unwrap() as usize;
        let directory_end = directory_offset + directory_size as usize;

        let name_bytes = name.as_bytes();
        let crc = crc32(data);
        let entry_length = 30 + name_bytes.len() + data.len();

        let mut out = Vec::with_capacity(original.len() + entry_length + 128);
        // Everything before the central directory, then the new local entry.
        out.extend_from_slice(&original[..directory_offset]);
        let local_offset = out.len() as u32;
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(data);

        // The original central directory follows the new local entry.
        out.extend_from_slice(&original[directory_offset..directory_end]);
        out.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // method
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra length
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out.extend_from_slice(&0u16.to_le_bytes()); // disk start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        out.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        out.extend_from_slice(&local_offset.to_le_bytes());
        out.extend_from_slice(name_bytes);

        let new_directory_size = directory_size + 46 + name_bytes.len() as u32;
        out.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk
        out.extend_from_slice(&0u16.to_le_bytes()); // directory disk
        out.extend_from_slice(&((parts + 1) as u16).to_le_bytes());
        out.extend_from_slice(&((parts + 1) as u16).to_le_bytes());
        out.extend_from_slice(&new_directory_size.to_le_bytes());
        out.extend_from_slice(&((directory_offset + entry_length) as u32).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        out
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }
}
