//! 新建/编辑连接:类型、名称、环境、主机端口、用户密码、数据库(SQLite 为文件),
//! 带连接串预览与真实连通性测试。

use crate::runtime;
use crate::store::{Stores, Toast};
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use rust_i18n::t;
use std::time::{Duration, Instant};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, PartialEq)]
enum TestStatus {
    Idle,
    Testing,
    Success(u128),
    Failed(String),
}

pub struct ConnectionForm {
    id: Option<String>,
    editing: bool,
    kind: DbKind,
    env: Env,
    ssl_mode: SslMode,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    password: Entity<InputState>,
    database: Entity<InputState>,
    file_path: Entity<InputState>,
    test: TestStatus,
    attempt: u64,
    error: Option<String>,
    _test_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConnectionForm {
    fn new(conn: Option<Connection>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editing = conn.is_some();
        let c = conn.unwrap_or(Connection {
            id: String::new(),
            name: String::new(),
            kind: DbKind::Pg,
            env: Env::Local,
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            database: "postgres".into(),
            plaintext_password: None,
            file_path: None,
            ssl_mode: SslMode::default(),
        });
        let input =
            |value: String, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
                let placeholder = placeholder.to_string();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                })
            };
        let name = input(
            c.name.clone(),
            &t!("conn_form.name_placeholder"),
            window,
            cx,
        );
        let host = input(c.host.clone(), "127.0.0.1", window, cx);
        let port = input(
            if c.port == 0 {
                String::new()
            } else {
                c.port.to_string()
            },
            "5432",
            window,
            cx,
        );
        let user = input(c.user.clone(), "postgres", window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(if editing {
                    t!("conn_form.password_keep")
                } else {
                    t!("conn_form.password_none")
                })
        });
        let database = input(c.database.clone(), "postgres", window, cx);
        let file_path = input(
            c.file_path.clone().unwrap_or_default(),
            &t!("conn_form.file_placeholder"),
            window,
            cx,
        );
        // 任一字段变化都要刷新连接串预览
        let subs = [&name, &host, &port, &user, &password, &database, &file_path]
            .into_iter()
            .map(|e| {
                cx.subscribe(e, |_, _, ev: &InputEvent, cx| {
                    if matches!(ev, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        Self {
            id: (!c.id.is_empty()).then_some(c.id.clone()),
            editing,
            kind: c.kind,
            env: c.env,
            ssl_mode: c.ssl_mode,
            name,
            host,
            port,
            user,
            password,
            database,
            file_path,
            test: TestStatus::Idle,
            attempt: 0,
            error: None,
            _test_task: None,
            _subscriptions: subs,
        }
    }

    fn value(e: &Entity<InputState>, cx: &gpui_kit::App) -> String {
        e.read(cx).value().to_string()
    }

    fn set(e: &Entity<InputState>, v: &str, window: &mut Window, cx: &mut Context<Self>) {
        let v = v.to_string();
        e.update(cx, |s, cx| s.set_value(v, window, cx));
    }

    /// 切换类型:按类型填入惯用默认值。
    fn set_kind(&mut self, kind: DbKind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = kind;
        if kind == DbKind::Sqlite {
            self.env = Env::Local;
        } else {
            let (port, user, db) = match kind {
                DbKind::Redis => ("6379", Self::value(&self.user, cx), "0".to_string()),
                DbKind::Mysql => ("3306", "root".to_string(), String::new()),
                _ => ("5432", "postgres".to_string(), "postgres".to_string()),
            };
            Self::set(&self.port, port, window, cx);
            Self::set(&self.user, &user, window, cx);
            Self::set(&self.database, &db, window, cx);
        }
        cx.notify();
    }

    fn connection(&self, cx: &gpui_kit::App) -> Connection {
        let sqlite = self.kind == DbKind::Sqlite;
        Connection {
            id: self
                .id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name: Self::value(&self.name, cx).trim().to_string(),
            kind: self.kind,
            env: self.env,
            host: if sqlite {
                String::new()
            } else {
                Self::value(&self.host, cx).trim().to_string()
            },
            port: if sqlite {
                0
            } else {
                Self::value(&self.port, cx).trim().parse().unwrap_or(0)
            },
            user: if sqlite {
                String::new()
            } else {
                Self::value(&self.user, cx).trim().to_string()
            },
            database: if sqlite {
                String::new()
            } else {
                Self::value(&self.database, cx).trim().to_string()
            },
            plaintext_password: None,
            file_path: sqlite.then(|| Self::value(&self.file_path, cx).trim().to_string()),
            ssl_mode: self.ssl_mode,
        }
    }

    /// 连接串预览(密码以圆点掩码)。
    fn conn_string(&self, cx: &gpui_kit::App) -> String {
        if self.kind == DbKind::Sqlite {
            let p = Self::value(&self.file_path, cx);
            return format!(
                "sqlite://{}",
                if p.is_empty() {
                    t!("conn_form.file_preview").to_string()
                } else {
                    p
                }
            );
        }
        let or = |v: String, d: &str| {
            if v.trim().is_empty() {
                d.to_string()
            } else {
                v
            }
        };
        let host = or(Self::value(&self.host, cx), "127.0.0.1");
        let port = Self::value(&self.port, cx);
        let user = or(Self::value(&self.user, cx), "postgres");
        let db = or(Self::value(&self.database, cx), "postgres");
        let auth = if Self::value(&self.password, cx).is_empty() {
            format!("{user}@")
        } else {
            format!("{user}:••••@")
        };
        let port = if port.trim().is_empty() {
            String::new()
        } else {
            format!(":{}", port.trim())
        };
        let scheme = match (self.kind, self.ssl_mode) {
            (DbKind::Redis, SslMode::Require | SslMode::VerifyFull) => "rediss",
            _ => self.kind.scheme(),
        };
        let query = match (self.kind, self.ssl_mode) {
            (DbKind::Redis, _) => "",
            (_, SslMode::Disable) => "?sslmode=disable",
            (_, SslMode::Prefer) => "?sslmode=prefer",
            (_, SslMode::Require) => "?sslmode=require",
            (_, SslMode::VerifyFull) => "?sslmode=verify-full",
        };
        format!("{scheme}://{auth}{host}{port}/{db}{query}")
    }

    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("conn_form.pick_sqlite").to_string().into()),
        });
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                Self::set(&this.file_path, &path.to_string_lossy(), window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn run_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.test == TestStatus::Testing {
            return;
        }
        self.attempt += 1;
        let attempt = self.attempt;
        self.test = TestStatus::Testing;
        let conn = self.connection(cx);
        let password = Self::value(&self.password, cx);
        let started = Instant::now();
        cx.notify();
        let probe = runtime::spawn(async move {
            match tokio::time::timeout(TEST_TIMEOUT, service::test_connection(&conn, &password))
                .await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e.display()),
                Err(_) => Err(t!("conn_form.timeout", secs = TEST_TIMEOUT.as_secs()).to_string()),
            }
        });
        self._test_task = Some(cx.spawn_in(
            window,
            async move |this, cx: &mut AsyncWindowContext| {
                let result = probe.await;
                let ms = started.elapsed().as_millis();
                let stale = this
                    .update(cx, |this, cx| {
                        if attempt != this.attempt {
                            return true;
                        }
                        this.test = match result {
                            Ok(()) => TestStatus::Success(ms),
                            Err(e) => TestStatus::Failed(e),
                        };
                        cx.notify();
                        false
                    })
                    .unwrap_or(true);
                if stale {
                    return;
                }
                // 结果出来后几秒淡回 idle(提示文字仍保留到下次测试)
                cx.background_executor().timer(Duration::from_secs(4)).await;
                let _ = this.update(cx, |this, cx| {
                    if attempt == this.attempt && matches!(this.test, TestStatus::Success(_)) {
                        this.test = TestStatus::Idle;
                        cx.notify();
                    }
                });
            },
        ));
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let conn = self.connection(cx);
        if conn.name.is_empty() {
            self.error = Some(t!("conn_form.name_required").to_string());
            cx.notify();
            return;
        }
        if conn.kind == DbKind::Sqlite && conn.file_path.as_deref().unwrap_or("").is_empty() {
            self.error = Some(t!("conn_form.file_required").to_string());
            cx.notify();
            return;
        }
        let pw = Self::value(&self.password, cx);
        // 编辑时密码留空 = 不修改;新建时留空 = 无密码
        let password = if self.editing && pw.is_empty() {
            None
        } else {
            Some(pw)
        };
        let conns = Stores::global(cx).conns.clone();
        match conns.update(cx, |s, cx| s.save(conn, password, cx)) {
            Ok(saved) => {
                conns.update(cx, |s, cx| s.activate(&saved.id, cx));
                window.close_dialog(cx);
            }
            Err(e) => {
                let msg = t!("conn_form.save_failed", error = e.display()).to_string();
                conns.update(cx, |_, cx| cx.emit(Toast::Error(msg.into())));
            }
        }
    }

    fn label(text: impl Into<SharedString>, cx: &gpui_kit::App) -> impl IntoElement {
        div()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(cx.palette().fg_faint)
            .child(text.into())
    }

    fn field(
        label: impl Into<SharedString>,
        control: impl IntoElement,
        cx: &gpui_kit::App,
    ) -> impl IntoElement {
        v_flex()
            .gap_2()
            .flex_1()
            .min_w_0()
            .child(Self::label(label, cx))
            .child(control)
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let sqlite = self.kind == DbKind::Sqlite;
        let mono = cx.theme().mono_font_family.clone();
        let this = cx.entity().downgrade();

        let kind_button = Button::new("kind")
            .outline()
            .w_full()
            .justify_start()
            .icon(ui::db_icon(self.kind).text_color(cx.theme().primary))
            .label(self.kind.label())
            .dropdown_caret(true)
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                DbKind::ALL.iter().fold(menu, |menu, k| {
                    let k = *k;
                    let this = this.clone();
                    menu.item(PopupMenuItem::new(k.label()).icon(ui::db_icon(k)).on_click(
                        move |_, window, cx| {
                            let _ = this.update(cx, |f, cx| f.set_kind(k, window, cx));
                        },
                    ))
                })
            });

        let env_segment = h_flex()
            .gap_1p5()
            .p_1()
            .rounded_full()
            .bg(palette.raised)
            .children(Env::ALL.iter().map(|e| {
                let e = *e;
                let active = self.env == e;
                let color = ui::env_color(e, cx);
                h_flex()
                    .id(SharedString::from(format!("env-{}", ui::env_label(e))))
                    .flex_1()
                    .h(px(32.))
                    .gap_2()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .when(active, |d| {
                        d.bg(cx.theme().background).shadow_sm().text_color(color)
                    })
                    .when(!active, |d| d.text_color(palette.fg_faint))
                    .child(ui::dot(if active { color } else { palette.fg_faint }, 7.))
                    .child(ui::env_label(e))
                    .on_click(cx.listener(move |f, _, _, cx| {
                        f.env = e;
                        cx.notify();
                    }))
            }));

        let ssl_mode = self.ssl_mode;
        let redis = self.kind == DbKind::Redis;
        let ssl_button = Button::new("ssl-mode")
            .outline()
            .w_full()
            .justify_start()
            .icon(ui::icon(if ssl_mode == SslMode::Disable {
                "lock-open"
            } else {
                "lock"
            }))
            .label(ssl_label(ssl_mode))
            .dropdown_caret(true)
            .tooltip(if redis {
                t!("conn_form.ssl_redis_hint")
            } else {
                t!("conn_form.ssl_hint")
            })
            .dropdown_menu({
                let this = cx.entity().downgrade();
                move |menu: PopupMenu, _, _| {
                    SslMode::ALL.iter().fold(menu, |menu, m| {
                        let (m, this) = (*m, this.clone());
                        menu.item(
                            PopupMenuItem::new(ssl_label(m))
                                .checked(m == ssl_mode)
                                .on_click(move |_, _, cx| {
                                    let _ = this.update(cx, |f, cx| {
                                        f.ssl_mode = m;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                }
            });

        let (test_icon, test_label) = match &self.test {
            TestStatus::Idle => ("activity", t!("conn_form.test")),
            TestStatus::Testing => ("loader-circle", t!("conn_form.testing")),
            TestStatus::Success(_) => ("check", t!("conn_form.test_success")),
            TestStatus::Failed(_) => ("circle-alert", t!("conn_form.test_failed")),
        };
        let test_note = match &self.test {
            TestStatus::Success(ms) => Some((
                t!("conn_form.test_success_note", ms = ms).to_string(),
                palette.data,
            )),
            TestStatus::Failed(e) => Some((e.clone(), palette.negative)),
            _ => None,
        };

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_4()
                    .child(Self::field(t!("conn_form.type"), kind_button, cx))
                    .child(Self::field(
                        t!("conn_form.name"),
                        Input::new(&self.name),
                        cx,
                    )),
            )
            .map(|this| {
                if sqlite {
                    this.child(Self::field(
                        t!("conn_form.file_path"),
                        h_flex()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.file_path)))
                            .child(
                                Button::new("pick-file")
                                    .outline()
                                    .icon(ui::icon("folder-open"))
                                    .tooltip(t!("conn_form.choose_file"))
                                    .on_click(
                                        cx.listener(|f, _, window, cx| f.pick_file(window, cx)),
                                    ),
                            ),
                        cx,
                    ))
                } else {
                    this.child(Self::field(t!("conn_form.env"), env_segment, cx))
                        .child(
                            h_flex()
                                .gap_4()
                                .child(Self::field(
                                    t!("conn_form.host"),
                                    Input::new(&self.host),
                                    cx,
                                ))
                                .child(div().w(px(132.)).flex_shrink_0().child(Self::field(
                                    t!("conn_form.port"),
                                    Input::new(&self.port),
                                    cx,
                                ))),
                        )
                        .child(
                            h_flex()
                                .gap_4()
                                .child(Self::field(
                                    t!("conn_form.user"),
                                    Input::new(&self.user),
                                    cx,
                                ))
                                .child(Self::field(
                                    t!("conn_form.password"),
                                    Input::new(&self.password).mask_toggle(),
                                    cx,
                                )),
                        )
                        .child(
                            h_flex()
                                .gap_4()
                                .child(Self::field(
                                    t!("conn_form.database"),
                                    Input::new(&self.database),
                                    cx,
                                ))
                                .child(div().w(px(200.)).flex_shrink_0().child(Self::field(
                                    t!("conn_form.ssl_mode"),
                                    ssl_button,
                                    cx,
                                ))),
                        )
                }
            })
            .child(
                h_flex()
                    .gap_2p5()
                    .px_3p5()
                    .py_3()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().background)
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(ui::icon("link-2").small().text_color(palette.fg_faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .font_family(mono.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.conn_string(cx)),
                    ),
            )
            .when_some(self.error.clone(), |this, e| {
                this.child(div().text_xs().text_color(palette.negative).child(e))
            })
            .child(
                h_flex()
                    .pt_2()
                    .gap_3()
                    .justify_between()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_3()
                            .child(
                                Button::new("test-conn")
                                    .outline()
                                    .icon(ui::icon(test_icon))
                                    .label(test_label)
                                    .loading(self.test == TestStatus::Testing)
                                    .on_click(
                                        cx.listener(|f, _, window, cx| f.run_test(window, cx)),
                                    ),
                            )
                            .when_some(test_note, |this, (text, color)| {
                                this.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(color)
                                        .child(text),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_shrink_0()
                            .child(
                                Button::new("cancel")
                                    .ghost()
                                    .label(t!("conn_form.cancel"))
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("save")
                                    .primary()
                                    .label(t!("conn_form.save"))
                                    .on_click(cx.listener(|f, _, window, cx| f.save(window, cx))),
                            ),
                    ),
            )
    }
}

pub fn open(conn: Option<Connection>, window: &mut Window, cx: &mut gpui_kit::App) {
    let editing = conn.is_some();
    let form = cx.new(|cx| ConnectionForm::new(conn, window, cx));
    let focus = form.read(cx).name.clone();
    focus.update(cx, |s, cx| s.focus(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(600.))
            .title(if editing {
                t!("conn_form.edit_title").to_string()
            } else {
                t!("conn_form.new_title").to_string()
            })
            .child(form.clone())
    });
}

fn ssl_label(m: SslMode) -> String {
    match m {
        SslMode::Disable => t!("conn_form.ssl_disable"),
        SslMode::Prefer => t!("conn_form.ssl_prefer"),
        SslMode::Require => t!("conn_form.ssl_require"),
        SslMode::VerifyFull => t!("conn_form.ssl_verify_full"),
    }
    .to_string()
}
