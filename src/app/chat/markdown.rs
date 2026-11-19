//! CommonMark/GFM parsed by pulldown-cmark and drawn as native GPUI elements.
//!
//! Parsed documents are retained between frames. HTML stays inert text, images
//! stay alt text, and external HTTP(S) links open only after an explicit click.

use gpui::{
    ClipboardItem, Div, ElementId, Font, FontStyle, FontWeight, InteractiveText, SharedString,
    StrikethroughStyle, StyledText, TextAlign, TextRun, UnderlineStyle, div, prelude::*, px, rgb,
    rgba,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag};
use std::ops::Range;

#[cfg(all(test, windows))]
#[path = "markdown_native_qa.rs"]
mod native_qa;

pub(crate) struct MarkdownStyle {
    pub font_family: SharedString,
    pub font_size: f32,
    pub line_height: f32,
    pub text_color: u32,
    pub muted_color: u32,
    pub link_color: u32,
    pub code_background: u32,
    pub rule_color: u32,
}

#[derive(Debug, Default)]
pub(crate) struct MarkdownDocument {
    nodes: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    Container(Tag<'static>, Vec<Node>),
    Leaf(Event<'static>),
}

impl MarkdownDocument {
    /// pulldown-cmark balances start/end events even for unfinished streams.
    pub(crate) fn parse(source: &str) -> Self {
        let options =
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        let mut roots = Vec::new();
        let mut stack: Vec<(Tag<'static>, Vec<Node>)> = Vec::new();
        // Keep native layout/paint and Drop recursion bounded for model output.
        // At the limit, text is preserved while extra container styling flattens.
        let mut flattened = 0usize;
        for event in Parser::new_ext(source, options) {
            let node = match event {
                Event::Start(tag) => {
                    if flattened > 0 || stack.len() >= 48 {
                        flattened += 1;
                    } else {
                        stack.push((tag.into_static(), Vec::new()));
                    }
                    continue;
                }
                Event::End(_) if flattened > 0 => {
                    flattened -= 1;
                    continue;
                }
                Event::End(_) => {
                    let Some((tag, children)) = stack.pop() else {
                        continue;
                    };
                    Node::Container(tag, children)
                }
                event => Node::Leaf(event.into_static()),
            };
            if let Some((_, children)) = stack.last_mut() {
                children.push(node);
            } else {
                roots.push(node);
            }
        }
        Self { nodes: roots }
    }

    /// Each message needs its own scope for link/code-scroll element state.
    pub(crate) fn render(&self, id: impl Into<ElementId>, style: &MarkdownStyle) -> Div {
        let mut renderer = Renderer { style, next_id: 0 };
        div().w_full().min_w_0().flex_shrink_0().child(
            div()
                .id(id)
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .child(renderer.blocks(&self.nodes)),
        )
    }
}

/// Compatibility helper for the single short appearance preview.
pub(crate) fn render(source: &str, style: &MarkdownStyle) -> Div {
    MarkdownDocument::parse(source).render("markdown-preview", style)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: Option<String>,
}

#[derive(Debug)]
struct Span {
    text: String,
    style: InlineStyle,
}

fn push_span(spans: &mut Vec<Span>, text: &str, style: &InlineStyle) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.style == *style
    {
        last.text.push_str(text);
    } else {
        spans.push(Span {
            text: text.to_owned(),
            style: style.clone(),
        });
    }
}

fn web_link(target: &str) -> Option<String> {
    if target.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(target).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.into())
}

fn inline_spans(nodes: &[Node], base: InlineStyle) -> Vec<Span> {
    fn collect(nodes: &[Node], style: &InlineStyle, spans: &mut Vec<Span>) {
        for node in nodes {
            match node {
                Node::Container(tag, children) => {
                    let mut inner = style.clone();
                    match tag {
                        Tag::Strong => inner.bold = true,
                        Tag::Emphasis => inner.italic = true,
                        Tag::Strikethrough => inner.strike = true,
                        Tag::Link { dest_url, .. } => inner.link = web_link(dest_url),
                        Tag::Image { .. } => {
                            inner.link = None;
                            push_span(spans, "[图片：", &inner);
                            collect(children, &inner, spans);
                            push_span(spans, "]", &inner);
                            continue;
                        }
                        _ => {}
                    }
                    collect(children, &inner, spans);
                }
                Node::Leaf(event) => match event {
                    Event::Text(text) => push_span(spans, text, style),
                    Event::Code(text) => push_span(
                        spans,
                        text,
                        &InlineStyle {
                            code: true,
                            ..style.clone()
                        },
                    ),
                    Event::SoftBreak => push_span(spans, " ", style),
                    Event::HardBreak => push_span(spans, "\n", style),
                    Event::Html(text) | Event::InlineHtml(text) => push_span(spans, text, style),
                    Event::TaskListMarker(checked) => {
                        push_span(spans, if *checked { "☑ " } else { "☐ " }, style);
                    }
                    Event::FootnoteReference(label) => {
                        push_span(spans, &format!("[{label}]"), style)
                    }
                    Event::InlineMath(text) | Event::DisplayMath(text) => {
                        push_span(spans, text, style)
                    }
                    _ => {}
                },
            }
        }
    }
    let mut spans = Vec::new();
    collect(nodes, &base, &mut spans);
    spans
}

struct TextContent {
    text: String,
    runs: Vec<TextRun>,
    links: Vec<(Range<usize>, String)>,
}

fn inline_runs(spans: &[Span], style: &MarkdownStyle) -> TextContent {
    let mut text = String::new();
    let mut runs = Vec::with_capacity(spans.len());
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    for span in spans {
        let start = text.len();
        text.push_str(&span.text);
        if let Some(url) = &span.style.link {
            if let Some((range, previous_url)) = links.last_mut()
                && *previous_url == *url
                && range.end == start
            {
                range.end = text.len();
            } else {
                links.push((start..text.len(), url.clone()));
            }
        }
        let color = rgb(if span.style.link.is_some() {
            style.link_color
        } else {
            style.text_color
        })
        .into();
        runs.push(TextRun {
            len: span.text.len(),
            font: Font {
                family: if span.style.code {
                    crate::app::appearance::mono_font_family().into()
                } else {
                    style.font_family.clone()
                },
                features: Default::default(),
                fallbacks: None,
                weight: if span.style.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::NORMAL
                },
                style: if span.style.italic {
                    FontStyle::Italic
                } else {
                    FontStyle::Normal
                },
            },
            color,
            background_color: span.style.code.then(|| rgba(style.code_background).into()),
            underline: span.style.link.as_ref().map(|_| UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: span.style.strike.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        });
    }
    TextContent { text, runs, links }
}

fn is_inline(node: &Node) -> bool {
    match node {
        Node::Container(tag, _) => matches!(
            tag,
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
        ),
        Node::Leaf(event) => !matches!(event, Event::Rule),
    }
}

fn starts_with_task(nodes: &[Node]) -> bool {
    match nodes.first() {
        Some(Node::Leaf(Event::TaskListMarker(_))) => true,
        Some(Node::Container(Tag::Paragraph, children)) => starts_with_task(children),
        _ => false,
    }
}

struct Renderer<'a> {
    style: &'a MarkdownStyle,
    next_id: usize,
}

impl Renderer<'_> {
    fn id(&mut self) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn blocks(&mut self, nodes: &[Node]) -> Div {
        let mut result = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(self.style.line_height * 0.4));
        let mut i = 0;
        while i < nodes.len() {
            if is_inline(&nodes[i]) {
                let start = i;
                while i < nodes.len() && is_inline(&nodes[i]) {
                    i += 1;
                }
                result = result.child(self.inline(&nodes[start..i], 1., false));
            } else {
                result = result.child(self.block(&nodes[i]));
                i += 1;
            }
        }
        result
    }

    fn inline(&mut self, nodes: &[Node], scale: f32, bold: bool) -> Div {
        let content = inline_runs(
            &inline_spans(
                nodes,
                InlineStyle {
                    bold,
                    ..Default::default()
                },
            ),
            self.style,
        );
        let container = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .text_size(px(self.style.font_size * scale))
            .line_height(px(self.style.line_height * scale))
            .text_color(rgb(self.style.text_color));
        if content.text.is_empty() {
            return container;
        }
        #[cfg(all(test, windows))]
        let probe_source = content.text.clone();
        let text = StyledText::new(content.text).with_runs(content.runs);
        #[cfg(all(test, windows))]
        let container = native_qa::observe_text(container, probe_source, text.layout().clone());
        if content.links.is_empty() {
            container.child(text)
        } else {
            let (ranges, urls): (Vec<_>, Vec<_>) = content.links.into_iter().unzip();
            container.child(
                InteractiveText::new(("markdown-link", self.id()), text).on_click(
                    ranges,
                    move |index, _, cx| {
                        if let Some(url) = urls.get(index) {
                            cx.open_url(url);
                        }
                    },
                ),
            )
        }
    }

    fn block(&mut self, node: &Node) -> Div {
        let style = self.style;
        match node {
            Node::Container(tag, children) => match tag {
                Tag::Heading { level, .. } => {
                    let scale = match *level as u8 {
                        1 => 1.5,
                        2 => 1.3,
                        3 => 1.15,
                        _ => 1.,
                    };
                    self.inline(children, scale, true)
                }
                Tag::Paragraph => self
                    .inline(children, 1., false)
                    .debug_selector(|| "md-paragraph".into()),
                Tag::CodeBlock(kind) => self.code(kind, children),
                Tag::BlockQuote(_) => div()
                    .w_full()
                    .min_w_0()
                    .border_l_3()
                    .border_color(rgba(style.rule_color))
                    .pl(px(12.))
                    .child(self.blocks(children)),
                Tag::List(start) => self.list(*start, children),
                Tag::Table(alignments) => self.table(alignments, children),
                Tag::HtmlBlock => self.inline(children, 1., false),
                _ => self.blocks(children),
            },
            Node::Leaf(Event::Rule) => div()
                .w_full()
                .h(px(1.))
                .my(px(4.))
                .bg(rgba(style.rule_color)),
            _ => self.inline(std::slice::from_ref(node), 1., false),
        }
    }

    fn code(&mut self, kind: &CodeBlockKind<'_>, children: &[Node]) -> Div {
        let style = self.style;
        let language = match kind {
            CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
            CodeBlockKind::Indented => "",
        };
        let code: String = children
            .iter()
            .filter_map(|node| match node {
                Node::Leaf(Event::Text(text)) => Some(text.as_ref()),
                _ => None,
            })
            .collect();
        let copied = code.clone();
        let id = self.id();
        let border = style.rule_color;
        let link = style.link_color;
        div()
            .w_full()
            .min_w_0()
            .rounded(px(6.))
            .border_1()
            .border_color(rgba(style.rule_color))
            .bg(rgba(style.code_background))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(12.))
                    .py(px(3.))
                    .text_size(px(style.font_size * 0.75))
                    .text_color(rgb(style.muted_color))
                    .child(
                        if language.is_empty() {
                            "代码"
                        } else {
                            language
                        }
                        .to_owned(),
                    )
                    .child(
                        div()
                            .id(("markdown-copy-code", id))
                            .tab_index(0)
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(rgba(0))
                            .focus(|button| button.border_color(rgb(link)))
                            .cursor_pointer()
                            .hover(|button| button.bg(rgba(border)))
                            .active(|button| button.bg(rgba(border)).opacity(0.75))
                            .child("复制代码")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                            }),
                    ),
            )
            .child(
                div()
                    .id(("markdown-code-scroll", id))
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .px(px(12.))
                    .pb(px(10.))
                    .font_family(crate::app::appearance::mono_font_family())
                    .text_size(px(style.font_size * 0.88))
                    .line_height(px(style.line_height * 0.92))
                    .text_color(rgb(style.text_color))
                    .whitespace_nowrap()
                    .child(code.trim_end_matches('\n').to_owned()),
            )
    }

    fn list(&mut self, start: Option<u64>, children: &[Node]) -> Div {
        let mut list = div()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(self.style.line_height * 0.25));
        let mut index = start;
        for child in children {
            let Node::Container(Tag::Item, content) = child else {
                // Container tags beyond the nesting limit are flattened, but
                // their text must still be visible rather than skipped.
                list = list.child(self.block(child));
                continue;
            };
            let marker = index.map_or_else(|| "•".to_owned(), |index| format!("{index}."));
            if let Some(index) = index.as_mut() {
                *index = index.saturating_add(1);
            }
            if starts_with_task(content) {
                list = list.child(self.blocks(content));
                continue;
            }
            let item_id = self.id();
            // Hanging indentation keeps text in normal block flow. A zero-basis
            // flex child can be measured at min-content width and retain a
            // one-character wrapping width during later native text layout.
            let marker_width =
                self.style.font_size * (marker.chars().count() as f32 * 0.65).max(1.4);
            let row = div()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .relative()
                .pl(px(marker_width + 8.))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .w(px(marker_width))
                        .text_align(TextAlign::Right)
                        .whitespace_nowrap()
                        .text_size(px(self.style.font_size))
                        .line_height(px(self.style.line_height))
                        .text_color(rgb(self.style.muted_color))
                        .child(marker),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .debug_selector(move || format!("md-list-content-{item_id}"))
                        .child(self.blocks(content)),
                );
            #[cfg(all(test, windows))]
            let row = native_qa::observe_children(row, "list-row");
            list = list.child(row);
        }
        list
    }

    fn table(&mut self, aligns: &[Alignment], children: &[Node]) -> Div {
        let mut rows = div()
            .w_full()
            .flex_shrink_0()
            .min_w(px((aligns.len().max(1) as f32 * 120.).max(240.)))
            .flex()
            .flex_col()
            .border_1()
            .border_color(rgba(self.style.rule_color))
            .rounded(px(5.));
        for node in children {
            if let Node::Container(tag @ (Tag::TableHead | Tag::TableRow), cells) = node {
                let header = matches!(tag, Tag::TableHead);
                let mut row = div()
                    .w_full()
                    .flex_shrink_0()
                    .flex()
                    .border_b_1()
                    .border_color(rgba(self.style.rule_color));
                if header {
                    row = row.bg(rgba(self.style.code_background));
                }
                for (i, cell) in cells.iter().enumerate() {
                    let Node::Container(Tag::TableCell, text) = cell else {
                        continue;
                    };
                    row = row.child(
                        div()
                            .w(gpui::relative(1. / aligns.len().max(1) as f32))
                            .flex_shrink_0()
                            .min_w_0()
                            .debug_selector(move || format!("md-table-cell-{i}"))
                            .px(px(10.))
                            .py(px(7.))
                            .text_align(match aligns.get(i) {
                                Some(Alignment::Center) => TextAlign::Center,
                                Some(Alignment::Right) => TextAlign::Right,
                                _ => TextAlign::Left,
                            })
                            .child(self.inline(text, 0.92, header)),
                    );
                }
                rows = rows.child(row);
            }
        }
        div().w_full().min_w_0().child(
            div()
                .id(("markdown-table-scroll", self.id()))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                .child(rows),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(nodes: &[Node]) -> String {
        inline_spans(nodes, Default::default())
            .into_iter()
            .map(|span| span.text)
            .collect()
    }

    #[test]
    fn commonmark_handles_nested_emphasis_escapes_entities_and_reference_links() {
        let document = MarkdownDocument::parse(
            r#"## 标题

**粗体与 *斜体***，\*字面星号\*，a_b_c &amp; [文档][docs]。

[docs]: https://example.com/guide_(v2)
"#,
        );
        assert!(matches!(
            document.nodes[0],
            Node::Container(Tag::Heading { .. }, _)
        ));
        let Node::Container(Tag::Paragraph, children) = &document.nodes[1] else {
            panic!()
        };
        let spans = inline_spans(children, Default::default());
        assert!(
            spans
                .iter()
                .any(|s| s.text == "斜体" && s.style.bold && s.style.italic)
        );
        assert!(visible(children).contains("*字面星号*，a_b_c & 文档"));
        assert!(
            spans
                .iter()
                .any(|s| s.style.link.as_deref() == Some("https://example.com/guide_(v2)"))
        );
    }

    #[test]
    fn preserves_nested_lists_block_content_and_task_markers() {
        let document = MarkdownDocument::parse(
            "3. 第一项\n   - [x] 子项\n   - [ ] 下一项\n\n4. 第二项\n\n   > 引用\n",
        );
        let Node::Container(Tag::List(Some(3)), items) = &document.nodes[0] else {
            panic!()
        };
        assert_eq!(items.len(), 2);
        let Node::Container(Tag::Item, first) = &items[0] else {
            panic!()
        };
        assert!(
            first
                .iter()
                .any(|node| matches!(node, Node::Container(Tag::List(None), _)))
        );
        let Node::Container(Tag::Item, second) = &items[1] else {
            panic!()
        };
        assert!(
            second
                .iter()
                .any(|node| matches!(node, Node::Container(Tag::BlockQuote(_), _)))
        );
        assert!(visible(first).contains("☑ 子项"));
        assert!(visible(first).contains("☐ 下一项"));
    }

    #[test]
    fn gfm_table_keeps_escaped_pipes_inline_formatting_and_alignment() {
        let document =
            MarkdownDocument::parse("| 名称 | 说明 |\n| :--- | ---: |\n| a\\|b | **粗体** |\n");
        let Node::Container(Tag::Table(aligns), rows) = &document.nodes[0] else {
            panic!()
        };
        assert_eq!(aligns, &[Alignment::Left, Alignment::Right]);
        let Node::Container(Tag::TableRow, cells) = &rows[1] else {
            panic!()
        };
        assert_eq!(visible(std::slice::from_ref(&cells[0])), "a|b");
        assert!(
            inline_spans(std::slice::from_ref(&cells[1]), Default::default())
                .iter()
                .any(|s| s.style.bold)
        );
    }

    #[test]
    fn every_unicode_stream_prefix_is_safe_and_unclosed_fence_retains_content() {
        let reply = "# 答案\r\n\r\n**中文** [文档](https://example.com)\n\n```rust\nfn main() { println!(\"你好\"); }\n";
        for end in reply.char_indices().map(|(i, _)| i).chain([reply.len()]) {
            let _ = MarkdownDocument::parse(&reply[..end]);
        }
        let document = MarkdownDocument::parse(reply);
        let Node::Container(Tag::CodeBlock(_), children) = document.nodes.last().unwrap() else {
            panic!()
        };
        assert!(visible(children).contains("println!(\"你好\")"));
    }

    #[test]
    fn hard_breaks_and_inline_code_preserve_commonmark_text() {
        let document =
            MarkdownDocument::parse("第一行  \n第二行\\\n第三行\n软换行 `a * b` ~~删除~~");
        let spans = inline_spans(&document.nodes, Default::default());
        assert_eq!(
            visible(&document.nodes),
            "第一行\n第二行\n第三行 软换行 a * b 删除"
        );
        assert!(spans.iter().any(|s| s.style.code && s.text == "a * b"));
        assert!(spans.iter().any(|s| s.style.strike && s.text == "删除"));
    }

    #[test]
    fn links_are_http_only_and_html_images_remain_inert_text() {
        let document = MarkdownDocument::parse(
            "[危险](javascript:alert(1)) [文件](file:///C:/secrets.txt) [终端](moin://run) ![图像](https://example.com/track.png) <https://example.com/中文> <script>alert(1)</script>",
        );
        let spans = inline_spans(&document.nodes, Default::default());
        let links: Vec<_> = spans
            .iter()
            .filter_map(|s| s.style.link.as_deref())
            .collect();
        assert_eq!(links.len(), 1);
        assert!(links[0].starts_with("https://example.com/"));
        assert!(visible(&document.nodes).contains("[图片：图像]"));
        assert!(visible(&document.nodes).contains("<script>alert(1)</script>"));
        assert!(web_link("https://user:pass@example.com/").is_none());
        assert!(web_link("https://example.com/\n").is_none());
    }

    #[test]
    fn link_ranges_use_utf8_bytes_and_keep_formatted_link_together() {
        let document = MarkdownDocument::parse("中文 [**点此**查看](https://example.com) 后续");
        let style = MarkdownStyle {
            font_family: "test".into(),
            font_size: 16.,
            line_height: 24.,
            text_color: 0xffffff,
            muted_color: 0x999999,
            link_color: 0x88bbff,
            code_background: 0x222222ff,
            rule_color: 0x444444ff,
        };
        let content = inline_runs(&inline_spans(&document.nodes, Default::default()), &style);
        assert_eq!(content.links.len(), 1);
        assert_eq!(&content.text[content.links[0].0.clone()], "点此查看");
        assert_eq!(
            content.runs.iter().map(|run| run.len).sum::<usize>(),
            content.text.len()
        );
    }

    #[test]
    fn deeply_nested_model_output_is_bounded_without_losing_text() {
        let document = MarkdownDocument::parse(&format!("{}正文", "> ".repeat(2048)));
        fn depth(nodes: &[Node]) -> usize {
            nodes
                .iter()
                .map(|node| match node {
                    Node::Container(_, children) => 1 + depth(children),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        assert!(depth(&document.nodes) <= 48);
        assert_eq!(visible(&document.nodes), "正文");
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use gpui::{App, Context, Render, TestAppContext, Window};
    struct Fixture {
        width: f32,
        document: MarkdownDocument,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let style = MarkdownStyle {
                font_family: crate::app::appearance::font_family().into(),
                font_size: 18.,
                line_height: 28.,
                text_color: 0xffffff,
                muted_color: 0xaaaaaa,
                link_color: 0x99bbff,
                code_background: 0x222222ff,
                rule_color: 0xffffff30,
            };
            div()
                .id("fixture-scroll")
                .w(px(self.width))
                .h(px(900.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex_shrink_0()
                        .w_full()
                        .child(self.document.render("fixture", &style)),
                )
        }
    }
    #[test]
    fn markdown_lists_keep_readable_width_and_do_not_overlap_following_text() {
        for width in [420., 760., 1100.] {
            let mut app = TestAppContext::single();
            let (_,cx)=app.add_window_view(|_,_|Fixture{width,document:MarkdownDocument::parse("## 本地 DNS\n\n- 192.168.1.1：本机网络使用的 DNS 服务器，长说明应该正常换行。\n- 192.168.1.2：第二台服务器。\n\n## 本机情况\n\n本机没有运行 DNS 服务，下面是正常的后续段落。")});
            cx.update(|window, cx: &mut App| {
                let _ = window.draw(cx);
            });
            let first = cx.debug_bounds("md-list-content-0").expect("list content");
            let second = cx
                .debug_bounds("md-list-content-1")
                .expect("second list item");
            let following = cx
                .debug_bounds("md-paragraph")
                .expect("following paragraph");
            assert!(
                first.size.width > px(width - 100.),
                "list collapsed at {width}: {first:?}"
            );
            assert!(
                first.bottom() <= second.top() + px(1.),
                "list items overlap: {first:?} {second:?}"
            );
            assert!(
                second.bottom() <= following.top() + px(1.),
                "list overlaps following paragraph: {second:?} {following:?}"
            );
        }
    }
    #[test]
    fn markdown_table_columns_have_stable_width_without_overlapping() {
        for width in [420., 760., 1100.] {
            let mut app = TestAppContext::single();
            let (_,cx)=app.add_window_view(|_,_|Fixture{width,document:MarkdownDocument::parse("| 网卡 | IPv4 DNS | 说明 |\n| --- | --- | --- |\n| Wi-Fi | `192.168.1.1` | 本机的网络配置 |\n\n后续段落。")});
            cx.update(|window, cx: &mut App| {
                let _ = window.draw(cx);
            });
            let a = cx.debug_bounds("md-table-cell-0").unwrap();
            let b = cx.debug_bounds("md-table-cell-1").unwrap();
            let c = cx.debug_bounds("md-table-cell-2").unwrap();
            assert!(a.size.width > px(100.) && b.size.width > px(100.) && c.size.width > px(100.));
            assert!(a.right() <= b.left() + px(1.) && b.right() <= c.left() + px(1.));
            assert!(a.bottom() <= cx.debug_bounds("md-paragraph").unwrap().top() + px(1.));
        }
    }
}
