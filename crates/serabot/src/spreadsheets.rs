//! Spreadsheet analysis with explicit units and decimal arithmetic.
//!
//! Workbooks are read through `calamine` as values only. Formulas are never
//! evaluated (only the cached value stored in the file is reported), VBA
//! projects are never loaded, and external workbook links are never resolved.
//! Every total carries its column provenance and the currency or unit it
//! applies to; ambiguous input is warned about instead of being guessed.
//!
//! All arithmetic uses [`rust_decimal::Decimal`], never binary floats.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use calamine::{Data, Range, Reader, open_workbook_auto};
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;
use serde::{Deserialize, Serialize};

use crate::error::{Result, SeraError};
use crate::util::truncate_chars;

/// Default upper bound for a workbook file on disk (32 MiB).
pub const MAX_WORKBOOK_BYTES: u64 = 32 * 1024 * 1024;
/// Maximum number of warnings kept for one workbook or timesheet.
const MAX_WARNINGS: usize = 200;
/// Maximum characters taken from a cell into the sample or a warning.
const MAX_CELL_CHARS: usize = 160;
/// Maximum characters kept in a column label.
const MAX_LABEL_CHARS: usize = 80;
/// Maximum characters kept for a formula text in a warning.
const MAX_FORMULA_CHARS: usize = 120;
/// Maximum characters for a timesheet input.
const MAX_TIMESHEET_CHARS: usize = 1_000_000;
/// Maximum number of timesheet rows.
const MAX_TIMESHEET_ROWS: usize = 5_000;
/// Maximum number of fields accepted in a timesheet row.
const MAX_TIMESHEET_FIELDS: usize = 32;

/// Structural limits applied while reading a workbook.
#[derive(Debug, Clone)]
pub struct WorkbookLimits {
    pub max_bytes: u64,
    pub max_sheets: usize,
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_cells: usize,
    pub max_sample_rows: usize,
    pub max_sample_columns: usize,
}

impl Default for WorkbookLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_WORKBOOK_BYTES,
            max_sheets: 64,
            max_rows: 100_000,
            max_columns: 1_024,
            max_cells: 500_000,
            max_sample_rows: 8,
            max_sample_columns: 12,
        }
    }
}

/// Result of a bounded workbook read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkbookAnalysis {
    pub sheets: Vec<SheetSummary>,
    pub warnings: Vec<String>,
}

/// Totals and provenance for one worksheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SheetSummary {
    pub name: String,
    pub rows: usize,
    pub columns: usize,
    pub numeric_totals: BTreeMap<String, DecimalTotal>,
    pub currencies: BTreeSet<String>,
    pub units: BTreeSet<String>,
    /// First rows formatted as `"D17: value"` so every sample cell keeps its address.
    pub sample: Vec<Vec<String>>,
}

/// Exact total for one column group. A column with mixed currencies or units
/// produces one `DecimalTotal` per qualifier instead of a combined value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecimalTotal {
    pub column: String,
    pub sum: Decimal,
    pub count: usize,
    pub currency: Option<String>,
    pub unit: Option<String>,
}

/// Deterministic summary of a TSV/CSV timesheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimesheetSummary {
    pub entries: usize,
    pub per_project: BTreeMap<String, Decimal>,
    pub total: Decimal,
    pub warnings: Vec<String>,
}

/// Read and summarize a workbook with the default limits.
pub fn analyze_workbook_with_defaults(path: impl AsRef<Path>) -> Result<WorkbookAnalysis> {
    analyze_workbook(path, &WorkbookLimits::default())
}

/// Read and summarize a workbook without evaluating formulas or loading macros.
///
/// The file size, sheet count, row count, column count and total cell count are
/// bounded by `limits`. Oversized sheets are skipped with a warning; an
/// oversized file is rejected before parsing.
pub fn analyze_workbook(
    path: impl AsRef<Path>,
    limits: &WorkbookLimits,
) -> Result<WorkbookAnalysis> {
    let path = path.as_ref();
    let metadata = std::fs::metadata(path).map_err(|error| {
        SeraError::Validation(format!("cannot read workbook {}: {error}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(SeraError::Validation(format!(
            "workbook {} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() == 0 {
        return Err(SeraError::Validation(format!(
            "workbook {} is empty",
            path.display()
        )));
    }
    if metadata.len() > limits.max_bytes {
        return Err(SeraError::Validation(format!(
            "workbook {} is {} bytes, above the {} byte limit",
            path.display(),
            metadata.len(),
            limits.max_bytes
        )));
    }

    let mut warnings = WarningSink::default();
    if is_macro_enabled_extension(path) {
        warnings.push(format!(
            "workbook {} is a macro-enabled or legacy format; VBA macros are ignored",
            path.display()
        ));
    }

    let mut workbook = open_workbook_auto(path).map_err(|error| {
        SeraError::Validation(format!("cannot open workbook {}: {error}", path.display()))
    })?;

    let names = workbook.sheet_names();
    if names.len() > limits.max_sheets {
        warnings.push(format!(
            "workbook has {} sheets; only the first {} are analyzed",
            names.len(),
            limits.max_sheets
        ));
    }

    let mut sheets = Vec::new();
    for name in names.iter().take(limits.max_sheets) {
        let range = match workbook.worksheet_range(name) {
            Ok(range) => range,
            Err(error) => {
                warnings.push(format!("sheet '{name}': could not be read ({error})"));
                continue;
            }
        };
        let formulas = match workbook.worksheet_formula(name) {
            Ok(formulas) => Some(formulas),
            Err(error) => {
                warnings.push(format!(
                    "sheet '{name}': formulas could not be inspected ({error}); only cached values are used"
                ));
                None
            }
        };
        if let Some(summary) = analyze_range(name, &range, formulas.as_ref(), limits, &mut warnings)
        {
            sheets.push(summary);
        }
    }

    Ok(WorkbookAnalysis {
        sheets,
        warnings: warnings.finish(),
    })
}

/// Format a zero-based (row, column) pair as an Excel address such as `D17`.
pub fn cell_address(row: usize, column: usize) -> String {
    format!("{}{}", column_letters(column), row + 1)
}

fn column_letters(mut column: usize) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(b'A' + (column % 26) as u8);
        if column < 26 {
            break;
        }
        column = column / 26 - 1;
    }
    letters.reverse();
    String::from_utf8_lossy(&letters).into_owned()
}

fn is_macro_enabled_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("xlsm" | "xlam" | "xla" | "xlsb" | "xls")
    )
}

#[derive(Debug, Default)]
struct WarningSink {
    items: Vec<String>,
    suppressed: usize,
}

impl WarningSink {
    fn push(&mut self, message: impl Into<String>) {
        if self.items.len() < MAX_WARNINGS {
            self.items.push(message.into());
        } else {
            self.suppressed += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.suppressed > 0 {
            self.items.push(format!(
                "{} additional warning(s) were suppressed",
                self.suppressed
            ));
        }
        self.items
    }
}

#[derive(Debug, Default)]
struct GroupAccum {
    sum: Decimal,
    count: usize,
}

#[derive(Debug)]
struct ColumnState {
    label: String,
    header_currency: Option<String>,
    header_unit: Option<String>,
    header_names_multiple: bool,
    numeric_keyword: bool,
    identifier_like: bool,
    groups: BTreeMap<(Option<String>, Option<String>), GroupAccum>,
    numeric: usize,
    non_numeric: usize,
    non_numeric_example: Option<String>,
    errors: Vec<String>,
    formula_cached: usize,
    formula_missing: Vec<(String, String)>,
    ambiguous_numbers: Vec<String>,
}

fn analyze_range(
    name: &str,
    range: &Range<Data>,
    formulas: Option<&Range<String>>,
    limits: &WorkbookLimits,
    warnings: &mut WarningSink,
) -> Option<SheetSummary> {
    let (rows, columns) = range.get_size();
    if rows > limits.max_rows || columns > limits.max_columns {
        warnings.push(format!(
            "sheet '{name}': skipped, {rows} rows x {columns} columns exceeds the {}-row/{}-column limit",
            limits.max_rows, limits.max_columns
        ));
        return None;
    }
    if rows.saturating_mul(columns) > limits.max_cells {
        warnings.push(format!(
            "sheet '{name}': skipped, {} cells exceeds the {}-cell limit",
            rows.saturating_mul(columns),
            limits.max_cells
        ));
        return None;
    }

    let start = range.start().unwrap_or((0, 0));
    let header_row = detect_header_row(range, rows, columns);
    let data_start = header_row.map(|row| row + 1).unwrap_or(0);

    let mut currencies = BTreeSet::new();
    let mut units = BTreeSet::new();
    let mut noted = BTreeSet::new();
    let mut states: Vec<ColumnState> = Vec::with_capacity(columns);

    for index in 0..columns {
        let header_text = match header_row {
            Some(row) => range
                .get((row, index))
                .map(|cell| cell.to_string())
                .unwrap_or_default(),
            None => String::new(),
        };
        let normalized = normalize_space(&header_text);
        let label = if normalized.is_empty() {
            column_letters(start.1 as usize + index)
        } else {
            truncate_chars(&normalized, MAX_LABEL_CHARS)
        };
        let signals = detect_signals(&header_text);
        currencies.extend(signals.currencies.iter().cloned());
        units.extend(signals.units.iter().cloned());

        if let Some(row) = header_row {
            let address = cell_address(start.0 as usize + row, start.1 as usize + index);
            for note in &signals.notes {
                if noted.insert(note.clone()) {
                    warnings.push(format!("sheet '{name}'!{address}: {note}"));
                }
            }
            if signals.currencies.len() > 1 {
                warnings.push(format!(
                    "sheet '{name}'!{address}: header names more than one currency; values must state their currency to be totalled"
                ));
            }
            if signals.units.len() > 1 {
                warnings.push(format!(
                    "sheet '{name}'!{address}: header names more than one unit; values must state their unit to be totalled"
                ));
            }
        }

        let header_currency = if signals.currencies.len() == 1 {
            signals.currencies.iter().next().cloned()
        } else {
            None
        };
        let header_unit = if signals.units.len() == 1 {
            signals.units.iter().next().cloned()
        } else {
            None
        };
        let lower = normalize_space(&header_text).to_lowercase();
        states.push(ColumnState {
            label,
            header_names_multiple: signals.currencies.len() > 1 || signals.units.len() > 1,
            header_currency,
            header_unit,
            numeric_keyword: has_token(&lower, NUMERIC_LABEL_WORDS),
            identifier_like: has_token(&lower, IDENTIFIER_LABEL_WORDS),
            groups: BTreeMap::new(),
            numeric: 0,
            non_numeric: 0,
            non_numeric_example: None,
            errors: Vec::new(),
            formula_cached: 0,
            formula_missing: Vec::new(),
            ambiguous_numbers: Vec::new(),
        });
    }

    for row in data_start..rows {
        let absolute_row = start.0 as usize + row;
        for (index, state) in states.iter_mut().enumerate() {
            let absolute_column = start.1 as usize + index;
            let address = cell_address(absolute_row, absolute_column);
            let formula = formula_at(formulas, absolute_row, absolute_column);
            let cell = range.get((row, index));

            match cell {
                None | Some(Data::Empty) => {
                    if let Some(formula) = formula {
                        state
                            .formula_missing
                            .push((address, truncate_chars(formula, MAX_FORMULA_CHARS)));
                    }
                }
                Some(Data::Int(value)) => {
                    if formula.is_some() {
                        state.formula_cached += 1;
                    }
                    record_numeric(
                        state,
                        Decimal::from(*value),
                        &Signals::default(),
                        &address,
                        warnings,
                    );
                }
                Some(Data::Float(value)) => {
                    if formula.is_some() {
                        state.formula_cached += 1;
                    }
                    match Decimal::from_f64(*value) {
                        Some(amount) => {
                            record_numeric(state, amount, &Signals::default(), &address, warnings)
                        }
                        None => {
                            state.non_numeric += 1;
                            if state.non_numeric_example.is_none() {
                                state.non_numeric_example =
                                    Some(format!("{address}: non-finite number"));
                            }
                        }
                    }
                }
                Some(Data::String(text)) => {
                    let text = text.trim();
                    if text.is_empty() {
                        if let Some(formula) = formula {
                            state
                                .formula_missing
                                .push((address, truncate_chars(formula, MAX_FORMULA_CHARS)));
                        }
                        continue;
                    }
                    let signals = detect_signals(text);
                    currencies.extend(signals.currencies.iter().cloned());
                    units.extend(signals.units.iter().cloned());
                    for note in &signals.notes {
                        if noted.insert(note.clone()) {
                            warnings.push(format!("sheet '{name}'!{address}: {note}"));
                        }
                    }
                    if formula.is_some() {
                        state.formula_cached += 1;
                    }
                    if signals.currencies.len() > 1 || signals.units.len() > 1 {
                        state.non_numeric += 1;
                        if state.non_numeric_example.is_none() {
                            state.non_numeric_example = Some(format!(
                                "{address}: '{}' names several currencies or units",
                                truncate_chars(text, 60)
                            ));
                        }
                        continue;
                    }
                    match parse_quantity(text, &signals) {
                        Some(quantity) => {
                            if quantity.ambiguous_grouping {
                                state.ambiguous_numbers.push(format!(
                                    "{address}: '{}' read as {}",
                                    truncate_chars(text, 40),
                                    quantity.amount
                                ));
                            }
                            record_numeric(state, quantity.amount, &signals, &address, warnings);
                        }
                        None => {
                            state.non_numeric += 1;
                            if state.non_numeric_example.is_none() {
                                state.non_numeric_example =
                                    Some(format!("{address}: '{}'", truncate_chars(text, 60)));
                            }
                        }
                    }
                }
                Some(Data::Bool(_)) => {
                    if formula.is_some() {
                        state.formula_cached += 1;
                    }
                    state.non_numeric += 1;
                }
                Some(Data::DateTime(_) | Data::DateTimeIso(_) | Data::DurationIso(_)) => {
                    if formula.is_some() {
                        state.formula_cached += 1;
                    }
                    state.non_numeric += 1;
                }
                Some(Data::Error(error)) => {
                    if formula.is_some() {
                        state.formula_cached += 1;
                    }
                    state.non_numeric += 1;
                    state.errors.push(format!("{address}: {error}"));
                }
            }
        }
    }

    let mut totals = BTreeMap::new();
    let mut used_keys: BTreeSet<String> = BTreeSet::new();
    let unqualified: (Option<String>, Option<String>) = (None, None);

    for (index, state) in states.iter().enumerate() {
        let letter = column_letters(start.1 as usize + index);

        for error in &state.errors {
            warnings.push(format!(
                "sheet '{name}' {error}: cell error excluded from totals"
            ));
        }

        let non_empty = state.numeric + state.non_numeric;
        let explicit =
            state.header_currency.is_some() || state.header_unit.is_some() || state.numeric_keyword;
        let majority = !state.identifier_like
            && state.numeric >= 2
            && state.numeric.saturating_mul(2) >= non_empty;
        if !(explicit || majority) {
            continue;
        }

        if state.numeric == 0 {
            if explicit {
                warnings.push(format!(
                    "sheet '{name}': column '{}' ({letter}) declares a currency, unit or numeric role but has no numeric values; no total produced",
                    state.label
                ));
            }
            continue;
        }

        let has_qualified_groups = state
            .groups
            .keys()
            .any(|(currency, unit)| currency.is_some() || unit.is_some());
        if state.header_names_multiple
            && !has_qualified_groups
            && state.groups.contains_key(&unqualified)
        {
            warnings.push(format!(
                "sheet '{name}': column '{}' ({letter}) header names several currencies or units but the values are unqualified; no total produced",
                state.label
            ));
            continue;
        }

        let group_currencies: BTreeSet<&str> = state
            .groups
            .keys()
            .filter_map(|(currency, _)| currency.as_deref())
            .collect();
        if group_currencies.len() > 1 {
            warnings.push(format!(
                "sheet '{name}': column '{}' ({letter}) mixes currencies ({}); totals are separated by currency and never combined",
                state.label,
                group_currencies.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        let group_units: BTreeSet<&str> = state
            .groups
            .keys()
            .filter_map(|(_, unit)| unit.as_deref())
            .collect();
        if group_units.len() > 1 {
            warnings.push(format!(
                "sheet '{name}': column '{}' ({letter}) mixes units ({}); totals are separated by unit and never combined",
                state.label,
                group_units.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }
        let has_qualified =
            state.header_currency.is_some() || state.header_unit.is_some() || has_qualified_groups;
        if has_qualified && state.groups.contains_key(&unqualified) {
            warnings.push(format!(
                "sheet '{name}': column '{}' ({letter}) contains values without an explicit currency or unit; those values are totalled separately",
                state.label
            ));
        }

        if state.formula_cached > 0 {
            warnings.push(format!(
                "sheet '{name}': column '{}' ({letter}) uses cached values for {} formula cell(s); formulas are not evaluated",
                state.label, state.formula_cached
            ));
        }
        for (address, formula) in state.formula_missing.iter().take(5) {
            warnings.push(format!(
                "sheet '{name}'!{address}: formula '{formula}' has no cached numeric value; excluded from totals"
            ));
        }
        for example in state.ambiguous_numbers.iter().take(3) {
            warnings.push(format!(
                "sheet '{name}'!{example}: ambiguous decimal separator interpreted deterministically"
            ));
        }
        if state.non_numeric > 0 {
            if let Some(example) = &state.non_numeric_example {
                warnings.push(format!(
                    "sheet '{name}': column '{}' ({letter}) excludes {} non-numeric value(s) from totals (first at {example})",
                    state.label, state.non_numeric
                ));
            }
        }

        for ((currency, unit), group) in &state.groups {
            if group.count == 0 {
                continue;
            }
            if state.header_names_multiple && currency.is_none() && unit.is_none() {
                warnings.push(format!(
                    "sheet '{name}': column '{}' ({letter}) contains unqualified values while the header names several currencies or units; those values were not totalled",
                    state.label
                ));
                continue;
            }
            let qualifier = match (currency, unit) {
                (Some(currency), Some(unit)) => Some(format!("{currency}/{unit}")),
                (Some(currency), None) => Some(currency.clone()),
                (None, Some(unit)) => Some(unit.clone()),
                (None, None) if has_qualified => Some("unspecified".into()),
                (None, None) => None,
            };
            let base = match qualifier {
                Some(qualifier) => format!("{} [{qualifier}]", state.label),
                None => state.label.clone(),
            };
            let mut key = base.clone();
            if used_keys.contains(&key) {
                key = format!("{base} ({letter})");
            }
            let mut suffix = 2usize;
            while used_keys.contains(&key) {
                key = format!("{base} ({letter}-{suffix})");
                suffix += 1;
                if suffix > 100 {
                    break;
                }
            }
            used_keys.insert(key.clone());
            totals.insert(
                key.clone(),
                DecimalTotal {
                    column: key,
                    sum: group.sum,
                    count: group.count,
                    currency: currency.clone(),
                    unit: unit.clone(),
                },
            );
        }
    }

    let sample = build_sample(
        range,
        formulas,
        rows,
        columns,
        start,
        limits.max_sample_rows,
        limits.max_sample_columns,
    );

    Some(SheetSummary {
        name: name.to_string(),
        rows,
        columns,
        numeric_totals: totals,
        currencies,
        units,
        sample,
    })
}

fn detect_header_row(range: &Range<Data>, rows: usize, columns: usize) -> Option<usize> {
    if rows == 0 || columns == 0 {
        return None;
    }
    let mut single_candidate = None;
    for row in 0..rows.min(10) {
        let mut non_empty = 0usize;
        let mut numeric = false;
        for column in 0..columns {
            match range.get((row, column)) {
                None | Some(Data::Empty) => {}
                Some(Data::Int(_) | Data::Float(_)) => {
                    numeric = true;
                    non_empty += 1;
                }
                Some(_) => non_empty += 1,
            }
        }
        if numeric {
            // The header, if any, must precede the first numeric row.
            break;
        }
        if non_empty >= 2 {
            return Some(row);
        }
        if non_empty == 1 && single_candidate.is_none() {
            // A one-cell row may be a title rather than a header. Accept it when
            // the next row is already numeric or when it is the last row;
            // otherwise keep it as a fallback for single-column sheets.
            let next_has_numeric = row + 1 < rows
                && (0..columns).any(|column| {
                    matches!(
                        range.get((row + 1, column)),
                        Some(Data::Int(_) | Data::Float(_))
                    )
                });
            if next_has_numeric || row + 1 >= rows {
                return Some(row);
            }
            single_candidate = Some(row);
        }
    }
    if columns <= 1 { single_candidate } else { None }
}

fn record_numeric(
    state: &mut ColumnState,
    amount: Decimal,
    signals: &Signals,
    address: &str,
    warnings: &mut WarningSink,
) {
    let currency = if signals.currencies.len() == 1 {
        signals.currencies.iter().next().cloned()
    } else if signals.currencies.is_empty() {
        state.header_currency.clone()
    } else {
        None
    };
    let unit = if signals.units.len() == 1 {
        signals.units.iter().next().cloned()
    } else if signals.units.is_empty() {
        state.header_unit.clone()
    } else {
        None
    };
    let group = state.groups.entry((currency, unit)).or_default();
    match group.sum.checked_add(amount) {
        Some(sum) => {
            group.sum = sum;
            group.count += 1;
            state.numeric += 1;
        }
        None => warnings.push(format!(
            "{address}: total overflow while adding {amount}; value excluded"
        )),
    }
}

fn build_sample(
    range: &Range<Data>,
    formulas: Option<&Range<String>>,
    rows: usize,
    columns: usize,
    start: (u32, u32),
    max_rows: usize,
    max_columns: usize,
) -> Vec<Vec<String>> {
    let sample_rows = rows.min(max_rows);
    let sample_columns = columns.min(max_columns);
    let mut sample = Vec::with_capacity(sample_rows);
    for row in 0..sample_rows {
        let mut line = Vec::with_capacity(sample_columns);
        for column in 0..sample_columns {
            let absolute_row = start.0 as usize + row;
            let absolute_column = start.1 as usize + column;
            let address = cell_address(absolute_row, absolute_column);
            let value = match range.get((row, column)) {
                None | Some(Data::Empty) => String::new(),
                Some(cell) => {
                    let text = truncate_chars(&normalize_space(&cell.to_string()), MAX_CELL_CHARS);
                    if formula_at(formulas, absolute_row, absolute_column).is_some() {
                        format!("{address}: {text} [formula]")
                    } else {
                        format!("{address}: {text}")
                    }
                }
            };
            line.push(value);
        }
        sample.push(line);
    }
    sample
}

fn formula_at(formulas: Option<&Range<String>>, row: usize, column: usize) -> Option<&str> {
    let formulas = formulas?;
    let (start_row, start_column) = formulas.start()?;
    if row < start_row as usize || column < start_column as usize {
        return None;
    }
    let value = formulas.get((row - start_row as usize, column - start_column as usize))?;
    if value.is_empty() { None } else { Some(value) }
}

fn normalize_space(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Currency and unit evidence found in a label or cell text.
#[derive(Debug, Default, Clone)]
struct Signals {
    currencies: BTreeSet<String>,
    units: BTreeSet<String>,
    notes: Vec<String>,
}

fn detect_signals(text: &str) -> Signals {
    let mut signals = Signals::default();
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|value: char| !value.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();

    let mut matched_currencies = BTreeSet::new();
    let mut matched_units = BTreeSet::new();
    for token in &tokens {
        for candidate in token_variants(token) {
            if let Some(code) = currency_code(&candidate) {
                matched_currencies.insert(code.to_string());
            }
            if let Some(unit) = unit_code(&candidate) {
                matched_units.insert(unit.to_string());
            }
        }
    }

    if text.contains('€') {
        matched_currencies.insert("EUR".into());
    }
    if text.contains('$') {
        matched_currencies.insert("USD".into());
        signals.notes.push("'$' symbol interpreted as USD".into());
    }
    if text.contains('£') {
        matched_currencies.insert("GBP".into());
    }
    if text.contains('%') {
        matched_units.insert("%".into());
    }

    for currency in matched_currencies {
        signals.currencies.insert(currency);
    }
    for unit in matched_units {
        if unit == "m" {
            signals
                .notes
                .push("unit 'm' interpreted as metres, not minutes".into());
        } else if unit == "d" {
            signals.notes.push("unit 'd' interpreted as days".into());
        }
        signals.units.insert(unit);
    }
    if signals.currencies.len() > 1 {
        signals
            .notes
            .push("text names more than one currency".into());
    }
    if signals.units.len() > 1 {
        signals.notes.push("text names more than one unit".into());
    }
    signals
}

fn token_variants(token: &str) -> Vec<String> {
    let mut variants = vec![token.to_string()];
    if let Some(index) = token.find(|value: char| value.is_alphabetic()) {
        if index > 0 {
            variants.push(token[index..].to_string());
        }
    }
    variants
}

fn currency_code(token: &str) -> Option<&'static str> {
    match token {
        "eur" | "euro" | "euros" => Some("EUR"),
        "usd" | "dollar" | "dollars" => Some("USD"),
        "mad" | "dirham" | "dirhams" => Some("MAD"),
        "gbp" => Some("GBP"),
        "chf" => Some("CHF"),
        _ => None,
    }
}

fn unit_code(token: &str) -> Option<&'static str> {
    match token {
        "h" | "hr" | "hrs" | "hour" | "hours" | "heure" | "heures" | "ora" | "ore" | "stunde"
        | "stunden" => Some("hours"),
        "d" | "day" | "days" | "j" | "jour" | "jours" | "giorno" | "giorni" | "tag" | "tage" => {
            Some("days")
        }
        "kg" | "kgs" | "kilogram" | "kilograms" | "kilogramme" | "kilogrammes" | "chilo"
        | "chili" => Some("kg"),
        "km" | "kilometre" | "kilometres" | "kilomètre" | "kilomètres" | "chilometri"
        | "chilometro" => Some("km"),
        "m" | "meter" | "meters" | "metre" | "metres" | "metro" | "metri" => Some("m"),
        "pc" | "pcs" | "piece" | "pieces" | "pièce" | "pièces" | "pezzo" | "pezzi" | "unite"
        | "unites" | "unita" | "unità" | "unit" | "units" => Some("pieces"),
        _ => None,
    }
}

#[derive(Debug)]
struct Quantity {
    amount: Decimal,
    ambiguous_grouping: bool,
}

fn parse_quantity(text: &str, signals: &Signals) -> Option<Quantity> {
    let run = numeric_run(text)?;
    let number = parse_number(run)?;
    if !leftover_is_qualifier(text, run) {
        return None;
    }
    if signals.currencies.len() > 1 || signals.units.len() > 1 {
        return None;
    }
    Some(Quantity {
        amount: number.amount,
        ambiguous_grouping: number.ambiguous,
    })
}

fn numeric_run(text: &str) -> Option<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut best: Option<(usize, usize)> = None;
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index].1.is_ascii_digit() {
            let mut start = index;
            while start > 0 && is_number_char(chars[start - 1].1) {
                start -= 1;
            }
            let mut end = index;
            while end + 1 < chars.len() && is_number_char(chars[end + 1].1) {
                end += 1;
            }
            let byte_start = chars[start].0;
            let byte_end = chars[end].0 + chars[end].1.len_utf8();
            let better = best
                .map(|(best_start, best_end)| byte_end - byte_start > best_end - best_start)
                .unwrap_or(true);
            if better {
                best = Some((byte_start, byte_end));
            }
            index = end + 1;
        } else {
            index += 1;
        }
    }
    best.map(|(start, end)| &text[start..end])
}

fn is_number_char(value: char) -> bool {
    value.is_ascii_digit()
        || matches!(
            value,
            '.' | ',' | '\'' | '’' | ' ' | '\u{00A0}' | '\u{202F}' | '-' | '+'
        )
}

#[derive(Debug)]
struct ParsedNumber {
    amount: Decimal,
    ambiguous: bool,
}

fn parse_number(raw: &str) -> Option<ParsedNumber> {
    let trimmed = raw.trim();
    let (negative, body) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    if body.is_empty() {
        return None;
    }
    if body
        .chars()
        .next()
        .is_some_and(|value| value.is_whitespace())
    {
        // A bullet like "- 10" is not a negative number.
        return None;
    }

    let mut digits = String::new();
    let mut digit_count = 0usize;
    let mut separators: Vec<(char, usize)> = Vec::new();
    let mut grouping: Vec<usize> = Vec::new();
    for value in body.chars() {
        match value {
            '0'..='9' => {
                digits.push(value);
                digit_count += 1;
            }
            '\'' | '’' | ' ' | '\u{00A0}' | '\u{202F}' => grouping.push(digit_count),
            '.' | ',' => separators.push((value, digit_count)),
            _ => return None,
        }
    }
    if digit_count == 0 || digit_count > 18 {
        return None;
    }

    let (decimal_position, ambiguous) = decimal_position(&separators, digit_count)?;
    if !validate_extra_grouping(&grouping, decimal_position, digit_count) {
        return None;
    }
    let body = match decimal_position {
        Some(position) => {
            let mut integer = digits[..position].to_string();
            if integer.is_empty() {
                integer.push('0');
            }
            format!("{integer}.{}", &digits[position..])
        }
        None => digits,
    };
    let normalized = if negative { format!("-{body}") } else { body };
    let amount = Decimal::from_str_exact(&normalized).ok()?;
    Some(ParsedNumber { amount, ambiguous })
}

/// Validate whitespace/apostrophe grouping (`1 234`, `1'234'567`). The first
/// group has 1-3 digits and every following group exactly 3, so `1 2 3` is
/// rejected instead of being read as 123.
fn validate_extra_grouping(
    grouping: &[usize],
    decimal_position: Option<usize>,
    digit_count: usize,
) -> bool {
    if grouping.is_empty() {
        return true;
    }
    let mut positions = grouping.to_vec();
    positions.sort_unstable();
    positions.dedup();
    if positions.len() != grouping.len() {
        return false;
    }
    let mut previous = 0usize;
    for (index, position) in positions.iter().enumerate() {
        if *position == 0 {
            return false;
        }
        if decimal_position.is_some_and(|decimal| *position >= decimal) {
            return false;
        }
        let group = position - previous;
        if index == 0 {
            if group > 3 {
                return false;
            }
        } else if group != 3 {
            return false;
        }
        previous = *position;
    }
    match decimal_position {
        Some(decimal) => decimal - previous == 3,
        None => digit_count - previous == 3,
    }
}

/// Return the digit index where the fractional part starts (`None` for an
/// integer) plus whether the separator layout was ambiguous. Separators that are
/// not decimal must form 3-digit groups; anything that cannot be read
/// deterministically is rejected instead of guessed.
fn decimal_position(
    separators: &[(char, usize)],
    digit_count: usize,
) -> Option<(Option<usize>, bool)> {
    if separators.is_empty() {
        return Some((None, false));
    }
    let kinds: BTreeSet<char> = separators.iter().map(|(value, _)| *value).collect();

    let mut ambiguous = false;
    let decimal_index = if kinds.len() == 2 {
        // The last separator is the decimal one; all earlier ones must group by 3.
        let last = separators.len() - 1;
        let after = digit_count - separators[last].1;
        if after == 0 || after > 6 {
            return None;
        }
        let kind = separators[last].0;
        if separators
            .iter()
            .filter(|(value, _)| *value == kind)
            .count()
            != 1
        {
            return None;
        }
        Some(last)
    } else if separators.len() == 1 {
        let (_, position) = separators[0];
        let after = digit_count - position;
        match after {
            0 => return None,
            1 | 2 => Some(0),
            3 => {
                if (1..=3).contains(&position) {
                    // "1,234" is almost certainly a thousands group, but it
                    // could be a decimal value; the caller is warned.
                    ambiguous = true;
                    None
                } else {
                    Some(0)
                }
            }
            4..=6 => {
                ambiguous = true;
                Some(0)
            }
            _ => return None,
        }
    } else {
        // Several separators of the same kind can only be thousands groups.
        None
    };

    let decimal_digit = decimal_index.map(|index| separators[index].1);
    let mut previous = 0usize;
    for (index, (_, position)) in separators.iter().enumerate() {
        if Some(index) == decimal_index {
            if index + 1 != separators.len() {
                return None;
            }
            let group = position - previous;
            if index == 0 {
                if group == 0 {
                    return None;
                }
            } else if group != 3 {
                return None;
            }
            return Some((decimal_digit, ambiguous));
        }
        let group = position - previous;
        if index == 0 {
            if group == 0 || group > 3 {
                return None;
            }
        } else if group != 3 {
            return None;
        }
        previous = *position;
    }
    let trailing = digit_count - previous;
    if trailing != 3 {
        return None;
    }
    Some((None, ambiguous))
}

fn leftover_is_qualifier(text: &str, run: &str) -> bool {
    let mut remaining = String::with_capacity(text.len());
    if let Some(index) = text.find(run) {
        remaining.push_str(&text[..index]);
        remaining.push(' ');
        remaining.push_str(&text[index + run.len()..]);
    } else {
        remaining.push_str(text);
    }
    for token in remaining
        .to_lowercase()
        .split(|value: char| !value.is_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        let lower = token.to_string();
        if currency_code(&lower).is_some() || unit_code(&lower).is_some() {
            continue;
        }
        if ALLOWED_FILLER.contains(&lower.as_str()) {
            continue;
        }
        return false;
    }
    true
}

const ALLOWED_FILLER: &[&str] = &[
    "approx",
    "approximately",
    "about",
    "around",
    "circa",
    "env",
    "ca",
    "each",
    "per",
    "of",
    "total",
    "net",
    "gross",
    "ht",
    "ttc",
    "incl",
    "excl",
    "included",
    "excluded",
    "tbd",
    "est",
];

const NUMERIC_LABEL_WORDS: &[&str] = &[
    "amount",
    "amounts",
    "total",
    "totals",
    "subtotal",
    "sum",
    "qty",
    "quantity",
    "quantite",
    "quantité",
    "count",
    "balance",
    "cost",
    "costs",
    "price",
    "prices",
    "budget",
    "montant",
    "montants",
    "somme",
    "importo",
    "totale",
    "costo",
    "costi",
    "prezzo",
    "prezzi",
    "saldo",
    "hours",
    "heures",
    "jours",
    "days",
    "ore",
    "giorni",
    "km",
    "kg",
    "tax",
    "paid",
    "paye",
    "payé",
    "versato",
];

const IDENTIFIER_LABEL_WORDS: &[&str] = &[
    "id",
    "identifier",
    "code",
    "ref",
    "reference",
    "réf",
    "référence",
    "numero",
    "numéro",
    "index",
    "year",
    "annee",
    "année",
    "date",
    "time",
    "phone",
    "telephone",
    "téléphone",
    "tel",
    "siret",
    "siren",
    "vat",
    "tva",
    "iban",
    "bic",
    "zip",
    "postal",
    "matricule",
    "rank",
    "position",
    "seq",
    "sku",
    "ean",
    "upc",
    "project",
    "projet",
    "progetto",
    "client",
    "customer",
    "name",
    "nom",
    "nome",
    "label",
    "description",
    "descrizione",
    "note",
    "comment",
    "commentaire",
    "commento",
    "type",
    "category",
    "categoria",
    "status",
    "statut",
    "stato",
];

fn has_token(text: &str, words: &[&str]) -> bool {
    text.split(|value: char| !value.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .any(|token| words.contains(&token))
}

/// Parse a TSV/CSV-like timesheet into exact per-project decimal totals.
///
/// The input must name its unit: either in a column header (`Hours`, `Heures`,
/// `Ore`, `Days`, ...) or as a suffix on every value (`8h`, `2 days`). Negative
/// values, mixed units and unparseable rows are rejected.
pub fn parse_timesheet_rows(input: &str) -> Result<TimesheetSummary> {
    if input.trim().is_empty() {
        return Err(SeraError::Validation("timesheet is empty".into()));
    }
    if input.chars().count() > MAX_TIMESHEET_CHARS {
        return Err(SeraError::Validation(format!(
            "timesheet is larger than the {MAX_TIMESHEET_CHARS} character limit"
        )));
    }
    let delimiter = detect_delimiter(input).ok_or_else(|| {
        SeraError::Validation(
            "timesheet has no column delimiter (expected a tab, semicolon or comma)".into(),
        )
    })?;

    let mut warnings = WarningSink::default();
    let mut rows: Vec<(usize, Vec<String>)> = Vec::new();
    for (index, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if rows.len() >= MAX_TIMESHEET_ROWS {
            return Err(SeraError::Validation(format!(
                "timesheet exceeds the {MAX_TIMESHEET_ROWS} row limit"
            )));
        }
        let fields = split_delimited(line, delimiter);
        if fields.len() > MAX_TIMESHEET_FIELDS {
            return Err(SeraError::Validation(format!(
                "line {}: more than {MAX_TIMESHEET_FIELDS} columns",
                index + 1
            )));
        }
        rows.push((index + 1, fields));
    }
    if rows.is_empty() {
        return Err(SeraError::Validation(
            "timesheet contains no data rows".into(),
        ));
    }

    let header = detect_timesheet_header(&rows[0].1);
    let start = if header.is_some() { 1 } else { 0 };

    let mut entries = 0usize;
    let mut per_project: BTreeMap<String, Decimal> = BTreeMap::new();
    let mut total = Decimal::ZERO;
    let mut declared_total: Option<(Decimal, usize)> = None;
    let mut sheet_unit: Option<TimeUnit> = header.as_ref().and_then(|header| header.unit);

    for (line_number, fields) in rows.iter().skip(start) {
        if let Some(text) = declared_total_value(fields) {
            match parse_quantity(text, &detect_signals(text)) {
                Some(quantity) if quantity.amount >= Decimal::ZERO => {
                    if declared_total.is_some() {
                        warnings.push(format!(
                            "line {line_number}: several total rows; the last one is used"
                        ));
                    }
                    declared_total = Some((quantity.amount, *line_number));
                }
                _ => warnings.push(format!(
                    "line {line_number}: total row has no numeric value"
                )),
            }
            continue;
        }

        let (hours_index, hours_text) = match header.as_ref().map(|header| header.hours) {
            Some(index) => match fields.get(index) {
                Some(value) if !value.trim().is_empty() => (index, value.trim()),
                _ => {
                    warnings.push(format!(
                        "line {line_number}: missing hours value; row skipped"
                    ));
                    continue;
                }
            },
            None => match find_quantity_field(fields) {
                Some(found) => found,
                None => {
                    return Err(SeraError::Validation(format!(
                        "line {line_number}: no hours or days value found"
                    )));
                }
            },
        };

        let signals = detect_signals(hours_text);
        let quantity = parse_quantity(hours_text, &signals).ok_or_else(|| {
            SeraError::Validation(format!(
                "line {line_number}: cannot parse hours or days from '{hours_text}'"
            ))
        })?;
        if quantity.amount < Decimal::ZERO {
            return Err(SeraError::Validation(format!(
                "line {line_number}: negative time ({} {}) is not allowed",
                quantity.amount, hours_text
            )));
        }

        let cell_unit = match signals.units.iter().next().map(String::as_str) {
            Some("hours") => Some(TimeUnit::Hours),
            Some("days") => Some(TimeUnit::Days),
            Some(other) => {
                return Err(SeraError::Validation(format!(
                    "line {line_number}: unit '{other}' is not valid for a timesheet (use hours or days)"
                )));
            }
            None => None,
        };
        let unit = cell_unit
            .or_else(|| header.as_ref().and_then(|header| header.unit))
            .or(sheet_unit)
            .ok_or_else(|| {
                SeraError::Validation(format!(
                    "line {line_number}: unit is ambiguous; add an Hours/Days column header or a unit suffix such as '8h' or '2 days'"
                ))
            })?;
        if let Some(existing) = sheet_unit {
            if existing != unit {
                return Err(SeraError::Validation(format!(
                    "line {line_number}: mixed units ({} and {}); a timesheet must use one unit",
                    existing.label(),
                    unit.label()
                )));
            }
        } else {
            sheet_unit = Some(unit);
        }

        let project = timesheet_project(fields, hours_index, header.as_ref());
        let project = match project {
            Some(value) if !value.is_empty() => truncate_chars(&value, 120),
            _ => {
                let person = header
                    .as_ref()
                    .and_then(|header| header.person)
                    .and_then(|index| fields.get(index))
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .unwrap_or("unknown");
                warnings.push(format!(
                    "line {line_number}: no project for '{person}'; recorded as unassigned"
                ));
                "unassigned".to_string()
            }
        };

        if quantity.amount.is_zero() {
            warnings.push(format!("line {line_number}: zero time recorded"));
        }

        entries += 1;
        let project_entry = per_project.entry(project).or_insert(Decimal::ZERO);
        let next = project_entry.checked_add(quantity.amount).ok_or_else(|| {
            SeraError::Validation(format!("line {line_number}: project total overflow"))
        })?;
        *project_entry = next;
        total = total.checked_add(quantity.amount).ok_or_else(|| {
            SeraError::Validation(format!("line {line_number}: timesheet total overflow"))
        })?;
    }

    if entries == 0 {
        return Err(SeraError::Validation(
            "timesheet contains no time entries".into(),
        ));
    }
    if let Some((declared, line_number)) = declared_total {
        if declared != total {
            warnings.push(format!(
                "line {line_number}: declared total {declared} does not match the computed total {total}"
            ));
        }
    }

    Ok(TimesheetSummary {
        entries,
        per_project,
        total,
        warnings: warnings.finish(),
    })
}

/// Alias for [`parse_timesheet_rows`] kept for callers that name the input format.
pub fn read_timesheet_csv(input: &str) -> Result<TimesheetSummary> {
    parse_timesheet_rows(input)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeUnit {
    Hours,
    Days,
}

impl TimeUnit {
    fn label(self) -> &'static str {
        match self {
            Self::Hours => "hours",
            Self::Days => "days",
        }
    }
}

#[derive(Debug)]
struct TimesheetHeader {
    hours: usize,
    unit: Option<TimeUnit>,
    project: Option<usize>,
    person: Option<usize>,
}

fn detect_timesheet_header(row: &[String]) -> Option<TimesheetHeader> {
    let mut hours = None;
    let mut unit = None;
    let mut project = None;
    let mut person = None;

    for (index, field) in row.iter().enumerate() {
        let tokens = lowercase_tokens(field);
        if tokens.is_empty() {
            continue;
        }
        let has = |set: &[&str]| tokens.iter().any(|token| set.contains(&token.as_str()));
        let is_time = has(HOUR_UNIT_WORDS) || has(DAY_UNIT_WORDS) || has(TIME_HEADER_WORDS);
        if is_time {
            if hours.is_none() {
                hours = Some(index);
            }
            let hour_word = has(HOUR_UNIT_WORDS);
            let day_word = has(DAY_UNIT_WORDS);
            if hour_word && !day_word {
                unit = Some(TimeUnit::Hours);
            } else if day_word && !hour_word {
                unit = Some(TimeUnit::Days);
            }
            continue;
        }
        if project.is_none() && has(PROJECT_HEADER_WORDS) {
            project = Some(index);
            continue;
        }
        if person.is_none() && has(PERSON_HEADER_WORDS) {
            person = Some(index);
        }
    }

    hours.map(|hours| TimesheetHeader {
        hours,
        unit,
        project,
        person,
    })
}

const HOUR_UNIT_WORDS: &[&str] = &[
    "hours", "hour", "hrs", "hr", "h", "heures", "heure", "ore", "ora", "stunden", "stunde",
];
const DAY_UNIT_WORDS: &[&str] = &[
    "days", "day", "jours", "jour", "giorni", "giorno", "tage", "tagen",
];
/// A time column whose header does not commit to hours or days; rows must then
/// state their unit explicitly or the timesheet is rejected as ambiguous.
const TIME_HEADER_WORDS: &[&str] = &["time", "duration", "temps", "durata", "tempo"];
const PROJECT_HEADER_WORDS: &[&str] = &[
    "project",
    "projet",
    "progetto",
    "client",
    "customer",
    "mission",
    "activity",
    "activité",
    "attività",
    "commessa",
    "task",
    "tâche",
    "tache",
];
const PERSON_HEADER_WORDS: &[&str] = &[
    "person",
    "name",
    "employee",
    "consultant",
    "collaborator",
    "collaborateur",
    "employé",
    "employe",
    "nome",
    "dipendente",
    "resource",
    "ressource",
];

fn lowercase_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|value: char| !value.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn detect_delimiter(input: &str) -> Option<char> {
    let mut tabs = 0usize;
    let mut semicolons = 0usize;
    let mut commas = 0usize;
    for line in input.lines().take(10) {
        let mut quoted = false;
        for value in line.chars() {
            match value {
                '"' => quoted = !quoted,
                '\t' if !quoted => tabs += 1,
                ';' if !quoted => semicolons += 1,
                ',' if !quoted => commas += 1,
                _ => {}
            }
        }
    }
    let max = tabs.max(semicolons).max(commas);
    if max == 0 {
        None
    } else if tabs == max {
        Some('\t')
    } else if semicolons == max {
        Some(';')
    } else {
        Some(',')
    }
}

fn split_delimited(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(value) = chars.next() {
        match value {
            '"' if quoted => {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            }
            '"' if !quoted && current.trim().is_empty() => {
                quoted = true;
                current.clear();
            }
            '"' => current.push('"'),
            value if value == delimiter && !quoted => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            value => current.push(value),
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn declared_total_value(fields: &[String]) -> Option<&str> {
    let index = fields.iter().position(|field| !field.trim().is_empty())?;
    let first = fields[index].trim();
    if !is_total_label(first) {
        // Also accept a single-field total such as "Total: 12.5".
        let (label, value) = first.split_once(':')?;
        if is_total_label(label.trim()) && !value.trim().is_empty() {
            return Some(value.trim());
        }
        return None;
    }
    fields[index + 1..]
        .iter()
        .rev()
        .find(|field| !field.trim().is_empty())
        .map(|field| field.trim())
}

fn is_total_label(text: &str) -> bool {
    matches!(
        text.trim_end_matches(':').trim().to_lowercase().as_str(),
        "total"
            | "totals"
            | "totale"
            | "totaux"
            | "grand total"
            | "total general"
            | "total général"
            | "sous-total"
            | "subtotal"
    )
}

fn find_quantity_field(fields: &[String]) -> Option<(usize, &str)> {
    let mut fallback = None;
    for (index, field) in fields.iter().enumerate() {
        let text = field.trim();
        if text.is_empty() {
            continue;
        }
        let signals = detect_signals(text);
        if let Some(quantity) = parse_quantity(text, &signals) {
            if quantity.amount.is_sign_negative() {
                return Some((index, text));
            }
            if signals
                .units
                .iter()
                .any(|unit| unit == "hours" || unit == "days")
            {
                return Some((index, text));
            }
            fallback = Some((index, text));
        }
    }
    fallback
}

fn timesheet_project(
    fields: &[String],
    hours_index: usize,
    header: Option<&TimesheetHeader>,
) -> Option<String> {
    if let Some(index) = header.and_then(|header| header.project) {
        return fields.get(index).map(|value| normalize_space(value));
    }
    for (index, field) in fields.iter().enumerate() {
        if index == hours_index {
            continue;
        }
        let text = field.trim();
        if text.is_empty() {
            continue;
        }
        let signals = detect_signals(text);
        if parse_quantity(text, &signals).is_some() {
            continue;
        }
        return Some(normalize_space(text));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimal(value: &str) -> Decimal {
        Decimal::from_str_exact(value).expect("valid decimal literal")
    }

    fn limits() -> WorkbookLimits {
        WorkbookLimits::default()
    }

    fn summarize(
        name: &str,
        range: &Range<Data>,
        formulas: Option<&Range<String>>,
    ) -> (Option<SheetSummary>, Vec<String>) {
        let mut warnings = WarningSink::default();
        let summary = analyze_range(name, range, formulas, &limits(), &mut warnings);
        (summary, warnings.finish())
    }

    #[test]
    fn decimal_totals_reconcile_exactly() {
        let mut range = Range::new((0, 0), (2, 1));
        range.set_value((0, 0), Data::String("Item".into()));
        range.set_value((0, 1), Data::String("Amount".into()));
        range.set_value((1, 0), Data::String("first".into()));
        range.set_value((1, 1), Data::Float(0.1));
        range.set_value((2, 0), Data::String("second".into()));
        range.set_value((2, 1), Data::Float(0.2));

        let (summary, warnings) = summarize("Budget", &range, None);
        let summary = summary.expect("sheet is within limits");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let total = summary.numeric_totals.get("Amount").expect("amount total");
        assert_eq!(total.sum, decimal("0.3"));
        assert_eq!(total.count, 2);
        assert_eq!(total.currency, None);
        assert_eq!(total.unit, None);
        assert_eq!(summary.rows, 3);
        assert_eq!(summary.columns, 2);
    }

    #[test]
    fn mixed_currencies_are_separated() {
        let mut range = Range::new((0, 0), (3, 1));
        range.set_value((0, 0), Data::String("Item".into()));
        range.set_value((0, 1), Data::String("Amount".into()));
        range.set_value((1, 1), Data::String("10 EUR".into()));
        range.set_value((2, 1), Data::String("20 USD".into()));
        range.set_value((3, 1), Data::String("5 EUR".into()));

        let (summary, warnings) = summarize("Budget", &range, None);
        let summary = summary.expect("sheet is within limits");
        let eur = summary
            .numeric_totals
            .get("Amount [EUR]")
            .expect("EUR total");
        assert_eq!(eur.sum, decimal("15"));
        assert_eq!(eur.count, 2);
        assert_eq!(eur.currency.as_deref(), Some("EUR"));
        let usd = summary
            .numeric_totals
            .get("Amount [USD]")
            .expect("USD total");
        assert_eq!(usd.sum, decimal("20"));
        assert_eq!(usd.count, 1);
        assert!(summary.currencies.contains("EUR"));
        assert!(summary.currencies.contains("USD"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("mixes currencies")),
            "expected a mixed currency warning, got {warnings:?}"
        );
    }

    #[test]
    fn mixed_units_are_separated() {
        let mut range = Range::new((0, 0), (3, 1));
        range.set_value((0, 0), Data::String("Task".into()));
        range.set_value((0, 1), Data::String("Duration".into()));
        range.set_value((1, 1), Data::String("8 hours".into()));
        range.set_value((2, 1), Data::String("2 days".into()));
        range.set_value((3, 1), Data::String("30 minutes".into()));

        let (summary, warnings) = summarize("Planning", &range, None);
        let summary = summary.expect("sheet is within limits");
        let hours = summary
            .numeric_totals
            .get("Duration [hours]")
            .expect("hours total");
        assert_eq!(hours.sum, decimal("8"));
        let days = summary
            .numeric_totals
            .get("Duration [days]")
            .expect("days total");
        assert_eq!(days.sum, decimal("2"));
        assert!(summary.units.contains("hours"));
        assert!(summary.units.contains("days"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("mixes units")),
            "expected a mixed unit warning, got {warnings:?}"
        );
    }

    #[test]
    fn formulas_and_errors_are_warned() {
        let mut range = Range::new((0, 0), (3, 2));
        range.set_value((0, 0), Data::String("Item".into()));
        range.set_value((0, 1), Data::String("Amount".into()));
        range.set_value((1, 1), Data::Float(10.0));
        // B3 is a formula without a cached value.
        // C2 is a cached error.
        range.set_value((1, 2), Data::Error(calamine::CellErrorType::Div0));

        let mut formulas = Range::new((0, 0), (3, 2));
        formulas.set_value((2, 1), "SUM(B2:B2)".to_string());

        let (summary, warnings) = summarize("Budget", &range, Some(&formulas));
        let summary = summary.expect("sheet is within limits");
        let total = summary.numeric_totals.get("Amount").expect("amount total");
        assert_eq!(total.sum, decimal("10"));
        assert_eq!(total.count, 1);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("B3") && warning.contains("formula")),
            "expected a missing formula warning, got {warnings:?}"
        );
        assert!(
            warnings.iter().any(|warning| warning.contains("#DIV/0!")),
            "expected a cell error warning, got {warnings:?}"
        );
    }

    #[test]
    fn sample_cells_keep_their_addresses() {
        let mut range = Range::new((0, 0), (2, 1));
        range.set_value((0, 0), Data::String("Item".into()));
        range.set_value((0, 1), Data::String("Amount".into()));
        range.set_value((1, 0), Data::String("first".into()));
        range.set_value((1, 1), Data::Float(12.5));

        let (summary, _) = summarize("Budget", &range, None);
        let summary = summary.expect("sheet is within limits");
        assert_eq!(summary.sample[1][1], "B2: 12.5");
        assert_eq!(cell_address(16, 3), "D17");
        assert_eq!(cell_address(0, 26), "AA1");
    }

    #[test]
    fn row_cap_skips_oversized_sheets() {
        let range = Range::new((0, 0), (2, 1));
        let mut warnings = WarningSink::default();
        let limits = WorkbookLimits {
            max_rows: 2,
            ..WorkbookLimits::default()
        };
        let summary = analyze_range("Huge", &range, None, &limits, &mut warnings);
        assert!(summary.is_none());
        let warnings = warnings.finish();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("exceeds") && warning.contains("row")),
            "expected a row cap warning, got {warnings:?}"
        );
    }

    #[test]
    fn oversized_files_are_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("book.xlsx");
        std::fs::write(&path, b"not really a workbook").expect("write fixture");
        let limits = WorkbookLimits {
            max_bytes: 4,
            ..WorkbookLimits::default()
        };
        let error = analyze_workbook(&path, &limits).expect_err("oversized file must fail");
        assert!(error.to_string().contains("byte limit"), "{error}");
    }

    #[test]
    fn ambiguous_thousands_are_flagged() {
        let mut range = Range::new((0, 0), (1, 1));
        range.set_value((0, 0), Data::String("Item".into()));
        range.set_value((0, 1), Data::String("Amount".into()));
        range.set_value((1, 1), Data::String("1,234".into()));

        let (summary, warnings) = summarize("Budget", &range, None);
        let summary = summary.expect("sheet is within limits");
        let total = summary.numeric_totals.get("Amount").expect("amount total");
        assert_eq!(total.sum, decimal("1234"));
        assert!(
            warnings.iter().any(|warning| warning.contains("ambiguous")),
            "expected an ambiguous number warning, got {warnings:?}"
        );
    }

    #[test]
    fn parses_european_and_symbol_amounts() {
        let signals = detect_signals("1 234,56 €");
        let quantity = parse_quantity("1 234,56 €", &signals).expect("quantity");
        assert_eq!(quantity.amount, decimal("1234.56"));
        let signals = detect_signals("$1,234.56");
        assert!(signals.currencies.contains("USD"));
        let quantity = parse_quantity("$1,234.56", &signals).expect("quantity");
        assert_eq!(quantity.amount, decimal("1234.56"));
        let signals = detect_signals("3,5h");
        assert!(signals.units.contains("hours"));
        let quantity = parse_quantity("3,5h", &signals).expect("quantity");
        assert_eq!(quantity.amount, decimal("3.5"));
    }

    #[test]
    fn timesheet_totals_are_exact() {
        let input = "\
Person\tProject\tHours
Alice\tAlpha\t7.5
Bob\tBeta\t0.1
Bob\tBeta\t0.2
";
        let summary = parse_timesheet_rows(input).expect("valid timesheet");
        assert_eq!(summary.entries, 3);
        assert_eq!(summary.total, decimal("7.8"));
        assert_eq!(summary.per_project.get("Alpha"), Some(&decimal("7.5")));
        assert_eq!(summary.per_project.get("Beta"), Some(&decimal("0.3")));
        assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
    }

    #[test]
    fn timesheet_rejects_negative_and_ambiguous_units() {
        let negative = "Person\tProject\tHours\nAlice\tAlpha\t-2\n";
        assert!(parse_timesheet_rows(negative).is_err());

        let ambiguous = "Person\tProject\tTime\nAlice\tAlpha\t8\n";
        assert!(parse_timesheet_rows(ambiguous).is_err());

        let malformed = "Person\tProject\tHours\nAlice\tAlpha\tnot-a-number\n";
        assert!(parse_timesheet_rows(malformed).is_err());

        let mixed = "Person\tProject\tHours\nAlice\tAlpha\t8\nBob\tBeta\t2 days\n";
        assert!(parse_timesheet_rows(mixed).is_err());
    }

    #[test]
    fn timesheet_validates_declared_totals() {
        let input = "\
Person;Project;Hours
Alice;Alpha;1,5
Bob;Beta;2,5
Total;;3,0
";
        let summary = read_timesheet_csv(input).expect("valid timesheet");
        assert_eq!(summary.total, decimal("4.0"));
        assert!(
            summary
                .warnings
                .iter()
                .any(|warning| warning.contains("does not match")),
            "{:?}",
            summary.warnings
        );
    }

    #[test]
    fn timesheet_row_cap() {
        let mut input = String::from("Person\tProject\tHours\n");
        for _ in 0..=MAX_TIMESHEET_ROWS {
            input.push_str("Person\tAlpha\t1\n");
        }
        let error = parse_timesheet_rows(&input).expect_err("row cap must reject");
        assert!(error.to_string().contains("row limit"), "{error}");
    }
}
