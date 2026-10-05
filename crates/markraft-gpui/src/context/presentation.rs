//! Native lookup redraws the reading text, so its metrics must come from paint.

use super::*;

impl EditorView {
    /// Snapshot the reading text and its painted fonts after the context selection
    /// has painted. Unpainted content inherits the first visible character's font.
    pub fn context_text_presentation(
        &self,
        request: &ContextRequest,
        cx: &App,
    ) -> Option<ContextTextPresentation> {
        let mapped = self.context_text_mapping(request)?;
        let presentation = |source: &Option<Range<usize>>| {
            let (row, offset) = self.row_at(source.as_ref()?.start)?;
            row.text_presentation_at(offset, cx.text_system())
        };
        let first = mapped.source.first().and_then(presentation).or_else(|| {
            // Atom labels have no literal source mapping. Their painted slot
            // still supplies a baseline; do not jump to a later literal word.
            let (row, offset) = self.row_at(request.text_range().start)?;
            row.text_presentation_at(offset, cx.text_system())
        })?;
        let fallback = gpui::font(self.style().font_family.clone());
        let mut runs: Vec<ContextFontRun> = Vec::new();
        let mut offset = 0;
        for (ch, source) in mapped.text.chars().zip(&mapped.source) {
            let painted = presentation(source);
            let painted = painted.as_ref().unwrap_or(&first);
            let font = painted.font.as_ref().unwrap_or(&fallback);
            let end = offset + ch.len_utf16();
            if let Some(previous) = runs.last_mut()
                && previous.font == *font
                && previous.font_size == painted.font_size
            {
                previous.range.end = end;
            } else {
                runs.push(ContextFontRun {
                    range: offset..end,
                    font: font.clone(),
                    font_size: painted.font_size,
                });
            }
            offset = end;
        }
        Some(ContextTextPresentation {
            text: mapped.text,
            baseline: first.baseline,
            runs,
        })
    }
}
