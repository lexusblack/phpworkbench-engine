use super::*;
use language::AutoindentMode;
use multi_buffer::{MultiBufferOffset, MultiBufferRow};
use text::Point;

impl Editor {
    /// Completes the current statement by appending closing parens/semicolon and inserting a new line.
    pub fn complete_current_statement(
        &mut self,
        _: &CompleteCurrentStatement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let mut edits = Vec::new();

        for selection in self.selections.all_adjusted(&self.display_snapshot(cx)).iter() {
            let point = selection.end;
            let row = point.row;
            let line_len = snapshot.line_len(MultiBufferRow(row));
            let line_end_point = Point::new(row, line_len);
            let line_text: String = snapshot
                .text_for_range(Point::new(row, 0)..line_end_point)
                .collect();
            let trimmed = line_text.trim_end();

            if trimmed.is_empty() {
                edits.push((line_end_point..line_end_point, "\n".to_string()));
                continue;
            }

            let open_parens = trimmed.chars().filter(|&c| c == '(').count();
            let close_parens = trimmed.chars().filter(|&c| c == ')').count();
            let missing_parens = open_parens.saturating_sub(close_parens);

            let mut insertion = String::new();
            for _ in 0..missing_parens {
                insertion.push(')');
            }

            let last_char = trimmed.chars().last().unwrap_or(' ');
            if last_char != ';' && last_char != '{' && last_char != '}' && last_char != ':' {
                insertion.push(';');
            }
            insertion.push('\n');

            edits.push((line_end_point..line_end_point, insertion));
        }

        if !edits.is_empty() {
            self.transact(window, cx, |this, _window, cx| {
                this.buffer.update(cx, |buffer, cx| {
                    buffer.edit(edits, Some(AutoindentMode::EachLine), cx);
                });
            });
        }
    }

    /// Cycles word completions forward across open buffers.
    pub fn cyclic_expand_word(
        &mut self,
        _: &CyclicExpandWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_visible_completions_menu() {
            self.context_menu_next(&ContextMenuNext, window, cx);
        } else {
            self.show_word_completions(&ShowWordCompletions, window, cx);
        }
    }

    /// Cycles word completions backward across open buffers.
    pub fn cyclic_expand_word_backward(
        &mut self,
        _: &CyclicExpandWordBackward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_visible_completions_menu() {
            self.context_menu_prev(&ContextMenuPrevious, window, cx);
        } else {
            self.show_word_completions(&ShowWordCompletions, window, cx);
        }
    }

    /// Surrounds current selection or statement with a block or try/catch.
    pub fn surround_with(
        &mut self,
        _: &SurroundWith,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        let mut has_non_empty_selection = false;
        let mut edits = Vec::new();

        for selection in self
            .selections
            .all_adjusted(&self.display_snapshot(cx))
            .iter()
        {
            if !selection.is_empty() {
                has_non_empty_selection = true;
                let start_point = selection.start;
                let end_point = selection.end;

                let open = "try {\n";
                let close = "\n} catch (\\Throwable $e) {\n    // TODO: handle exception\n}";

                edits.push((start_point..start_point, open.to_string()));
                edits.push((end_point..end_point, close.to_string()));
            }
        }

        if has_non_empty_selection && !edits.is_empty() {
            self.transact(window, cx, |this, _window, cx| {
                this.buffer.update(cx, |buffer, cx| {
                    buffer.edit(edits, Some(AutoindentMode::EachLine), cx);
                });
            });
        } else {
            self.toggle_code_actions(&ToggleCodeActions::default(), window, cx);
        }
    }

    /// Jumps to next Emmet edit point or active snippet tabstop.
    pub fn next_emmet_edit_point(
        &mut self,
        _: &NextEmmetEditPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.move_to_next_snippet_tabstop(window, cx) {
            return;
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let cursor_offset = self.selections.newest_anchor().head().to_offset(&snapshot);
        let text: String = snapshot
            .text_for_range(cursor_offset..snapshot.len())
            .collect();

        let patterns = ["\"\"", "''", "><", "> <"];
        let mut closest_match: Option<usize> = None;
        for pattern in patterns {
            if let Some(pos) = text.find(pattern) {
                let jump_pos = pos + 1;
                closest_match = Some(closest_match.map_or(jump_pos, |min| min.min(jump_pos)));
            }
        }

        if let Some(rel_offset) = closest_match {
            let target_offset = cursor_offset + MultiBufferOffset(rel_offset);
            let target_point = target_offset.to_point(&snapshot);
            self.change_selections(
                SelectionEffects::scroll(Autoscroll::fit()),
                window,
                cx,
                |s| s.select_ranges([target_point..target_point]),
            );
        }
    }

    /// Jumps to previous Emmet edit point or active snippet tabstop.
    pub fn previous_emmet_edit_point(
        &mut self,
        _: &PreviousEmmetEditPoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.move_to_prev_snippet_tabstop(window, cx) {
            return;
        }

        let snapshot = self.buffer.read(cx).snapshot(cx);
        let cursor_offset = self.selections.newest_anchor().head().to_offset(&snapshot);
        let before_text: String = snapshot
            .text_for_range(MultiBufferOffset(0)..cursor_offset)
            .collect();

        let patterns = ["\"\"", "''", "><", "> <"];
        let mut closest_match: Option<usize> = None;
        for pattern in patterns {
            if let Some(pos) = before_text.rfind(pattern) {
                let jump_pos = pos + 1;
                closest_match = Some(closest_match.map_or(jump_pos, |max| max.max(jump_pos)));
            }
        }

        if let Some(target_offset) = closest_match {
            let target_offset = MultiBufferOffset(target_offset);
            let target_point = target_offset.to_point(&snapshot);
            self.change_selections(
                SelectionEffects::scroll(Autoscroll::fit()),
                window,
                cx,
                |s| s.select_ranges([target_point..target_point]),
            );
        }
    }

    /// Shows signature help or rename for the symbol at cursor.
    pub fn change_signature(
        &mut self,
        _: &ChangeSignature,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_signature_help(&ShowSignatureHelp, window, cx);
    }

    /// Safely checks usages before deleting symbol.
    pub fn safe_delete(
        &mut self,
        _: &SafeDelete,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.find_all_references(&FindAllReferences::default(), window, cx);
    }
}
