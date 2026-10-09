use raikiri_html::{PaintEvent, PositionedGlyphRun, RunSource};
use std::collections::HashSet;

pub(super) fn supported(events: &[PaintEvent<'_>], runs: &[PositionedGlyphRun<'_>]) -> bool {
    let mut text_nodes = HashSet::new();
    let mut text_lines = HashSet::new();
    let run_lines: HashSet<_> = runs.iter().map(|run| run.line).collect();
    for event in events {
        match event {
            PaintEvent::Text(fragment) => {
                if !text_nodes.insert(fragment.node()) {
                    return false;
                }
            }
            PaintEvent::TextLine(line) => {
                if !run_lines.contains(line) || !text_lines.insert(*line) {
                    return false;
                }
            }
            PaintEvent::PushOpacity(alpha) => {
                if !alpha.is_finite() || !(0.0..=1.0).contains(alpha) {
                    return false;
                }
            }
            PaintEvent::Box(_)
            | PaintEvent::Replaced(_)
            | PaintEvent::MarkerImage(_)
            | PaintEvent::PushClip(_, _)
            | PaintEvent::PopClip
            | PaintEvent::PopOpacity => {}
            _ => return false,
        }
    }
    // Combining the two text streams could draw the same glyph twice.
    if !text_nodes.is_empty() && !text_lines.is_empty() {
        return false;
    }
    runs.iter().all(|run| match run.source {
        RunSource::Text(node) => text_lines.contains(&run.line) || text_nodes.contains(&node),
        RunSource::Generated(_, _) | RunSource::Ellipsis(_) => text_lines.contains(&run.line),
        _ => false,
    })
}

#[cfg(test)]
mod tests;
