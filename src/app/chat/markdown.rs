//! Markdown and other long-form rich text are rendered by GPUI Kit's `TextView`.
//!
//! Parsing, layout, selection, links, tables and code highlighting live in
//! `gpui_kit::component::text`; this module only maps the app's typography.

use gpui::{ClipboardItem, ElementId, SharedString, div, prelude::*, px, rgb};
use gpui_kit::component::text::{markdown, TextView};
use std::hash::{Hash, Hasher};

pub(crate) struct MarkdownStyle {
    pub font_family: SharedString,
    pub font_size: f32,
    pub line_height: f32,
    pub text_color: u32,
    pub muted_color: u32,
}

pub(crate) struct MarkdownDocument {
    source: SharedString,
}

impl MarkdownDocument {
    pub(crate) fn parse(source: &str) -> Self {
        Self {
            source: source.to_owned().into(),
        }
    }

    /// Each message keeps its own element id so selection and link state stay scoped.
    /// Callers must pass an id that is stable for one message and never reused for a
    /// different message, otherwise stale layout/selection state is carried over.
    pub(crate) fn render(&self, id: impl Into<ElementId>, style: &MarkdownStyle) -> TextView {
        styled(TextView::markdown(id, self.source.clone()), style)
    }
}

/// Compatibility helper for the single short appearance preview.
pub(crate) fn render(source: &str, style: &MarkdownStyle) -> TextView {
    styled(markdown(source), style)
}

fn styled(view: TextView, style: &MarkdownStyle) -> TextView {
    let copy_color = rgb(style.muted_color);
    let copy_size = (style.font_size * 0.72).max(10.);
    view.font_family(style.font_family.clone())
        .text_size(px(style.font_size))
        .line_height(px(style.line_height))
        .text_color(rgb(style.text_color))
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .selectable(true)
        // Keep the previous policy: only plain HTTP(S) URLs are clickable.
        .link(|link, _| {
            let url = url::Url::parse(&link.url).ok()?;
            (matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none())
            .then(|| link.clone())
        })
        .code_block_actions(move |block, _, _cx| {
            let code = block.code();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            code.to_string().hash(&mut hasher);
            let id = hasher.finish() as usize;
            div()
                .id(("markdown-copy-code", id))
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .text_size(px(copy_size))
                .text_color(copy_color)
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0xffffff18)))
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                })
                .child("复制代码")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Render, TestAppContext, Window};

    #[test]
    fn streamed_source_is_kept_verbatim() {
        let document = MarkdownDocument::parse("# 标题\n\n- [x] 完成\n\n```rust\nfn main() {}\n");
        assert_eq!(
            document.source.as_ref(),
            "# 标题\n\n- [x] 完成\n\n```rust\nfn main() {}\n"
        );
    }

    struct Fixture {
        document: MarkdownDocument,
    }

    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let style = MarkdownStyle {
                font_family: crate::app::appearance::font_family().into(),
                font_size: 16.,
                line_height: 24.,
                text_color: 0xffffff,
                muted_color: 0xaaaaaa,
            };
            crate::app::ui::dark(
                div()
                    .w(px(720.))
                    .child(self.document.render("markdown-layout", &style)),
                cx,
            )
        }
    }

    #[test]
    fn markdown_views_render_rich_blocks_in_a_themed_window() {
        let mut app = TestAppContext::single();
        app.update(crate::app::ui::init);
        let (_, cx) = app.add_window_view(|_, _| Fixture {
            document: MarkdownDocument::parse(
                "# 标题\n\n- 列表\n- [x] 任务\n\n| 列 | 值 |\n| --- | --- |\n| a | b |\n\n```rust\nfn main() {}\n```\n\n[链接](https://example.com)",
            ),
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    struct ViewSwitch {
        document: MarkdownDocument,
        other: MarkdownDocument,
        show_markdown: bool,
    }

    impl ViewSwitch {
        fn style() -> MarkdownStyle {
            MarkdownStyle {
                font_family: crate::app::appearance::font_family().into(),
                font_size: 16.,
                line_height: 24.,
                text_color: 0xffffff,
                muted_color: 0xaaaaaa,
            }
        }
    }

    impl Render for ViewSwitch {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let style = Self::style();
            let body = if self.show_markdown {
                self.document.render(("switch-markdown", 1u64), &style)
            } else {
                self.other.render(("other-markdown", 1u64), &style)
            };
            crate::app::ui::dark(div().w(px(720.)).child(body), cx)
        }
    }

    /// Leaving the AI view and coming back must tear the old rich-text state down
    /// cleanly instead of reusing element state for a different document.
    #[test]
    fn leaving_and_returning_to_a_view_rebuilds_markdown_state() {
        let mut app = TestAppContext::single();
        app.update(crate::app::ui::init);
        let (view, cx) = app.add_window_view(|_, _| ViewSwitch {
            document: MarkdownDocument::parse(
                "# 标题\n\n- 列表\n\n```rust\nfn main() {}\n```",
            ),
            other: MarkdownDocument::parse("| 列 | 值 |\n| --- | --- |\n| a | b |"),
            show_markdown: true,
        });
        for show_markdown in [false, true, false, true, false, true] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.show_markdown = show_markdown;
                    cx.notify();
                });
                window.draw(cx).clear(cx);
            });
            cx.run_until_parked();
        }
    }
}
