//! A page parsed within a node and a time budget. The HTML tree builder re-creates every open
//! formatting element at each paragraph it reopens, so a page of a few hundred kilobytes can ask
//! for billions of nodes; past the budget the rest of the page is never read.

use std::borrow::Cow;
use std::cell::{Cell, Ref};
use std::time::{Duration, Instant};

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, LocalName, Namespace, QualName};
use scraper::{Html, HtmlTreeSink};

/// Input given to the parser between budget checks. Small, because one reopened paragraph can
/// create a node for every formatting element still open.
const CHUNK_BYTES: usize = 64;

type NodeId = <HtmlTreeSink as TreeSink>::Handle;

/// `html` as a document, of at most about `max_nodes` nodes, parsed for at most `max_time`.
pub(crate) fn parse_within(html: &str, max_nodes: usize, max_time: Duration) -> Html {
    let started = Instant::now();
    let mut parser = html5ever::parse_document(BudgetSink::new(max_nodes), Default::default());
    let mut rest = html;
    while !rest.is_empty() {
        let mut end = CHUNK_BYTES.min(rest.len());
        while !rest.is_char_boundary(end) {
            end += 1;
        }
        let (chunk, tail) = rest.split_at(end);
        parser.process(StrTendril::from_slice(chunk));
        rest = tail;
        if parser.tokenizer.sink.sink.spent() || started.elapsed() >= max_time {
            break;
        }
    }
    parser.finish()
}

/// A node the page built, or one asked for past the budget and never made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Kept(NodeId),
    Dropped(usize),
}

/// scraper's sink, with every node past the budget dropped instead of built. Dropped handles stay
/// distinct, so the tree builder's bookkeeping holds and nothing it does with them can fail.
struct BudgetSink {
    inner: HtmlTreeSink,
    budget: usize,
    built: Cell<usize>,
    dropped: Cell<usize>,
    /// What a dropped element answers to: an inline element, never a scope boundary.
    dropped_name: QualName,
}

impl BudgetSink {
    fn new(budget: usize) -> Self {
        Self {
            inner: HtmlTreeSink::new(Html::new_document()),
            budget,
            built: Cell::new(0),
            dropped: Cell::new(0),
            dropped_name: QualName::new(
                None,
                Namespace::from("http://www.w3.org/1999/xhtml"),
                LocalName::from("span"),
            ),
        }
    }

    fn spent(&self) -> bool {
        self.built.get() >= self.budget
    }

    /// A handle for one more node; `make` runs only while the budget lasts.
    fn node(&self, make: impl FnOnce() -> NodeId) -> Handle {
        if self.spent() {
            let n = self.dropped.get();
            self.dropped.set(n + 1);
            return Handle::Dropped(n);
        }
        self.built.set(self.built.get() + 1);
        Handle::Kept(make())
    }
}

/// `child` as the inner sink takes it; `None` for a dropped node.
fn kept(child: NodeOrText<Handle>) -> Option<NodeOrText<NodeId>> {
    match child {
        NodeOrText::AppendNode(Handle::Kept(id)) => Some(NodeOrText::AppendNode(id)),
        NodeOrText::AppendNode(Handle::Dropped(_)) => None,
        NodeOrText::AppendText(text) => Some(NodeOrText::AppendText(text)),
    }
}

#[derive(Debug)]
enum BudgetName<'a> {
    Kept(Ref<'a, QualName>),
    Dropped(&'a QualName),
}

impl ElemName for BudgetName<'_> {
    fn ns(&self) -> &Namespace {
        match self {
            Self::Kept(name) => &name.ns,
            Self::Dropped(name) => &name.ns,
        }
    }

    fn local_name(&self) -> &LocalName {
        match self {
            Self::Kept(name) => &name.local,
            Self::Dropped(name) => &name.local,
        }
    }
}

impl TreeSink for BudgetSink {
    type Handle = Handle;
    type Output = Html;
    type ElemName<'a> = BudgetName<'a>;

    fn finish(self) -> Html {
        self.inner.finish()
    }

    fn parse_error(&self, msg: Cow<'static, str>) {
        self.inner.parse_error(msg);
    }

    fn get_document(&self) -> Handle {
        Handle::Kept(self.inner.get_document())
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> BudgetName<'a> {
        match target {
            Handle::Kept(id) => BudgetName::Kept(self.inner.elem_name(id)),
            Handle::Dropped(_) => BudgetName::Dropped(&self.dropped_name),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        self.node(|| self.inner.create_element(name, attrs, flags))
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        self.node(|| self.inner.create_comment(text))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Handle {
        self.node(|| self.inner.create_pi(target, data))
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        if let (Handle::Kept(parent), Some(child)) = (parent, kept(child)) {
            self.inner.append(parent, child);
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        if let (Handle::Kept(element), Handle::Kept(prev), Some(child)) =
            (element, prev_element, kept(child))
        {
            self.inner.append_based_on_parent_node(element, prev, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        self.inner
            .append_doctype_to_document(name, public_id, system_id);
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        match target {
            Handle::Kept(id) => Handle::Kept(self.inner.get_template_contents(id)),
            dropped => *dropped,
        }
    }

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.inner.set_quirks_mode(mode);
    }

    fn append_before_sibling(&self, sibling: &Handle, new_node: NodeOrText<Handle>) {
        if let (Handle::Kept(sibling), Some(new_node)) = (sibling, kept(new_node)) {
            self.inner.append_before_sibling(sibling, new_node);
        }
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        if let Handle::Kept(target) = target {
            self.inner.add_attrs_if_missing(target, attrs);
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        if let Handle::Kept(target) = target {
            self.inner.remove_from_parent(target);
        }
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        if let (Handle::Kept(node), Handle::Kept(new_parent)) = (node, new_parent) {
            self.inner.reparent_children(node, new_parent);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(60);

    /// `formatting` distinct unclosed `<b>`s, then `paragraphs` reopened paragraphs: each one
    /// makes the tree builder re-create every one of those `<b>`s.
    fn multiplying_page(formatting: usize, paragraphs: usize) -> String {
        let mut html = String::from(
            "<html><body><p>The library opens at nine every weekday morning and closes at five.</p><p>",
        );
        for i in 0..formatting {
            html.push_str(&format!("<b c{i}>"));
        }
        html.push_str(&"<p>x".repeat(paragraphs));
        html
    }

    fn nodes(document: &Html) -> usize {
        document.tree.nodes().count()
    }

    #[test]
    fn the_page_really_multiplies_without_a_budget() {
        let unbounded = Html::parse_document(&multiplying_page(300, 300));
        assert!(nodes(&unbounded) > 80_000, "{}", nodes(&unbounded));
    }

    #[test]
    fn a_page_built_to_multiply_its_nodes_stops_at_the_budget() {
        let document = parse_within(&multiplying_page(300, 300), 5_000, LONG);
        assert!(
            nodes(&document) <= 5_000 + CHUNK_BYTES,
            "{}",
            nodes(&document)
        );
        let text: String = document.root_element().text().collect();
        assert!(
            text.contains("closes at five"),
            "the page before the attack is kept"
        );
    }

    #[test]
    fn an_ordinary_page_parses_as_scraper_parses_it() {
        let html = "<html><head><title>T</title></head><body><table><tr><td>Length</td>\
                    <td>472 km</td></tr></table><p>One <b>two</b> <i>three</i></p></body></html>";
        let ours = parse_within(html, 200_000, LONG);
        assert_eq!(ours.html(), Html::parse_document(html).html());
    }

    #[test]
    fn past_its_time_a_page_is_read_no_further() {
        let mut html = String::from("<p>first</p>");
        html.push_str(&"<p>filler paragraph</p>".repeat(500));
        html.push_str("<p>LAST</p>");
        let document = parse_within(&html, 200_000, Duration::ZERO);
        let text: String = document.root_element().text().collect();
        assert!(text.contains("first"), "{text}");
        assert!(!text.contains("LAST"), "the whole page was read");
    }
}
