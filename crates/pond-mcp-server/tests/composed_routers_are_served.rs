//! A server that composes several tool routers must hand the composed one to `#[tool_handler]`.
//!
//! The macro's default is the impl block's own router. The schedule server composed its to-do
//! tools into a field, served the default, and so never offered `add_reminder`, `list_todos` or
//! `complete_todo` to a model whose prompt told it to use them: every such turn ended empty.
//! The tool-block oracle could not see it, since it reads the routers, not what a server lists.

use std::fs;
use std::path::Path;

/// Whether the source adds another router to its own (`Self::tool_router() + Self::…`).
fn composes_routers(source: &str) -> bool {
    let squeezed: String = source.split_whitespace().collect();
    squeezed.contains("Self::tool_router()+Self::")
}

#[test]
fn every_server_that_composes_routers_serves_the_composed_one() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut composing = Vec::new();
    for entry in fs::read_dir(&src).expect("the crate's src directory") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }
        let source = fs::read_to_string(&path).expect("a source file");
        if !composes_routers(&source) {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            source.contains("#[tool_handler(router = self.tool_router)]"),
            "{name} composes tool routers but its `#[tool_handler]` serves only its own: the \
             composed tools are never listed, so no model is offered them"
        );
        composing.push(name);
    }
    composing.sort();
    assert_eq!(
        composing,
        ["knowledge.rs", "schedule.rs"],
        "the servers found composing routers changed; if one was added, this guard covers it, \
         and if this list is shorter the detection has rotted"
    );
}

#[test]
fn the_detection_sees_a_composition_however_it_is_wrapped() {
    assert!(composes_routers(
        "tool_router: Self::tool_router()\n                + Self::wolfram_tool_router(),"
    ));
    assert!(composes_routers(
        "tool_router: Self::tool_router() + Self::todo_tool_router(),"
    ));
    assert!(!composes_routers("tool_router: Self::tool_router(),"));
}
