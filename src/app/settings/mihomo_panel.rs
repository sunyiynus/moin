//! Clash-style profile and controller pages, rendered with GPUI Kit components.
use super::{
    ACCENT, Page as SettingsPage, Settings, button, column, description, error_message,
    input_field, row, section, title,
};
use crate::app::{
    Moin,
    input::{Changed, SearchInput},
};
use anyhow::{Context as _, Result};
use gpui::{prelude::*, *};
use gpui_kit::component::{
    Disableable, Sizable, label::Label, list::ListItem, switch::Switch, tag::Tag,
};
use moin::mihomo::management::{self, Catalog, Controller};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Page {
    Overview,
    Profiles,
    Proxies,
    Connections,
    Rules,
    Logs,
}

#[derive(Clone, Default)]
struct Live {
    config: Value,
    proxies: Value,
    connections: Value,
    rules: Value,
    logs: String,
}

pub(super) struct MihomoPanel {
    launcher: Entity<Moin>,
    owner: WeakEntity<Settings>,
    home: PathBuf,
    page: Page,
    visible: bool,
    catalog: Catalog,
    loaded: bool,
    name: Entity<SearchInput>,
    url: Entity<SearchInput>,
    filter: Entity<SearchInput>,
    live: Live,
    selected_group: Option<String>,
    busy: bool,
    polling: bool,
    generation: u64,
    error: Option<String>,
    api_error: Option<String>,
    notice: Option<String>,
    delays: HashMap<String, u64>,
    rates: (u64, u64),
    previous_traffic: Option<(Instant, u64, u64)>,
    _poll: Option<Task<()>>,
    _job: Option<Task<()>>,
    _fields: Vec<Subscription>,
    _source: Subscription,
}

impl MihomoPanel {
    pub(super) fn new(
        launcher: Entity<Moin>,
        owner: WeakEntity<Settings>,
        cx: &mut Context<Self>,
    ) -> Self {
        let home = launcher.read(cx).settings_snapshot().data_dir;
        let name = cx.new(|cx| SearchInput::field("", "配置名称（可选）", cx));
        let url = cx.new(|cx| SearchInput::field("", "粘贴 Clash / Mihomo 订阅链接", cx));
        let filter = cx.new(|cx| SearchInput::field("", "搜索节点、连接或规则…", cx));
        let fields = [&name, &url, &filter]
            .into_iter()
            .map(|e| cx.subscribe(e, |_, _, _: &Changed, cx| cx.notify()))
            .collect();
        let source = cx.observe(&launcher, |_, _, cx| cx.notify());
        let mut this = Self {
            launcher,
            owner,
            home: home.clone(),
            page: Page::Profiles,
            visible: false,
            catalog: Catalog::default(),
            loaded: false,
            name,
            url,
            filter,
            live: Live::default(),
            selected_group: None,
            busy: false,
            polling: false,
            generation: 0,
            error: None,
            api_error: None,
            notice: None,
            delays: HashMap::new(),
            rates: (0, 0),
            previous_traffic: None,
            _poll: None,
            _job: None,
            _fields: fields,
            _source: source,
        };
        this.job(
            "正在读取配置…",
            cx,
            move || Catalog::load(&home),
            |view, catalog, cx| {
                view.catalog = catalog;
                view.loaded = true;
                cx.notify();
            },
        );
        let executor = cx.background_executor().clone();
        this._poll = Some(cx.spawn(async move |view, cx| {
            loop {
                executor.timer(Duration::from_secs(2)).await;
                if view
                    .update(cx, |view, cx| {
                        if !matches!(view.page, Page::Profiles | Page::Rules) {
                            view.refresh(cx)
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        this
    }

    pub(super) fn set_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.visible = true;
        if self.page != page {
            self.page = page;
            self.generation += 1;
            self.filter.update(cx, |field, cx| field.clear(cx));
            self.error = None;
            self.notice = None;
        }
        self.refresh(cx);
        cx.notify();
    }
    pub(super) fn hide(&mut self) {
        self.visible = false;
        self.generation += 1;
    }

    fn job<T: Send + 'static>(
        &mut self,
        label: &str,
        cx: &mut Context<Self>,
        work: impl FnOnce() -> Result<T> + Send + 'static,
        finish: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.generation += 1;
        self.error = None;
        self.notice = Some(label.into());
        let task = cx.background_spawn(async move { work() });
        self._job = Some(cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                view.generation += 1;
                match result {
                    Ok(result) => {
                        view.notice = None;
                        finish(view, result, cx);
                    }
                    Err(error) => {
                        view.error = Some(format!("{error:#}"));
                        view.notice = None;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.busy || self.polling {
            return;
        }
        if self.page == Page::Profiles {
            let home = self.home.clone();
            self.job(
                "正在读取配置…",
                cx,
                move || Catalog::load(&home),
                |view, catalog, _| {
                    view.catalog = catalog;
                    view.loaded = true;
                },
            );
            return;
        }
        let snapshot = self.launcher.read(cx).settings_snapshot();
        if !snapshot.mihomo_state.status.is_running() && self.page != Page::Logs {
            self.live = Live::default();
            self.rates = (0, 0);
            self.previous_traffic = None;
            self.api_error = None;
            return;
        }
        self.polling = true;
        let config = snapshot.mihomo;
        let page = self.page;
        let home = self.home.clone();
        let generation = self.generation;
        let work = cx.background_spawn(async move {
            let mut live = Live::default();
            if page == Page::Logs {
                live.logs = management::log_tail(&home)?;
                return Ok(live);
            }
            let client = Controller::new(&config)?;
            match page {
                Page::Overview => {
                    live.config = client.config()?;
                    live.connections = client.connections()?;
                }
                Page::Proxies => live.proxies = client.proxies()?,
                Page::Connections => live.connections = client.connections()?,
                Page::Rules => live.rules = client.rules()?,
                _ => {}
            }
            Ok::<_, anyhow::Error>(live)
        });
        cx.spawn(async move |view, cx| {
            let result = work.await;
            let _ = view.update(cx, |view, cx| {
                view.polling = false;
                if view.generation != generation || view.page != page {
                    view.refresh(cx);
                    return;
                }
                match result {
                    Ok(live) => {
                        if live.connections.is_object() {
                            let up = number(&live.connections, "uploadTotal");
                            let down = number(&live.connections, "downloadTotal");
                            let now = Instant::now();
                            if let Some((then, old_up, old_down)) = view.previous_traffic {
                                let secs = now.duration_since(then).as_secs_f64().max(0.1);
                                view.rates = (
                                    (up.saturating_sub(old_up) as f64 / secs) as u64,
                                    (down.saturating_sub(old_down) as f64 / secs) as u64,
                                );
                            }
                            view.previous_traffic = Some((now, up, down));
                        }
                        view.live = live;
                        view.api_error = None;
                    }
                    Err(error) => view.api_error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import_remote(&mut self, cx: &mut Context<Self>) {
        let address = self.url.read(cx).value().to_owned();
        let name = self.name.read(cx).value().to_owned();
        let submitted_address = address.clone();
        let submitted_name = name.clone();
        let home = self.home.clone();
        let mut catalog = self.catalog.clone();
        self.job(
            "正在下载订阅…",
            cx,
            move || {
                catalog.import_remote(&home, &name, &address)?;
                Ok(catalog)
            },
            move |view, catalog, cx| {
                view.catalog = catalog;
                view.loaded = true;
                if view.url.read(cx).value() == submitted_address {
                    view.url.update(cx, |field, cx| field.clear(cx));
                }
                if view.name.read(cx).value() == submitted_name {
                    view.name.update(cx, |field, cx| field.clear(cx));
                }
                view.notice = Some("订阅已添加，点击“使用配置”加载。".into());
            },
        );
    }

    fn import_local(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("导入 Mihomo YAML 配置".into()),
        });
        cx.spawn(async move |view, cx| {
            let result = picker.await;
            let _ = view.update(cx, |view, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        let mut catalog = view.catalog.clone();
                        let home = view.home.clone();
                        let name = view.name.read(cx).value().to_owned();
                        view.job(
                            "正在导入配置…",
                            cx,
                            move || {
                                catalog.import_local(&home, &name, &path)?;
                                Ok(catalog)
                            },
                            |view, catalog, _| {
                                view.catalog = catalog;
                                view.loaded = true;
                                view.notice = Some("本地配置已导入。".into());
                            },
                        );
                    }
                }
                Ok(Ok(None)) => {}
                _ => {
                    view.error = Some("无法选择配置文件".into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn update_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let mut catalog = self.catalog.clone();
        let home = self.home.clone();
        self.job(
            "正在更新配置…",
            cx,
            move || {
                catalog.update(&home, &id)?;
                Ok((catalog, id))
            },
            |view, (catalog, id), cx| {
                view.catalog = catalog;
                view.notice = Some("配置已更新。".into());
                if view.catalog.selected.as_deref() == Some(&id) {
                    view.activate(id, cx);
                }
            },
        );
    }

    fn activate(&mut self, id: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let snapshot = self.launcher.read(cx).settings_snapshot();
        if snapshot.mihomo_state.status.is_busy() {
            self.error = Some("请等待核心启动或停止完成，再切换配置。".into());
            cx.notify();
            return;
        }
        let mut config = snapshot.mihomo;
        let old_config = config.clone();
        let running = snapshot.mihomo_state.status.is_running();
        let mut catalog = self.catalog.clone();
        let home = self.home.clone();
        let launcher = self.launcher.clone();
        let previous = self.catalog.clone();
        let rollback_home = self.home.clone();
        let executor = cx.background_executor().clone();
        self.busy = true;
        self.generation += 1;
        self.error = None;
        self.notice = Some("正在加载配置…".into());
        let work = cx.background_spawn(async move {
            let path = catalog.prepare(&home, &id, &config)?;
            config.config_path = path.clone();
            if running {
                if let Err(error) =
                    Controller::new(&old_config).and_then(|client| client.reload(&path))
                {
                    management::discard_runtime(&home, &id, &path);
                    return Err(error);
                }
            }
            if let Err(error) = catalog.select(&home, &id) {
                if running {
                    let _ = Controller::new(&old_config)
                        .and_then(|client| client.reload(&old_config.config_path));
                }
                management::discard_runtime(&home, &id, &path);
                return Err(error);
            }
            Ok::<_, anyhow::Error>((catalog, config, old_config, running, id))
        });
        // Loading belongs to the application: closing Settings must not leave
        // the core switched while the saved startup configuration stays behind.
        cx.spawn(async move |view, cx| {
            let result = match work.await {
                Err(error) => Err(error),
                Ok((catalog, config, old, running, id)) => {
                    let candidate = config.config_path.clone();
                    let result =
                        launcher.update(cx, |app, cx| app.adopt_mihomo_profile(config, cx));
                    match result {
                        Ok(()) => Ok((catalog, running)),
                        Err(error) => {
                            let restored = executor
                                .spawn(async move {
                                    let core = if running {
                                        Controller::new(&old)
                                            .and_then(|client| client.reload(&old.config_path))
                                    } else {
                                        Ok(())
                                    };
                                    let saved = previous.save(&rollback_home);
                                    management::discard_runtime(&rollback_home, &id, &candidate);
                                    saved.context("恢复订阅列表失败")?;
                                    core.context("恢复核心配置失败")
                                })
                                .await;
                            match restored {
                                Ok(()) => Err(error.context("保存当前配置失败，已恢复之前的配置")),
                                Err(restore) => Err(anyhow::anyhow!(
                                    "保存当前配置失败 · {error:#}；{restore:#}"
                                )),
                            }
                        }
                    }
                }
            };
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                view.generation += 1;
                view.notice = None;
                match result {
                    Ok((catalog, running)) => {
                        view.catalog = catalog;
                        view.notice = Some(
                            if running {
                                "配置已加载到核心。"
                            } else {
                                "配置已选择，可在概览中启动核心。"
                            }
                            .into(),
                        );
                        if view.visible {
                            if let Some(owner) = view.owner.upgrade() {
                                owner.update(cx, |owner, cx| {
                                    owner.page = SettingsPage::MihomoOverview;
                                    cx.notify();
                                });
                            }
                            view.page = Page::Overview;
                            view.refresh(cx);
                        }
                    }
                    Err(error) => view.error = Some(format!("{error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn remove_profile(&mut self, id: String, cx: &mut Context<Self>) {
        let mut catalog = self.catalog.clone();
        let home = self.home.clone();
        self.job(
            "正在删除配置…",
            cx,
            move || {
                catalog.remove(&home, &id)?;
                Ok(catalog)
            },
            |view, catalog, _| {
                view.catalog = catalog;
            },
        );
    }

    fn control(
        &mut self,
        label: &str,
        cx: &mut Context<Self>,
        action: impl FnOnce(Controller) -> Result<()> + Send + 'static,
    ) {
        let config = self.launcher.read(cx).settings_snapshot().mihomo;
        self.job(
            label,
            cx,
            move || action(Controller::new(&config)?),
            |view, _, cx| {
                view.notice = Some("操作已完成。".into());
                view.refresh(cx);
            },
        );
    }

    fn profiles(&self, cx: &mut Context<Self>) -> Div {
        let can_import = !self.busy && self.loaded && !self.url.read(cx).value().trim().is_empty();
        let mut body = column().gap_4().child(
            section("添加配置").child(
                column()
                    .gap_3()
                    .child(input_field("subscription-name", "配置名称", &self.name, cx))
                    .child(input_field("subscription-url", "订阅链接", &self.url, cx))
                    .child(
                        row()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                button("add-subscription", "导入订阅", true, can_import)
                                    .on_click(cx.listener(|view, _, _, cx| view.import_remote(cx))),
                            )
                            .child(
                                button(
                                    "import-local",
                                    "导入本地文件",
                                    false,
                                    !self.busy && self.loaded,
                                )
                                .on_click(cx.listener(|view, _, _, cx| view.import_local(cx))),
                            )
                            .child(button("core-settings", "内核设置", false, true).on_click(
                                cx.listener(|view, _, _, cx| {
                                    view.hide();
                                    let _ = view.owner.update(cx, |owner, cx| {
                                        owner.page = SettingsPage::Mihomo;
                                        cx.notify();
                                    });
                                }),
                            )),
                    )
                    .child(description(
                        "支持 Clash / Mihomo YAML 订阅；导入后选择要使用的配置。",
                    )),
            ),
        );
        if self.catalog.profiles.is_empty() {
            let current = self
                .launcher
                .read(cx)
                .settings_snapshot()
                .mihomo
                .config_path;
            body = body.child(section("配置文件").child(description(
                if current.as_os_str().is_empty() {
                    "尚未添加配置。".into()
                } else {
                    format!(
                        "当前使用：{}。可导入本地文件将其加入配置列表。",
                        current.display()
                    )
                },
            )));
        }
        for profile in &self.catalog.profiles {
            let active = self.catalog.selected.as_deref() == Some(&profile.id);
            let id = profile.id.clone();
            let update = id.clone();
            let remove = id.clone();
            let mut card = column()
                .id((ElementId::from("mihomo-profile-card"), profile.id.clone()))
                .gap_3()
                .p_4()
                .rounded(px(10.))
                .border_1()
                .border_color(rgb(if active { ACCENT } else { 0xe5e5e5 }))
                .bg(rgb(0xffffff))
                .child(
                    row()
                        .justify_between()
                        .gap_3()
                        .child(title(&profile.name, 17.))
                        .child(Tag::secondary().small().child(if active {
                            "正在使用"
                        } else if profile.remote() {
                            "订阅"
                        } else {
                            "本地"
                        })),
                )
                .child(description(format!(
                    "{} · {} 个内联节点 · {} 个策略组 · {}",
                    profile.origin(),
                    profile.proxies,
                    profile.groups,
                    updated_label(profile.updated)
                )));
            if let Some(usage) = &profile.usage {
                card = card.child(description(format!(
                    "已用 {} / {}{}",
                    bytes(usage.upload.saturating_add(usage.download)),
                    if usage.total == 0 {
                        "不限量".into()
                    } else {
                        bytes(usage.total)
                    },
                    if usage.expire > 0 {
                        if usage.expire <= management::now() {
                            " · 已过期".into()
                        } else {
                            format!(
                                " · 剩余 {} 天",
                                (usage.expire - management::now()).div_ceil(86400)
                            )
                        }
                    } else {
                        String::new()
                    }
                )));
            }
            card = card.child(
                row()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        button(
                            (ElementId::from("use-profile"), id.clone()),
                            if active {
                                "重新加载"
                            } else {
                                "使用配置"
                            },
                            !active,
                            !self.busy,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| view.activate(id.clone(), cx))),
                    )
                    .child(
                        button(
                            (ElementId::from("update-profile"), update.clone()),
                            "更新",
                            false,
                            !self.busy,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.update_profile(update.clone(), cx)
                        })),
                    )
                    .child(
                        button(
                            (ElementId::from("remove-profile"), remove.clone()),
                            "删除",
                            false,
                            !self.busy && !active,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.remove_profile(remove.clone(), cx)
                        })),
                    ),
            );
            body = body.child(card);
        }
        body
    }

    fn overview(&self, cx: &mut Context<Self>) -> Div {
        let snapshot = self.launcher.read(cx).settings_snapshot();
        let running = snapshot.mihomo_state.status.is_running();
        let busy = snapshot.mihomo_state.status.is_busy() || self.busy;
        let mode = string(&self.live.config, "mode");
        let tun = self
            .live
            .config
            .pointer("/tun/enable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let selected = self
            .catalog
            .selected
            .as_ref()
            .and_then(|id| self.catalog.profiles.iter().find(|p| &p.id == id))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "本地配置".into());
        column()
            .gap_4()
            .child(
                row()
                    .flex_wrap()
                    .gap_3()
                    .child(metric("下载速度", format!("{}/s", bytes(self.rates.1))))
                    .child(metric("上传速度", format!("{}/s", bytes(self.rates.0))))
                    .child(metric(
                        "活动连接",
                        array(&self.live.connections, "connections")
                            .len()
                            .to_string(),
                    )),
            )
            .child(
                section("运行状态").child(
                    column()
                        .gap_3()
                        .child(title(
                            if running {
                                "Mihomo 正在运行"
                            } else {
                                "Mihomo 未运行"
                            },
                            18.,
                        ))
                        .child(description(format!("当前配置：{selected}")))
                        .child(
                            row()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    button(
                                        "manager-start",
                                        "启动核心",
                                        true,
                                        !busy && !running && snapshot.mihomo.configured(),
                                    )
                                    .on_click(cx.listener(
                                        |view, _, _, cx| {
                                            view.launcher
                                                .update(cx, |app, cx| app.start_mihomo(cx));
                                        },
                                    )),
                                )
                                .child(
                                    button("manager-stop", "停止", false, !busy && running)
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.launcher.update(cx, |app, cx| app.stop_mihomo(cx));
                                        })),
                                )
                                .child(
                                    button("manager-reload", "重新加载", false, !busy && running)
                                        .on_click(cx.listener(|view, _, _, cx| {
                                            view.launcher
                                                .update(cx, |app, cx| app.reload_mihomo(cx));
                                        })),
                                ),
                        ),
                ),
            )
            .child(
                section("代理模式").child(
                    row().flex_wrap().gap_2().children(
                        [("rule", "规则"), ("global", "全局"), ("direct", "直连")]
                            .into_iter()
                            .map(|(value, label)| {
                                button(
                                    (ElementId::from("proxy-mode"), value),
                                    label,
                                    mode == value,
                                    running && !self.busy,
                                )
                                .on_click(cx.listener(
                                    move |view, _, _, cx| {
                                        view.control("正在切换模式…", cx, move |client| {
                                            client.patch(json!({"mode":value}))
                                        })
                                    },
                                ))
                            }),
                    ),
                ),
            )
            .child(
                section("网络设置").child(
                    column()
                        .gap_4()
                        .child(
                            row()
                                .justify_between()
                                .gap_4()
                                .child(column().child(Label::new("TUN 模式")).child(description(
                                    "由 Mihomo 接管流量，需要核心具备相应系统权限。",
                                )))
                                .child(
                                    Switch::new("mihomo-tun")
                                        .checked(tun)
                                        .disabled(!running || self.busy)
                                        .on_click(cx.listener(|view, checked: &bool, _, cx| {
                                            let enabled = *checked;
                                            view.control("正在更新 TUN…", cx, move |client| {
                                                client.patch(json!({"tun":{"enable":enabled}}))
                                            });
                                        })),
                                ),
                        )
                        .child(
                            row()
                                .justify_between()
                                .gap_4()
                                .child(Label::new("允许局域网访问"))
                                .child(
                                    Switch::new("mihomo-lan")
                                        .checked(
                                            self.live
                                                .config
                                                .get("allow-lan")
                                                .and_then(Value::as_bool)
                                                .unwrap_or(false),
                                        )
                                        .disabled(!running || self.busy)
                                        .on_click(cx.listener(|view, checked: &bool, _, cx| {
                                            let enabled = *checked;
                                            view.control(
                                                "正在更新网络设置…",
                                                cx,
                                                move |client| {
                                                    client.patch(json!({"allow-lan":enabled}))
                                                },
                                            );
                                        })),
                                ),
                        )
                        .child(description(format!(
                            "Mixed 端口：{} · 控制器：{}",
                            number(&self.live.config, "mixed-port"),
                            snapshot.mihomo.controller
                        )))
                        .child(description(
                            "模式和网络开关即时生效；重新加载或重启后以配置文件为准。",
                        )),
                ),
            )
    }

    fn proxies(&self, cx: &mut Context<Self>) -> Div {
        let proxies = self.live.proxies.get("proxies").and_then(Value::as_object);
        let Some(proxies) = proxies else {
            return column().child(description("启动核心后将显示策略组和节点。"));
        };
        let mut groups = proxies
            .iter()
            .filter(|(_, p)| p.get("all").is_some())
            .collect::<Vec<_>>();
        groups.sort_by_key(|(name, _)| *name);
        let selected = self
            .selected_group
            .as_deref()
            .and_then(|name| groups.iter().position(|(key, _)| key.as_str() == name))
            .unwrap_or(0);
        let group = groups.get(selected);
        let filter = self.filter.read(cx).value().to_lowercase();
        let mut body = column()
            .gap_4()
            .child(input_field("proxy-filter", "搜索节点", &self.filter, cx))
            .child(
                row()
                    .flex_wrap()
                    .gap_2()
                    .children(groups.iter().enumerate().map(|(i, (name, p))| {
                        let name = (*name).clone();
                        button(
                            (ElementId::from("proxy-group"), name.clone()),
                            format!("{name} · {}", string(p, "now")),
                            i == selected,
                            !self.busy,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.selected_group = Some(name.clone());
                            cx.notify();
                        }))
                    })),
            );
        if let Some((group_name, group)) = group {
            let now = string(group, "now");
            let kind = string(group, "type");
            let selectable = matches!(kind.as_str(), "Selector" | "URLTest" | "Fallback");
            let nodes = array(group, "all")
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| name.to_lowercase().contains(&filter))
                .collect::<Vec<_>>();
            body = body.child(description(format!(
                "{kind} · {} 个节点{}",
                nodes.len(),
                if selectable {
                    " · 点击节点切换"
                } else {
                    " · 核心自动分配"
                }
            )));
            for node in nodes.into_iter().take(250) {
                let group_name = (*group_name).clone();
                let name = node.to_owned();
                let test = name.clone();
                let active = now == node;
                let latency = self.delays.get(node).copied().or_else(|| {
                    proxies
                        .get(node)
                        .and_then(|p| p.get("history"))
                        .and_then(Value::as_array)
                        .and_then(|v| v.last())
                        .and_then(|v| v.get("delay"))
                        .and_then(Value::as_u64)
                });
                body = body.child(
                    row()
                        .id((ElementId::from("mihomo-node-row"), node.to_owned()))
                        .gap_3()
                        .child(
                            button(
                                (ElementId::from("select-node"), name.clone()),
                                format!("{} {name}", if active { "●" } else { "○" }),
                                active,
                                selectable && !self.busy,
                            )
                            .flex_1()
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    let group = group_name.clone();
                                    let name = name.clone();
                                    view.control("正在切换节点…", cx, move |client| {
                                        client.select_proxy(&group, &name)
                                    });
                                },
                            )),
                        )
                        .child(description(latency.map_or("未测速".into(), |delay| {
                            if delay == 0 {
                                "超时".into()
                            } else {
                                format!("{delay} ms")
                            }
                        })))
                        .child(
                            button(
                                (ElementId::from("test-node"), test.clone()),
                                "测速",
                                false,
                                !self.busy,
                            )
                            .on_click(cx.listener(
                                move |view, _, _, cx| {
                                    let name = test.clone();
                                    let config = view.launcher.read(cx).settings_snapshot().mihomo;
                                    view.job(
                                        "正在测速…",
                                        cx,
                                        move || {
                                            Ok((
                                                name.clone(),
                                                Controller::new(&config)?.delay(&name)?,
                                            ))
                                        },
                                        |view, (name, delay), _| {
                                            view.delays.insert(name, delay);
                                        },
                                    );
                                },
                            )),
                        ),
                );
            }
            body = body.child(description(
                "每次最多显示 250 个节点，可通过上方搜索框缩小范围。",
            ));
        }
        body
    }

    fn connections(&self, cx: &mut Context<Self>) -> Div {
        let filter = self.filter.read(cx).value().to_lowercase();
        let connections = array(&self.live.connections, "connections");
        let mut body = column().gap_3().child(
            row()
                .gap_3()
                .child(div().flex_1().child(input_field(
                    "connection-filter",
                    "搜索连接",
                    &self.filter,
                    cx,
                )))
                .child(
                    button(
                        "close-all-connections",
                        "断开全部",
                        false,
                        !self.busy && !connections.is_empty(),
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.control("正在断开连接…", cx, |client| client.close(None))
                    })),
                ),
        );
        for connection in connections
            .iter()
            .filter(|c| c.to_string().to_lowercase().contains(&filter))
            .take(200)
        {
            let metadata = connection.get("metadata").unwrap_or(&Value::Null);
            let host = string(metadata, "host");
            let destination = if host.is_empty() {
                string(metadata, "destinationIP")
            } else {
                host
            };
            let id = string(connection, "id");
            let chains = array(connection, "chains")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" → ");
            body = body.child(
                row()
                    .id((ElementId::from("mihomo-connection-row"), id.clone()))
                    .gap_3()
                    .p_3()
                    .border_b_1()
                    .border_color(rgb(0xe5e5e5))
                    .child(
                        column()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(format!(
                                "{destination}:{}",
                                metadata
                                    .get("destinationPort")
                                    .map(|v| v
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| v.to_string()))
                                    .unwrap_or_default()
                            )))
                            .child(description(format!(
                                "{} · {} · {chains}",
                                string(metadata, "network"),
                                string(connection, "rule")
                            ))),
                    )
                    .child(description(format!(
                        "↓{}  ↑{}",
                        bytes(number(connection, "download")),
                        bytes(number(connection, "upload"))
                    )))
                    .child(
                        button(
                            (ElementId::from("close-connection"), id.clone()),
                            "断开",
                            false,
                            !self.busy,
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            let id = id.clone();
                            view.control("正在断开连接…", cx, move |client| {
                                client.close(Some(&id))
                            });
                        })),
                    ),
            );
        }
        body.child(description(format!(
            "{} 个活动连接 · 每次最多显示 200 条，可搜索过滤。",
            connections.len()
        )))
    }

    fn rules(&self, cx: &mut Context<Self>) -> Div {
        let filter = self.filter.read(cx).value().to_lowercase();
        let rules = array(&self.live.rules, "rules");
        column()
            .gap_3()
            .child(input_field("rule-filter", "搜索规则", &self.filter, cx))
            .child(description(format!(
                "{} 条规则 · 按核心返回顺序展示",
                rules.len()
            )))
            .children(
                rules
                    .iter()
                    .filter(|rule| rule.to_string().to_lowercase().contains(&filter))
                    .take(300)
                    .enumerate()
                    .map(|(i, rule)| {
                        ListItem::new(("mihomo-rule", i)).child(
                            row()
                                .gap_3()
                                .child(Tag::secondary().small().child(string(rule, "type")))
                                .child(Label::new(string(rule, "payload")).flex_1().min_w_0())
                                .child(description(string(rule, "proxy"))),
                        )
                    }),
            )
            .child(description(
                "每次最多显示 300 条，输入关键词可检索其余规则。",
            ))
    }
}

impl Render for MihomoPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.launcher.read(cx).settings_snapshot();
        let content = match self.page {
            Page::Overview => self.overview(cx),
            Page::Profiles => self.profiles(cx),
            Page::Proxies => self.proxies(cx),
            Page::Connections => self.connections(cx),
            Page::Rules => self.rules(cx),
            Page::Logs => column()
                .gap_3()
                .child(description("核心日志 · 每两秒刷新 · 保留末尾 200 行"))
                .child(
                    button("open-log-folder", "打开日志目录", false, true).on_click(cx.listener(
                        |view, _, _, _| {
                            let _ = super::super::open_path(&view.home.join("mihomo/logs"));
                        },
                    )),
                )
                .child(
                    div()
                        .p_4()
                        .rounded(px(8.))
                        .bg(rgb(0xf0f0f0))
                        .font_family(crate::app::appearance::mono_font_family())
                        .text_size(px(12.))
                        .child(self.live.logs.clone()),
                ),
        };
        column()
            .gap_3()
            .child(
                row()
                    .justify_between()
                    .gap_3()
                    .child(description(if snapshot.mihomo_state.status.is_running() {
                        if self.api_error.is_some() && self.page != Page::Profiles {
                            "● 核心运行中 · 控制器连接异常"
                        } else {
                            "● 核心运行中"
                        }
                    } else {
                        "○ 核心未运行"
                    }))
                    .child(
                        button(
                            "refresh-manager",
                            if self.polling {
                                "刷新中…"
                            } else {
                                "刷新"
                            },
                            false,
                            !self.busy && !self.polling,
                        )
                        .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    ),
            )
            .children(
                self.error
                    .clone()
                    .or_else(|| snapshot.mihomo_error.clone())
                    .or_else(|| snapshot.mihomo_state.error.clone())
                    .into_iter()
                    .map(error_message),
            )
            .when(self.page != Page::Profiles, |body| {
                body.children(self.api_error.clone().into_iter().map(error_message))
            })
            .children(self.notice.clone().into_iter().map(description))
            .child(content)
    }
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}
fn bytes(value: u64) -> String {
    let value = value as f64;
    if value >= 1024. * 1024. * 1024. {
        format!("{:.1} GiB", value / (1024. * 1024. * 1024.))
    } else if value >= 1024. * 1024. {
        format!("{:.1} MiB", value / (1024. * 1024.))
    } else if value >= 1024. {
        format!("{:.1} KiB", value / 1024.)
    } else {
        format!("{value:.0} B")
    }
}
fn updated_label(updated: u64) -> String {
    let seconds = management::now().saturating_sub(updated);
    if seconds < 60 {
        "刚刚更新".into()
    } else if seconds < 3600 {
        format!("{} 分钟前更新", seconds / 60)
    } else if seconds < 86400 {
        format!("{} 小时前更新", seconds / 3600)
    } else {
        format!("{} 天前更新", seconds / 86400)
    }
}
fn metric(label: &str, value: String) -> impl IntoElement {
    column()
        .id((ElementId::from("mihomo-metric"), label.to_owned()))
        .flex_1()
        .min_w(px(135.))
        .gap_2()
        .p_4()
        .rounded(px(10.))
        .border_1()
        .border_color(rgb(0xe5e5e5))
        .bg(rgb(0xffffff))
        .child(description(label.to_owned()))
        .child(title(&value, 24.))
}
