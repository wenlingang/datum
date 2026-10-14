//! 设置:AI 服务商(多服务商 + 模型)/ 知识库 / 外观(语言、主题、字体、字号、粗细)/ 关于与更新。

use crate::prefs::{
    FONT_FAMILIES, FONT_SIZES, FONT_WEIGHTS, LanguagePreference, Prefs, ThemePreference,
    font_family_label, font_size_label, font_weight_label,
};
use crate::store::{Stores, UpdateStatus, list_ai_models, test_ai_config};
use crate::theme::{self, ActivePalette as _};
use crate::ui;
use datum_core::ai::presets::{PRESETS, Preset};
use datum_core::ai::settings::{AiSettings, ModelEntry, Protocol, ProviderConfig};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, AsyncWindowContext, Context, Entity, FontWeight, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Task,
    Window, div, px,
};
use rust_i18n::t;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Ai,
    Knowledge,
    Appearance,
    About,
}

impl Section {
    const ALL: [Section; 4] = [Self::Ai, Self::Knowledge, Self::Appearance, Self::About];

    fn label(self) -> String {
        match self {
            Self::Ai => t!("settings.section_ai"),
            Self::Knowledge => t!("settings.section_knowledge"),
            Self::Appearance => t!("settings.section_appearance"),
            Self::About => t!("settings.section_about"),
        }
        .to_string()
    }

    /// 稳定的元素 id(不随界面语言变化)。
    fn id(self) -> &'static str {
        match self {
            Self::Ai => "settings-section-ai",
            Self::Knowledge => "settings-section-knowledge",
            Self::Appearance => "settings-section-appearance",
            Self::About => "settings-section-about",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Ai => "sparkles",
            Self::Knowledge => "file-text",
            Self::Appearance => "type",
            Self::About => "refresh-cw",
        }
    }
}

/// 表单标签列宽度
const LABEL_W: f32 = 76.;

struct Notice {
    ok: bool,
    text: String,
}

/// 单个模型的「测试」状态。
#[derive(Clone)]
enum Probe {
    Running,
    Ok,
    Failed(String),
}

/// 服务商详情表单里的文本字段;每个字段单独回写,切换服务商时 set_value 不会串写。
#[derive(Clone, Copy)]
enum Field {
    Name,
    BaseUrl,
    ApiKey,
}

pub struct SettingsView {
    section: Section,
    /// 当前编辑的服务商 id
    selected: Option<String>,
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
    new_model: Entity<InputState>,
    model_filter: Entity<InputState>,
    fetching: bool,
    probes: HashMap<(String, String), Probe>,
    notice: Option<Notice>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ai = Stores::global(cx).ai.clone();
        let update = Stores::global(cx).update.clone();
        let name = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("settings.provider_name_placeholder"))
        });
        let base_url =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.openai.com/v1"));
        let api_key = cx.new(|cx| InputState::new(window, cx).placeholder("sk-…").masked(true));
        let new_model = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("settings.model_add_placeholder"))
        });
        let model_filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("settings.model_search_placeholder"))
        });
        let field_sub = |input: &Entity<InputState>,
                         field: Field,
                         window: &mut Window,
                         cx: &mut Context<Self>| {
            cx.subscribe_in(input, window, move |this, input, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    this.write_field(field, value, cx);
                }
            })
        };
        let subs = vec![
            field_sub(&name, Field::Name, window, cx),
            field_sub(&base_url, Field::BaseUrl, window, cx),
            field_sub(&api_key, Field::ApiKey, window, cx),
            cx.subscribe_in(&new_model, window, |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.add_manual_model(window, cx);
                }
            }),
            cx.subscribe(&model_filter, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.observe(&ai, |_, _, cx| cx.notify()),
            cx.observe(&update, |_, _, cx| cx.notify()),
        ];
        let mut this = Self {
            section: Section::Ai,
            selected: None,
            name,
            base_url,
            api_key,
            new_model,
            model_filter,
            fetching: false,
            probes: HashMap::new(),
            notice: None,
            _tasks: vec![],
            _subscriptions: subs,
        };
        let first = ai
            .read(cx)
            .settings
            .current
            .as_ref()
            .map(|c| c.provider_id.clone())
            .or_else(|| ai.read(cx).settings.providers.first().map(|p| p.id.clone()));
        this.select(first, window, cx);
        this
    }

    fn settings(cx: &gpui_kit::App) -> AiSettings {
        Stores::global(cx).ai.read(cx).settings.clone()
    }

    fn current_provider(&self, cx: &gpui_kit::App) -> Option<ProviderConfig> {
        let id = self.selected.as_ref()?;
        Self::settings(cx).provider(id).cloned()
    }

    /// 改一份配置副本并立即落盘(所见即所存,不设单独的「保存」按钮)。
    fn mutate(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut AiSettings)) {
        let mut settings = Self::settings(cx);
        f(&mut settings);
        let ai = Stores::global(cx).ai.clone();
        if let Err(e) = ai.update(cx, |ai, cx| ai.save_settings(settings, cx)) {
            self.notice = Some(Notice { ok: false, text: e });
        }
        cx.notify();
    }

    fn mutate_provider(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ProviderConfig)) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.mutate(cx, |s| {
            if let Some(p) = s.provider_mut(&id) {
                f(p);
            }
        });
    }

    fn write_field(&mut self, field: Field, value: String, cx: &mut Context<Self>) {
        let Some(p) = self.current_provider(cx) else {
            return;
        };
        let unchanged = match field {
            Field::Name => p.name == value,
            Field::BaseUrl => p.base_url == value,
            Field::ApiKey => p.api_key == value,
        };
        if unchanged {
            return;
        }
        self.mutate_provider(cx, |p| match field {
            Field::Name => p.name = value,
            Field::BaseUrl => p.base_url = value,
            Field::ApiKey => p.api_key = value,
        });
    }

    fn select(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = id;
        self.notice = None;
        let p = self.current_provider(cx).unwrap_or_default();
        self.name
            .update(cx, |i, cx| i.set_value(p.name.clone(), window, cx));
        self.base_url
            .update(cx, |i, cx| i.set_value(p.base_url.clone(), window, cx));
        self.api_key
            .update(cx, |i, cx| i.set_value(p.api_key.clone(), window, cx));
        self.new_model
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.model_filter
            .update(cx, |i, cx| i.set_value("", window, cx));
        cx.notify();
    }

    fn add_provider(
        &mut self,
        preset: Option<Preset>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = uuid::Uuid::new_v4().to_string();
        let provider = ProviderConfig {
            id: id.clone(),
            name: preset.map_or_else(
                || t!("settings.custom_provider_name").to_string(),
                |p| p.display_name(),
            ),
            protocol: preset.map_or(Protocol::OpenAi, |p| p.protocol),
            base_url: preset.map_or(String::new(), |p| p.base_url.into()),
            api_key: String::new(),
            enabled: true,
            models: vec![],
        };
        self.mutate(cx, |s| s.providers.push(provider));
        self.select(Some(id), window, cx);
    }

    fn delete_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.mutate(cx, |s| s.providers.retain(|p| p.id != id));
        let next = Self::settings(cx).providers.first().map(|p| p.id.clone());
        self.select(next, window, cx);
    }

    fn add_manual_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.new_model.read(cx).value().trim().to_string();
        if model.is_empty() {
            return;
        }
        self.mutate_provider(cx, |p| {
            if !p.models.iter().any(|m| m.id == model) {
                p.models.push(ModelEntry {
                    id: model,
                    enabled: true,
                });
            }
        });
        self.new_model
            .update(cx, |i, cx| i.set_value("", window, cx));
    }

    /// 拉取服务商的模型列表并合并:已有的保留勾选状态,新拉到的默认不勾选。
    fn fetch_models(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.current_provider(cx) else {
            return;
        };
        self.fetching = true;
        self.notice = None;
        cx.notify();
        let id = provider.id.clone();
        let task = list_ai_models(provider);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.fetching = false;
                match result {
                    Ok(ids) => {
                        let mut added = 0;
                        this.mutate(cx, |s| {
                            if let Some(p) = s.provider_mut(&id) {
                                for m in ids {
                                    if !p.models.iter().any(|e| e.id == m) {
                                        p.models.push(ModelEntry {
                                            id: m,
                                            enabled: false,
                                        });
                                        added += 1;
                                    }
                                }
                            }
                        });
                        this.notice = Some(Notice {
                            ok: true,
                            text: if added > 0 {
                                t!("settings.models_added", count = added).to_string()
                            } else {
                                t!("settings.models_up_to_date").to_string()
                            },
                        });
                    }
                    Err(e) => this.notice = Some(Notice { ok: false, text: e }),
                }
                cx.notify();
            });
        }));
    }

    fn test_model(&mut self, model: String, cx: &mut Context<Self>) {
        let Some(provider) = self.current_provider(cx) else {
            return;
        };
        let key = (provider.id.clone(), model.clone());
        self.probes.insert(key.clone(), Probe::Running);
        cx.notify();
        let task = test_ai_config(provider, model);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.probes.insert(
                    key,
                    match result {
                        Ok(()) => Probe::Ok,
                        Err(e) => Probe::Failed(e),
                    },
                );
                cx.notify();
            });
        }));
    }

    /// 知识库的选择/恢复默认立即落盘:选文件本身就是一次完整、明确的操作。
    fn save_knowledge(&mut self, name: String, path: String, cx: &mut Context<Self>) {
        let n = name.clone();
        self.mutate(cx, |s| {
            s.knowledge_name = name;
            s.knowledge_path = path;
        });
        if self.notice.is_none() {
            self.notice = Some(Notice {
                ok: true,
                text: if n.is_empty() {
                    t!("settings.knowledge_reset_done").to_string()
                } else {
                    t!("settings.knowledge_enabled", name = n).to_string()
                },
            });
        }
    }

    fn pick_knowledge(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("settings.knowledge_pick_prompt").to_string().into()),
        });
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.save_knowledge(name, path.to_string_lossy().to_string(), cx)
            });
        })
        .detach();
    }

    fn row(
        label: impl Into<SharedString>,
        control: impl IntoElement,
        cx: &gpui_kit::App,
    ) -> impl IntoElement {
        h_flex()
            .gap_3()
            .child(
                div()
                    .w(px(LABEL_W))
                    .flex_shrink_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(label.into()),
            )
            .child(div().flex_1().min_w_0().child(control))
    }

    /// 带边框的分组卡片:标题 + 内容。
    fn card(title: impl Into<SharedString>, cx: &gpui_kit::App) -> gpui_kit::Div {
        v_flex()
            .gap_3p5()
            .p_4()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.palette().panel)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.into()),
            )
    }

    /// 服务商头像:首字 + 协议色。
    fn avatar(p: &ProviderConfig, size: f32, cx: &gpui_kit::App) -> impl IntoElement {
        let color: gpui_kit::Hsla = match p.protocol {
            Protocol::OpenAi => cx.theme().primary,
            Protocol::Anthropic => gpui_kit::rgb(0xd97757).into(),
            Protocol::Gemini => gpui_kit::rgb(0x3b82f6).into(),
        };
        let initial: String = p
            .name
            .trim()
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".into());
        div()
            .size(px(size))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(size * 0.3))
            .bg(color.opacity(0.16))
            .text_color(color)
            .text_size(px(size * 0.45))
            .font_weight(FontWeight::BOLD)
            .child(initial)
    }

    fn render_notice(&self, cx: &gpui_kit::App) -> Option<impl IntoElement> {
        let palette = *cx.palette();
        self.notice.as_ref().map(|n| {
            div()
                .text_sm()
                .text_color(if n.ok {
                    cx.theme().muted_foreground
                } else {
                    palette.negative
                })
                .child(n.text.clone())
        })
    }

    fn render_ai(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let settings = Self::settings(cx);
        let view = cx.entity().downgrade();
        let add_menu = Button::new("provider-add")
            .outline()
            .small()
            .w_full()
            .icon(ui::icon("plus"))
            .label(t!("settings.add_provider"))
            .dropdown_menu(move |menu: PopupMenu, _, _| {
                let mut menu = menu.min_w(px(220.));
                for preset in PRESETS {
                    let (view, preset) = (view.clone(), *preset);
                    menu = menu.item(PopupMenuItem::new(preset.display_name()).on_click(
                        move |_, window, cx| {
                            let _ = view
                                .update(cx, |this, cx| this.add_provider(Some(preset), window, cx));
                        },
                    ));
                }
                let view = view.clone();
                menu.separator()
                    .item(PopupMenuItem::new(t!("settings.custom_openai")).on_click(
                        move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| this.add_provider(None, window, cx));
                        },
                    ))
            });
        let rows =
            settings.providers.iter().map(|p| {
                let id = p.id.clone();
                let selected = self.selected.as_deref() == Some(p.id.as_str());
                let in_use = settings
                    .current
                    .as_ref()
                    .is_some_and(|c| c.provider_id == p.id);
                h_flex()
                    .id(SharedString::from(format!("provider-{}", p.id)))
                    .h(px(48.))
                    .px_2p5()
                    .gap_2p5()
                    .rounded(cx.theme().radius_lg)
                    .cursor_pointer()
                    .when(selected, |d| d.bg(palette.raised))
                    .when(!selected, |d| d.hover(|s| s.bg(cx.theme().list_hover)))
                    .when(!p.enabled, |d| d.opacity(0.5))
                    .child(Self::avatar(p, 28., cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_sm()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                                    .child(if p.name.trim().is_empty() {
                                        t!("settings.unnamed").to_string()
                                    } else {
                                        p.name.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.fg_faint)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(match p.models.iter().filter(|m| m.enabled).count() {
                                        0 => p.protocol.label().to_string(),
                                        1 => t!(
                                            "settings.provider_models_one",
                                            protocol = p.protocol.label()
                                        )
                                        .to_string(),
                                        n => t!(
                                            "settings.provider_models_n",
                                            protocol = p.protocol.label(),
                                            count = n
                                        )
                                        .to_string(),
                                    }),
                            ),
                    )
                    .when(in_use, |d| {
                        d.child(
                            div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(palette.ai)
                                .flex_shrink_0(),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select(Some(id.clone()), window, cx)
                    }))
            });
        let list = v_flex()
            .w(px(220.))
            .flex_shrink_0()
            .h_full()
            .pr_4()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .pb_2p5()
                    .child(ui::section_label(t!("settings.providers"), cx)),
            )
            .child(
                v_flex()
                    .id("provider-list")
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(div().pt_3().child(add_menu));
        let detail = match self.current_provider(cx) {
            Some(p) => self.render_provider(&p, cx).into_any_element(),
            None => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_sm()
                .text_color(palette.fg_faint)
                .child(ui::icon("sparkles").size(px(28.)).text_color(palette.ai))
                .child(
                    div()
                        .text_base()
                        .text_color(cx.theme().foreground)
                        .child(t!("settings.no_providers")),
                )
                .child(t!("settings.no_providers_hint"))
                .into_any_element(),
        };
        h_flex().size_full().items_start().child(list).child(
            div()
                .id("provider-detail")
                .flex_1()
                .min_w_0()
                .h_full()
                .pl_6()
                .pr_1()
                .overflow_y_scroll()
                .child(detail),
        )
    }

    fn render_provider(&self, p: &ProviderConfig, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let protocol = p.protocol;
        let endpoint_hint = match protocol {
            Protocol::OpenAi => t!("settings.endpoint_openai"),
            Protocol::Anthropic => t!("settings.endpoint_anthropic"),
            Protocol::Gemini => t!("settings.endpoint_gemini"),
        };
        let protocol_btn = Button::new("provider-protocol")
            .outline()
            .w(px(220.))
            .justify_start()
            .label(protocol.label())
            .dropdown_caret(true)
            .dropdown_menu({
                let view = cx.entity().downgrade();
                move |menu: PopupMenu, _, _| {
                    Protocol::ALL.iter().fold(menu, |menu, pr| {
                        let (view, pr) = (view.clone(), *pr);
                        menu.item(
                            PopupMenuItem::new(pr.label())
                                .checked(pr == protocol)
                                .on_click(move |_, _, cx| {
                                    let _ = view.update(cx, |this, cx| {
                                        this.mutate_provider(cx, |p| p.protocol = pr)
                                    });
                                }),
                        )
                    })
                }
            });
        let filter = self.model_filter.read(cx).value().trim().to_lowercase();
        let enabled_count = p.models.iter().filter(|m| m.enabled).count();
        let shown: Vec<&ModelEntry> = p
            .models
            .iter()
            .filter(|m| filter.is_empty() || m.id.to_lowercase().contains(&filter))
            .collect();
        let shown_len = shown.len();
        let models = shown.into_iter().enumerate().map(|(ix, m)| {
            let id = m.id.clone();
            let (toggle_id, test_id, del_id) = (id.clone(), id.clone(), id.clone());
            let checked = m.enabled;
            let probe = self.probes.get(&(p.id.clone(), m.id.clone())).cloned();
            let group = SharedString::from(format!("model-row-{id}"));
            h_flex()
                .group(group.clone())
                .h(px(40.))
                .px_3()
                .gap_3()
                .when(ix > 0, |d| d.border_t_1().border_color(cx.theme().border))
                .hover(|s| s.bg(cx.theme().list_hover))
                .child(
                    Checkbox::new(SharedString::from(format!("model-on-{id}")))
                        .checked(checked)
                        .on_click(cx.listener(move |this, v: &bool, _, cx| {
                            let (id, v) = (toggle_id.clone(), *v);
                            this.mutate_provider(cx, |p| {
                                if let Some(m) = p.models.iter_mut().find(|m| m.id == id) {
                                    m.enabled = v;
                                }
                            })
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_sm()
                        .font_family(cx.theme().mono_font_family.clone())
                        .when(!checked, |d| d.text_color(cx.theme().muted_foreground))
                        .child(id.clone()),
                )
                .map(|row| match &probe {
                    Some(Probe::Ok) => row.child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .text_color(palette.env_local)
                            .child(ui::icon("circle-check").small())
                            .child(t!("settings.model_available")),
                    ),
                    Some(Probe::Failed(e)) => row.child(
                        h_flex()
                            .id(SharedString::from(format!("model-err-{id}")))
                            .gap_1()
                            .max_w(px(220.))
                            .text_xs()
                            .text_color(palette.negative)
                            .child(ui::icon("circle-x").small().flex_shrink_0())
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(e.clone()),
                            )
                            .tooltip({
                                let e = e.clone();
                                move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(e.clone())
                                        .build(window, cx)
                                }
                            }),
                    ),
                    _ => row,
                })
                // 操作按钮悬停才出现,测试中保持可见
                .child(
                    h_flex()
                        .gap_1()
                        .when(!matches!(probe, Some(Probe::Running)), |d| {
                            d.invisible().group_hover(group, |s| s.visible())
                        })
                        .child(
                            Button::new(SharedString::from(format!("model-test-{id}")))
                                .ghost()
                                .xsmall()
                                .label(t!("settings.model_test"))
                                .loading(matches!(probe, Some(Probe::Running)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.test_model(test_id.clone(), cx)
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("model-del-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("trash"))
                                .tooltip(t!("settings.model_remove"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let id = del_id.clone();
                                    this.mutate_provider(cx, |p| p.models.retain(|m| m.id != id))
                                })),
                        ),
                )
        });
        let name = if p.name.trim().is_empty() {
            t!("settings.unnamed").to_string()
        } else {
            p.name.clone()
        };
        let header = h_flex()
            .gap_3()
            .pb_1()
            .child(Self::avatar(p, 40., cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.fg_faint)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(if p.base_url.trim().is_empty() {
                                protocol.label().to_string()
                            } else {
                                format!("{} · {}", protocol.label(), p.base_url.trim())
                            }),
                    ),
            )
            .child(
                Switch::new("provider-enabled")
                    .checked(p.enabled)
                    .label(SharedString::from(
                        t!("settings.provider_enabled").to_string(),
                    ))
                    .on_click(cx.listener(|this, v: &bool, _, cx| {
                        let v = *v;
                        this.mutate_provider(cx, |p| p.enabled = v)
                    })),
            )
            .child(
                Button::new("provider-delete")
                    .ghost()
                    .small()
                    .icon(ui::icon("trash"))
                    .tooltip(t!("settings.delete_provider"))
                    .on_click(cx.listener(|this, _, window, cx| this.delete_provider(window, cx))),
            );
        let connection = Self::card(t!("settings.connection_card"), cx)
            .child(Self::row(t!("settings.name"), Input::new(&self.name), cx))
            .child(Self::row(t!("settings.protocol"), protocol_btn, cx))
            .child(
                v_flex()
                    .gap_1p5()
                    .child(Self::row(
                        t!("settings.api_url"),
                        Input::new(&self.base_url),
                        cx,
                    ))
                    .child(
                        div()
                            .pl(px(LABEL_W + 12.))
                            .text_xs()
                            .text_color(palette.fg_faint)
                            .child(endpoint_hint),
                    ),
            )
            .child(Self::row(
                "API Key",
                Input::new(&self.api_key).mask_toggle(),
                cx,
            ));
        let models_card =
            v_flex()
                .gap_3()
                .p_4()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .bg(palette.panel)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(t!("settings.models")),
                        )
                        .child(
                            div()
                                .px_1p5()
                                .rounded(px(4.))
                                .bg(palette.raised)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!(
                                    "settings.models_enabled_count",
                                    enabled = enabled_count,
                                    total = p.models.len()
                                )),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("models-fetch")
                                .outline()
                                .small()
                                .icon(ui::icon("refresh-cw"))
                                .label(if self.fetching {
                                    t!("settings.fetching")
                                } else {
                                    t!("settings.fetch_models")
                                })
                                .loading(self.fetching)
                                .tooltip(t!("settings.fetch_models_tip"))
                                .on_click(cx.listener(|this, _, _, cx| this.fetch_models(cx))),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(
                            Input::new(&self.model_filter).prefix(ui::icon("search").small()),
                        ))
                        .child(
                            div().flex_1().child(
                                Input::new(&self.new_model).prefix(ui::icon("plus").small()),
                            ),
                        ),
                )
                .map(|this| {
                    if p.models.is_empty() {
                        this.child(
                            v_flex()
                                .py_6()
                                .items_center()
                                .gap_1()
                                .text_sm()
                                .text_color(palette.fg_faint)
                                .child(t!("settings.no_models"))
                                .child(t!("settings.no_models_hint")),
                        )
                    } else {
                        this.when(enabled_count == 0, |this| {
                            this.child(ui::hint(t!("settings.models_check_hint"), cx))
                        })
                        .child(
                            v_flex()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background)
                                .overflow_hidden()
                                .children(models)
                                .when(shown_len == 0, |this| {
                                    this.child(
                                        div()
                                            .py_4()
                                            .text_center()
                                            .text_sm()
                                            .text_color(palette.fg_faint)
                                            .child(t!("settings.no_matching_models")),
                                    )
                                }),
                        )
                    }
                });
        v_flex()
            .gap_4()
            .pb_4()
            .child(header)
            .when_some(self.render_notice(cx), |this, n| this.child(n))
            .child(connection)
            .child(models_card)
    }

    fn render_knowledge(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = *cx.palette();
        let ai = Stores::global(cx).ai.read(cx);
        let doc = ai.knowledge();
        let custom = !ai.settings.knowledge_path.is_empty();
        let chars = doc.content.chars().count();
        v_flex()
            .gap_3()
            .child(ui::hint(t!("settings.knowledge_hint"), cx))
            .child(Self::row(
                t!("settings.document"),
                h_flex()
                    .gap_1p5()
                    .child(
                        ui::icon("file-text")
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .child(doc.name.clone()),
                    )
                    .when(!custom, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(palette.fg_faint)
                                .child(t!("settings.builtin_default")),
                        )
                    })
                    .when(custom, |this| {
                        this.child(
                            Button::new("knowledge-reset")
                                .ghost()
                                .xsmall()
                                .icon(ui::icon("x"))
                                .tooltip(t!("settings.restore_default"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.save_knowledge(String::new(), String::new(), cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("knowledge-pick")
                            .outline()
                            .small()
                            .label(if custom {
                                t!("settings.change_file")
                            } else {
                                t!("settings.choose_file")
                            })
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pick_knowledge(window, cx)),
                            ),
                    ),
                cx,
            ))
            .child(
                div()
                    .pl(px(LABEL_W + 12.))
                    .text_xs()
                    .text_color(if doc.warning.is_some() {
                        palette.negative
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(doc.warning.clone().unwrap_or_else(|| {
                        t!("settings.knowledge_stats", count = chars).to_string()
                    })),
            )
            .when_some(self.render_notice(cx), |this, n| this.child(n))
    }

    fn render_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = Prefs::global(cx).clone();
        let size_option = |size: u32| {
            font_size_label(size)
                .map(|label| t!("settings.font_size_option", label = label, px = size).to_string())
        };
        let family_label = font_family_label(&prefs.font_family)
            .unwrap_or_else(|| t!("settings.font_custom").to_string());
        let size_label = size_option(prefs.font_size).unwrap_or_default();
        let weight_label = font_weight_label(prefs.font_weight)
            .unwrap_or_else(|| t!("prefs.weight_regular").to_string());
        let dropdown =
            |id: &'static str, label: String, build: Box<dyn Fn(PopupMenu) -> PopupMenu>| {
                Button::new(id)
                    .outline()
                    .small()
                    .w(px(260.))
                    .justify_start()
                    .label(label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |m: PopupMenu, _, _| build(m))
            };
        let family_menu = Box::new(move |menu: PopupMenu| {
            FONT_FAMILIES.iter().fold(menu, |menu, value| {
                let label = font_family_label(value).unwrap_or_else(|| value.to_string());
                let value = value.to_string();
                menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let v = value.clone();
                    Prefs::update(cx, |p| p.font_family = v);
                    theme::apply(Some(window.appearance()), cx);
                }))
            })
        });
        let size_menu = Box::new(move |menu: PopupMenu| {
            FONT_SIZES.iter().fold(menu, |menu, value| {
                let value = *value;
                menu.item(
                    PopupMenuItem::new(size_option(value).unwrap_or_default()).on_click(
                        move |_, window, cx| {
                            Prefs::update(cx, |p| p.font_size = value);
                            theme::apply(Some(window.appearance()), cx);
                        },
                    ),
                )
            })
        });
        let weight_menu = Box::new(move |menu: PopupMenu| {
            FONT_WEIGHTS.iter().fold(menu, |menu, value| {
                let value = *value;
                let label = font_weight_label(value).unwrap_or_default();
                menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    Prefs::update(cx, |p| p.font_weight = value);
                    window.refresh();
                }))
            })
        });
        let theme_pref = prefs.theme;
        let theme_menu = Box::new(move |menu: PopupMenu| {
            ThemePreference::ALL.iter().fold(menu, |menu, p| {
                let p = *p;
                menu.item(
                    PopupMenuItem::new(p.label())
                        .icon(ui::icon(match p {
                            ThemePreference::Light => "sun",
                            ThemePreference::Dark => "moon",
                            ThemePreference::System => "monitor",
                        }))
                        .checked(p == theme_pref)
                        .on_click(move |_, window, cx| theme::set_preference(p, window, cx)),
                )
            })
        });
        let lang = prefs.language;
        let language_menu = Box::new(move |menu: PopupMenu| {
            LanguagePreference::ALL.iter().fold(menu, |menu, l| {
                let l = *l;
                menu.item(
                    PopupMenuItem::new(l.label())
                        .checked(l == lang)
                        .on_click(move |_, _, cx| crate::i18n::set_preference(l, cx)),
                )
            })
        });
        v_flex()
            .gap_3p5()
            .child(Self::row(
                t!("prefs.language"),
                dropdown("language", lang.label(), language_menu),
                cx,
            ))
            .child(Self::row(
                t!("settings.theme"),
                dropdown("theme", theme_pref.label(), theme_menu),
                cx,
            ))
            .child(Self::row(
                t!("settings.font"),
                dropdown("font-family", family_label, family_menu),
                cx,
            ))
            .child(Self::row(
                t!("settings.font_size"),
                dropdown("font-size", size_label, size_menu),
                cx,
            ))
            .child(Self::row(
                t!("settings.font_weight"),
                dropdown("font-weight", weight_label, weight_menu),
                cx,
            ))
            .child(
                v_flex()
                    .gap_2()
                    .px_3()
                    .py_2p5()
                    .rounded(cx.theme().radius_lg)
                    .bg(cx.palette().raised)
                    .child(ui::section_label(t!("settings.preview"), cx))
                    .child(div().text_sm().child(t!("settings.preview_text"))),
            )
    }

    fn render_about(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let store = Stores::global(cx).update.clone();
        let u = store.read(cx);
        let palette = *cx.palette();
        let current_notes = u
            .latest
            .as_ref()
            .filter(|m| m.version == u.version)
            .and_then(|m| m.notes.clone());
        let check = |label: SharedString| {
            let store = store.clone();
            Button::new("check-update")
                .outline()
                .small()
                .label(label)
                .on_click(move |_, _, cx| store.update(cx, |u, cx| u.check(cx)))
        };
        let action = if !u.supported {
            ui::hint(t!("settings.dev_build"), cx).into_any_element()
        } else {
            match &u.status {
                UpdateStatus::Idle => check(t!("settings.check_updates").into()).into_any_element(),
                UpdateStatus::Checking => ui::hint(t!("settings.checking"), cx).into_any_element(),
                UpdateStatus::UpToDate => h_flex()
                    .gap_3()
                    .child(ui::hint(t!("settings.up_to_date"), cx))
                    .child(check(t!("settings.check_updates").into()))
                    .into_any_element(),
                UpdateStatus::Available(m) => {
                    let store = store.clone();
                    h_flex()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(t!("settings.new_version", version = m.version)),
                        )
                        .child(
                            Button::new("install")
                                .primary()
                                .small()
                                .label(t!("settings.update_now"))
                                .on_click(move |_, _, cx| store.update(cx, |u, cx| u.install(cx))),
                        )
                        .into_any_element()
                }
                UpdateStatus::Downloading(pct) => ui::hint(
                    pct.map_or(t!("settings.downloading").to_string(), |p| {
                        t!("settings.downloading_pct", pct = p).to_string()
                    }),
                    cx,
                )
                .into_any_element(),
                UpdateStatus::Installing => {
                    ui::hint(t!("settings.installing"), cx).into_any_element()
                }
                UpdateStatus::ReadyRestart => h_flex()
                    .gap_3()
                    .child(ui::hint(t!("settings.ready_restart"), cx))
                    .child(
                        Button::new("restart")
                            .primary()
                            .small()
                            .label(t!("settings.restart"))
                            .on_click(|_, _, cx| cx.restart()),
                    )
                    .into_any_element(),
                UpdateStatus::Failed(e) => h_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.negative)
                            .child(t!("settings.update_failed", error = e)),
                    )
                    .child(check(t!("settings.retry").into()))
                    .into_any_element(),
            }
        };
        let available_notes = match &u.status {
            UpdateStatus::Available(m) if u.supported => m.notes.clone(),
            _ => None,
        };
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("settings.current_version"))
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().foreground)
                            .child(u.version.clone()),
                    ),
            )
            .when_some(current_notes, |this, notes| {
                this.child(
                    v_flex()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded(cx.theme().radius_lg)
                        .bg(palette.raised)
                        .child(ui::section_label(t!("settings.release_notes"), cx))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(notes),
                        ),
                )
            })
            .child(h_flex().min_h(px(30.)).child(action))
            .when_some(available_notes, |this, notes| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(notes),
                )
            })
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.section {
            Section::Ai => self.render_ai(cx).into_any_element(),
            Section::Knowledge => self.render_knowledge(cx).into_any_element(),
            Section::Appearance => self.render_appearance(cx).into_any_element(),
            Section::About => self.render_about(cx).into_any_element(),
        };
        h_flex()
            .items_stretch()
            .h(px(620.))
            .child(
                v_flex()
                    .w(px(168.))
                    .flex_shrink_0()
                    .pr_4()
                    .gap_1()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .children(Section::ALL.iter().map(|s| {
                        let s = *s;
                        ui::nav_row(s.id(), s.label(), Some(s.icon()), self.section == s, cx)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.section = s;
                                this.notice = None;
                                cx.notify();
                            }))
                    })),
            )
            .child(if self.section == Section::Ai {
                // AI 服务商自带两栏,各自滚动
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl_6()
                    .child(content)
                    .into_any_element()
            } else {
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .pl_6()
                    .gap_3()
                    .child(ui::section_label(self.section.label(), cx))
                    .child(
                        div()
                            .id("settings-content")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(content),
                    )
                    .into_any_element()
            })
    }
}

pub fn open(window: &mut Window, cx: &mut gpui_kit::App) {
    let view = cx.new(|cx| SettingsView::new(window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .w(px(980.))
            .title(t!("settings.title").to_string())
            .child(view.clone())
    });
}

use gpui_kit::{InteractiveElement as _, StatefulInteractiveElement as _};
