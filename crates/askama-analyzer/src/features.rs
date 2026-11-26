//! What rust-analyzer answers inside templates: hover, definitions, references, renames and
//! completion. Each runs on the lane, against the analysis as it stands when the request
//! arrives, and its answer is mapped back into template positions before it leaves.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama_analyzer_codegen::Origin;
use async_lsp::{LanguageServer, ServerSocket};
use lsp_types::{
    CompletionItem, CompletionList, CompletionParams, CompletionResponse, CompletionTextEdit,
    DocumentChangeOperation, DocumentChanges, GotoDefinitionParams, GotoDefinitionResponse, Hover,
    HoverParams, Location, OneOf, Position, PrepareRenameResponse, ReferenceParams, RenameParams,
    TextDocumentIdentifier, TextDocumentPositionParams, TextEdit, Url, WorkspaceEdit,
};
use tokio::sync::oneshot;

use crate::complete;
use crate::ra::READY;
use crate::text::Lines;
use crate::workspace::{Asked, Workspace};

/// Inserted at the cursor for a completion, so the template parses while an expression is
/// half typed (`{{ user. }}`), then removed from the generated code before rust-analyzer is
/// asked. An identifier no template would use.
const MARKER: &str = "__askama_analyzer_cursor";

/// A request the lane answers, with where the answer goes.
pub enum Query {
    Hover(HoverParams, oneshot::Sender<Option<Hover>>),
    Definition(
        GotoDefinitionParams,
        oneshot::Sender<Option<GotoDefinitionResponse>>,
    ),
    References(ReferenceParams, oneshot::Sender<Option<Vec<Location>>>),
    PrepareRename(
        TextDocumentPositionParams,
        oneshot::Sender<Option<PrepareRenameResponse>>,
    ),
    Rename(RenameParams, oneshot::Sender<Option<WorkspaceEdit>>),
    Completion(
        CompletionParams,
        oneshot::Sender<Option<CompletionResponse>>,
    ),
}

impl Query {
    /// The document the request is about.
    pub fn uri(&self) -> &Url {
        match self {
            Self::Hover(params, _) => &params.text_document_position_params.text_document.uri,
            Self::Definition(params, _) => &params.text_document_position_params.text_document.uri,
            Self::References(params, _) => &params.text_document_position.text_document.uri,
            Self::PrepareRename(params, _) => &params.text_document.uri,
            Self::Rename(params, _) => &params.text_document_position.text_document.uri,
            Self::Completion(params, _) => &params.text_document_position.text_document.uri,
        }
    }
}

/// Answers a query. A receiver that has gone away (the request was cancelled) is not an error.
pub async fn answer(workspaces: &mut [Workspace], query: Query) {
    match query {
        Query::Hover(params, reply) => {
            let _ = reply.send(hover(workspaces, params).await);
        }
        Query::Definition(params, reply) => {
            let _ = reply.send(definition(workspaces, params).await);
        }
        Query::References(params, reply) => {
            let _ = reply.send(references(workspaces, params).await);
        }
        Query::PrepareRename(params, reply) => {
            let _ = reply.send(prepare_rename(workspaces, params).await);
        }
        Query::Rename(params, reply) => {
            let _ = reply.send(rename(workspaces, params).await);
        }
        Query::Completion(params, reply) => {
            let _ = reply.send(completion(workspaces, params).await);
        }
    }
}

/// The workspace that reads a template, with the template's path, text and the offset asked
/// about.
fn locate<'w>(
    workspaces: &'w mut [Workspace],
    uri: &Url,
    position: Position,
) -> Option<(&'w mut Workspace, PathBuf, Arc<str>, usize)> {
    let path = uri.to_file_path().ok()?;
    let workspace = workspaces
        .iter_mut()
        .find(|workspace| workspace.analysis.owns(&path))?;
    let text = workspace.analysis.vfs.read(&path).ok()?;
    let offset = Lines::new(&text, workspace.encoding()).offset(position)?;
    Some((workspace, path, text, offset))
}

/// The session's connection once rust-analyzer has loaded, or after [`READY`] regardless.
async fn server(workspace: &mut Workspace) -> Option<ServerSocket> {
    let session = workspace.session()?;
    if !session.ready(READY).await {
        tracing::debug!("asking rust-analyzer before it has finished loading");
    }
    Some(session.server())
}

fn position_params(asked: &Asked) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
            uri: asked.uri.clone(),
        },
        position: asked.position,
    }
}

async fn hover(workspaces: &mut [Workspace], params: HoverParams) -> Option<Hover> {
    let at = params.text_document_position_params;
    let (workspace, path, _, offset) = locate(workspaces, &at.text_document.uri, at.position)?;
    let asked = workspace.ask(&path, offset)?;
    let hover = server(workspace)
        .await?
        .hover(HoverParams {
            text_document_position_params: position_params(&asked),
            work_done_progress_params: Default::default(),
        })
        .await
        .ok()??;
    let range = hover
        .range
        .and_then(|range| {
            workspace.user_location(&Location {
                uri: asked.uri.clone(),
                range,
            })
        })
        .filter(|location| location.uri == at.text_document.uri)
        .map(|location| location.range);
    Some(Hover {
        contents: hover.contents,
        range,
    })
}

async fn definition(
    workspaces: &mut [Workspace],
    params: GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let at = params.text_document_position_params;
    let (workspace, path, text, offset) = locate(workspaces, &at.text_document.uri, at.position)?;
    if let Some(location) = template_definition(workspace, &path, &text, offset) {
        return Some(GotoDefinitionResponse::Scalar(location));
    }
    let asked = workspace.ask(&path, offset)?;
    let response = server(workspace)
        .await?
        .definition(GotoDefinitionParams {
            text_document_position_params: position_params(&asked),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .ok()??;
    let locations = match response {
        GotoDefinitionResponse::Scalar(location) => vec![location],
        GotoDefinitionResponse::Array(locations) => locations,
        GotoDefinitionResponse::Link(links) => links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect(),
    };
    let mapped: Vec<Location> = locations
        .iter()
        .filter_map(|location| workspace.user_location(location))
        .collect();
    (!mapped.is_empty()).then_some(GotoDefinitionResponse::Array(mapped))
}

/// The definition of a template-level name at the offset: a path, a macro or a block.
fn template_definition(
    workspace: &Workspace,
    path: &Path,
    text: &str,
    offset: usize,
) -> Option<Location> {
    let known = workspace.analysis.templates();
    let read = |file: &Path| {
        workspace
            .analysis
            .vfs
            .read(file)
            .ok()
            .map(|text| text.to_string())
    };
    let target = crate::navigate::definition(path, text, offset, &known, &read)?;
    let target_text = workspace.analysis.vfs.read(&target.path).ok()?;
    Some(Location {
        uri: Url::from_file_path(&target.path).ok()?,
        range: Lines::new(&target_text, workspace.encoding()).range(&target.range),
    })
}

async fn references(
    workspaces: &mut [Workspace],
    params: ReferenceParams,
) -> Option<Vec<Location>> {
    let at = params.text_document_position;
    let (workspace, path, _, offset) = locate(workspaces, &at.text_document.uri, at.position)?;
    let asked = workspace.ask(&path, offset)?;
    let locations = server(workspace)
        .await?
        .references(ReferenceParams {
            text_document_position: position_params(&asked),
            context: params.context,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .ok()??;
    let mut mapped: Vec<Location> = Vec::new();
    for location in locations
        .iter()
        .filter_map(|location| workspace.user_location(location))
    {
        if !mapped.contains(&location) {
            mapped.push(location);
        }
    }
    Some(mapped)
}

async fn prepare_rename(
    workspaces: &mut [Workspace],
    params: TextDocumentPositionParams,
) -> Option<PrepareRenameResponse> {
    let (workspace, path, _, offset) =
        locate(workspaces, &params.text_document.uri, params.position)?;
    let asked = workspace.ask(&path, offset)?;
    let response = server(workspace)
        .await?
        .prepare_rename(position_params(&asked))
        .await
        .ok()??;
    let range = match response {
        PrepareRenameResponse::Range(range) => range,
        PrepareRenameResponse::RangeWithPlaceholder { range, .. } => range,
        PrepareRenameResponse::DefaultBehavior { .. } => return None,
    };
    let location = workspace.user_location(&Location {
        uri: asked.uri.clone(),
        range,
    })?;
    (location.uri == params.text_document.uri)
        .then_some(PrepareRenameResponse::Range(location.range))
}

async fn rename(workspaces: &mut [Workspace], params: RenameParams) -> Option<WorkspaceEdit> {
    let at = params.text_document_position;
    let (workspace, path, text, offset) = locate(workspaces, &at.text_document.uri, at.position)?;
    let old_name = word_at(&text, offset)?.to_owned();
    let asked = workspace.ask(&path, offset)?;
    let edit = server(workspace)
        .await?
        .rename(RenameParams {
            text_document_position: position_params(&asked),
            new_name: params.new_name,
            work_done_progress_params: Default::default(),
        })
        .await
        .ok()??;
    Some(user_edit(workspace, edit, &old_name))
}

/// A rename's edits in the user's files. An edit is kept only where the text it replaces is
/// the old name: generated code that askama wrote itself, with no template text behind it, can
/// map to a nearby template range, and must not be allowed to rewrite it.
fn user_edit(workspace: &Workspace, edit: WorkspaceEdit, old_name: &str) -> WorkspaceEdit {
    let mut by_file: Vec<(Url, Vec<TextEdit>)> = edit.changes.into_iter().flatten().collect();
    match edit.document_changes {
        Some(DocumentChanges::Edits(edits)) => {
            for edit in edits {
                by_file.push((
                    edit.text_document.uri,
                    edit.edits.into_iter().map(plain_edit).collect(),
                ));
            }
        }
        Some(DocumentChanges::Operations(operations)) => {
            for operation in operations {
                if let DocumentChangeOperation::Edit(edit) = operation {
                    by_file.push((
                        edit.text_document.uri,
                        edit.edits.into_iter().map(plain_edit).collect(),
                    ));
                }
            }
        }
        None => {}
    }

    let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for (uri, edits) in by_file {
        for edit in edits {
            let Some(location) = workspace.user_location(&Location {
                uri: uri.clone(),
                range: edit.range,
            }) else {
                continue;
            };
            if !replaces(workspace, &location, old_name) {
                tracing::debug!(
                    ?location,
                    "dropping a rename edit that does not replace the old name"
                );
                continue;
            }
            let edits = changes.entry(location.uri).or_default();
            if !edits.iter().any(|known| known.range == location.range) {
                edits.push(TextEdit {
                    range: location.range,
                    new_text: edit.new_text,
                });
            }
        }
    }
    WorkspaceEdit {
        changes: Some(changes),
        ..WorkspaceEdit::default()
    }
}

fn plain_edit(edit: OneOf<TextEdit, lsp_types::AnnotatedTextEdit>) -> TextEdit {
    match edit {
        OneOf::Left(edit) => edit,
        OneOf::Right(annotated) => annotated.text_edit,
    }
}

/// Whether the text at a location of the user's is `name`.
fn replaces(workspace: &Workspace, location: &Location, name: &str) -> bool {
    let Ok(path) = location.uri.to_file_path() else {
        return false;
    };
    let Ok(text) = workspace.analysis.vfs.read(&path) else {
        return false;
    };
    let lines = Lines::new(&text, workspace.encoding());
    let (Some(start), Some(end)) = (
        lines.offset(location.range.start),
        lines.offset(location.range.end),
    ) else {
        return false;
    };
    text.get(start..end) == Some(name)
}

/// The identifier around an offset.
fn word_at(text: &str, offset: usize) -> Option<&str> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_word(c))
        .last()
        .map_or(offset, |(at, _)| at);
    let end = offset
        + text[offset..]
            .char_indices()
            .find(|&(_, c)| !is_word(c))
            .map_or(text.len() - offset, |(at, _)| at);
    (start < end).then(|| &text[start..end])
}

/// Completion: tag keywords, template paths and askama's filters from the text alone, and
/// everything Rust from rust-analyzer.
async fn completion(
    workspaces: &mut [Workspace],
    params: CompletionParams,
) -> Option<CompletionResponse> {
    let at = params.text_document_position;
    let path = at.text_document.uri.to_file_path().ok()?;
    let index = workspaces
        .iter()
        .position(|workspace| workspace.analysis.owns(&path))
        .or_else(|| (!workspaces.is_empty()).then_some(0))?;
    let workspace = &mut workspaces[index];
    let text = workspace.analysis.vfs.read(&path).ok()?;
    let lines = Lines::new(&text, workspace.encoding());
    let offset = lines.offset(at.position)?;
    let replacing = |typed: &str| lines.range(&(offset - typed.len()..offset));

    let mut items: Vec<CompletionItem> = Vec::new();
    let mut incomplete = false;
    match complete::spot(&text, offset) {
        complete::Spot::Keyword { typed } => {
            items = complete::keywords(&text, offset);
            give_range(&mut items, replacing(typed));
        }
        complete::Spot::Path { typed } => {
            // The innermost package holding the template, whose askama.toml askama reads.
            let dirs: Vec<PathBuf> = workspace
                .analysis
                .packages
                .iter()
                .filter(|package| path.starts_with(&package.manifest_dir))
                .max_by_key(|package| package.manifest_dir.components().count())
                .map(|package| {
                    let config = workspace
                        .analysis
                        .vfs
                        .read(&package.manifest_dir.join("askama.toml"))
                        .ok();
                    complete::template_dirs(&package.manifest_dir, config.as_deref())
                })
                .unwrap_or_default();
            items = complete::paths(&dirs);
            give_range(&mut items, replacing(typed));
        }
        spot => {
            if let complete::Spot::Filter { typed } = spot {
                let mut filters = complete::filters();
                give_range(&mut filters, replacing(typed));
                items.extend(filters);
            }
            if workspace.analysis.owns(&path)
                && let Some((rust, more)) =
                    rust_completion(workspace, &path, &text, offset, params.context).await
            {
                items.extend(rust);
                incomplete = more;
            }
        }
    }
    Some(CompletionResponse::List(CompletionList {
        is_incomplete: incomplete,
        items,
    }))
}

/// Makes each item replace `range`: the part of a word already typed.
fn give_range(items: &mut [CompletionItem], range: lsp_types::Range) {
    for item in items {
        let new_text = item
            .insert_text
            .take()
            .unwrap_or_else(|| item.label.clone());
        item.text_edit = Some(CompletionTextEdit::Edit(TextEdit { range, new_text }));
    }
}

/// rust-analyzer's completions at a template offset. It is asked about a probe: the item
/// generated with a marker typed at the cursor, so that `{{ user. }}` parses, and the marker
/// taken out again before rust-analyzer sees the code. A bare name, which askama writes as
/// `self.name` because it is no local, is asked about twice: as written, for the struct's
/// fields and methods, and with `self.` blanked, for the locals in scope.
async fn rust_completion(
    workspace: &mut Workspace,
    path: &Path,
    text: &Arc<str>,
    offset: usize,
    context: Option<lsp_types::CompletionContext>,
) -> Option<(Vec<CompletionItem>, bool)> {
    let (rust_file, item) = workspace.analysis.readers(path).into_iter().next()?;
    let marked: Arc<str> = Arc::from(format!("{}{MARKER}{}", &text[..offset], &text[offset..]));
    let mut probe = workspace
        .analysis
        .probe(&rust_file, item, path, marked)
        .await?;

    // The identifier the marker landed in: a variable, a field or a filter's name.
    let token = probe.map.iter().find(|mapping| {
        matches!(&mapping.origin, Origin::Template { path: from, range }
            if **from == *path && range.start <= offset && offset + MARKER.len() <= range.end)
            && probe.code[mapping.generated.clone()].contains(MARKER)
            && probe.code[mapping.generated.clone()]
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_')
    })?;
    let token_start = token.generated.start;
    let marker_at = token_start + probe.code[token.generated.clone()].find(MARKER)?;
    let removed = marker_at..marker_at + MARKER.len();
    probe.code.replace_range(removed.clone(), "");
    for mapping in &mut probe.map {
        mapping.generated = without(&mapping.generated, &removed);
    }
    for error in &mut probe.errors {
        error.generated = without(&error.generated, &removed);
    }

    let mut probes = vec![(probe.clone(), false)];
    if probe.code[..token_start].ends_with("self.") {
        let mut locals = probe;
        let at = token_start - "self.".len();
        locals.code.replace_range(at..token_start, "     ");
        probes.push((locals, true));
    }

    let mut items = Vec::new();
    let mut incomplete = false;
    for (probe, locals_only) in probes {
        let Some((found, more)) = ask_completion(
            workspace,
            &rust_file,
            item,
            &probe,
            marker_at,
            text,
            offset,
            context.clone(),
        )
        .await
        else {
            continue;
        };
        incomplete |= more;
        items.extend(found.into_iter().filter(|item| {
            !item.label.starts_with("__askama")
                && (!locals_only || item.kind == Some(lsp_types::CompletionItemKind::VARIABLE))
        }));
    }
    workspace.restore(&rust_file);
    Some((items, incomplete))
}

/// Sends one probe and asks rust-analyzer to complete at the marker's place, moving each
/// completion's edit from around the cursor in the probe to around it in the template: the
/// identifier being typed is the same text in both.
#[expect(
    clippy::too_many_arguments,
    reason = "one probe's question, passed apart"
)]
async fn ask_completion(
    workspace: &mut Workspace,
    rust_file: &Path,
    item: usize,
    probe: &askama_analyzer_codegen::Generated,
    marker_at: usize,
    text: &str,
    offset: usize,
    context: Option<lsp_types::CompletionContext>,
) -> Option<(Vec<CompletionItem>, bool)> {
    let (overlay, _) = workspace.send_probe(rust_file, item, probe)?;
    let cursor = overlay.from_generated(item, marker_at)?;
    let lines = Lines::new(overlay.text(), workspace.encoding());
    let asked = TextDocumentPositionParams {
        text_document: TextDocumentIdentifier {
            uri: Url::from_file_path(rust_file).ok()?,
        },
        position: lines.position(cursor),
    };
    let response = server(workspace)
        .await?
        .completion(CompletionParams {
            text_document_position: asked,
            context,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .await
        .ok()??;
    let (items, incomplete) = match response {
        CompletionResponse::Array(items) => (items, false),
        CompletionResponse::List(list) => (list.items, list.is_incomplete),
    };
    let template = Lines::new(text, workspace.encoding());
    let move_range = |range: lsp_types::Range| -> Option<lsp_types::Range> {
        let start = lines.offset(range.start)?;
        let end = lines.offset(range.end)?;
        if start > cursor || end < cursor {
            return None;
        }
        let from = offset.checked_sub(cursor - start)?;
        let to = offset + (end - cursor);
        (to <= text.len()).then(|| template.range(&(from..to)))
    };
    let items = items
        .into_iter()
        .filter_map(|mut item| {
            item.text_edit = match item.text_edit.take() {
                Some(CompletionTextEdit::Edit(edit)) => Some(CompletionTextEdit::Edit(TextEdit {
                    range: move_range(edit.range)?,
                    new_text: edit.new_text,
                })),
                Some(CompletionTextEdit::InsertAndReplace(mut edit)) => {
                    edit.insert = move_range(edit.insert)?;
                    edit.replace = move_range(edit.replace)?;
                    Some(CompletionTextEdit::InsertAndReplace(edit))
                }
                None => None,
            };
            // These would edit Rust code, or need the probe that is gone.
            item.additional_text_edits = None;
            item.command = None;
            item.data = None;
            Some(item)
        })
        .collect();
    Some((items, incomplete))
}

/// A range with `removed` cut out of the text it refers to.
fn without(range: &Range<usize>, removed: &Range<usize>) -> Range<usize> {
    let moved = |at: usize| {
        if at <= removed.start {
            at
        } else if at >= removed.end {
            at - removed.len()
        } else {
            removed.start
        }
    };
    moved(range.start)..moved(range.end)
}

/// Whether `path` is a template some workspace reads.
pub fn is_template(workspaces: &[Workspace], path: &Path) -> bool {
    workspaces
        .iter()
        .any(|workspace| workspace.analysis.owns(path))
}

#[cfg(test)]
mod tests {
    use super::{without, word_at};

    #[test]
    fn the_word_around_an_offset_is_the_identifier_there() {
        assert_eq!(word_at("{{ user.name }}", 9), Some("name"), "inside a word");
        assert_eq!(word_at("{{ user.name }}", 12), Some("name"), "at its end");
        assert_eq!(word_at("{{ user.name }}", 3), Some("user"), "at its start");
        assert_eq!(word_at("{{  }}", 3), None, "no word in whitespace");
    }

    #[test]
    fn cutting_text_moves_later_ranges_back_and_shrinks_the_range_holding_it() {
        let removed = 10..15;
        assert_eq!(without(&(0..5), &removed), 0..5, "before the cut");
        assert_eq!(without(&(20..25), &removed), 15..20, "after the cut");
        assert_eq!(without(&(8..17), &removed), 8..12, "around the cut");
        assert_eq!(without(&(11..13), &removed), 10..10, "inside the cut");
    }
}
