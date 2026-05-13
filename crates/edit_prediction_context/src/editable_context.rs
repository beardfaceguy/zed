use collections::HashMap;
use gpui::{App, AsyncApp, Entity, EntityId};
use language::{Buffer, Point, ToPoint as _};
use project::Project;
use std::{ops::Range, path::Path, sync::Arc};
use text::Anchor;
use zeta_prompt::{RelatedExcerpt, RelatedFile};

/// This module contains collectors for editable context:
/// excerpts in files that are likely to be edited.

const CURSOR_CONTEXT_LINE_COUNT: u32 = 20;

#[derive(Clone)]
pub struct EditHistoryContextEntry {
    pub buffer: Entity<Buffer>,
    pub edited_range: Range<Anchor>,
}

pub async fn collect_editable_context(
    project: Entity<Project>,
    active_buffer: Entity<Buffer>,
    cursor_position: Anchor,
    edit_history: Vec<EditHistoryContextEntry>,
    cx: &mut AsyncApp,
) -> anyhow::Result<Vec<RelatedFile>> {
    let mut ranges_by_buffer: HashMap<EntityId, (Entity<Buffer>, Vec<(Range<Anchor>, usize)>)> =
        HashMap::default();

    let cursor_range = active_buffer.read_with(cx, |buffer, _cx| {
        let snapshot = buffer.snapshot();
        let cursor_point = cursor_position.to_point(&snapshot);
        let start_row = cursor_point.row.saturating_sub(CURSOR_CONTEXT_LINE_COUNT);
        let end_row = (cursor_point.row + CURSOR_CONTEXT_LINE_COUNT).min(snapshot.max_point().row);
        let start = snapshot.anchor_before(Point::new(start_row, 0));
        let end = snapshot.anchor_after(Point::new(end_row, snapshot.line_len(end_row)));
        start..end
    });
    ranges_by_buffer
        .entry(active_buffer.entity_id())
        .or_insert_with(|| (active_buffer.clone(), Vec::new()))
        .1
        .push((cursor_range, 0));

    for (index, entry) in edit_history.into_iter().enumerate() {
        ranges_by_buffer
            .entry(entry.buffer.entity_id())
            .or_insert_with(|| (entry.buffer.clone(), Vec::new()))
            .1
            .push((entry.edited_range, index + 1));
    }

    Ok(cx.update(|cx| {
        let project = project.read(cx);
        let mut related_files = ranges_by_buffer
            .into_values()
            .filter_map(|(buffer, ranges)| related_file_for_ranges(&project, &buffer, ranges, cx))
            .collect::<Vec<_>>();
        related_files.sort_by_key(|file| {
            file.excerpts
                .iter()
                .map(|excerpt| excerpt.order)
                .min()
                .unwrap_or(usize::MAX)
        });
        related_files
    }))
}

fn related_file_for_ranges(
    project: &Project,
    buffer: &Entity<Buffer>,
    ranges: Vec<(Range<Anchor>, usize)>,
    cx: &App,
) -> Option<RelatedFile> {
    let buffer = buffer.read(cx);
    let snapshot = buffer.snapshot();
    let file = snapshot.file()?;
    let worktree = project.worktree_for_id(file.worktree_id(cx), cx)?;
    let path: Arc<Path> = Path::new(&format!(
        "{}/{}",
        worktree.read(cx).root_name().as_unix_str(),
        file.path().as_unix_str()
    ))
    .into();

    let mut excerpts = ranges
        .into_iter()
        .filter_map(|(range, order)| {
            let start = range.start.to_point(&snapshot);
            let end = range.end.to_point(&snapshot);
            if start >= end {
                return None;
            }
            Some(RelatedExcerpt {
                row_range: start.row..end.row,
                text: snapshot
                    .text_for_range(start..end)
                    .collect::<String>()
                    .into(),
                order,
            })
        })
        .collect::<Vec<_>>();
    excerpts.sort_by_key(|excerpt| excerpt.order);

    Some(RelatedFile {
        path,
        max_row: snapshot.max_point().row,
        excerpts,
        in_open_source_repo: false,
    })
}
