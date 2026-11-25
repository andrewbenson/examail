//! A small toolkit over html5ever's reference tree, which the render pipeline edits as a script
//! would edit the DOM.

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use html5ever::{
    Attribute, LocalName, QualName, ns, parse_document,
    serialize::{SerializeOpts, TraversalScope, serialize},
    tendril::TendrilSink as _,
};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom, SerializableHandle};

/// A node in the tree.
pub type NodeRef = Handle;

/// Identifies a node while it's alive, for maps keyed by node.
pub(crate) type NodeKey = *const Node;

/// The node's key.
pub(crate) fn key(node: &NodeRef) -> NodeKey {
    Rc::as_ptr(node)
}

/// Drops a node and everything under it iteratively, so a deeply nested tree cannot overflow the
/// call stack inside recursive `Rc::drop`.
pub fn drop_tree(root: NodeRef) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        node.parent.set(None);
        if let Ok(mut children) = node.children.try_borrow_mut() {
            stack.append(&mut *children);
        }
        if let NodeData::Element {
            template_contents, ..
        } = &node.data
            && let Ok(mut contents) = template_contents.try_borrow_mut()
            && let Some(contents) = contents.take()
        {
            stack.push(contents);
        }
    }
}

/// Empties a `<template>` element's `template_contents`, dropping its subtree iteratively.
pub fn clear_template_contents(node: &NodeRef) {
    if let NodeData::Element {
        template_contents, ..
    } = &node.data
        && let Some(contents) = template_contents.borrow_mut().take()
    {
        drop_tree(contents);
    }
}

/// Deep-clones `root` and its descendants iteratively (`Node.cloneNode(true)`).
pub fn clone_node(root: &NodeRef) -> NodeRef {
    fn shallow(node: &NodeRef) -> NodeRef {
        let data = match &node.data {
            NodeData::Document => NodeData::Document,
            NodeData::Doctype {
                name,
                public_id,
                system_id,
            } => NodeData::Doctype {
                name: name.clone(),
                public_id: public_id.clone(),
                system_id: system_id.clone(),
            },
            NodeData::Text { contents } => NodeData::Text {
                contents: RefCell::new(contents.borrow().clone()),
            },
            NodeData::Comment { contents } => NodeData::Comment {
                contents: contents.clone(),
            },
            NodeData::Element {
                name,
                attrs,
                mathml_annotation_xml_integration_point,
                ..
            } => NodeData::Element {
                name: name.clone(),
                attrs: RefCell::new(attrs.borrow().clone()),
                template_contents: RefCell::new(None),
                mathml_annotation_xml_integration_point: *mathml_annotation_xml_integration_point,
            },
            NodeData::ProcessingInstruction { target, contents } => {
                NodeData::ProcessingInstruction {
                    target: target.clone(),
                    contents: contents.clone(),
                }
            }
        };
        Node::new(data)
    }
    let cloned_root = shallow(root);
    let mut stack = vec![(root.clone(), cloned_root.clone())];
    while let Some((src, dst)) = stack.pop() {
        let children = src.children.borrow();
        let mut cloned_children = Vec::with_capacity(children.len());
        for child in children.iter() {
            let cloned_child = shallow(child);
            cloned_child.parent.set(Some(Rc::downgrade(&dst)));
            cloned_children.push(cloned_child);
        }
        for (child, cloned_child) in children.iter().zip(&cloned_children).rev() {
            if !child.children.borrow().is_empty() {
                stack.push((child.clone(), cloned_child.clone()));
            }
        }
        drop(children);
        *dst.children.borrow_mut() = cloned_children;
    }
    cloned_root
}

/// Parses a whole document (`<html>`, `<head>`, `<body>` are always there afterwards).
pub fn parse(html: &str) -> NodeRef {
    parse_document(RcDom::default(), Default::default())
        .one(html)
        .document
}

/// Parses `html` as `<body>` content and returns its detached top-level nodes, dropping the
/// temporary document iteratively.
pub fn parse_fragment(html: &str) -> Vec<NodeRef> {
    let document = parse(html);
    let nodes = body(&document)
        .map(|body| take_children(&body))
        .unwrap_or_default();
    for node in &nodes {
        node.parent.set(None);
    }
    drop_tree(document);
    nodes
}

/// The node's outer HTML (or, for the document, its children's).
pub fn outer_html(node: &NodeRef) -> String {
    let scope = if matches!(node.data, NodeData::Document) {
        TraversalScope::ChildrenOnly(None)
    } else {
        TraversalScope::IncludeNode
    };
    write(node, scope)
}

/// The node's children as HTML.
pub fn inner_html(node: &NodeRef) -> String {
    write(node, TraversalScope::ChildrenOnly(None))
}

fn write(node: &NodeRef, traversal_scope: TraversalScope) -> String {
    let mut out = Vec::new();
    let options = SerializeOpts {
        traversal_scope,
        ..SerializeOpts::default()
    };
    serialize(&mut out, &SerializableHandle::from(node.clone()), options)
        .expect("writing to a Vec doesn't fail");
    String::from_utf8(out).expect("the serializer writes UTF-8")
}

/// Escapes `&`, `<`, `>`, `"` and `'` (`&#39;`) as HTML entities in a single pass.
pub fn escape_html(text: &str) -> String {
    if !text.contains(['&', '<', '>', '"', '\'']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            ch => out.push(ch),
        }
    }
    out
}

/// Escapes `&`, `<` and `>` as HTML entities in a single pass, for text nodes.
pub fn escape_html_text(text: &str) -> String {
    if !text.contains(['&', '<', '>']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ch => out.push(ch),
        }
    }
    out
}

/// Escapes text for a double-quoted attribute value, in a single pass.
pub fn escape_attribute(text: &str) -> String {
    if !text.contains(['&', '"']) {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            ch => out.push(ch),
        }
    }
    out
}

/// Appends `text` as `innerHTML` serializes text nodes (`&`, `<`, `>`, and `\u{a0}` as `&nbsp;`).
pub fn push_inner_html_text(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            ch => out.push(ch),
        }
    }
}

/// Appends `text` escaped for a double-quoted HTML attribute with `\u{a0}` written as `&nbsp;`.
pub fn push_inner_html_attr(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ch => out.push(ch),
        }
    }
}

/// A new element in the HTML namespace.
pub fn element(name: &str, attributes: &[(&str, &str)]) -> NodeRef {
    Node::new(NodeData::Element {
        name: html_name(name),
        attrs: RefCell::new(
            attributes
                .iter()
                .map(|(name, value)| Attribute {
                    name: QualName::new(None, ns!(), LocalName::from(*name)),
                    value: (*value).into(),
                })
                .collect(),
        ),
        template_contents: RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

/// A new text node.
pub fn text_node(text: &str) -> NodeRef {
    Node::new(NodeData::Text {
        contents: RefCell::new(text.into()),
    })
}

fn html_name(name: &str) -> QualName {
    QualName::new(None, ns!(html), LocalName::from(name))
}

/// The element's tag name (lowercase for HTML), or `None` for other nodes.
pub fn tag(node: &NodeRef) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(&name.local),
        _ => None,
    }
}

/// Whether `node` is a `name` element.
pub fn is(node: &NodeRef, name: &str) -> bool {
    tag(node) == Some(name)
}

/// Whether `node` is an SVG `<image>` or `<feImage>` element.
pub fn is_svg_image(node: &NodeRef) -> bool {
    tag(node).is_some_and(|t| t.eq_ignore_ascii_case("image") || t.eq_ignore_ascii_case("feimage"))
}

/// Whether `node` is an element.
pub fn is_element(node: &NodeRef) -> bool {
    matches!(node.data, NodeData::Element { .. })
}

/// Whether `node` is text.
pub fn is_text(node: &NodeRef) -> bool {
    matches!(node.data, NodeData::Text { .. })
}

/// Whether `node` is a comment.
pub fn is_comment(node: &NodeRef) -> bool {
    matches!(node.data, NodeData::Comment { .. })
}

/// The namespaces HTML parsing puts elements in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    Html,
    Svg,
    MathMl,
}

/// The element's namespace (`None` for other nodes).
pub fn namespace(node: &NodeRef) -> Option<Namespace> {
    match &node.data {
        NodeData::Element { name, .. } if name.ns == ns!(html) => Some(Namespace::Html),
        NodeData::Element { name, .. } if name.ns == ns!(svg) => Some(Namespace::Svg),
        NodeData::Element { name, .. } if name.ns == ns!(mathml) => Some(Namespace::MathMl),
        _ => None,
    }
}

/// The text of a text node.
pub fn text(node: &NodeRef) -> Option<String> {
    match &node.data {
        NodeData::Text { contents } => Some(contents.borrow().to_string()),
        _ => None,
    }
}

/// Replaces a text node's text.
pub fn set_text(node: &NodeRef, value: &str) {
    if let NodeData::Text { contents } = &node.data {
        *contents.borrow_mut() = value.into();
    }
}

fn attr_name(name: &QualName) -> Cow<'_, str> {
    match &name.prefix {
        Some(prefix) if !prefix.is_empty() => Cow::Owned(format!("{prefix}:{}", name.local)),
        _ => Cow::Borrowed(&name.local),
    }
}

fn attr_matches(attr: &QualName, name: &str) -> bool {
    match &attr.prefix {
        Some(prefix) if !prefix.is_empty() => {
            name.strip_prefix(&**prefix)
                .and_then(|rest| rest.strip_prefix(':'))
                == Some(&*attr.local)
        }
        _ => &*attr.local == name,
    }
}

fn attr_qual_name(node: &NodeRef, name: &str) -> QualName {
    if matches!(namespace(node), Some(Namespace::Svg | Namespace::MathMl)) {
        if let Some(local) = name.strip_prefix("xlink:") {
            return QualName::new(Some("xlink".into()), ns!(xlink), LocalName::from(local));
        }
        if let Some(local) = name.strip_prefix("xml:") {
            return QualName::new(Some("xml".into()), ns!(xml), LocalName::from(local));
        }
        if let Some(local) = name.strip_prefix("xmlns:") {
            return QualName::new(Some("xmlns".into()), ns!(xmlns), LocalName::from(local));
        }
        if name == "xmlns" {
            return QualName::new(None, ns!(xmlns), LocalName::from("xmlns"));
        }
    }
    QualName::new(None, ns!(), LocalName::from(name))
}

/// An attribute's value.
pub fn attr(node: &NodeRef, name: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|attribute| attr_matches(&attribute.name, name))
            .map(|attribute| attribute.value.to_string()),
        _ => None,
    }
}

/// The element's attributes, names and values, in order.
pub fn attrs(node: &NodeRef) -> Vec<(String, String)> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .map(|attribute| {
                (
                    attr_name(&attribute.name).into_owned(),
                    attribute.value.to_string(),
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Replaces the element's attributes with `attributes`, in order.
pub fn set_attrs(node: &NodeRef, attributes: &[(String, String)]) {
    let updated: Vec<Attribute> = attributes
        .iter()
        .map(|(name, value)| Attribute {
            name: attr_qual_name(node, name),
            value: value.as_str().into(),
        })
        .collect();
    if let NodeData::Element { attrs, .. } = &node.data {
        *attrs.borrow_mut() = updated;
    }
}

/// Whether the element has the attribute.
pub fn has_attr(node: &NodeRef, name: &str) -> bool {
    attr(node, name).is_some()
}

/// Sets an attribute.
pub fn set_attr(node: &NodeRef, name: &str, value: &str) {
    let qual = attr_qual_name(node, name);
    if let NodeData::Element { attrs, .. } = &node.data {
        let mut attrs = attrs.borrow_mut();
        match attrs
            .iter_mut()
            .find(|attribute| attr_matches(&attribute.name, name))
        {
            Some(attribute) => attribute.value = value.into(),
            None => attrs.push(Attribute {
                name: qual,
                value: value.into(),
            }),
        }
    }
}

/// Removes an attribute.
pub fn remove_attr(node: &NodeRef, name: &str) {
    if let NodeData::Element { attrs, .. } = &node.data {
        attrs
            .borrow_mut()
            .retain(|attribute| !attr_matches(&attribute.name, name));
    }
}

/// Keeps the attributes `keep` gives a value for (given each name and value), with that value.
pub fn retain_attrs(node: &NodeRef, mut keep: impl FnMut(&str, &str) -> Option<String>) {
    if let NodeData::Element { attrs, .. } = &node.data {
        attrs.borrow_mut().retain_mut(|attribute| {
            let qualified = attr_name(&attribute.name);
            match keep(&qualified, &attribute.value) {
                Some(value) => {
                    if *attribute.value != *value {
                        attribute.value = value.into();
                    }
                    true
                }
                None => false,
            }
        });
    }
}

/// Whether the element's `class` contains `class`.
pub fn has_class(node: &NodeRef, class: &str) -> bool {
    attr(node, "class").is_some_and(|classes| classes.split_ascii_whitespace().any(|c| c == class))
}

/// Adds classes (in order, skipping ones already there).
pub fn add_class(node: &NodeRef, classes: &[&str]) {
    let mut list: Vec<String> = attr(node, "class")
        .map(|value| value.split_ascii_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    for class in classes {
        if !list.iter().any(|c| c == class) {
            list.push((*class).to_owned());
        }
    }
    set_attr(node, "class", &list.join(" "));
}

/// Removes classes.
pub fn remove_class(node: &NodeRef, classes: &[&str]) {
    if let Some(value) = attr(node, "class")
        && value
            .split_ascii_whitespace()
            .any(|class| classes.contains(&class))
    {
        let list: Vec<&str> = value
            .split_ascii_whitespace()
            .filter(|class| !classes.contains(class))
            .collect();
        set_attr(node, "class", &list.join(" "));
    }
}

/// The node's parent.
pub fn parent(node: &NodeRef) -> Option<NodeRef> {
    let weak = node.parent.take();
    let parent = weak.as_ref().and_then(|weak| weak.upgrade());
    node.parent.set(weak);
    parent
}

/// The node's children.
pub fn children(node: &NodeRef) -> Vec<NodeRef> {
    node.children.borrow().clone()
}

/// The node's element children.
pub fn element_children(node: &NodeRef) -> Vec<NodeRef> {
    node.children
        .borrow()
        .iter()
        .filter(|c| is_element(c))
        .cloned()
        .collect()
}

/// Takes the node's children out, for [`set_children`] to put back edited. Their parent links
/// stay until then.
pub fn take_children(node: &NodeRef) -> Vec<NodeRef> {
    std::mem::take(&mut *node.children.borrow_mut())
}

/// Makes `children` the node's children.
///
/// Editing the child list in one go keeps passes over long sibling lists linear. The tree has no
/// sibling links, so each single-node edit has to find the node among its siblings first.
pub fn set_children(node: &NodeRef, children: Vec<NodeRef>) {
    for child in &children {
        child.parent.set(Some(Rc::downgrade(node)));
    }
    *node.children.borrow_mut() = children;
}

fn index_in_parent(node: &NodeRef) -> Option<(NodeRef, usize)> {
    let parent = parent(node)?;
    let index = parent
        .children
        .borrow()
        .iter()
        .position(|c| Rc::ptr_eq(c, node))?;
    Some((parent, index))
}

/// The siblings after the node, in order.
pub fn following_siblings(node: &NodeRef) -> Vec<NodeRef> {
    index_in_parent(node)
        .map(|(parent, index)| parent.children.borrow()[index + 1..].to_vec())
        .unwrap_or_default()
}

/// The siblings before the node, in order.
pub fn preceding_siblings(node: &NodeRef) -> Vec<NodeRef> {
    index_in_parent(node)
        .map(|(parent, index)| parent.children.borrow()[..index].to_vec())
        .unwrap_or_default()
}

/// The previous element sibling.
pub fn previous_element_sibling(node: &NodeRef) -> Option<NodeRef> {
    preceding_siblings(node).into_iter().rev().find(is_element)
}

/// Takes the node out of the tree.
pub fn detach(node: &NodeRef) {
    if let Some((parent, index)) = index_in_parent(node) {
        parent.children.borrow_mut().remove(index);
    }
    node.parent.set(None);
}

/// Takes the nodes out of the tree, visiting each parent's child list once.
pub fn detach_all(nodes: &[NodeRef]) {
    let gone: HashSet<NodeKey> = nodes.iter().map(key).collect();
    for parent in parents_of(nodes) {
        parent
            .children
            .borrow_mut()
            .retain(|child| !gone.contains(&key(child)));
    }
    for node in nodes {
        node.parent.set(None);
    }
}

/// The nodes' parents, each once.
pub fn parents_of(nodes: &[NodeRef]) -> Vec<NodeRef> {
    let mut seen = HashSet::new();
    nodes
        .iter()
        .filter_map(parent)
        .filter(|parent| seen.insert(key(parent)))
        .collect()
}

/// Appends `child` to `parent`.
pub fn append(parent: &NodeRef, child: &NodeRef) {
    detach(child);
    child.parent.set(Some(Rc::downgrade(parent)));
    parent.children.borrow_mut().push(child.clone());
}

/// Prepends `child` as `parent`'s first child.
pub fn prepend(parent: &NodeRef, child: &NodeRef) {
    detach(child);
    child.parent.set(Some(Rc::downgrade(parent)));
    parent.children.borrow_mut().insert(0, child.clone());
}

/// Inserts `node` before `reference`.
pub fn insert_before(reference: &NodeRef, node: &NodeRef) {
    insert_at(reference, 0, node);
}

/// Inserts `node` after `reference`.
pub fn insert_after(reference: &NodeRef, node: &NodeRef) {
    insert_at(reference, 1, node);
}

fn insert_at(reference: &NodeRef, offset: usize, node: &NodeRef) {
    detach(node);
    if let Some((parent, index)) = index_in_parent(reference) {
        node.parent.set(Some(Rc::downgrade(&parent)));
        parent
            .children
            .borrow_mut()
            .insert(index + offset, node.clone());
    }
}

/// Puts each node's replacements (new nodes, not ones in the tree) where it is, editing each
/// parent's child list once.
pub fn replace_all(mut replacements: Vec<(NodeRef, Vec<NodeRef>)>) {
    let nodes: Vec<NodeRef> = replacements.iter().map(|(node, _)| node.clone()).collect();
    let index: HashMap<NodeKey, usize> = replacements
        .iter()
        .enumerate()
        .map(|(i, (node, _))| (key(node), i))
        .collect();
    for parent in parents_of(&nodes) {
        let mut children = Vec::new();
        for child in take_children(&parent) {
            match index.get(&key(&child)) {
                Some(&i) => children.append(&mut replacements[i].1),
                None => children.push(child),
            }
        }
        set_children(&parent, children);
    }
    for node in &nodes {
        node.parent.set(None);
    }
}

/// Merges adjacent text nodes and drops empty ones, throughout the subtree (`Node.normalize`).
pub fn normalize(node: &NodeRef) {
    let mut kept: Vec<NodeRef> = Vec::new();
    for child in take_children(node) {
        match text(&child) {
            Some(contents) if contents.is_empty() => {}
            Some(contents) => match kept.last().filter(|last| is_text(last)) {
                Some(previous) => {
                    let merged = text(previous).unwrap_or_default() + &contents;
                    set_text(previous, &merged);
                }
                None => kept.push(child),
            },
            None => {
                normalize(&child);
                kept.push(child);
            }
        }
    }
    set_children(node, kept);
}

/// Replaces each of `nodes` with its children, editing each affected child list once, and
/// returns the parents those children ended up in.
pub fn unwrap_all(nodes: &[NodeRef]) -> Vec<NodeRef> {
    let gone: HashSet<NodeKey> = nodes.iter().map(key).collect();
    let mut parents = parents_of(nodes);
    parents.retain(|parent| !gone.contains(&key(parent)));
    for parent in &parents {
        let mut children = Vec::new();
        let mut stack: Vec<NodeRef> = take_children(parent).into_iter().rev().collect();
        while let Some(node) = stack.pop() {
            if gone.contains(&key(&node)) {
                stack.extend(take_children(&node).into_iter().rev());
                drop_tree(node);
            } else {
                children.push(node);
            }
        }
        set_children(parent, children);
    }
    parents
}

/// Every node under `node`, in document order (not `node` itself).
pub fn descendants(node: &NodeRef) -> impl Iterator<Item = NodeRef> {
    let mut stack: Vec<NodeRef> = children(node).into_iter().rev().collect();
    std::iter::from_fn(move || {
        let next = stack.pop()?;
        stack.extend(children(&next).into_iter().rev());
        Some(next)
    })
}

/// The elements under `node` that `matches` accepts, in document order (`querySelectorAll`).
pub fn select(node: &NodeRef, matches: impl Fn(&NodeRef) -> bool) -> Vec<NodeRef> {
    descendants(node)
        .filter(|n| is_element(n) && matches(n))
        .collect()
}

/// The first descendant element `matches` accepts.
pub fn select_first(node: &NodeRef, matches: impl Fn(&NodeRef) -> bool) -> Option<NodeRef> {
    descendants(node).find(|n| is_element(n) && matches(n))
}

/// The nearest inclusive ancestor element `matches` accepts (`closest`).
pub fn closest(node: &NodeRef, matches: impl Fn(&NodeRef) -> bool) -> Option<NodeRef> {
    let mut current = Some(node.clone());
    while let Some(candidate) = current {
        if is_element(&candidate) && matches(&candidate) {
            return Some(candidate);
        }
        current = parent(&candidate);
    }
    None
}

/// The concatenated text of every text node under `node` (`textContent`).
pub fn text_content(node: &NodeRef) -> String {
    if let Some(text) = text(node) {
        return text;
    }
    descendants(node).filter_map(|n| text(&n)).collect()
}

/// The text of `html` read as a document (the parsed document's text content): its entities
/// decoded, its tags dropped.
pub fn html_text(html: &str) -> String {
    let document = parse(html);
    let text = text_content(&document);
    drop_tree(document);
    text
}

/// The document's `<body>` (or `<html>` when a fragment has none).
pub fn body(document: &NodeRef) -> Option<NodeRef> {
    let html = element_children(document)
        .into_iter()
        .find(|n| is(n, "html"))?;
    element_children(&html)
        .into_iter()
        .find(|n| is(n, "body"))
        .or(Some(html))
}

/// The document's `<head>`.
pub fn head(document: &NodeRef) -> Option<NodeRef> {
    let html = element_children(document)
        .into_iter()
        .find(|n| is(n, "html"))?;
    element_children(&html).into_iter().find(|n| is(n, "head"))
}

/// The declarations of an inline `style` attribute, as `CSSStyleDeclaration` exposes them.
#[derive(Clone, Debug, Default)]
pub struct Style {
    declarations: Vec<(String, String, bool)>,
}

impl Style {
    /// The element's inline style.
    pub fn of(node: &NodeRef) -> Self {
        attr(node, "style")
            .map(|css| Self::parse(&css))
            .unwrap_or_default()
    }

    /// Parses a declaration list, ignoring semicolons inside parentheses and quotes.
    pub fn parse(css: &str) -> Self {
        let mut declarations: Vec<(String, String, bool)> = Vec::new();
        for declaration in split_top_level(css, ';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            let mut value = value.trim().to_owned();
            let important = value.to_ascii_lowercase().ends_with("!important");
            if important {
                value.truncate(value.len() - "!important".len());
                value = value.trim_end().to_owned();
            }
            if name.is_empty() || value.is_empty() {
                continue;
            }
            declarations.push((name, value, important));
        }
        // A property declared again keeps only its last declaration, in that one's place.
        let mut seen = HashSet::new();
        declarations.reverse();
        declarations.retain(|(name, _, _)| seen.insert(name.clone()));
        declarations.reverse();
        Self { declarations }
    }

    /// A property's value (`""` when unset, as the DOM returns it). The `border` shorthand
    /// only reads back when it was set as a shorthand.
    pub fn get(&self, name: &str) -> &str {
        self.declarations
            .iter()
            .find(|(n, _, _)| n == name)
            .map_or("", |(_, value, _)| value.as_str())
    }

    /// Whether a property is set with `!important`.
    pub fn is_important(&self, name: &str) -> bool {
        self.declarations
            .iter()
            .any(|(n, _, important)| n == name && *important)
    }

    /// Sets a property, updating an existing declaration in place (as the CSSOM does) or appending
    /// it if unset.
    pub fn set(&mut self, name: &str, value: &str, important: bool) {
        match self.declarations.iter_mut().find(|(n, _, _)| n == name) {
            Some((_, existing, existing_important)) => {
                value.clone_into(existing);
                *existing_important = important;
            }
            None => self
                .declarations
                .push((name.to_owned(), value.to_owned(), important)),
        }
    }

    /// Removes a property.
    pub fn remove(&mut self, name: &str) {
        self.declarations.retain(|(n, _, _)| n != name);
    }

    /// Whether no property is set.
    pub fn is_empty(&self) -> bool {
        self.declarations.is_empty()
    }

    /// Iterates `(property, value, important)` declarations in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, bool)> {
        self.declarations
            .iter()
            .map(|(name, value, important)| (name.as_str(), value.as_str(), *important))
    }

    /// The declarations, serialized the way the DOM does.
    pub fn css(&self) -> String {
        let css: Vec<String> = self
            .declarations
            .iter()
            .map(|(name, value, important)| {
                format!(
                    "{name}: {value}{};",
                    if *important { " !important" } else { "" }
                )
            })
            .collect();
        css.join(" ")
    }

    /// Writes the declarations back to `node`'s `style`.
    pub fn apply(&self, node: &NodeRef) {
        set_attr(node, "style", &self.css());
    }
}

/// Splits on `separator` outside parentheses and quotes.
fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut escaped, mut start) = (0usize, None, false, 0);
    for (i, c) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, c) {
            (Some(_), '\\') => escaped = true,
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, _) if c == separator && depth == 0 => {
                parts.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_serializes() {
        let document = parse("<p class=a>Hi <b>there</b></p>");
        let body = body(&document).unwrap();
        assert_eq!(inner_html(&body), "<p class=\"a\">Hi <b>there</b></p>");
        let p = element_children(&body).remove(0);
        add_class(&p, &["b", "a"]);
        assert_eq!(attr(&p, "class").unwrap(), "a b");
        unwrap_all(&[p]);
        assert_eq!(inner_html(&body), "Hi <b>there</b>");
    }

    #[test]
    fn reads_a_documents_text() {
        assert_eq!(
            html_text("Tom &amp; Ann&#39;s <b>call</b> &lt;3"),
            "Tom & Ann's call <3"
        );
    }

    #[test]
    fn reads_inline_styles() {
        let style = Style::parse("color: RED; background: url(a;b.png) ; COLOR:blue !important");
        assert_eq!(style.get("color"), "blue");
        assert!(style.is_important("color"));
        assert_eq!(style.get("background"), "url(a;b.png)");
        assert_eq!(style.get("border"), "");
    }
}
