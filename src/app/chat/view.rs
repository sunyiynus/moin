use super::*;
impl AiPane {
    pub(super) fn ai_workspace(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .w(px(self.sidebar_width))
                    .flex_shrink_0()
                    .min_h_0()
                    .border_r_1()
                    .border_color(rgba(CHAT_LINE))
                    .bg(rgba(0x00000012))
                    .flex()
                    .flex_col()
                    .px(px(10.))
                    .py(px(12.))
                    .gap(px(10.))
                    // .child(
                    //     row().px(px(6.)).justify_between().child(
                    //         div()
                    //             .text_size(rem(12.))
                    //             .text_color(rgb(CHAT_MUTED))
                    //             .child("会话"),
                    //     ),
                    // )
                    .child(
                        div()
                            .id("ai-session-list")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .children(self.sessions.iter().enumerate().map(|(index, session)| {
                                let id = session.id;
                                let title = session.turns.first().map_or_else(
                                    || "新对话".to_owned(),
                                    |turn| {
                                        turn.question
                                            .lines()
                                            .next()
                                            .unwrap_or("新对话")
                                            .chars()
                                            .take(40)
                                            .collect()
                                    },
                                );
                                row()
                                    .gap(px(2.))
                                    .rounded(px(6.))
                                    .when(index == self.selected, |s| s.bg(rgba(0xffffff15)))
                                    .child(
                                        chat_button(("session-select", id), "")
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().flex_1().min_w_0().truncate().child(title))
                                            .when(session.busy, |s| {
                                                s.child(
                                                    div()
                                                        .size(px(5.))
                                                        .rounded_full()
                                                        .bg(rgb(CHAT_ACCENT)),
                                                )
                                            })
                                            .on_click(cx.listener(move |view, _, window, cx| {
                                                if let Some(index) = view
                                                    .sessions
                                                    .iter()
                                                    .position(|session| session.id == id)
                                                {
                                                    view.selected = index;
                                                }
                                                view.focus_input(window, cx);
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        chat_button(("session-delete", id), "删除")
                                            .px(px(4.))
                                            .text_size(rem(10.))
                                            .on_click(cx.listener(move |view, _, window, cx| {
                                                view.delete_session(id, window, cx);
                                                view.focus_input(window, cx);
                                                cx.notify();
                                            })),
                                    )
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.ai_body(window, cx))
                    .child(self.ai_composer(cx))
                    .child(self.ai_footer(window, cx)),
            )
    }

    pub(super) fn ai_header(&self, _window: &Window, cx: &mut Context<Self>) -> Div {
        row()
            .h(rem(52.))
            .flex_shrink_0()
            .px(px(20.))
            .gap(px(12.))
            .child(
                chat_button(
                    "ai-back",
                    if self.detached {
                        "浮窗对话"
                    } else {
                        "返回搜索"
                    },
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    if view.detached {
                        view.return_ai_popup(cx);
                    } else {
                        view.set_ai(false, window, cx);
                    }
                })),
            )
            .child(div().w(px(1.)).h(px(16.)).bg(rgba(CHAT_LINE)))
            .child(
                div()
                    .text_size(rem(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Moin"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(rem(12.))
                    .text_color(rgb(CHAT_MUTED))
                    .child(if self.model_config.model.is_empty() {
                        "选择模型".to_owned()
                    } else {
                        self.model_config.model.clone()
                    }),
            )
            .child(
                chat_button("new-ai-chat", "新对话")
                    .child(keycap("Ctrl+N"))
                    .on_click(cx.listener(|view, _, window, cx| view.new_ai_chat(window, cx))),
            )
            .child(
                chat_button("ai-header-settings", "设置")
                    .on_click(cx.listener(|view, _, window, cx| view.open_ai_settings(window, cx))),
            )
            .when(!self.detached, |header| {
                header.child(
                    chat_button("ai-detach", "单独窗口").on_click(
                        cx.listener(|view, _, window, cx| view.open_ai_window(window, cx)),
                    ),
                )
            })
    }

    pub(super) fn ai_body(&self, _window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let appearance = &self.appearance;
        let family = crate::app::appearance::resolve_ai_family(appearance);
        let ai_size = appearance.ai_font_size;
        let ai_line = ai_size * 1.65;
        let style = markdown::MarkdownStyle {
            font_family: family.clone(),
            font_size: ai_size,
            line_height: ai_line,
            text_color: CHAT_INK,
            muted_color: CHAT_MUTED,
            link_color: CHAT_ACCENT,
            code_background: 0x101114b0,
            rule_color: CHAT_LINE,
        };
        let mut body = div()
            .id("ai-conversation")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(self.content_gutter))
            .py(px(20.))
            .font_family(family)
            .flex()
            .flex_col()
            .gap(px(30.));
        if self.turns.is_empty() {
            let configured = !self.model_config.model.trim().is_empty();
            let welcome =
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap(px(16.))
                    .child(chat_avatar(40.))
                    .child(
                        div()
                            .text_size(rem(25.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if configured {
                                "今天想完成什么？"
                            } else {
                                "连接模型，开始对话"
                            }),
                    )
                    .child(
                        div()
                            .text_size(rem(13.))
                            .line_height(rem(21.))
                            .text_color(rgb(CHAT_MUTED))
                            .child(if configured {
                                "提问、检索文档，或交给 Moin 执行任务。"
                            } else {
                                "配置本机 Ollama 或 OpenAI 兼容服务。"
                            }),
                    )
                    .child(if configured {
                        row()
                            .flex_wrap()
                            .justify_center()
                            .gap(px(8.))
                            .mt(px(6.))
                            .children(
                                [
                                    ("解释代码", "请解释下面这段代码："),
                                    ("检索文档", "请在本地文档中查找："),
                                    ("执行任务", "请在命令工作目录中执行以下任务："),
                                ]
                                .into_iter()
                                .enumerate()
                                .map(|(i, (label, prompt))| {
                                    chat_button(("ai-suggestion", i), label)
                                        .border_1()
                                        .border_color(rgba(CHAT_LINE))
                                        .rounded(px(18.))
                                        .px(px(14.))
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.input.update(cx, |input, cx| {
                                                input.set_value(prompt.into(), cx)
                                            });
                                            view.focus_input(window, cx);
                                        }))
                                }),
                            )
                    } else {
                        row().child(
                            chat_button("configure-ai", "配置模型")
                                .bg(rgba(0x4d6bfe30))
                                .text_color(rgb(CHAT_ACCENT))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.open_ai_settings(window, cx)
                                })),
                        )
                    });
            body = body.child(welcome);
        }
        for (i, turn) in self.turns.iter().enumerate() {
            let mut block = div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .min_w_0()
                .gap(px(16.))
                .child(
                    div().w_full().flex().justify_end().child(
                        div()
                            .max_w(relative(0.78))
                            .min_w_0()
                            .px(px(18.))
                            .py(px(12.))
                            .rounded(px(16.))
                            .bg(rgba(0x34363bdf))
                            .child(
                                turn.question_markdown
                                    .render(("ai-question-markdown", i), &style),
                            ),
                    ),
                );
            for (j, (request, report, expanded)) in turn.commands.iter().enumerate() {
                let running = report.is_none() && self.busy && i + 1 == self.turns.len();
                let (state, color) = match report {
                    Some(report) if report.timed_out => ("超时".to_owned(), 0xf3b86c),
                    Some(report) if report.success => ("已完成".to_owned(), 0x83c9a2),
                    Some(report) => (
                        format!(
                            "失败 · {}",
                            report
                                .exit_code
                                .map_or("未启动".into(), |code| code.to_string())
                        ),
                        0xffa29b,
                    ),
                    None if running => ("执行中".into(), CHAT_ACCENT),
                    None => ("已结束".into(), CHAT_MUTED),
                };
                let mut execution = div().min_w_0().flex().flex_col().gap(px(6.)).child(
                    row()
                        .id(("command-disclosure", i * 100 + j))
                        .tab_index(0)
                        .min_h(rem(30.))
                        .min_w_0()
                        .gap(px(8.))
                        .px(px(6.))
                        .rounded(px(5.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(0xffffff08)))
                        .focus(|s| s.bg(rgba(0x4d6bfe20)))
                        .child(
                            div()
                                .text_size(rem(12.))
                                .text_color(rgb(CHAT_MUTED))
                                .child(if *expanded { "▾" } else { "▸" }),
                        )
                        .child(div().size(px(5.)).rounded_full().bg(rgb(color)))
                        .child(
                            div()
                                .text_size(rem(12.))
                                .text_color(rgb(CHAT_MUTED))
                                .child("执行命令"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(crate::app::appearance::mono_font_family())
                                .text_size(rem(12.))
                                .text_color(rgb(CHAT_MUTED))
                                .child(request.script.lines().next().unwrap_or("").to_owned()),
                        )
                        .child(
                            div()
                                .text_size(rem(12.))
                                .text_color(rgb(color))
                                .child(state),
                        )
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if let Some((_, _, expanded)) = view
                                .turns
                                .get_mut(i)
                                .and_then(|turn| turn.commands.get_mut(j))
                            {
                                *expanded = !*expanded;
                            }
                            cx.notify();
                        })),
                );
                if *expanded {
                    let code = |label: String, content: String, id| {
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(rem(12.))
                                    .text_color(rgb(CHAT_MUTED))
                                    .child(label),
                            )
                            .child(
                                div()
                                    .id(id)
                                    .w_full()
                                    .min_w_0()
                                    .max_h(px(180.))
                                    .overflow_y_scroll()
                                    .overflow_x_scroll()
                                    .font_family(crate::app::appearance::mono_font_family())
                                    .text_size(rem(12.))
                                    .line_height(rem(20.))
                                    .whitespace_nowrap()
                                    .child(content),
                            )
                    };
                    let mut details = div()
                        .ml(px(20.))
                        .p(px(14.))
                        .rounded(px(8.))
                        .bg(rgba(0x101114b0))
                        .border_1()
                        .border_color(rgba(CHAT_LINE))
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap(px(12.))
                        .child(code(
                            request.working_directory.clone(),
                            request.script.clone(),
                            ("command-script", i * 100 + j),
                        ));
                    if let Some(report) = report {
                        if !report.stdout.is_empty() {
                            details = details.child(code(
                                "标准输出".into(),
                                report.stdout.clone(),
                                ("command-stdout", i * 100 + j),
                            ));
                        }
                        if !report.stderr.is_empty() {
                            details = details.child(code(
                                "错误输出".into(),
                                report.stderr.clone(),
                                ("command-stderr", i * 100 + j),
                            ));
                        }
                        if report.truncated {
                            details = details.child(
                                div()
                                    .text_size(rem(12.))
                                    .text_color(rgb(CHAT_MUTED))
                                    .child("输出过长，已截断"),
                            );
                        }
                    }
                    execution = execution.child(details);
                }
                block = block.child(execution);
            }
            block = block.child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .gap(px(12.))
                    .child(
                        row().gap(px(8.)).child(chat_avatar(22.)).child(
                            div()
                                .text_size(rem(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(CHAT_INK))
                                .child("Moin"),
                        ),
                    )
                    .child(if turn.answer.is_empty() {
                        div()
                            .text_size(px(ai_size))
                            .line_height(px(ai_line))
                            .text_color(rgb(CHAT_MUTED))
                            .child(if self.busy && i + 1 == self.turns.len() {
                                self.status.clone()
                            } else if turn.stopped {
                                "已停止生成".into()
                            } else {
                                "尚未收到回复".into()
                            })
                    } else {
                        turn.answer_markdown
                            .render(("ai-answer-markdown", i), &style)
                    }),
            );
            if !turn.sources.is_empty() {
                block = block.child(row().flex_wrap().gap(px(6.)).children(
                    turn.sources.iter().enumerate().map(|(j, source)| {
                        let path = source.path.clone();
                        chat_button(
                            ("ai-source", i * 100 + j),
                            format!("[{}] {} · {}", j + 1, source.title, source.chunk),
                        )
                        .max_w_full()
                        .min_w_0()
                        .border_1()
                        .border_color(rgba(CHAT_LINE))
                        .text_color(rgb(CHAT_ACCENT))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if let Err(error) = open_path(&path) {
                                view.error = Some(format!("无法打开来源 · {error:#}"));
                            }
                            cx.notify();
                        }))
                    }),
                ));
            }
            if !turn.answer.is_empty() {
                block =
                    block.child(
                        row()
                            .gap(px(6.))
                            .child(chat_button(("copy-ai-turn", i), "复制回复").on_click(
                                cx.listener(move |view, _, _, cx| {
                                    if let Some(turn) = view.turns.get(i) {
                                        let mut output = turn.answer.clone();
                                        for (index, source) in turn.sources.iter().enumerate() {
                                            output.push_str(&format!(
                                                "\n[{}] {} · 片段 {}",
                                                index + 1,
                                                source.path.display(),
                                                source.chunk
                                            ));
                                        }
                                        cx.write_to_clipboard(ClipboardItem::new_string(output));
                                        if !view.busy {
                                            view.status = "已复制回复".into();
                                        }
                                        cx.notify();
                                    }
                                }),
                            ))
                            .when(turn.stopped, |row| {
                                row.child(
                                    div()
                                        .text_size(rem(12.))
                                        .text_color(rgb(CHAT_MUTED))
                                        .child("已停止 · 不加入上下文"),
                                )
                            }),
                    );
            }
            body = body.child(block);
        }
        if let Some(error) = self.error.as_ref().or(self.config_error.as_ref()) {
            body = body.child(
                div()
                    .flex_shrink_0()
                    .p(px(12.))
                    .rounded(px(8.))
                    .bg(rgba(0xec6c5214))
                    .child(
                        div()
                            .text_size(rem(13.))
                            .line_height(rem(21.))
                            .text_color(rgb(0xffb4a5))
                            .child(error.clone()),
                    )
                    .child(
                        row()
                            .gap(px(8.))
                            .mt(px(8.))
                            .when(
                                self.turns.last().is_some_and(|turn| !turn.complete) && !self.busy,
                                |row| {
                                    row.child(
                                        chat_button("retry-ai", "重试").on_click(
                                            cx.listener(|view, _, _, cx| view.retry_ai(cx)),
                                        ),
                                    )
                                },
                            )
                            .child(chat_button("fix-ai-settings", "模型设置").on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.open_ai_settings(window, cx)
                                }),
                            )),
                    ),
            );
        } else if !self.busy && self.turns.last().is_some_and(|turn| turn.stopped) {
            body = body.child(
                chat_button("continue-stopped-ai", "重新生成")
                    .on_click(cx.listener(|view, _, _, cx| view.retry_ai(cx))),
            );
        }
        body
    }

    pub(super) fn ai_composer(&self, cx: &mut Context<Self>) -> Div {
        let enabled = !self.input.read(cx).value().trim().is_empty();
        let family = crate::app::appearance::resolve_ai_family(&self.appearance);
        let focus = self.input.read(cx).focus_handle(cx);
        div()
            .flex_shrink_0()
            .px(px((self.content_gutter - 8.).max(12.)))
            .pt(px(8.))
            .child(
                div()
                    .id("ai-composer-card")
                    .track_focus(&focus)
                    .rounded(px(18.))
                    .border_1()
                    .border_color(rgba(0xffffff20))
                    .bg(rgba(0xffffff10))
                    .focus(|s| s.border_color(rgb(CHAT_ACCENT)))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .px(px(18.))
                    .pt(px(14.))
                    .pb(px(10.))
                    .child(
                        div()
                            .min_h(rem(30.))
                            .min_w_0()
                            .font_family(family)
                            .child(self.input.clone()),
                    )
                    .child(
                        row()
                            .gap(px(8.))
                            .child(
                                chat_button(
                                    "ai-documents-capability",
                                    if self.model_config.use_local_documents {
                                        "本地检索 · 开启"
                                    } else {
                                        "本地检索 · 关闭"
                                    },
                                )
                                .border_1()
                                .border_color(rgba(CHAT_LINE))
                                .rounded(px(14.))
                                .on_click(cx.listener(
                                    |view, _, window, cx| view.open_ai_settings(window, cx),
                                )),
                            )
                            .child(
                                chat_button(
                                    "ai-command-capability",
                                    if self.model_config.allow_commands {
                                        "命令执行 · 开启"
                                    } else {
                                        "命令执行 · 关闭"
                                    },
                                )
                                .border_1()
                                .border_color(rgba(CHAT_LINE))
                                .rounded(px(14.))
                                .when(self.model_config.allow_commands, |s| {
                                    s.text_color(rgb(CHAT_ACCENT))
                                })
                                .on_click(cx.listener(
                                    |view, _, window, cx| view.open_ai_settings(window, cx),
                                )),
                            )
                            .child(div().flex_1())
                            .child(if self.busy {
                                chat_button("stop-ai", "停止")
                                    .bg(rgba(0xffffff18))
                                    .rounded(px(16.))
                                    .child(keycap("Esc"))
                                    .on_click(cx.listener(|view, _, _, cx| view.stop_ai(cx)))
                            } else {
                                chat_button("send-ai", "发送 ↑")
                                    .rounded(px(16.))
                                    .px(px(14.))
                                    .when(enabled, |s| {
                                        s.bg(rgb(0x4d6bfe)).text_color(rgb(0xffffff)).on_click(
                                            cx.listener(|view, _, window, cx| {
                                                view.send_ai(window, cx)
                                            }),
                                        )
                                    })
                                    .when(!enabled, |s| {
                                        s.bg(rgba(0xffffff0c)).opacity(0.45).cursor_default()
                                    })
                            }),
                    ),
            )
    }

    pub(super) fn ai_footer(&self, _window: &Window, cx: &mut Context<Self>) -> Div {
        row()
            .h(rem(28.))
            .flex_shrink_0()
            .px(px(self.content_gutter))
            .gap(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(rem(11.))
                    .text_color(rgb(CHAT_MUTED))
                    .child(if self.status.is_empty() {
                        if self.busy {
                            "Esc 停止生成 · 对话仅在本次运行中保留".into()
                        } else {
                            "Enter 发送 · Esc 返回搜索 · 对话仅在本次运行中保留".into()
                        }
                    } else {
                        self.status.clone()
                    }),
            )
            .child(
                chat_button("ai-scroll-bottom", "回到底部").on_click(cx.listener(
                    move |view, _, _, cx| {
                        view.scroll.scroll_to_bottom();
                        cx.notify();
                    },
                )),
            )
    }
}
