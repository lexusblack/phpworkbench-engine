use std::ops::Range;
use std::path::{Path, PathBuf};
use gpui::AsyncWindowContext;
use language::Buffer;
use project::{Project, ProjectPath, ResolvedPath};
use text::{Anchor, ToOffset};
use util::rel_path::RelPath;
use crate::hover_links::ResolvedFileTarget;

/// Resolves PHP and Laravel controller methods from string literals in route definitions,
/// e.g.:
/// - Route::resource('projects', ProjectController::class)->only(['store', 'destroy'])
/// - Route::get('register', [AuthController::class, 'showRegister'])
/// - Route::get('users', 'UserController@index')
/// - action([ProjectController::class, 'store'])
pub async fn find_php_controller_method(
    buffer: &gpui::Entity<Buffer>,
    project: Option<gpui::Entity<Project>>,
    position: Anchor,
    cx: &mut AsyncWindowContext,
) -> Option<(Range<Anchor>, ResolvedFileTarget)> {
    let project = project?;
    let (is_php, snapshot) = buffer.read_with(cx, |buffer, _| {
        let snapshot = buffer.snapshot();
        let is_php = snapshot.language_at(position).map_or(false, |l| {
            let name = l.name();
            name == "PHP" || name == "Blade"
        }) || snapshot.file().map_or(false, |f| {
            let path = f.path().to_string();
            path.ends_with(".php") || path.ends_with(".blade.php")
        });
        (is_php, snapshot)
    });

    if !is_php {
        return None;
    }

    let offset = position.to_offset(&snapshot);
    let (string_range, string_content) = surrounding_string_literal(&snapshot, offset)?;

    if string_content.is_empty() || string_content.len() > 100 {
        return None;
    }

    let (controller_name, method_name) =
        resolve_controller_and_method(&snapshot, offset, &string_content)?;

    let (abs_path, resolved_path) =
        resolve_controller_file(&snapshot, &controller_name, &project, cx).await?;

    let (line_idx, col_idx) = find_method_in_file(&abs_path, &method_name, &controller_name)?;

    Some((
        string_range,
        ResolvedFileTarget {
            resolved_path,
            row: Some(line_idx + 1), // 1-based line number for navigation
            column: Some(col_idx + 1),
        },
    ))
}

pub(crate) fn surrounding_string_literal(
    snapshot: &language::BufferSnapshot,
    offset: usize,
) -> Option<(Range<Anchor>, String)> {
    const SCAN_LIMIT: usize = 256;

    let total_len = snapshot.len();
    if offset > total_len {
        return None;
    }

    let scan_start = offset.saturating_sub(SCAN_LIMIT);
    let backwards_text: String = snapshot.text_for_range(scan_start..offset).collect();

    let mut quote_char = None;
    let mut quote_start = None;
    let mut prev_char = None;

    for (idx, ch) in backwards_text.char_indices().rev() {
        if ch == '\n' {
            break;
        }
        if (ch == '\'' || ch == '"') && prev_char != Some('\\') {
            quote_char = Some(ch);
            quote_start = Some(scan_start + idx);
            break;
        }
        prev_char = Some(ch);
    }

    let quote = quote_char?;
    let start_idx = quote_start?;

    let scan_end = (offset + SCAN_LIMIT).min(total_len);
    let forwards_text: String = snapshot.text_for_range(start_idx + 1..scan_end).collect();

    let mut quote_end = None;
    let mut is_escaped = false;

    for (idx, ch) in forwards_text.char_indices() {
        if ch == '\n' {
            break;
        }
        if ch == '\\' {
            is_escaped = !is_escaped;
            continue;
        }
        if ch == quote && !is_escaped {
            quote_end = Some(start_idx + 1 + idx);
            break;
        }
        is_escaped = false;
    }

    let end_idx = quote_end?;

    if offset < start_idx || offset > end_idx + 1 {
        return None;
    }

    let content: String = snapshot.text_for_range(start_idx + 1..end_idx).collect();
    let anchor_range = snapshot.anchor_before(start_idx + 1)..snapshot.anchor_after(end_idx);

    Some((anchor_range, content))
}

pub(crate) fn resolve_controller_and_method(
    snapshot: &language::BufferSnapshot,
    offset: usize,
    string_content: &str,
) -> Option<(String, String)> {
    // Case 1: String contains 'Controller@method'
    if let Some((controller, method)) = string_content.split_once('@') {
        let clean_controller = controller.trim().trim_start_matches('\\');
        let base_controller = clean_controller.rsplit('\\').next().unwrap_or(clean_controller);
        if !base_controller.is_empty() && !method.trim().is_empty() {
            return Some((base_controller.to_string(), method.trim().to_string()));
        }
    }

    let is_valid_method = !string_content.is_empty()
        && string_content.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !string_content.chars().next().unwrap().is_ascii_digit();

    if !is_valid_method {
        return None;
    }

    let method_name = string_content.to_string();

    let point = snapshot.offset_to_point(offset);
    let start_row = point.row.saturating_sub(25);
    let start_offset = snapshot.point_to_offset(text::Point::new(start_row, 0));
    let end_row = (point.row + 3).min(snapshot.max_point().row);
    let end_offset = snapshot.point_to_offset(text::Point::new(end_row + 1, 0));

    let context_before: String = snapshot.text_for_range(start_offset..offset).collect();
    let context_all: String = snapshot.text_for_range(start_offset..end_offset).collect();

    // Pattern 2: [Controller::class, 'method']
    if let Some(bracket_idx) = context_before.rfind('[') {
        let inside_bracket = &context_before[bracket_idx..];
        if let Some(class_idx) = inside_bracket.find("::class") {
            let before_class = inside_bracket[..class_idx].trim();
            if let Some(controller) = extract_class_identifier(before_class) {
                return Some((controller, method_name));
            }
        }
    }

    // Pattern 3: Route::resource / Route::apiResource with ->only(...) or ->except(...)
    // e.g. Route::resource('projects', ProjectController::class)->only(['store', 'destroy'])
    if context_all.contains("only") || context_all.contains("except") || context_all.contains("resource") {
        if let Some(controller) = find_controller_in_resource_chain(&context_before, &context_all) {
            return Some((controller, method_name));
        }
    }

    // Pattern 4: Any statement containing Route:: or -> and a *Controller::class
    if let Some(controller) = find_any_controller_class(&context_before) {
        return Some((controller, method_name));
    }

    None
}

fn extract_class_identifier(s: &str) -> Option<String> {
    let s = s.trim();
    let chars: Vec<char> = s.chars().rev().collect();
    let mut id_chars = Vec::new();
    for c in chars {
        if c.is_ascii_alphanumeric() || c == '_' || c == '\\' {
            id_chars.push(c);
        } else {
            break;
        }
    }
    if id_chars.is_empty() {
        return None;
    }
    id_chars.reverse();
    let raw: String = id_chars.into_iter().collect();
    let clean = raw.trim_start_matches('\\');
    let base = clean.rsplit('\\').next().unwrap_or(clean);
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

fn find_controller_in_resource_chain(context_before: &str, context_all: &str) -> Option<String> {
    let mut search_idx = context_before.len();
    while let Some(class_pos) = context_before[..search_idx].rfind("::class") {
        let before_class = &context_before[..class_pos];
        if let Some(controller) = extract_class_identifier(before_class) {
            if controller.ends_with("Controller") || !controller.is_empty() {
                return Some(controller);
            }
        }
        search_idx = class_pos;
        if search_idx == 0 {
            break;
        }
    }

    find_any_controller_class(context_all)
}

fn find_any_controller_class(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.split("::class").collect();
    for part in parts.into_iter().rev().skip(1) {
        if let Some(controller) = extract_class_identifier(part) {
            if controller.ends_with("Controller") {
                return Some(controller);
            }
        }
    }
    None
}

async fn resolve_controller_file(
    snapshot: &language::BufferSnapshot,
    controller_name: &str,
    project: &gpui::Entity<Project>,
    cx: &mut AsyncWindowContext,
) -> Option<(PathBuf, ResolvedPath)> {
    let max_header_row = 150.min(snapshot.max_point().row);
    let header_end = snapshot.point_to_offset(text::Point::new(max_header_row, 0));
    let header_text: String = snapshot.text_for_range(0..header_end).collect();

    let mut full_namespace_path = None;
    for line in header_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("use ") && trimmed.ends_with(';') {
            let import = trimmed.trim_start_matches("use ").trim_end_matches(';').trim();
            let class_part = import.split_whitespace().next().unwrap_or(import);
            if class_part.ends_with(controller_name) {
                let mut path_str = class_part.replace('\\', "/");
                if path_str.starts_with("App/") {
                    path_str = format!("app/{}", &path_str[4..]);
                }
                full_namespace_path = Some(format!("{path_str}.php"));
                break;
            }
        }
    }

    let controller_filename = format!("{controller_name}.php");
    let standard_laravel_path = format!("app/Http/Controllers/{controller_name}.php");

    let candidates = vec![
        full_namespace_path,
        Some(standard_laravel_path),
        Some(format!("app/Controllers/{controller_name}.php")),
        Some(format!("src/Controller/{controller_name}.php")),
    ];

    let found_in_project = project.update(cx, |project, cx| {
        for worktree in project.worktrees(cx) {
            let wt = worktree.read(cx);
            // Check direct candidates
            for cand in candidates.iter().flatten() {
                if let Ok(rel) = RelPath::new(cand.as_ref(), wt.path_style()) {
                    if let Some(entry) = wt.entry_for_path(&rel) {
                        if entry.is_file() {
                            let abs = wt.absolutize(&entry.path);
                            let resolved = ResolvedPath::ProjectPath {
                                project_path: ProjectPath {
                                    worktree_id: wt.id(),
                                    path: entry.path.clone(),
                                },
                                is_dir: false,
                            };
                            return Some((abs, resolved));
                        }
                    }
                }
            }
            // Scan worktree entries for matching filename
            for entry in wt.entries(false, 0) {
                if entry.is_file() && entry.path.to_string().ends_with(&controller_filename) {
                    let abs = wt.absolutize(&entry.path);
                    let resolved = ResolvedPath::ProjectPath {
                        project_path: ProjectPath {
                            worktree_id: wt.id(),
                            path: entry.path.clone(),
                        },
                        is_dir: false,
                    };
                    return Some((abs, resolved));
                }
            }
        }
        None
    });

    if let Some(res) = found_in_project {
        return Some(res);
    }

    None
}

pub(crate) fn find_method_in_file(
    abs_path: &Path,
    method_name: &str,
    controller_name: &str,
) -> Option<(u32, u32)> {
    let content = std::fs::read_to_string(abs_path).ok()?;
    let method_pattern = format!("function {}", method_name.to_lowercase());

    let mut class_row = 0;
    let mut class_col = 0;

    for (row_idx, line) in content.lines().enumerate() {
        let lower = line.to_lowercase();

        if lower.contains(&format!("class {}", controller_name.to_lowercase())) {
            class_row = row_idx as u32;
            class_col = line.find("class").unwrap_or(0) as u32;
        }

        if let Some(pos) = lower.find(&method_pattern) {
            let after = pos + method_pattern.len();
            let remaining = line[after..].trim_start();
            if remaining.starts_with('(') || remaining.starts_with('&') || remaining.is_empty() {
                return Some((row_idx as u32, pos as u32));
            }
        }
    }

    Some((class_row, class_col))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    #[test]
    fn test_extract_class_identifier() {
        assert_eq!(extract_class_identifier("ProjectController"), Some("ProjectController".to_string()));
        assert_eq!(extract_class_identifier("App\\Http\\Controllers\\ProjectController"), Some("ProjectController".to_string()));
        assert_eq!(extract_class_identifier("Route::resource('projects', ProjectController"), Some("ProjectController".to_string()));
        assert_eq!(extract_class_identifier("[AuthController"), Some("AuthController".to_string()));
    }

    #[test]
    fn test_find_method_in_file_content() {
        let temp_dir = std::env::temp_dir();
        let file_path = temp_dir.join("TestProjectController.php");
        let sample = r#"<?php
namespace App\Http\Controllers;

class TestProjectController extends Controller
{
    public function index() {}
    public function store(Request $request) {}
    public function destroy(int $id) {}
}
"#;
        std::fs::write(&file_path, sample).unwrap();

        let (row, _) = find_method_in_file(&file_path, "store", "TestProjectController").unwrap();
        assert_eq!(row, 6);

        let (row, _) = find_method_in_file(&file_path, "destroy", "TestProjectController").unwrap();
        assert_eq!(row, 7);

        let (row, _) = find_method_in_file(&file_path, "nonexistent", "TestProjectController").unwrap();
        assert_eq!(row, 3); // falls back to class line

        let _ = std::fs::remove_file(&file_path);
    }

    #[gpui::test]
    async fn test_laravel_route_action_resolution(cx: &mut gpui::TestAppContext) {
        let code = r#"<?php
use App\Http\Controllers\AuthController;
use App\Http\Controllers\ProjectController;

Route::middleware('guest')->group(function () {
    Route::get('register', [AuthController::class, 'showRegister'])->name('register');
    Route::post('register', [AuthController::class, 'register']);
});

Route::middleware('auth')->group(function () {
    Route::resource('projects', ProjectController::class)->only(['store', 'destroy']);
});
"#;
        let buffer = cx.new(|cx| language::Buffer::local(code, cx));
        let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());

        // 1. Test 'showRegister' in [AuthController::class, 'showRegister']
        let show_reg_pos = code.find("showRegister").unwrap() + 2;
        let (_, str_content) = surrounding_string_literal(&snapshot, show_reg_pos).unwrap();
        assert_eq!(str_content, "showRegister");
        let (controller, method) = resolve_controller_and_method(&snapshot, show_reg_pos, &str_content).unwrap();
        assert_eq!(controller, "AuthController");
        assert_eq!(method, "showRegister");

        // 2. Test 'store' in Route::resource(..., ProjectController::class)->only(['store', 'destroy'])
        let store_pos = code.find("store").unwrap() + 2;
        let (_, str_content) = surrounding_string_literal(&snapshot, store_pos).unwrap();
        assert_eq!(str_content, "store");
        let (controller, method) = resolve_controller_and_method(&snapshot, store_pos, &str_content).unwrap();
        assert_eq!(controller, "ProjectController");
        assert_eq!(method, "store");

        // 3. Test 'destroy' in ->only(['store', 'destroy'])
        let destroy_pos = code.find("destroy").unwrap() + 2;
        let (_, str_content) = surrounding_string_literal(&snapshot, destroy_pos).unwrap();
        assert_eq!(str_content, "destroy");
        let (controller, method) = resolve_controller_and_method(&snapshot, destroy_pos, &str_content).unwrap();
        assert_eq!(controller, "ProjectController");
        assert_eq!(method, "destroy");
    }
}
