//! Opt-in native-font regression check. The window remains hidden and unfocused.
//!
//! Unlike TestAppContext, Application::new() uses Windows DirectWrite. Run this
//! test by itself in a subprocess; no running Moin instance is read or changed.

use super::*;
use gpui::{
    App, Application, Bounds, Context, Pixels, Point, Render, TextLayout, Window, WindowBounds,
    WindowOptions, point, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{cell::RefCell, rc::Rc};
use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindowVisible};

#[derive(Debug)]
struct TextMeasurement {
    source: String,
    bounds: Bounds<Pixels>,
    positions: Vec<Point<Pixels>>,
    line_height: Pixels,
}

#[derive(Debug, Default)]
struct Snapshot {
    texts: Vec<TextMeasurement>,
    children: Vec<(&'static str, Vec<Bounds<Pixels>>)>,
}

thread_local! {
    static OBSERVATIONS: RefCell<Option<Rc<RefCell<Snapshot>>>> = const { RefCell::new(None) };
}

/// Adds a prepaint observer without inserting elements or changing any style.
pub(super) fn observe_text(element: Div, source: String, layout: TextLayout) -> Div {
    let observations = OBSERVATIONS.with_borrow(Clone::clone);
    let Some(observations) = observations else {
        return element;
    };
    element.on_children_prepainted(move |_, _, _| {
        let positions = source
            .char_indices()
            .filter_map(|(index, _)| layout.position_for_index(index))
            .collect();
        observations.borrow_mut().texts.push(TextMeasurement {
            source: source.clone(),
            bounds: layout.bounds(),
            positions,
            line_height: layout.line_height(),
        });
    })
}

/// Observes marker/body or table cell bounds without affecting layout.
pub(super) fn observe_children(element: Div, kind: &'static str) -> Div {
    let observations = OBSERVATIONS.with_borrow(Clone::clone);
    let Some(observations) = observations else {
        return element;
    };
    element.on_children_prepainted(move |bounds, _, _| {
        observations.borrow_mut().children.push((kind, bounds));
    })
}

const FIXTURE: &str = r#"## 本地 DNS

- 192.168.1.1：本机网络使用的 **DNS 服务器**，长说明应该正常换行，而不是每行一个字。
- 192.168.1.2：第二台服务器。[查看详情](https://example.com)

## 本机情况

本机没有运行 DNS 服务，下面是正常的后续段落。

10. 第一项有序列表包含中文和 inline `code`。
11. 第二项应该保持横向排版。

- 父级列表正文，后面还有子列表。
  - 嵌套列表内容应该在缩进后的可用宽度内正常排版。
  - 第二个嵌套项目。

## 表格之后

| 名称 | 说明 |
| --- | --- |
| 本地地址 | 服务器配置与中文说明保持正常宽度 |

最后一段内容。"#;

struct NativeFixture {
    width: f32,
    font_family: &'static str,
    document: MarkdownDocument,
}

impl Render for NativeFixture {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let style = MarkdownStyle {
            font_family: self.font_family.into(),
            font_size: 18.,
            line_height: 28.,
            text_color: 0xffffff,
            muted_color: 0xaaaaaa,
            link_color: 0x99bbff,
            code_background: 0x222222ff,
            rule_color: 0xffffff30,
        };
        div()
            .id("native-markdown-fixture-scroll")
            .w(px(self.width))
            .h(px(1600.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .font_family(self.font_family)
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .child(self.document.render("native-markdown-fixture", &style)),
            )
    }
}

#[derive(Debug)]
struct CaseReport {
    width: f32,
    font_family: &'static str,
    snapshot: Snapshot,
}

fn run_cases(cx: &mut App) -> Result<Vec<CaseReport>, String> {
    let mut reports = Vec::new();
    // Include Segoe UI's real CJK fallback and an explicit installed CJK family.
    for font_family in ["Segoe UI", "Microsoft YaHei"] {
        for width in [260., 420., 760., 1100.] {
            let observations = Rc::new(RefCell::new(Snapshot::default()));
            OBSERVATIONS.with_borrow_mut(|active| *active = Some(observations.clone()));
            let handle = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                            point(px(0.), px(0.)),
                            size(px(1200.), px(1600.)),
                        ))),
                        show: false,
                        focus: false,
                        ..Default::default()
                    },
                    move |_, cx| {
                        cx.new(|_| NativeFixture {
                            width,
                            font_family,
                            document: MarkdownDocument::parse(FIXTURE),
                        })
                    },
                )
                .map_err(|error| format!("open hidden window: {error:#}"))?;

            let result = cx
                .update_window(handle.into(), |_, window, cx| -> Result<(), String> {
                    let raw = window
                        .window_handle()
                        .map_err(|error| error.to_string())?
                        .as_raw();
                    let RawWindowHandle::Win32(raw) = raw else {
                        return Err("native check did not create a Win32 window".into());
                    };
                    let hwnd = HWND(raw.hwnd.get() as *mut std::ffi::c_void);
                    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                        return Err("native check window unexpectedly became visible".into());
                    }
                    window.draw(cx).clear();
                    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                        return Err("drawing unexpectedly made the native window visible".into());
                    }
                    window.remove_window();
                    Ok(())
                })
                .map_err(|error| format!("draw hidden window: {error:#}"))?;
            result?;
            OBSERVATIONS.with_borrow_mut(|active| *active = None);
            reports.push(CaseReport {
                width,
                font_family,
                snapshot: std::mem::take(&mut *observations.borrow_mut()),
            });
        }
    }
    Ok(reports)
}

fn validate_case(report: &CaseReport) {
    let context = format!("{} at {}px", report.font_family, report.width);
    let snapshot = &report.snapshot;
    assert!(
        snapshot.texts.len() >= 15,
        "missing actual renderer text probes: {context}: {snapshot:#?}"
    );

    for item in &snapshot.texts {
        assert_eq!(
            item.positions.len(),
            item.source.chars().count(),
            "native font shaping lost text: {context}: {item:#?}"
        );
        // Every fixture string has at least two characters that fit horizontally.
        assert!(
            item.positions[1].x > item.positions[0].x
                && (item.positions[1].y - item.positions[0].y).abs() <= px(1.),
            "text wrapped vertically: {context}: {item:#?}"
        );
        let mut line_tops = item
            .positions
            .iter()
            .map(|position| position.y)
            .collect::<Vec<_>>();
        line_tops.sort_by(|a, b| a.partial_cmp(b).unwrap());
        line_tops.dedup_by(|a, b| (*a - *b).abs() <= px(1.));
        assert!(
            line_tops.len() <= 6,
            "fixture text wrapped into too many native lines: {context}: {item:#?}"
        );
        let painted_bottom = item
            .positions
            .iter()
            .map(|position| position.y)
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap()
            + item.line_height;
        assert!(
            painted_bottom <= item.bounds.bottom() + px(1.),
            "glyphs paint below their allocated text bounds: {context}: {item:#?}"
        );
    }

    let list_text = snapshot
        .texts
        .iter()
        .find(|item| item.source.starts_with("192.168.1.1"))
        .expect("first list text probe");
    let following_heading = snapshot
        .texts
        .iter()
        .find(|item| item.source == "本机情况")
        .expect("following heading text probe");
    let second_text = snapshot
        .texts
        .iter()
        .find(|item| item.source.starts_with("192.168.1.2"))
        .expect("second list text probe");
    assert!(
        list_text.bounds.size.width > px(100.),
        "list body collapsed horizontally: {context}: {list_text:#?}"
    );
    assert!(
        list_text.bounds.bottom() <= second_text.bounds.top() + px(1.),
        "list rows overlap: {context}: {list_text:#?} {second_text:#?}"
    );
    assert!(
        second_text.bounds.bottom() <= following_heading.bounds.top() + px(1.),
        "list overlaps following heading: {context}: {second_text:#?} {following_heading:#?}"
    );

    let rows: Vec<_> = snapshot
        .children
        .iter()
        .filter(|(kind, _)| *kind == "list-row")
        .collect();
    assert!(rows.len() >= 7, "missing list row probes: {context}");
    for (_, children) in rows {
        assert_eq!(
            children.len(),
            2,
            "expected list marker and body: {context}: {children:?}"
        );
        assert!(
            children[1].size.width > px(100.),
            "list body too narrow: {context}: {children:?}"
        );
        assert!(
            children[0].right() <= children[1].left() + px(1.),
            "list marker overlaps body: {context}: {children:?}"
        );
    }
}

#[test]
#[ignore = "opt-in DirectWrite check; creates only hidden, unfocused windows; run by itself"]
fn native_windows_fonts_keep_markdown_lists_horizontal_and_nonoverlapping() {
    let result = Rc::new(RefCell::new(None));
    let captured = result.clone();
    Application::new().run(move |cx| {
        // Keep assertions outside the native app callback and always request quit.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_cases(cx)));
        OBSERVATIONS.with_borrow_mut(|active| *active = None);
        *captured.borrow_mut() = Some(match outcome {
            Ok(outcome) => outcome,
            Err(panic) => Err(panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    panic
                        .downcast_ref::<&str>()
                        .map(|message| (*message).to_owned())
                })
                .unwrap_or_else(|| "panic in native layout callback".into())),
        });
        cx.quit();
    });
    let reports = result
        .borrow_mut()
        .take()
        .expect("native application returned no result")
        .expect("native layout check failed");
    assert_eq!(reports.len(), 8);
    for report in reports {
        eprintln!(
            "native markdown: {} at {}px: {} shaped texts, {} child groups",
            report.font_family,
            report.width,
            report.snapshot.texts.len(),
            report.snapshot.children.len()
        );
        validate_case(&report);
    }
}
