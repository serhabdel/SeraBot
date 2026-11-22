//! Visual page geometry extraction for PDF previews (v0.44.0).
//!
//! Decodes each page's `/MediaBox`/`/CropBox` and content stream via `lopdf`
//! to extract top-left SVG-ready rectangles, background fills, and positioned
//! text blocks. Malformed or encrypted PDFs degrade gracefully without panicking.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use lopdf::{Dictionary, Document, Object, ObjectId};
use serde_json::{Value, json};

/// Maximum pages extracted for visual preview per PDF.
const MAX_PDF_PREVIEW_PAGES: usize = 30;
/// Maximum rectangles extracted per PDF page.
const MAX_RECTS_PER_PAGE: usize = 80;
/// Maximum text blocks extracted per PDF page.
const MAX_TEXT_BLOCKS_PER_PAGE: usize = 120;

#[derive(Clone)]
struct GState {
    ctm: [f64; 6],
    fill_hex: Option<String>,
    stroke_hex: Option<String>,
    font_size: f64,
    bold: bool,
}

impl Default for GState {
    fn default() -> Self {
        Self {
            ctm: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            fill_hex: Some("#000000".to_owned()),
            stroke_hex: Some("#000000".to_owned()),
            font_size: 12.0,
            bold: false,
        }
    }
}

/// Extract up to [`MAX_PDF_PREVIEW_PAGES`] visual page descriptors from `bytes`.
pub(super) fn extract_pdf_pages(bytes: &[u8]) -> Vec<Value> {
    let Ok(Ok(doc)) = catch_unwind(AssertUnwindSafe(|| Document::load_mem(bytes))) else {
        return Vec::new();
    };
    if catch_unwind(AssertUnwindSafe(|| doc.is_encrypted())).unwrap_or(true) {
        return Vec::new();
    }
    let Ok(pages) = catch_unwind(AssertUnwindSafe(|| doc.get_pages())) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(pages.len().min(MAX_PDF_PREVIEW_PAGES));
    for (page_num, page_id) in pages.into_iter().take(MAX_PDF_PREVIEW_PAGES) {
        out.push(extract_single_page(&doc, page_num, page_id));
    }
    out
}

fn extract_single_page(doc: &Document, page_num: u32, page_id: ObjectId) -> Value {
    let (media_x0, media_y0, width_pt, height_pt) = page_box(doc, page_id);
    let page_fonts = collect_page_fonts(doc, page_id);

    let mut background_hex: Option<String> = None;
    let mut rects: Vec<Value> = Vec::new();
    let mut text_blocks: Vec<Value> = Vec::new();
    let mut fallback_text_parts: Vec<String> = Vec::new();

    if let Ok(Ok(content_bytes)) = catch_unwind(AssertUnwindSafe(|| doc.get_page_content(page_id)))
        && let Ok(Ok(content)) = catch_unwind(AssertUnwindSafe(|| {
            lopdf::content::Content::decode(&content_bytes)
        }))
    {
        let mut stack: Vec<GState> = Vec::new();
        let mut state = GState::default();
        let mut tm = [1.0_f64, 0.0, 0.0, 1.0, 0.0, 0.0];
        let mut tlm = tm;
        let mut leading = 0.0_f64;
        let mut pending_rects: Vec<(f64, f64, f64, f64)> = Vec::new();
        let mut poly_pts: Vec<(f64, f64)> = Vec::new();

        for op in content.operations {
            match op.operator.as_str() {
                "q" if stack.len() < 64 => {
                    stack.push(state.clone());
                }
                "Q" => {
                    if let Some(prev) = stack.pop() {
                        state = prev;
                    }
                }
                "cm" => {
                    if let Some(m) = nums6(&op.operands) {
                        state.ctm = mat_mul(state.ctm, m);
                    }
                }
                "rg" => {
                    if let Some(hex) = rgb_from_ops(&op.operands) {
                        state.fill_hex = Some(hex);
                    }
                }
                "RG" => {
                    if let Some(hex) = rgb_from_ops(&op.operands) {
                        state.stroke_hex = Some(hex);
                    }
                }
                "g" => {
                    if let Some(gray) = op_f64(&op.operands, 0) {
                        state.fill_hex = Some(rgb_to_hex(gray, gray, gray));
                    }
                }
                "G" => {
                    if let Some(gray) = op_f64(&op.operands, 0) {
                        state.stroke_hex = Some(rgb_to_hex(gray, gray, gray));
                    }
                }
                "k" => {
                    if let Some(hex) = cmyk_from_ops(&op.operands) {
                        state.fill_hex = Some(hex);
                    }
                }
                "K" => {
                    if let Some(hex) = cmyk_from_ops(&op.operands) {
                        state.stroke_hex = Some(hex);
                    }
                }
                "sc" | "scn" => {
                    if let Some(hex) = generic_color_from_ops(&op.operands) {
                        state.fill_hex = Some(hex);
                    }
                }
                "SC" | "SCN" => {
                    if let Some(hex) = generic_color_from_ops(&op.operands) {
                        state.stroke_hex = Some(hex);
                    }
                }
                "re" => {
                    if let (Some(x), Some(y), Some(w), Some(h)) = (
                        op_f64(&op.operands, 0),
                        op_f64(&op.operands, 1),
                        op_f64(&op.operands, 2),
                        op_f64(&op.operands, 3),
                    ) {
                        pending_rects.push((x, y, w, h));
                    }
                }
                "m" => {
                    if let (Some(x), Some(y)) = (op_f64(&op.operands, 0), op_f64(&op.operands, 1)) {
                        poly_pts.clear();
                        poly_pts.push((x, y));
                    }
                }
                "l" => {
                    if let (Some(x), Some(y)) = (op_f64(&op.operands, 0), op_f64(&op.operands, 1)) {
                        poly_pts.push((x, y));
                    }
                }
                "h" => {
                    if let Some(rect) = poly_to_axis_rect(&poly_pts) {
                        pending_rects.push(rect);
                    }
                    poly_pts.clear();
                }
                "f" | "F" | "f*" => {
                    if let Some(rect) = poly_to_axis_rect(&poly_pts) {
                        pending_rects.push(rect);
                    }
                    poly_pts.clear();
                    emit_rects(
                        &pending_rects,
                        state.ctm,
                        media_x0,
                        media_y0,
                        width_pt,
                        height_pt,
                        state.fill_hex.as_deref(),
                        None,
                        &mut background_hex,
                        &mut rects,
                    );
                    pending_rects.clear();
                }
                "S" | "s" => {
                    if let Some(rect) = poly_to_axis_rect(&poly_pts) {
                        pending_rects.push(rect);
                    }
                    poly_pts.clear();
                    emit_rects(
                        &pending_rects,
                        state.ctm,
                        media_x0,
                        media_y0,
                        width_pt,
                        height_pt,
                        None,
                        state.stroke_hex.as_deref(),
                        &mut background_hex,
                        &mut rects,
                    );
                    pending_rects.clear();
                }
                "B" | "B*" | "b" | "b*" => {
                    if let Some(rect) = poly_to_axis_rect(&poly_pts) {
                        pending_rects.push(rect);
                    }
                    poly_pts.clear();
                    emit_rects(
                        &pending_rects,
                        state.ctm,
                        media_x0,
                        media_y0,
                        width_pt,
                        height_pt,
                        state.fill_hex.as_deref(),
                        state.stroke_hex.as_deref(),
                        &mut background_hex,
                        &mut rects,
                    );
                    pending_rects.clear();
                }
                "n" => {
                    pending_rects.clear();
                    poly_pts.clear();
                }
                "BT" => {
                    tm = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                    tlm = tm;
                }
                "Tf" => {
                    if let Some(Object::Name(name)) = op.operands.first() {
                        let key = String::from_utf8_lossy(name);
                        let resolved = page_fonts.get(key.as_ref()).map(String::as_str);
                        state.bold = is_bold_font_name(resolved.unwrap_or(key.as_ref()));
                    }
                    if let Some(size) = op_f64(&op.operands, 1) {
                        state.font_size = size.abs().max(1.0);
                    }
                }
                "TL" => {
                    if let Some(l) = op_f64(&op.operands, 0) {
                        leading = l;
                    }
                }
                "Tm" => {
                    if let Some(m) = nums6(&op.operands) {
                        tm = m;
                        tlm = m;
                    }
                }
                "Td" => {
                    if let (Some(tx), Some(ty)) = (op_f64(&op.operands, 0), op_f64(&op.operands, 1))
                    {
                        tlm = mat_mul(tlm, [1.0, 0.0, 0.0, 1.0, tx, ty]);
                        tm = tlm;
                    }
                }
                "TD" => {
                    if let (Some(tx), Some(ty)) = (op_f64(&op.operands, 0), op_f64(&op.operands, 1))
                    {
                        leading = -ty;
                        tlm = mat_mul(tlm, [1.0, 0.0, 0.0, 1.0, tx, ty]);
                        tm = tlm;
                    }
                }
                "T*" => {
                    tlm = mat_mul(tlm, [1.0, 0.0, 0.0, 1.0, 0.0, -leading]);
                    tm = tlm;
                }
                "Tj" | "'" | "\"" => {
                    if op.operator == "'" || op.operator == "\"" {
                        tlm = mat_mul(tlm, [1.0, 0.0, 0.0, 1.0, 0.0, -leading]);
                        tm = tlm;
                    }
                    let mut text = String::new();
                    for operand in &op.operands {
                        if let Object::String(raw, _) = operand {
                            text.push_str(&decode_pdf_string(raw));
                        }
                    }
                    push_text_block(
                        &text,
                        state.ctm,
                        tm,
                        &state,
                        media_x0,
                        media_y0,
                        height_pt,
                        &mut text_blocks,
                        &mut fallback_text_parts,
                    );
                }
                "TJ" => {
                    if let Some(Object::Array(items)) = op.operands.first() {
                        let mut text = String::new();
                        for item in items {
                            if let Object::String(raw, _) = item {
                                text.push_str(&decode_pdf_string(raw));
                            }
                        }
                        push_text_block(
                            &text,
                            state.ctm,
                            tm,
                            &state,
                            media_x0,
                            media_y0,
                            height_pt,
                            &mut text_blocks,
                            &mut fallback_text_parts,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    let extracted = catch_unwind(AssertUnwindSafe(|| doc.extract_text(&[page_num])))
        .ok()
        .and_then(|res| res.ok())
        .unwrap_or_default();
    let page_text = if !extracted.trim().is_empty() {
        extracted.trim().to_owned()
    } else {
        fallback_text_parts.join(" ")
    };

    json!({
        "page": page_num,
        "width_pt": round2(width_pt),
        "height_pt": round2(height_pt),
        "text": page_text,
        "background_hex": background_hex,
        "rects": rects,
        "text_blocks": text_blocks,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_rects(
    pending: &[(f64, f64, f64, f64)],
    ctm: [f64; 6],
    media_x0: f64,
    media_y0: f64,
    page_w: f64,
    page_h: f64,
    fill_hex: Option<&str>,
    stroke_hex: Option<&str>,
    background_hex: &mut Option<String>,
    rects: &mut Vec<Value>,
) {
    let page_area = (page_w * page_h).max(1.0);
    for &(x, y, w, h) in pending {
        let (x0, y0) = transform_pt(ctm, x, y);
        let (x1, y1) = transform_pt(ctm, x + w, y + h);
        let rx = x0.min(x1) - media_x0;
        let ry_pdf = y0.min(y1) - media_y0;
        let rw = (x1 - x0).abs();
        let rh = (y1 - y0).abs();
        if rw <= 0.1 || rh <= 0.1 {
            continue;
        }
        if background_hex.is_none()
            && let Some(fill) = fill_hex
            && (rw * rh) / page_area >= 0.85
        {
            *background_hex = Some(fill.to_owned());
        }
        if rects.len() >= MAX_RECTS_PER_PAGE {
            continue;
        }
        let top_y = page_h - ry_pdf - rh;
        rects.push(json!({
            "x_pt": round2(rx),
            "y_pt": round2(top_y),
            "w_pt": round2(rw),
            "h_pt": round2(rh),
            "fill_hex": fill_hex,
            "stroke_hex": stroke_hex,
        }));
    }
}

#[allow(clippy::too_many_arguments)]
fn push_text_block(
    raw_text: &str,
    ctm: [f64; 6],
    tm: [f64; 6],
    state: &GState,
    media_x0: f64,
    media_y0: f64,
    page_h: f64,
    text_blocks: &mut Vec<Value>,
    fallback_parts: &mut Vec<String>,
) {
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        return;
    }
    fallback_parts.push(trimmed.to_owned());
    if text_blocks.len() >= MAX_TEXT_BLOCKS_PER_PAGE {
        return;
    }
    let combined = mat_mul(ctm, tm);
    let (px, py) = transform_pt(combined, 0.0, 0.0);
    let tm_scale = (tm[0] * tm[0] + tm[1] * tm[1])
        .sqrt()
        .max((tm[2] * tm[2] + tm[3] * tm[3]).sqrt())
        .max(0.01);
    let ctm_scale = (ctm[0] * ctm[0] + ctm[1] * ctm[1])
        .sqrt()
        .max((ctm[2] * ctm[2] + ctm[3] * ctm[3]).sqrt())
        .max(0.01);
    let font_size = (state.font_size * tm_scale * ctm_scale).clamp(4.0, 200.0);
    let x_pt = px - media_x0;
    let y_pt = page_h - (py - media_y0);
    text_blocks.push(json!({
        "x_pt": round2(x_pt),
        "y_pt": round2(y_pt),
        "font_size": round2(font_size),
        "color_hex": state.fill_hex.as_deref().unwrap_or("#000000"),
        "bold": state.bold,
        "text": trimmed,
    }));
}

fn page_box(doc: &Document, page_id: ObjectId) -> (f64, f64, f64, f64) {
    let mut cur = Some(page_id);
    for _ in 0..16 {
        let Some(id) = cur else { break };
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        for key in [b"CropBox".as_slice(), b"MediaBox".as_slice()] {
            if let Ok(obj) = dict.get(key)
                && let Some(arr) = resolve_array(doc, obj)
                && arr.len() >= 4
                && let (Some(x0), Some(y0), Some(x1), Some(y1)) = (
                    obj_f64(&arr[0]),
                    obj_f64(&arr[1]),
                    obj_f64(&arr[2]),
                    obj_f64(&arr[3]),
                )
            {
                let w = (x1 - x0).abs();
                let h = (y1 - y0).abs();
                if w > 1.0 && h > 1.0 {
                    return (x0.min(x1), y0.min(y1), w, h);
                }
            }
        }
        cur = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    (0.0, 0.0, 595.0, 842.0)
}

fn collect_page_fonts(doc: &Document, page_id: ObjectId) -> HashMap<String, String> {
    let mut fonts = HashMap::new();
    let mut cur = Some(page_id);
    for _ in 0..16 {
        let Some(id) = cur else { break };
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        if let Ok(res_obj) = dict.get(b"Resources")
            && let Some(res_dict) = resolve_dict(doc, res_obj)
            && let Ok(font_obj) = res_dict.get(b"Font")
            && let Some(font_dict) = resolve_dict(doc, font_obj)
        {
            for (k, v) in font_dict.iter() {
                let key = String::from_utf8_lossy(k).into_owned();
                if fonts.contains_key(&key) {
                    continue;
                }
                if let Some(fd) = resolve_dict(doc, v)
                    && let Ok(Object::Name(base)) = fd.get(b"BaseFont")
                {
                    fonts.insert(key, String::from_utf8_lossy(base).into_owned());
                }
            }
        }
        cur = dict.get(b"Parent").ok().and_then(|o| o.as_reference().ok());
    }
    fonts
}

fn resolve_array<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Vec<Object>> {
    match obj {
        Object::Array(arr) => Some(arr),
        Object::Reference(id) => doc.get_object(*id).ok().and_then(|o| o.as_array().ok()),
        _ => None,
    }
}

fn resolve_dict<'a>(doc: &'a Document, obj: &'a Object) -> Option<&'a Dictionary> {
    match obj {
        Object::Dictionary(d) => Some(d),
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        _ => None,
    }
}

fn is_bold_font_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("bold") || lower.contains("semibold") || lower.contains("black")
}

fn decode_pdf_string(raw: &[u8]) -> String {
    let decoded = if raw.starts_with(&[0xFE, 0xFF]) && raw.len() >= 4 {
        let units: Vec<u16> = raw[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&[hi, lo]| u16::from_be_bytes([hi, lo]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        std::str::from_utf8(raw)
            .map(str::to_owned)
            .unwrap_or_else(|_| raw.iter().map(|&b| char::from(b)).collect())
    };
    decoded
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .collect()
}

fn poly_to_axis_rect(pts: &[(f64, f64)]) -> Option<(f64, f64, f64, f64)> {
    if pts.len() < 4 || pts.len() > 5 {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for &(x, y) in pts {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let w = max_x - min_x;
    let h = max_y - min_y;
    (w > 0.1 && h > 0.1).then_some((min_x, min_y, w, h))
}

fn obj_f64(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(f) => Some(f64::from(*f)),
        _ => None,
    }
}

fn op_f64(operands: &[Object], idx: usize) -> Option<f64> {
    operands.get(idx).and_then(obj_f64)
}

fn nums6(operands: &[Object]) -> Option<[f64; 6]> {
    Some([
        op_f64(operands, 0)?,
        op_f64(operands, 1)?,
        op_f64(operands, 2)?,
        op_f64(operands, 3)?,
        op_f64(operands, 4)?,
        op_f64(operands, 5)?,
    ])
}

fn mat_mul(outer: [f64; 6], inner: [f64; 6]) -> [f64; 6] {
    [
        outer[0] * inner[0] + outer[2] * inner[1],
        outer[1] * inner[0] + outer[3] * inner[1],
        outer[0] * inner[2] + outer[2] * inner[3],
        outer[1] * inner[2] + outer[3] * inner[3],
        outer[0] * inner[4] + outer[2] * inner[5] + outer[4],
        outer[1] * inner[4] + outer[3] * inner[5] + outer[5],
    ]
}

fn transform_pt(m: [f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn rgb_from_ops(operands: &[Object]) -> Option<String> {
    Some(rgb_to_hex(
        op_f64(operands, 0)?,
        op_f64(operands, 1)?,
        op_f64(operands, 2)?,
    ))
}

fn cmyk_from_ops(operands: &[Object]) -> Option<String> {
    let c = op_f64(operands, 0)?.clamp(0.0, 1.0);
    let m = op_f64(operands, 1)?.clamp(0.0, 1.0);
    let y = op_f64(operands, 2)?.clamp(0.0, 1.0);
    let k = op_f64(operands, 3)?.clamp(0.0, 1.0);
    Some(rgb_to_hex(
        (1.0 - c) * (1.0 - k),
        (1.0 - m) * (1.0 - k),
        (1.0 - y) * (1.0 - k),
    ))
}

fn generic_color_from_ops(operands: &[Object]) -> Option<String> {
    let nums: Vec<f64> = operands.iter().filter_map(obj_f64).collect();
    match nums.as_slice() {
        [g] => Some(rgb_to_hex(*g, *g, *g)),
        [r, g, b] => Some(rgb_to_hex(*r, *g, *b)),
        [c, m, y, k] => {
            let r = (1.0 - c.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
            let g = (1.0 - m.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
            let b = (1.0 - y.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
            Some(rgb_to_hex(r, g, b))
        }
        _ => None,
    }
}

fn rgb_to_hex(r: f64, g: f64, b: f64) -> String {
    let to_u8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", to_u8(r), to_u8(g), to_u8(b))
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
