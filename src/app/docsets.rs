//! Documentation search inside the existing acrylic command palette.
use super::{INK, LINE, MUTED, Moin, appearance::rem, control, entry, launcher_window, row};
use gpui::{prelude::*, *};
use gpui_kit::component::{
    Disableable, Icon, IconName, Sizable, label::Label, spinner::Spinner, text::TextView,
};
use moin::docsets::{DocHit, DocPage, LinkTarget, Manager, SearchReport};
use std::{sync::Arc, time::Duration};

struct Reader {
    target: DocHit,
    page: Option<Arc<DocPage>>,
    history: Vec<Arc<DocPage>>,
    loading: bool,
}

pub(super) struct DocSearch {
    manager: Manager,
    owner: Option<WeakEntity<Moin>>,
    active: bool,
    query: String,
    report: SearchReport,
    error: Option<String>,
    busy: bool,
    selected: usize,
    generation: u64,
    revision: u64,
    scroll: ScrollHandle,
    _task: Option<Task<()>>,
    _watch: Task<()>,
    reader: Option<Reader>,
    reader_generation: u64,
    reader_scroll: ScrollHandle,
    reader_focus: FocusHandle,
    _reader_task: Option<Task<()>>,
}
impl DocSearch {
    pub(super) fn new(manager: Manager, owner: WeakEntity<Moin>, cx: &mut Context<Self>) -> Self {
        Self::with_owner(manager, Some(owner), cx)
    }
    fn with_owner(
        manager: Manager,
        owner: Option<WeakEntity<Moin>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut changes = manager.subscribe();
        let revision = manager.state().revision;
        let watch = cx.spawn(async move |view, cx| {
            while changes.changed().await.is_ok() {
                let revision = changes.borrow().revision;
                if view
                    .update(cx, |view, cx| {
                        if view.revision != revision {
                            view.revision = revision;
                            if view.active {
                                view.refresh(cx);
                            }
                        }
                        if view.active {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            manager,
            owner,
            active: false,
            query: String::new(),
            report: SearchReport::default(),
            error: None,
            busy: false,
            selected: 0,
            generation: 0,
            revision,
            scroll: ScrollHandle::new(),
            _task: None,
            _watch: watch,
            reader: None,
            reader_generation: 0,
            reader_scroll: ScrollHandle::new(),
            reader_focus: cx.focus_handle(),
            _reader_task: None,
        }
    }
    pub(super) fn activate(&mut self, active: bool, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.refresh(cx);
        } else {
            self.generation += 1;
            self._task.take();
            self.busy = false;
            self.close_reader(cx);
        }
    }
    pub(super) fn query(&mut self, query: String, cx: &mut Context<Self>) {
        if self.query == query && self.reading() {
            return;
        }
        self.query = query;
        self.refresh(cx);
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.close_reader(cx);
        self.generation += 1;
        let generation = self.generation;
        self.error = None;
        self.busy = true;
        self.selected = 0;
        self.report = SearchReport::default();
        self.scroll.set_offset(point(px(0.), px(0.)));
        let manager = self.manager.clone();
        let query = self.query.clone();
        let executor = cx.background_executor().clone();
        self._task = Some(cx.spawn(async move |view, cx| {
            executor.timer(Duration::from_millis(120)).await;
            let result = executor
                .spawn(async move { manager.search(&query, 80) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if generation != view.generation {
                    return;
                }
                view.busy = false;
                match result {
                    Ok(report) => view.report = report,
                    Err(error) => view.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    pub(super) fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.reading() {
            let offset = self.reader_scroll.offset();
            self.reader_scroll
                .set_offset(point(offset.x, offset.y - px(delta as f32 * 40.)));
            cx.notify();
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.report.hits.len().saturating_sub(1));
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }
    pub(super) fn has_selection(&self) -> bool {
        !self.reading() && !self.busy && self.report.hits.get(self.selected).is_some()
    }
    pub(super) fn select_number(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        if self.reading() {
            return false;
        }
        let index = self.scroll.top_item().saturating_add(index);
        if self.report.hits.get(index).is_none() {
            return false;
        }
        self.selected = index;
        self.scroll.scroll_to_item(index);
        cx.notify();
        true
    }
    pub(super) fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.reading() {
            return;
        }
        if let Some(hit) = self.report.hits.get(self.selected).cloned() {
            self.read(hit, false, false, cx);
            self.reader_focus.focus(window, cx);
        }
    }
    pub(super) fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(page) = self.reader.as_ref().and_then(|reader| reader.page.as_ref()) {
            cx.write_to_clipboard(ClipboardItem::new_string(format!(
                "{} · {} {}\n\n{}",
                page.hit.title,
                page.hit.docset,
                page.hit.version,
                page.text.trim()
            )));
            return;
        }
        if self.reading() {
            return;
        }
        if let Some(hit) = self.report.hits.get(self.selected) {
            match self.manager.page_url(hit) {
                Ok(url) => cx.write_to_clipboard(ClipboardItem::new_string(url.into())),
                Err(error) => self.error = Some(format!("{error:#}")),
            }
        }
        cx.notify();
    }
    pub(super) fn reading(&self) -> bool {
        self.reader.is_some()
    }
    pub(super) fn back_hint(&self) -> &'static str {
        if self
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.history.is_empty())
        {
            "Esc 返回上一页"
        } else if self.reading() {
            if cfg!(target_os = "macos") {
                "Esc 返回搜索结果 · Cmd+L 重新搜索"
            } else {
                "Esc 返回搜索结果 · Ctrl+L 重新搜索"
            }
        } else {
            "Esc 返回全局搜索"
        }
    }
    fn close_reader(&mut self, cx: &mut Context<Self>) {
        self.reader_generation += 1;
        self._reader_task.take();
        self.reader = None;
        cx.notify();
    }
    pub(super) fn back(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(reader) = self.reader.as_mut() else {
            return false;
        };
        self.reader_generation += 1;
        self._reader_task.take();
        self.error = None;
        if reader.loading && reader.page.is_some() {
            reader.target = reader.page.as_ref().unwrap().hit.clone();
            reader.loading = false;
        } else if let Some(page) = reader.history.pop() {
            reader.target = page.hit.clone();
            reader.page = Some(page);
            reader.loading = false;
        } else {
            self.reader = None;
        }
        self.reader_scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
        true
    }
    fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(owner) = &self.owner {
            let _ = owner.update(cx, |owner, cx| {
                owner.input.update(cx, |input, cx| input.focus(window, cx))
            });
        }
    }
    fn read(&mut self, hit: DocHit, full_page: bool, remember: bool, cx: &mut Context<Self>) {
        self.reader_generation += 1;
        let generation = self.reader_generation;
        self.error = None;
        if let Some(reader) = self.reader.as_mut() {
            reader.target = hit.clone();
            reader.loading = true;
        } else {
            self.reader = Some(Reader {
                target: hit.clone(),
                page: None,
                history: vec![],
                loading: true,
            });
        }
        let manager = self.manager.clone();
        let work = cx.background_spawn(async move { manager.read_page(&hit, full_page) });
        self._reader_task = Some(cx.spawn(async move |view, cx| {
            let result = work.await;
            let _ = view.update(cx, |view, cx| {
                if generation != view.reader_generation {
                    return;
                }
                let Some(reader) = view.reader.as_mut() else {
                    return;
                };
                reader.loading = false;
                if remember {
                    if let Some(old) = reader.page.take() {
                        reader.history.push(old);
                        if reader.history.len() > 32 {
                            reader.history.remove(0);
                        }
                    }
                }
                match result {
                    Ok(page) => {
                        reader.page = Some(Arc::new(page));
                        view.reader_scroll.set_offset(point(px(0.), px(0.)));
                    }
                    Err(error) => view.error = Some(format!("无法读取文档 · {error:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn follow(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.reader.as_ref().and_then(|reader| reader.page.as_ref()) else {
            return;
        };
        match self.manager.follow_link(&page.hit, url) {
            Ok(LinkTarget::Local(hit)) => {
                self.read(hit, false, true, cx);
                self.reader_focus.focus(window, cx);
            }
            Ok(LinkTarget::External(url)) => cx.open_url(url.as_str()),
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
            }
        }
    }
    fn browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(hit) = self.reader.as_ref().map(|reader| &reader.target) {
            match self.manager.page_url(hit) {
                Ok(url) => {
                    cx.open_url(url.as_str());
                    launcher_window::hide(window);
                }
                Err(error) => {
                    self.error = Some(format!("{error:#}"));
                    cx.notify();
                }
            }
        }
    }
    fn reader_view(&self, cx: &mut Context<Self>) -> Div {
        let reader = self.reader.as_ref().unwrap();
        let mut pane = div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .track_focus(&self.reader_focus)
            .child(
                row()
                    .px(px(18.))
                    .py(px(8.))
                    .gap_2()
                    .flex_wrap()
                    .child(
                        control("docset-reader-results", "返回结果").on_click(cx.listener(
                            |view, _, window, cx| {
                                view.close_reader(cx);
                                view.error = None;
                                view.focus_search(window, cx);
                            },
                        )),
                    )
                    .when(!reader.history.is_empty(), |row| {
                        row.child(
                            control("docset-reader-back", "上一页").on_click(cx.listener(
                                |view, _, _, cx| {
                                    view.back(cx);
                                },
                            )),
                        )
                    })
                    .child(div().flex_1())
                    .when(
                        !reader.loading
                            && reader
                                .page
                                .as_ref()
                                .is_some_and(|page| page.scoped || page.full_page),
                        |row| {
                            row.child(
                                control(
                                    "docset-reader-full",
                                    if reader.page.as_ref().is_some_and(|page| page.full_page) {
                                        "定位函数"
                                    } else {
                                        "查看整页"
                                    },
                                )
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        if let Some(page) = view
                                            .reader
                                            .as_ref()
                                            .and_then(|reader| reader.page.as_ref())
                                        {
                                            view.read(page.hit.clone(), !page.full_page, true, cx);
                                        }
                                    },
                                )),
                            )
                        },
                    )
                    .child(
                        control("docset-reader-copy", "复制正文")
                            .disabled(reader.loading || reader.page.is_none())
                            .on_click(cx.listener(|view, _, _, cx| view.copy(cx))),
                    )
                    .child(
                        control("docset-reader-browser", "浏览器")
                            .on_click(cx.listener(|view, _, window, cx| view.browser(window, cx))),
                    ),
            )
            .child(
                div().px(px(24.)).pb(px(8.)).child(
                    Label::new(format!(
                        "{} · {} · {}",
                        reader.target.docset, reader.target.title, reader.target.version
                    ))
                    .text_size(rem(13.))
                    .text_color(rgb(MUTED))
                    .truncate(),
                ),
            )
            .when_some(self.error.clone(), |pane, error| {
                pane.child(
                    div()
                        .px(px(24.))
                        .py_2()
                        .text_size(rem(13.))
                        .text_color(rgb(0xffb4a5))
                        .child(error),
                )
            });
        if reader.loading {
            return pane.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Spinner::new().small())
                    .child(Label::new("正在读取文档…")),
            );
        }
        let Some(page) = &reader.page else {
            return pane.child(div().flex_1().px(px(24.)).py_4().child(
                Label::new("可返回结果重新选择，或使用浏览器打开此页面。").text_color(rgb(MUTED)),
            ));
        };
        if let Some(notice) = &page.notice {
            pane = pane.child(
                div()
                    .px(px(24.))
                    .pb_2()
                    .text_size(rem(12.))
                    .text_color(rgb(MUTED))
                    .child(notice.clone()),
            );
        }
        let weak = cx.entity().downgrade();
        let images = page.clone();
        let text = TextView::html(
            ("docset-reader-text", self.reader_generation as usize),
            page.html.clone(),
        )
        .selectable(true)
        .scrollable(false)
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .text_size(rem(15.))
        .line_height(rem(24.))
        .text_color(rgb(INK))
        .image_source(move |uri| {
            images
                .images
                .get(uri.as_ref())
                .cloned()
                .map(ImageSource::from)
                .unwrap_or_else(|| ImageSource::Custom(Arc::new(|_, _| None)))
        })
        .on_link_click(move |url, event, window, cx| {
            if !event.is_right_click() {
                let _ = weak.update(cx, |view, cx| view.follow(url.as_ref(), window, cx));
            }
        });
        pane.child(
            div()
                .id(("docset-reader-scroll", self.reader_generation as usize))
                .debug_selector(|| "docset-reader-scroll".into())
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.reader_scroll)
                .px(px(24.))
                .py(px(12.))
                .child(text),
        )
    }
    fn settings(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(owner) = &self.owner {
            let _ = owner.update(cx, |owner, cx| owner.open_docset_settings(window, cx));
        }
    }
}
impl Render for DocSearch {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.reading() {
            return self.reader_view(cx).into_any_element();
        }
        let state = self.manager.state();
        let mut list = div()
            .id("docset-search-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(10.))
            .py(px(8.));
        for (i, hit) in self.report.hits.iter().enumerate() {
            list = list.child(
                entry(
                    ("docset-result", i),
                    Icon::new(IconName::BookOpen)
                        .text_color(rgb(0x9bd4ff))
                        .into_any_element(),
                    hit.title.clone(),
                    format!("{} · {} · {}", hit.docset, hit.kind, hit.version),
                    self.selected == i,
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.selected = i;
                    view.open_selected(window, cx);
                })),
            );
        }
        if self.report.hits.is_empty() {
            list = list.child(
                div()
                    .size_full()
                    .min_h(px(160.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        Label::new(if !state.loaded {
                            "正在读取文档集…"
                        } else if self.busy {
                            "正在搜索文档…"
                        } else if state.installed.is_empty() {
                            "还没有安装文档集"
                        } else if self.report.enabled == 0 && self.query.trim().is_empty() {
                            "文档集均已停用"
                        } else {
                            "没有找到匹配的文档"
                        })
                        .text_color(rgb(MUTED)),
                    )
                    .child(
                        Label::new("安装后可离线搜索；使用 rust: Vec 等关键词限定文档集。")
                            .text_size(rem(13.))
                            .text_color(rgb(MUTED)),
                    )
                    .child(
                        control("docsets-empty-settings", "下载与管理 Docsets")
                            .on_click(cx.listener(|view, _, window, cx| view.settings(window, cx))),
                    ),
            );
        }
        let feedback = self
            .error
            .clone()
            .or_else(|| self.report.warning.clone())
            .or_else(|| state.error.clone());
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .text_color(rgb(INK))
            .child(
                row()
                    .px(px(24.))
                    .py(px(8.))
                    .gap_2()
                    .when(self.busy, |row| row.child(Spinner::new().small()))
                    .child(
                        Label::new(format!(
                            "{} 个结果 · {} 个已启用文档集",
                            self.report.hits.len(),
                            state.installed.iter().filter(|item| item.enabled).count()
                        ))
                        .text_size(rem(12.))
                        .text_color(rgb(MUTED)),
                    )
                    .child(div().flex_1())
                    .child(
                        control("docs-manage", "管理文档集")
                            .on_click(cx.listener(|view, _, window, cx| view.settings(window, cx))),
                    ),
            )
            .when_some(feedback, |view, message| {
                view.child(
                    div()
                        .px(px(24.))
                        .pb(px(8.))
                        .text_size(rem(12.))
                        .text_color(rgb(0xffb4a5))
                        .child(message),
                )
            })
            .child(list)
            .when_some(self.report.hits.get(self.selected), |view, hit| {
                view.child(
                    div()
                        .px(px(24.))
                        .py(px(10.))
                        .border_t_1()
                        .border_color(rgba(LINE))
                        .child(
                            Label::new(hit.path.clone())
                                .text_size(rem(12.))
                                .text_color(rgb(MUTED))
                                .truncate(),
                        ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    use std::fs;

    struct Harness {
        view: Entity<DocSearch>,
        width: f32,
    }
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(self.width))
                .h(px(500.))
                .flex()
                .flex_col()
                .child(self.view.clone())
        }
    }

    #[::core::prelude::v1::test]
    fn selected_function_renders_in_textview_and_back_preserves_search_at_two_widths() {
        let home = tempfile::tempdir().unwrap();
        let directory = format!("{}-{}", "a".repeat(32), "b".repeat(32));
        let resources = home
            .path()
            .join("docsets/sets")
            .join(&directory)
            .join("Fixture.docset/Contents/Resources");
        fs::create_dir_all(resources.join("Documents")).unwrap();
        fs::write(
            resources.join("Documents/index.html"),
            r##"<main><h1>Vec</h1>
            <section id="new"><h2>Vec::new</h2><p>Creates an empty vector. 创建空向量。</p>
            <pre><code class="language-rust">let mut v = Vec::new();
  v.push(1);</code></pre><table><tr><th>Returns</th></tr><tr><td>Vec</td></tr></table>
            <a href="#push">Read push</a></section>
            <section id="push"><h2>Vec::push</h2><p>Appends an element.</p></section></main>"##,
        )
        .unwrap();
        let db = rusqlite::Connection::open(resources.join("docSet.dsidx")).unwrap();
        db.execute_batch("CREATE TABLE searchIndex(id INTEGER PRIMARY KEY,name TEXT,type TEXT,path TEXT);
            INSERT INTO searchIndex(name,type,path) VALUES('Vec::new','Function','index.html#new');
            INSERT INTO searchIndex(name,type,path) VALUES('Vec::push','Function','index.html#push');").unwrap();
        drop(db);
        fs::write(home.path().join("docsets/installed.json"), serde_json::to_vec(&serde_json::json!([{
            "key":"Rust","title":"Rust","version":"test","enabled":true,"keywords":["rust"],
            "symbols":2,"size":1024,"directory":directory,"bundle":"Fixture.docset","homepage":"index.html"
        }])).unwrap()).unwrap();
        let manager = Manager::new(home.path());
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while !manager.state().loaded {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let report = manager.search("rust: Vec", 20).unwrap();
        let selected = report
            .hits
            .iter()
            .position(|hit| hit.title == "Vec::new")
            .unwrap();
        for width in [480., 860.] {
            let mut app = TestAppContext::single();
            app.update(crate::app::ui::init);
            let mut handle = None;
            let (_, cx) = app.add_window_view(|window, cx| {
                let view = cx.new(|cx| DocSearch::with_owner(manager.clone(), None, cx));
                view.update(cx, |view, _| {
                    view.active = true;
                    view.query = "rust: Vec".into();
                    view.report = report.clone();
                    view.selected = selected;
                });
                handle = Some(view.clone());
                let harness = cx.new(|_| Harness { view, width });
                Root::new(harness, window, cx)
            });
            let view = handle.unwrap();
            cx.update(|window, cx| view.update(cx, |view, cx| view.open_selected(window, cx)));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.run_until_parked();
            cx.update(|window, cx| window.draw(cx).clear(cx));
            cx.read(|cx| {
                let reader = view.read(cx).reader.as_ref().unwrap();
                assert!(!reader.loading);
                let page = reader.page.as_ref().unwrap();
                assert!(page.scoped && page.text.contains("创建空向量"));
                assert!(!page.text.contains("Appends an element"));
            });
            let bounds = cx
                .debug_bounds("docset-reader-scroll")
                .expect("TextView scroll pane is rendered");
            assert!(bounds.size.width > px(300.) && bounds.size.width <= px(width));
            assert!(bounds.size.height > px(100.) && bounds.size.height < px(500.));
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    view.query("rust: Vec".into(), cx);
                    assert!(view.reading(), "an unchanged query must preserve reading");
                    view.follow("#push", window, cx);
                })
            });
            cx.run_until_parked();
            cx.read(|cx| {
                let reader = view.read(cx).reader.as_ref().unwrap();
                assert_eq!(reader.history.len(), 1);
                assert!(
                    reader
                        .page
                        .as_ref()
                        .unwrap()
                        .text
                        .contains("Appends an element")
                );
            });
            cx.update(|_, cx| {
                view.update(cx, |view, cx| {
                    assert!(view.back(cx));
                    assert!(
                        view.reader
                            .as_ref()
                            .unwrap()
                            .page
                            .as_ref()
                            .unwrap()
                            .text
                            .contains("创建空向量")
                    );
                    assert!(view.back(cx));
                    assert!(!view.reading());
                    assert_eq!(view.query, "rust: Vec");
                    assert_eq!(view.selected, selected);
                    assert_eq!(view.report.hits.len(), 2);
                    assert!(!view.back(cx), "the next Esc belongs to the global scope");
                })
            });
        }
    }
}
