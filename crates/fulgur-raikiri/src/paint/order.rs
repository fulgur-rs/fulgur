use raikiri_html::{PaintEvent, PositionedGlyphRun, RunSource};
use std::collections::HashSet;

pub(super) fn supported(events: &[PaintEvent<'_>], runs: &[PositionedGlyphRun<'_>]) -> bool {
    let mut text_nodes = HashSet::new();
    for event in events {
        match event {
            PaintEvent::Text(fragment) => {
                if !text_nodes.insert(fragment.node()) {
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
            | PaintEvent::PushClip(_, _)
            | PaintEvent::PopClip
            | PaintEvent::PopOpacity => {}
            _ => return false,
        }
    }
    runs.iter().all(|run| match run.source {
        RunSource::Text(node) => text_nodes.contains(&node),
        _ => false,
    })
}

#[cfg(test)]
mod tests;
