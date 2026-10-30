//! Bounded XLSX creation, inspection and editing.
//!
//! The model is deliberately literal. Serde denies unknown fields, so a larger JSON document
//! cannot smuggle formulas, macros, external links, images, or embedded objects into a
//! workbook. Every string written is length- and control-character-checked here, mirroring
//! `deck.rs`.
//!
//! Two invariants shape the module:
//!
//! * Formulas are never evaluated and never written. `gridcraft-calc` is not a dependency; the
//!   crate that is (`gridcraft-xlsx`) only reads and writes the file format, and this module
//!   writes literal values only.
//! * Input is bounded before it reaches the parser. The crate caps decompressed parts at
//!   512 MB, which is far too generous for a single-operator agent, so the package's own
//!   central directory is read here first: oversize input, macros, external workbook links,
//!   images and embedded objects are rejected by name rather than silently stripped.
//!
//! All entry points take byte slices. Files arrive through the inbox or the `ArtifactStore`;
//! no model-chosen path is ever opened here.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use gridcraft_core::date::{days_in_month, serial_from_ymd};
use gridcraft_core::{CellRef, DateSystem, Value};
use gridcraft_model::{Cell, NumFmt, Preserved, Sheet, Style, StyleId, Workbook as GridWorkbook};
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
pub const MAX_SHEETS: usize = 20;
pub const MAX_COLUMNS: usize = 64;
pub const MAX_ROWS_PER_SHEET: usize = 5000;
pub const MAX_CELLS: usize = 50_000;
pub const MAX_TEXT_CHARS: usize = 500;
pub const MAX_EDITS: usize = 500;
pub const MAX_WARNINGS: usize = 20;

/// Number format used for literal dates: they are stored as serial numbers, as Excel does.
pub const DATE_NUMBER_FORMAT: &str = "yyyy-mm-dd";

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

/// A literal cell value. There is no formula variant on purpose.
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

/// A bounded cell edit against an imported workbook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellEdit {
    pub sheet: String,
    /// A single `A1`-style address (`B7`, `$B$7`). Ranges are rejected.
    pub address: String,
    pub value: CellValue,
}

/// What an imported workbook looks like: sheet names, used ranges and cell counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workbook {
    pub sheets: Vec<SheetSummary>,
    /// Bounded notes from the lenient reader (skipped optional parts, repairs).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetSummary {
    pub name: String,
    /// Smallest range containing a stored cell, e.g. `A1:D10`; `None` for an empty sheet.
    pub used_range: Option<String>,
    pub cells: usize,
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

/// Apply a bounded set of literal cell edits to an imported workbook and re-export it.
pub fn edit(bytes: &[u8], edits: &[CellEdit]) -> Result<Vec<u8>> {
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
    let needs_date_style = edits
        .iter()
        .any(|edit| matches!(edit.value, CellValue::Date(_)));
    let date_style = needs_date_style.then(|| {
        workbook.styles.intern(Style {
            num_fmt: NumFmt::new(DATE_NUMBER_FORMAT),
            ..Style::default()
        })
    });

    for (index, edit) in edits.iter().enumerate() {
        let sheet_index = workbook.sheet_index(&edit.sheet).ok_or_else(|| {
            SeraError::Validation(format!(
                "edits[{index}].sheet: no sheet named '{}' in this workbook",
                edit.sheet
            ))
        })?;
        let cell = CellRef::parse(&edit.address).ok_or_else(|| {
            SeraError::Validation(format!(
                "edits[{index}].address: '{}' is not a single A1-style cell address",
                edit.address
            ))
        })?;
        edit.value.validate(&format!("edits[{index}].value"))?;
        let style = date_style.unwrap_or(StyleId::DEFAULT);
        let sheet = workbook.sheet_mut(sheet_index).ok_or_else(|| {
            SeraError::Internal("workbook sheet index disappeared during edit".into())
        })?;
        write_literal(sheet, cell, &edit.value, style)?;
    }

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
        if !sheet.images.is_empty() || !sheet.cell_pictures.is_empty() {
            return Err(SeraError::Validation(format!(
                "sheet '{}' contains images; SeraBot workbooks hold literal cell values only",
                sheet.name
            )));
        }
        if let Some(preserved) = sheet.preserved.as_deref() {
            check_preserved(preserved)?;
        }
    }

    Ok((workbook, report))
}

fn summarize(workbook: &GridWorkbook, report: &ReadReport) -> Workbook {
    Workbook {
        sheets: workbook
            .sheets
            .iter()
            .map(|sheet| SheetSummary {
                name: sheet.name.clone(),
                used_range: sheet.cells.used_range().map(|range| range.a1()),
                cells: sheet.cells.len(),
            })
            .collect(),
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
        if lowered.starts_with("xl/media/") || lowered.starts_with("xl/embeddings/") {
            return Err(SeraError::Validation(
                "workbook contains images or embedded objects; SeraBot workbooks hold literal cell values only"
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
    if preserved.vml.is_some() {
        return Err(SeraError::Validation(
            "workbook contains embedded objects or form controls; SeraBot workbooks hold literal cell values only"
                .into(),
        ));
    }
    Ok(())
}

fn truncated_package() -> SeraError {
    SeraError::Validation("workbook package is truncated or malformed".into())
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
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
            "{label} looks like a formula; only literal values are accepted"
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
                 Sheets carry a header row and rows of text, number, or date cells. Formulas, \
                 macros, external links, images, and embedded objects are rejected; nothing is \
                 ever calculated."
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
                "Apply bounded literal cell edits to an existing workbook artifact and store the \
                 result as a new artifact that records its source. Formulas, macros, external \
                 links, images, and embedded objects are rejected; nothing is ever calculated."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source_artifact": {"type": "string", "description": "Artifact hash of the workbook to edit"},
                    "edits": {
                        "type": "array",
                        "description": "CellEdit: {sheet, address (A1), value: {type: text|number|date, value}}",
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
        let edits: Vec<CellEdit> = serde_json::from_value(
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
        let edited = tokio::task::spawn_blocking(move || edit(&bytes, &edits))
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
            })),
        )?;

        let data = json!({
            "artifact_hash": stored.hash,
            "filename": stored.filename,
            "source_artifact": source.hash,
            "edits": edit_count,
        });
        Ok(ToolOutput {
            content: format!(
                "edited {edit_count} cell(s) from {}\nfilename: {}\nartifact: {}",
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
                "Read-only summary of a workbook artifact: sheet names, used ranges, and cell \
                 counts. Macros, external links, images, and embedded objects are rejected."
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::gateway::ToolRegistry;

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
    fn rejects_macro_and_external_link_parts() {
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

        let image = append_stored_part(&bytes, "xl/media/image1.png", b"\x89PNG");
        assert!(import(&image).is_err());
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

        let edit_tool = registry.get("xlsx_edit").unwrap();
        let edited = edit_tool
            .execute(
                json!({
                    "source_artifact": built_hash,
                    "edits": [{
                        "sheet": "Summary",
                        "address": "B2",
                        "value": {"type": "number", "value": 42.0}
                    }]
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
    /// that carries a macro, a link, or an image without any extra dependency.
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
