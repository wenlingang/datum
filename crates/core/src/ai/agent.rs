//! 带工具的对话(AI 自己查数据):模型每一轮可以调用工具,我们执行后把结果交回,
//! 直到它给出最终回答或用完步数。
//!
//! 与协议无关的部分在这里:中立的对话记录([`Item`])、工具定义([`ToolSpec`])、循环与事件。
//! 各家协议只实现「一轮」:把记录转成自己的请求、流式解析出文本与工具调用
//! (`openai::turn` / `anthropic::turn` / `gemini::turn`)。
//!
//! 工具由调用方通过 [`ToolRunner`] 执行(查库在 service 层,这里不碰数据库)。

use crate::ai::chat::{http, split_system, ChatMessage, REQUEST_TIMEOUT};
use crate::ai::provider::Endpoint;
use crate::ai::settings::Protocol;
use crate::ai::{anthropic, gemini, openai};
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// 一条用户消息里最多调用几次工具;用完后最后一轮不再允许调用,逼模型作答。
pub const MAX_STEPS: usize = 8;

/// 提供给模型的一个工具:名称、说明与参数的 JSON Schema。
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// 模型发起的一次工具调用。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
    /// 协议特有、下一轮需原样带回的原始片段(Gemini 的 functionCall part,含 thoughtSignature)
    pub raw: Option<Value>,
}

/// 中立的对话记录。
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User(String),
    Assistant { text: String, calls: Vec<ToolCall> },
    /// 一轮里各个工具调用的结果(给模型看的文本)
    Results(Vec<(ToolCall, String)>),
}

/// 一轮的产出:这一轮说的话,以及要调用的工具。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TurnOut {
    pub text: String,
    pub calls: Vec<ToolCall>,
}

/// 推给界面的事件。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    Delta { text: String },
    #[serde(rename_all = "camelCase")]
    ToolStart { id: String, name: String, args: Value },
    /// `detail` 给界面展示(如查询结果表),`summary` 为一句话摘要
    #[serde(rename_all = "camelCase")]
    ToolEnd { id: String, ok: bool, summary: String, detail: Value },
    Done { full: String },
    Error { message: String },
}

/// 一次工具执行的结果。
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// 交回模型的文本
    pub for_model: String,
    pub ok: bool,
    pub summary: String,
    pub detail: Value,
}

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolOutput> + Send + 'a>>;

/// 「停止生成」:协作式取消。正在流式接收的回复立即断开;正在执行的工具(查库)跑完再停,
/// 不在查询中途打断(否则只读会话可能来不及恢复)。
#[derive(Clone, Default)]
pub struct Cancel(Arc<(AtomicBool, Notify)>);

impl Cancel {
    pub fn cancel(&self) {
        self.0 .0.store(true, Ordering::SeqCst);
        self.0 .1.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0 .0.load(Ordering::SeqCst)
    }
    /// 被取消时完成。
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0 .1.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

/// 执行工具的一方(service 层实现,持有连接)。
pub trait ToolRunner: Send + Sync {
    fn run<'a>(&'a self, name: &'a str, args: &'a Value) -> ToolFuture<'a>;
}

/// 把前端传来的对话(含 system)转成 (system, 中立记录)。
pub fn items_of(messages: &[ChatMessage]) -> (String, Vec<Item>) {
    let (system, turns) = split_system(messages);
    let items = turns
        .into_iter()
        .map(|(is_user, text)| {
            if is_user {
                Item::User(text)
            } else {
                Item::Assistant { text, calls: vec![] }
            }
        })
        .collect();
    (system, items)
}

/// 一轮请求:按协议分派。`allow_tools` 为 false 时仍带工具定义(历史里有工具调用时部分协议要求),
/// 但要求模型不再调用。
pub(crate) async fn turn(
    client: &reqwest::Client,
    ep: &Endpoint,
    system: &str,
    items: &[Item],
    tools: &[ToolSpec],
    allow_tools: bool,
    on_delta: &(dyn Fn(String) + Send + Sync),
) -> Result<TurnOut, String> {
    match ep.protocol {
        Protocol::OpenAi => openai::turn(client, ep, system, items, tools, allow_tools, on_delta).await,
        Protocol::Anthropic => anthropic::turn(client, ep, system, items, tools, allow_tools, on_delta).await,
        Protocol::Gemini => gemini::turn(client, ep, system, items, tools, allow_tools, on_delta).await,
    }
}

/// 跑完一条用户消息:模型调用工具 → 执行 → 交回,直到它不再调用或用完步数。返回全部文本。
/// 被 `cancel` 停止时以已收到的文本正常结束(仍发 Done)。
pub async fn run(
    ep: &Endpoint,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    runner: &dyn ToolRunner,
    cancel: &Cancel,
    on_event: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Result<String, String> {
    let client = http(REQUEST_TIMEOUT)?;
    let (system, mut items) = items_of(messages);
    let mut full = String::new();
    let mut steps = 0usize;
    loop {
        if cancel.is_cancelled() {
            break;
        }
        let allow = steps < MAX_STEPS;
        let on_delta = |t: String| on_event(AgentEvent::Delta { text: t });
        let request = turn(&client, ep, &system, &items, tools, allow, &on_delta);
        // 断开流式请求是安全的(不涉及数据库)
        let out = tokio::select! {
            out = request => out?,
            _ = cancel.cancelled() => break,
        };
        full.push_str(&out.text);
        if out.calls.is_empty() || !allow {
            break;
        }
        // 两段文字之间夹着工具调用:换行隔开,拼接后的全文不粘在一起
        if !out.text.is_empty() && !out.text.ends_with('\n') {
            full.push('\n');
            on_event(AgentEvent::Delta { text: "\n".into() });
        }
        let mut results = Vec::new();
        for call in &out.calls {
            // 停止后不再开始新的工具;已开始的跑完
            if cancel.is_cancelled() {
                break;
            }
            steps += 1;
            on_event(AgentEvent::ToolStart {
                id: call.id.clone(),
                name: call.name.clone(),
                args: call.args.clone(),
            });
            let o = runner.run(&call.name, &call.args).await;
            on_event(AgentEvent::ToolEnd {
                id: call.id.clone(),
                ok: o.ok,
                summary: o.summary.clone(),
                detail: o.detail.clone(),
            });
            results.push((call.clone(), o.for_model));
        }
        if cancel.is_cancelled() {
            break;
        }
        items.push(Item::Assistant {
            text: out.text,
            calls: out.calls,
        });
        items.push(Item::Results(results));
    }
    on_event(AgentEvent::Done { full: full.clone() });
    Ok(full)
}

/// 流式拼接工具调用:各家把一个调用拆成多帧(名称、id、参数片段),按序号累积。
#[derive(Debug, Default)]
pub(crate) struct CallAcc {
    calls: Vec<(String, String, String)>,
}

impl CallAcc {
    /// 第 `index` 个调用的新片段:id / 名称出现即记下,参数片段追加。
    pub fn push(&mut self, index: usize, id: Option<&str>, name: Option<&str>, args: Option<&str>) {
        while self.calls.len() <= index {
            self.calls.push(Default::default());
        }
        let c = &mut self.calls[index];
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            c.0 = id.to_string();
        }
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            c.1.push_str(name);
        }
        if let Some(a) = args {
            c.2.push_str(a);
        }
    }

    /// 拼好的调用:参数解析不了的当作空对象(交给工具报「缺参数」);没 id 的补一个。
    pub fn finish(self) -> Vec<ToolCall> {
        self.calls
            .into_iter()
            .enumerate()
            .filter(|(_, c)| !c.1.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                name,
                args: serde_json::from_str(if args.trim().is_empty() { "{}" } else { &args })
                    .unwrap_or_else(|_| Value::Object(Default::default())),
                raw: None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_fragments_accumulate_by_index() {
        let mut acc = CallAcc::default();
        acc.push(0, Some("a1"), Some("run_query"), Some("{\"sq"));
        acc.push(1, Some("b2"), Some("describe_table"), Some("{\"table\":\"users\"}"));
        acc.push(0, None, None, Some("l\":\"SELECT 1\"}"));
        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "run_query");
        assert_eq!(calls[0].args["sql"], "SELECT 1");
        assert_eq!(calls[1].id, "b2");
        assert_eq!(calls[1].args["table"], "users");
    }

    #[test]
    fn bad_args_become_empty_object_and_missing_id_is_filled() {
        let mut acc = CallAcc::default();
        acc.push(0, None, Some("list_tables"), Some(""));
        acc.push(1, None, Some("run_query"), Some("{oops"));
        let calls = acc.finish();
        assert_eq!(calls[0].id, "call_0");
        assert_eq!(calls[0].args, serde_json::json!({}));
        assert_eq!(calls[1].args, serde_json::json!({}));
    }

    #[tokio::test]
    async fn cancel_wakes_waiters_and_sticks() {
        let c = Cancel::default();
        assert!(!c.is_cancelled());
        let waiter = tokio::spawn({
            let c = c.clone();
            async move { c.cancelled().await }
        });
        tokio::task::yield_now().await;
        c.cancel();
        waiter.await.unwrap();
        // 已取消之后再等,立即完成
        c.cancelled().await;
        assert!(c.is_cancelled());
    }

    #[test]
    fn history_becomes_items() {
        let m = |role: &str, content: &str| ChatMessage { role: role.into(), content: content.into() };
        let (system, items) = items_of(&[m("system", "S"), m("user", "q1"), m("assistant", "a1"), m("user", "q2")]);
        assert_eq!(system, "S");
        assert_eq!(
            items,
            vec![
                Item::User("q1".into()),
                Item::Assistant { text: "a1".into(), calls: vec![] },
                Item::User("q2".into())
            ]
        );
    }
}
