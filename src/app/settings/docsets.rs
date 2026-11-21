use super::{button, column, description, error_message, input_field, row, section, title};
use crate::app::input::{Changed, SearchInput};
use gpui::{prelude::*, *};
use gpui_kit::component::{
    Disableable, Sizable, label::Label, progress::Progress, switch::Switch, tag::Tag,
};
use moin::docsets::{CATALOG_SOURCE, Manager};

pub(super) struct DocsetsPanel {
    manager: Manager,
    filter: Entity<SearchInput>,
    error: Option<String>,
    canceling: bool,
    _filter: Subscription,
    _watch: Task<()>,
}
impl DocsetsPanel {
    pub(super) fn new(manager: Manager, cx: &mut Context<Self>) -> Self {
        let filter =
            cx.new(|cx| SearchInput::field("", "搜索 Rust、Python、JavaScript 等文档集…", cx));
        let subscription = cx.subscribe(&filter, |_, _, _: &Changed, cx| cx.notify());
        let mut changes = manager.subscribe();
        let watch = cx.spawn(async move |view, cx| {
            while changes.changed().await.is_ok() {
                let finished = changes.borrow().progress.is_none();
                if view
                    .update(cx, |view, cx| {
                        if finished {
                            view.canceling = false;
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            manager,
            filter,
            error: None,
            canceling: false,
            _filter: subscription,
            _watch: watch,
        }
    }
    fn run(&mut self, action: impl FnOnce(&Manager) -> anyhow::Result<()>, cx: &mut Context<Self>) {
        self.error = action(&self.manager)
            .err()
            .map(|error| format!("{error:#}"));
        cx.notify();
    }
}
impl Render for DocsetsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.manager.state();
        let busy = state.progress.is_some() || !state.loaded;
        let filter = self.filter.read(cx).value().trim().to_lowercase();
        let mut page=column().gap_4()
            .child(section("离线文档搜索").child(column().gap_2()
                .child(description("首页键入 h 进入文档搜索，也可粘贴 docs rust: Vec。使用 rust: Vec 或 python list 限定文档集，Enter 在浮窗内阅读函数正文；Esc 先返回上一页或结果列表，再返回全局搜索。"))
                .child(row().gap_2().flex_wrap()
                    .child(button("docsets-refresh","刷新官方目录",false,state.progress.is_none()).on_click(cx.listener(|view,_,_,cx|view.run(Manager::refresh_catalog,cx))))
                    .child(button("docsets-source","Dash 官方源",false,true).on_click(|_,_,cx|cx.open_url(CATALOG_SOURCE)))
                    .child(Tag::secondary().small().child(format!("{} 个可下载文档集",state.catalog.len()))))))
            .children(self.error.clone().or(state.error.clone()).into_iter().map(error_message))
            .children(state.notice.clone().into_iter().map(description));
        if let Some(progress) = &state.progress {
            let text = match progress.total.filter(|value| *value > 0) {
                Some(total) => format!("{} / {}", bytes(progress.received), bytes(total)),
                None if progress.received > 0 => bytes(progress.received),
                _ => String::new(),
            };
            page =
                page.child(
                    section("当前任务").child(
                        column()
                            .gap_3()
                            .child(
                                row()
                                    .gap_3()
                                    .justify_between()
                                    .child(title(
                                        format!(
                                            "{} · {}",
                                            progress.title,
                                            if self.canceling {
                                                "正在取消…"
                                            } else {
                                                &progress.stage
                                            }
                                        ),
                                        15.,
                                    ))
                                    .child(
                                        button(
                                            "cancel-docset-download",
                                            if self.canceling {
                                                "取消中…"
                                            } else {
                                                "取消"
                                            },
                                            false,
                                            !self.canceling,
                                        )
                                        .on_click(
                                            cx.listener(|view, _, _, cx| {
                                                view.manager.cancel();
                                                view.canceling = true;
                                                cx.notify();
                                            }),
                                        ),
                                    ),
                            )
                            .child(
                                Progress::new("docset-download-progress")
                                    .value(progress.total.filter(|total| *total > 0).map_or(
                                        0.,
                                        |total| {
                                            (progress.received as f64 / total as f64 * 100.) as f32
                                        },
                                    ))
                                    .loading(progress.total.is_none()),
                            )
                            .child(description(text)),
                    ),
                );
        }
        page = page.child(input_field("docset-filter", "筛选文档集", &self.filter, cx));
        let mut installed = column().gap_3();
        let mut count = 0;
        for docset in &state.installed {
            if !format!(
                "{} {} {}",
                docset.title,
                docset.key,
                docset.keywords.join(" ")
            )
            .to_lowercase()
            .contains(&filter)
            {
                continue;
            }
            count += 1;
            let enable = docset.key.clone();
            let update = docset.key.clone();
            let remove = docset.key.clone();
            installed = installed.child(
                column()
                    .id((ElementId::from("installed-docset"), docset.key.clone()))
                    .gap_3()
                    .p_4()
                    .bg(rgb(0xffffff))
                    .border_1()
                    .border_color(rgb(0xe5e5e5))
                    .rounded(px(8.))
                    .child(
                        row()
                            .gap_3()
                            .justify_between()
                            .child(title(&docset.title, 17.))
                            .child(
                                Switch::new("docset-enabled")
                                    .checked(docset.enabled)
                                    .disabled(busy)
                                    .on_click(cx.listener(move |view, enabled: &bool, _, cx| {
                                        view.run(|manager| manager.enable(&enable, *enabled), cx)
                                    })),
                            ),
                    )
                    .child(description(format!(
                        "{} · {} 个索引条目 · {}",
                        docset.version,
                        docset.symbols,
                        bytes(docset.size)
                    )))
                    .child(description(format!(
                        "关键词：{}",
                        docset.keywords.join(" · ")
                    )))
                    .child(
                        row()
                            .gap_2()
                            .child(button("update-docset", "检查更新", false, !busy).on_click(
                                cx.listener(move |view, _, _, cx| {
                                    view.run(|manager| manager.install(&update), cx)
                                }),
                            ))
                            .child(button("remove-docset", "删除", false, !busy).on_click(
                                cx.listener(move |view, _, _, cx| {
                                    view.run(|manager| manager.remove(&remove), cx)
                                }),
                            )),
                    ),
            );
        }
        if count == 0 {
            installed = installed.child(description(if state.installed.is_empty() {
                "尚未安装文档集，请从下方选择下载。"
            } else {
                "没有匹配的已安装文档集。"
            }));
        }
        page = page.child(section(format!("已安装 · {}", state.installed.len())).child(installed));
        let mut available = column().gap_0();
        let mut count = 0;
        for docset in &state.catalog {
            if state.installed.iter().any(|item| item.key == docset.key)
                || !format!("{} {}", docset.title, docset.key)
                    .to_lowercase()
                    .contains(&filter)
            {
                continue;
            }
            count += 1;
            let key = docset.key.clone();
            available = available.child(
                row()
                    .id((ElementId::from("available-docset"), key.clone()))
                    .debug_selector(|| format!("available-docset-{}", docset.key))
                    .py(px(10.))
                    .gap_3()
                    .border_b_1()
                    .border_color(rgb(0xe5e5e5))
                    .child(Label::new(docset.title.clone()).flex_1().min_w_0())
                    .child(
                        button("install-docset", "下载", false, !busy).on_click(cx.listener(
                            move |view, _, _, cx| view.run(|manager| manager.install(&key), cx),
                        )),
                    ),
            );
        }
        if count == 0 {
            available = available.child(description("没有匹配的待下载文档集。"));
        }
        page.child(section(format!("可下载 · {count}")).child(available))
    }
}
fn bytes(size: u64) -> String {
    if size >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", size as f64 / (1024. * 1024. * 1024.))
    } else if size >= 1024 * 1024 {
        format!("{:.1} MiB", size as f64 / (1024. * 1024.))
    } else {
        format!("{:.0} KiB", size as f64 / 1024.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    struct Harness(Entity<DocsetsPanel>);
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("docset-settings-test-scroll")
                .size_full()
                .overflow_y_scroll()
                .child(self.0.clone())
        }
    }
    #[::core::prelude::v1::test]
    fn official_catalog_filters_through_the_kit_input() {
        let folder = tempfile::tempdir().unwrap();
        let manager = Manager::new(folder.path());
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !manager.state().loaded {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut app = TestAppContext::single();
        app.update(crate::app::ui::init);
        let mut editor = None;
        let (_, cx) = app.add_window_view(|window, cx| {
            let panel = cx.new(|cx| DocsetsPanel::new(manager, cx));
            editor = Some(panel.read(cx).filter.clone());
            let harness = cx.new(|_| Harness(panel));
            Root::new(harness, window, cx)
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            editor
                .as_ref()
                .unwrap()
                .update(cx, |editor, cx| editor.focus(window, cx));
        });
        cx.simulate_input("Rust");
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(
            cx.read(|cx| editor.as_ref().unwrap().read(cx).value().to_owned()),
            "Rust"
        );
        assert!(cx.debug_bounds("available-docset-Rust").is_some());
        assert!(cx.debug_bounds("available-docset-Python").is_none());
    }
}
