use serde::{Deserialize, Serialize};

use crate::error::{ServiceError, ServiceErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPage {
    pub top: u32,
    pub height: u32,
}

/// Plans contiguous non-overlapping pages of up to 3200 CSS pixels each,
/// preferring candidate cut points in `[top + 1600, top + 3200]`.
pub fn export_pages(height: i64, candidates: &[f64]) -> Result<Vec<ExportPage>, ServiceError> {
    if height <= 0 {
        return Err(ServiceError::new(
            ServiceErrorKind::InvalidInput,
            "export_layout_invalid",
            "导出文档没有有效高度",
        ));
    }
    if height > 100_000 {
        return Err(ServiceError::new(
            ServiceErrorKind::TooLarge,
            "export_too_tall",
            "图片超过导出高度限制，请缩短当前分支或改用 HTML。",
        ));
    }

    let height_u32 = height as u32;
    let mut cuts: Vec<u32> = candidates
        .iter()
        .copied()
        .filter(|v| v.is_finite() && v.fract() == 0.0 && *v > 0.0 && (*v as i64) < height)
        .map(|v| v as u32)
        .collect();
    cuts.sort_unstable();

    let mut pages = Vec::new();
    let mut top = 0u32;
    let mut index = 0usize;
    while top < height_u32 {
        let target = (top + 3200).min(height_u32);
        let mut cut = target;
        if target < height_u32 {
            while index < cuts.len() && cuts[index] <= target {
                if cuts[index] >= top + 1600 {
                    cut = cuts[index];
                }
                index += 1;
            }
        }
        pages.push(ExportPage {
            top,
            height: cut - top,
        });
        top = cut;
    }

    Ok(pages)
}
