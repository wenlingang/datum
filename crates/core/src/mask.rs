//! 敏感数据识别与遮罩:按列名、取值抽样认出密码 / 邮箱 / 手机号 / 证件号 / 卡号 / 地址 / 姓名等列,
//! 在数据离开「只有自己看着」的场景时(网格展示、复制、导出、发给 AI)换成遮罩后的值。
//!
//! - 识别([`detect_name`] / [`detect_values`])与遮罩([`mask_value`])都是纯函数,结果确定;
//! - 每个连接的开关、自动识别、手动覆盖(某列强制遮罩 / 永不遮罩)与全局「演示模式」
//!   存在配置目录的 `mask_settings.json`(只含表名列名,不含数据;仍按私有文件写入);
//! - [`policy`] 读出某个连接当前生效的规则,[`Policy::plan`] 给一份结果算出每列的遮罩方式。
//!   AI 相关的出口(工具查询结果、对话历史里的结果表、一句话筛选的样例行)在 core 里强制套用,
//!   不依赖界面。

use crate::commands::AppState;
use crate::core::connection::Env;
use crate::grid::Cell;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// 敏感列的类别,决定遮罩样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaskKind {
    /// 密码、密钥、令牌、盐:整体换成 `••••••`(不透露长度)
    Secret,
    /// 邮箱:`a***@example.com`
    Email,
    /// 电话:`138****1234`
    Phone,
    /// 身份证等证件号:保留前 3 位与后 4 位
    IdNumber,
    /// 银行卡 / 信用卡号:保留前 4 位与后 4 位
    Card,
    /// 地址:首字 + `**`
    Address,
    /// 姓名:首字 + `**`
    Name,
}

impl MaskKind {
    pub const ALL: [MaskKind; 7] = [
        Self::Secret,
        Self::Email,
        Self::Phone,
        Self::IdNumber,
        Self::Card,
        Self::Address,
        Self::Name,
    ];
}

/// 整体遮罩用的占位(固定长度,不透露原值长度)。
pub const SECRET_MASK: &str = "••••••";

// ---------------------------------------------------------------- 列名识别

/// 列名拆成小写词:`passwordHash` / `password_hash` / `Password-Hash` → `["password", "hash"]`。
fn name_tokens(name: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut prev_lower = false;
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            // camelCase 的大写处断词
            if ch.is_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
            cur.extend(ch.to_lowercase());
        } else {
            prev_lower = false;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 说明「这一列存的不是那份数据本身」的词:计数、类型、状态、时间、布尔标记……
/// 如 `token_count`、`password_changed_at`、`is_email_verified`、`phone_type`。
const NOT_DATA: &[&str] = &[
    "count", "cnt", "type", "kind", "status", "state", "verified", "verify", "enabled", "flag", "at", "time", "date",
    "expires", "expire", "expiry", "expired", "len", "length", "hint", "policy", "changed", "updated", "created", "sent",
    "valid", "confirmed", "is", "has", "required", "strength", "attempts", "format", "rule", "level", "visible",
];

/// 按列名识别。只看名字,不看数据;认不出为 None。
pub fn detect_name(column: &str) -> Option<MaskKind> {
    let tokens = name_tokens(column);
    if tokens.is_empty() {
        return None;
    }
    let has = |w: &str| tokens.iter().any(|t| t == w);
    let joined: String = tokens.concat();
    let lower = column.to_lowercase();
    if tokens.iter().any(|t| NOT_DATA.contains(&t.as_str())) {
        return None;
    }
    // 中文列名(SQLite / MySQL 里不少见)
    let cn = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if cn(&["密码", "口令", "密钥", "令牌", "秘钥"]) {
        return Some(MaskKind::Secret);
    }
    if cn(&["邮箱", "电子邮件", "郵箱", "電子郵件"]) {
        return Some(MaskKind::Email);
    }
    if cn(&["身份证", "身份證", "证件号", "證件號", "护照", "護照"]) {
        return Some(MaskKind::IdNumber);
    }
    if cn(&["银行卡", "銀行卡", "卡号", "卡號", "信用卡"]) {
        return Some(MaskKind::Card);
    }
    if cn(&["手机", "手機", "电话", "電話", "联系方式", "聯繫方式"]) {
        return Some(MaskKind::Phone);
    }
    if cn(&["地址", "住址"]) {
        return Some(MaskKind::Address);
    }
    if cn(&["姓名", "真实姓名", "真實姓名"]) {
        return Some(MaskKind::Name);
    }

    // 密码、密钥、令牌
    if ["password", "passwd", "pwd", "secret", "token", "salt", "otp", "totp", "passphrase", "pin"].iter().any(|w| has(w))
        || ["apikey", "accesskey", "privatekey", "secretkey", "signingkey", "authkey", "encryptionkey", "sessionkey", "passcode"]
            .iter()
            .any(|w| joined.contains(w))
    {
        return Some(MaskKind::Secret);
    }
    if has("email") || has("mail") || joined.contains("email") {
        return Some(MaskKind::Email);
    }
    let id_no = tokens.windows(2).any(|w| w[0] == "id" && matches!(w[1].as_str(), "no" | "num" | "number" | "card"));
    if has("ssn")
        || id_no
        || joined == "idnumber"
        || joined == "idno"
        || ["idcard", "identityno", "identitynumber", "identitycard", "nationalid", "passport", "taxid", "sfz"]
            .iter()
            .any(|w| joined.contains(w))
    {
        return Some(MaskKind::IdNumber);
    }
    if has("iban")
        || ["cardno", "cardnum", "creditcard", "debitcard", "bankcard", "bankaccount", "accountnumber", "accountno", "pan"]
            .iter()
            .any(|w| joined == *w || (w.len() > 3 && joined.contains(w)))
    {
        return Some(MaskKind::Card);
    }
    if ["phone", "mobile", "tel", "telephone", "cellphone", "fax", "msisdn", "whatsapp"].iter().any(|w| has(w))
        || joined.contains("phone")
        || joined.contains("mobile")
    {
        return Some(MaskKind::Phone);
    }
    // 地址:排除网络地址(ip_address、mac_addr、remote_address、url…)
    if (has("address") || has("addr") || has("street") || joined.contains("address"))
        && !["ip", "ipv4", "ipv6", "mac", "remote", "server", "host", "url", "web", "wallet", "contract", "bind", "listen", "node"]
            .iter()
            .any(|w| has(w))
    {
        return Some(MaskKind::Address);
    }
    if ["realname", "fullname", "firstname", "lastname", "surname", "givenname", "familyname", "legalname", "contactname", "cardholder", "holdername"]
        .iter()
        .any(|w| joined.contains(w))
    {
        return Some(MaskKind::Name);
    }
    None
}

// ---------------------------------------------------------------- 取值识别

static EMAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}$").unwrap());
static CN_MOBILE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\+?86[\s\-]?)?1[3-9]\d{9}$").unwrap());
/// 国际号码:必须以 + 开头(没有 + 的长串数字太容易和普通编号混淆)
static INTL_PHONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\+\d{1,3}[\s\-.]?\(?\d{1,4}\)?(?:[\s\-.]?\d{2,4}){2,4}$").unwrap());
/// 北美写法 (555) 123-4567 / 555-123-4567
static NANP_PHONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\(?\d{3}\)?[\s\-.]\d{3}[\s\-.]\d{4}$").unwrap());
static CN_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{17}[\dXx]$").unwrap());
/// 卡号:13–19 位数字,可按 4 位一组用空格 / 短横分隔
static CARD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d(?:[\s\-]?\d){12,18}$").unwrap());
static JWT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*$").unwrap());
static HASHED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\$2[abxy]?\$\d{2}\$[./A-Za-z0-9]{53}|\$argon2(?:id|i|d)\$\S+|\$[156]\$\S{8,}|pbkdf2_\w+\$\S+|scrypt:\S+)$").unwrap());
/// 常见服务商密钥前缀:OpenAI / Anthropic、GitHub、AWS、Slack、Stripe、Google
static KEY_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,}|AKIA[0-9A-Z]{16}|xox[abposr]-[A-Za-z0-9\-]{10,}|[rs]k_(?:live|test)_[A-Za-z0-9]{16,}|AIza[0-9A-Za-z_\-]{35})$").unwrap()
});
static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]+$").unwrap());
static B64: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9+/_\-]{32,}={0,2}$").unwrap());

/// 18 位身份证:校验码正确,出生日期像样。
fn is_cn_id(v: &str) -> bool {
    if !CN_ID.is_match(v) {
        return false;
    }
    let b = v.as_bytes();
    const W: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    let sum: u32 = (0..17).map(|i| (b[i] - b'0') as u32 * W[i]).sum();
    let check = b"10X98765432"[(sum % 11) as usize];
    if b[17].to_ascii_uppercase() != check {
        return false;
    }
    let num = |s: &str| s.parse::<u32>().unwrap_or(0);
    let (y, m, d) = (num(&v[6..10]), num(&v[10..12]), num(&v[12..14]));
    (1900..=2099).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// Luhn 校验。
fn luhn(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    for (i, d) in digits.iter().rev().enumerate() {
        let mut x = (*d - b'0') as u32;
        if i % 2 == 1 {
            x *= 2;
            if x > 9 {
                x -= 9;
            }
        }
        sum += x;
    }
    sum % 10 == 0
}

/// 卡号:13–19 位、Luhn 通过、开头是常见卡组织(2–6)、不是同一个数字重复。
fn is_card(v: &str) -> bool {
    if !CARD.is_match(v) {
        return false;
    }
    let digits: Vec<u8> = v.bytes().filter(u8::is_ascii_digit).collect();
    (13..=19).contains(&digits.len())
        && (b'2'..=b'6').contains(&digits[0])
        && digits.iter().any(|d| *d != digits[0])
        && luhn(&digits)
}

/// 像密钥 / 令牌 / 口令散列的值。
fn is_secret(v: &str) -> bool {
    if JWT.is_match(v) || HASHED.is_match(v) || KEY_PREFIX.is_match(v) {
        return true;
    }
    // md5 / sha1 / sha256 / sha512 的十六进制散列
    if HEX.is_match(v) && matches!(v.len(), 32 | 40 | 64 | 128) {
        return v.bytes().any(|b| b.is_ascii_alphabetic()) && v.bytes().any(|b| b.is_ascii_digit());
    }
    // 长的随机 base64 / base64url 串:大小写字母和数字都有
    B64.is_match(v)
        && v.bytes().any(|b| b.is_ascii_uppercase())
        && v.bytes().any(|b| b.is_ascii_lowercase())
        && v.bytes().filter(u8::is_ascii_digit).count() >= 3
}

/// 单个值像哪一类(只认格式特征明显的;地址、姓名靠列名)。
pub fn classify_value(v: &str) -> Option<MaskKind> {
    let v = v.trim();
    if v.is_empty() || v.len() > 512 {
        return None;
    }
    if EMAIL.is_match(v) {
        return Some(MaskKind::Email);
    }
    if is_cn_id(v) {
        return Some(MaskKind::IdNumber);
    }
    if CN_MOBILE.is_match(v) || INTL_PHONE.is_match(v) || NANP_PHONE.is_match(v) {
        return Some(MaskKind::Phone);
    }
    if is_card(v) {
        return Some(MaskKind::Card);
    }
    if is_secret(v) {
        return Some(MaskKind::Secret);
    }
    None
}

/// 每列最多看多少个值。
pub const SAMPLE_VALUES: usize = 50;

/// 按取值抽样识别:非空值里至少 80% 是同一类,且命中不少于 min(3, 非空数) 个。
/// 一个随机的长数字编号偶然通过 Luhn 的概率约 1/10,整列都通过几乎不可能,不会被当成卡号。
pub fn detect_values<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<MaskKind> {
    let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
    let mut n = 0usize;
    for v in values.into_iter().filter(|v| !v.trim().is_empty()).take(SAMPLE_VALUES) {
        n += 1;
        if let Some(k) = classify_value(v) {
            *counts.entry(k as u8).or_default() += 1;
        }
    }
    if n == 0 {
        return None;
    }
    let (kind, hits) = counts.into_iter().max_by_key(|(_, c)| *c)?;
    if hits * 5 >= n * 4 && hits >= n.min(3) {
        MaskKind::ALL.into_iter().find(|k| *k as u8 == kind)
    } else {
        None
    }
}

// ---------------------------------------------------------------- 遮罩

/// 保留前 `head` 个、后 `tail` 个数字 / 字母,中间的数字 / 字母换成 `*`,分隔符原样。
fn keep_ends(v: &str, head: usize, tail: usize) -> String {
    let total = v.chars().filter(|c| c.is_alphanumeric()).count();
    let (head, tail) = if total <= head + tail {
        // 太短:只留最后 2 位(1–2 位的整体遮掉)
        (0, if total > 2 { 2 } else { 0 })
    } else {
        (head, tail)
    };
    let mut i = 0;
    v.chars()
        .map(|c| {
            if !c.is_alphanumeric() {
                return c;
            }
            i += 1;
            if i <= head || i > total - tail {
                c
            } else {
                '*'
            }
        })
        .collect()
}

fn first_char_plus(v: &str) -> String {
    match v.trim().chars().next() {
        Some(c) => format!("{c}**"),
        None => String::new(),
    }
}

/// 按类别遮罩一个值。空串原样;确定、无副作用。
pub fn mask_value(kind: MaskKind, v: &str) -> String {
    if v.is_empty() {
        return String::new();
    }
    match kind {
        MaskKind::Secret => SECRET_MASK.to_string(),
        MaskKind::Email => match v.trim().rsplit_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
                format!("{}***@{domain}", local.chars().next().unwrap_or('*'))
            }
            _ => first_char_plus(v),
        },
        MaskKind::Phone => keep_ends(v.trim(), 3, 4),
        MaskKind::IdNumber => keep_ends(v.trim(), 3, 4),
        MaskKind::Card => keep_ends(v.trim(), 4, 4),
        MaskKind::Address | MaskKind::Name => first_char_plus(v),
    }
}

/// 可空单元格的遮罩:NULL 仍是 NULL。
pub fn mask_cell(kind: Option<MaskKind>, v: &Cell) -> Cell {
    match (kind, v) {
        (Some(k), Some(s)) => Some(mask_value(k, s)),
        _ => v.clone(),
    }
}

// ---------------------------------------------------------------- 设置

/// 某列的手动覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Override {
    /// 强制遮罩(自动识别没认出来的列)
    Mask,
    /// 永不遮罩(误判的列)
    Plain,
}

/// 一个连接的遮罩设置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ConnMask {
    /// 总开关;None 表示按环境默认(local 关,staging / prod 开)
    pub enabled: Option<bool>,
    /// 按列名 / 取值自动识别;关掉后只遮罩手动标记的列
    pub auto_detect: bool,
    /// `表.列`(或只有 `列`,对所有表生效)→ 覆盖
    pub overrides: BTreeMap<String, Override>,
}

impl Default for ConnMask {
    fn default() -> Self {
        Self { enabled: None, auto_detect: true, overrides: BTreeMap::new() }
    }
}

/// `mask_settings.json` 的整体内容。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MaskSettings {
    /// 演示模式:所有连接强制遮罩、不能「显示原值」,界面隐藏主机 / 端口 / 用户名 / 文件路径
    pub demo_mode: bool,
    pub connections: BTreeMap<String, ConnMask>,
}

impl MaskSettings {
    pub fn conn(&self, id: &str) -> ConnMask {
        self.connections.get(id).cloned().unwrap_or_default()
    }
}

/// 环境对应的默认开关:本地库默认不遮罩,预发 / 生产默认遮罩;不知道环境时按遮罩处理。
pub fn default_enabled(env: Option<Env>) -> bool {
    !matches!(env, Some(Env::Local))
}

/// 仿 AiSettingsRepo:config_dir + 固定文件名。
pub struct MaskRepo {
    path: PathBuf,
}

impl MaskRepo {
    pub fn new(config_dir: &Path) -> Self {
        Self { path: config_dir.join("mask_settings.json") }
    }

    /// 文件缺失 / 损坏都回默认值(按环境默认遮罩),不让应用报错。
    pub fn load(&self) -> MaskSettings {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, s: &MaskSettings) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
        crate::core::fsutil::write_private(&self.path, &json).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------- 生效规则

/// 某个连接当前生效的遮罩规则。
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub enabled: bool,
    pub auto_detect: bool,
    pub demo: bool,
    pub overrides: BTreeMap<String, Override>,
}

impl Policy {
    /// 由设置与连接环境算出;演示模式强制打开。
    pub fn from_settings(settings: &MaskSettings, conn_id: &str, env: Option<Env>) -> Policy {
        let c = settings.conn(conn_id);
        Policy {
            enabled: settings.demo_mode || c.enabled.unwrap_or_else(|| default_enabled(env)),
            auto_detect: c.auto_detect,
            demo: settings.demo_mode,
            overrides: c.overrides,
        }
    }

    /// 不遮罩任何列。
    pub fn off() -> Policy {
        Policy { enabled: false, auto_detect: false, demo: false, overrides: BTreeMap::new() }
    }

    /// 这一列的手动覆盖。`table` 未知(查询结果)时:「强制遮罩」按列名匹配任意表,
    /// 「永不遮罩」只认不带表名的规则——宁可多遮,不因同名列误放。
    fn override_of(&self, table: Option<&str>, column: &str) -> Option<Override> {
        let col = column.to_lowercase();
        let table = table.map(|t| t.to_lowercase());
        let short = table.as_deref().map(|t| t.rsplit('.').next().unwrap_or(t).to_string());
        let mut found: Option<Override> = None;
        for (key, o) in &self.overrides {
            let key = key.to_lowercase();
            let (kt, kc) = match key.rsplit_once('.') {
                Some((t, c)) => (Some(t), c),
                None => (None, key.as_str()),
            };
            if kc != col {
                continue;
            }
            let hit = match (kt, table.as_deref()) {
                (None, _) => true,
                (Some(kt), Some(t)) => kt == t || Some(kt.rsplit('.').next().unwrap_or(kt)) == short.as_deref(),
                (Some(_), None) => *o == Override::Mask,
            };
            if hit {
                // 同一列既有强制遮罩又有永不遮罩(不同写法的键):遮罩优先
                if found != Some(Override::Mask) {
                    found = Some(*o);
                }
            }
        }
        found
    }

    /// 一列的遮罩方式。`values` 为该列的样本值(取值识别用)。
    pub fn column_kind<'a>(&self, table: Option<&str>, column: &str, values: impl IntoIterator<Item = &'a str>) -> Option<MaskKind> {
        if !self.enabled {
            return None;
        }
        match self.override_of(table, column) {
            Some(Override::Plain) => None,
            Some(Override::Mask) => Some(detect_name(column).or_else(|| detect_values(values)).unwrap_or(MaskKind::Secret)),
            None if self.auto_detect => detect_name(column).or_else(|| detect_values(values)),
            None => None,
        }
    }

    /// 一份结果每列的遮罩方式(与 `columns` 一一对应)。`rows` 只用来抽样识别。
    pub fn plan(&self, table: Option<&str>, columns: &[String], rows: &[Vec<Cell>]) -> Vec<Option<MaskKind>> {
        columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                self.column_kind(
                    table,
                    c,
                    rows.iter().take(SAMPLE_VALUES * 2).filter_map(|r| r.get(i).and_then(|v| v.as_deref())),
                )
            })
            .collect()
    }

    /// 就地遮罩一份结果;返回是否遮罩了任何列。
    pub fn mask_result(&self, table: Option<&str>, columns: &[String], rows: &mut [Vec<Cell>]) -> bool {
        let plan = self.plan(table, columns, rows);
        apply_plan(&plan, rows)
    }
}

/// 按遮罩方式就地改写行;返回是否有列被遮罩。
pub fn apply_plan(plan: &[Option<MaskKind>], rows: &mut [Vec<Cell>]) -> bool {
    if plan.iter().all(Option::is_none) {
        return false;
    }
    for row in rows.iter_mut() {
        for (i, cell) in row.iter_mut().enumerate() {
            if let Some(Some(k)) = plan.get(i) {
                if let Some(s) = cell.as_deref() {
                    *cell = Some(mask_value(*k, s));
                }
            }
        }
    }
    true
}

/// 读出连接 `conn_id` 当前生效的规则(连接与遮罩设置都现读)。读不到连接时按非本地处理(遮罩)。
pub fn policy(state: &AppState, conn_id: &str) -> Policy {
    let settings = MaskRepo::new(&state.config_dir).load();
    let env = crate::core::repository::ConnectionRepo::new(&state.config_dir)
        .load()
        .ok()
        .and_then(|cs| cs.into_iter().find(|c| c.id == conn_id).map(|c| c.env));
    Policy::from_settings(&settings, conn_id, env)
}

/// 查询语句的来源表(手动覆盖按 `表.列` 匹配用):只认单表查询,带 JOIN 的不猜。
pub fn source_table(sql: &str) -> Option<String> {
    static JOIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bjoin\b|\bfrom\s+[^;]*,").unwrap());
    if JOIN.is_match(sql) {
        return None;
    }
    crate::sql::build::guess_table_name(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn names_are_recognized() {
        use MaskKind::*;
        for (name, kind) in [
            ("password", Secret),
            ("password_hash", Secret),
            ("passwordHash", Secret),
            ("user_pwd", Secret),
            ("api_key", Secret),
            ("apiKey", Secret),
            ("AccessKeyId", Secret),
            ("private_key", Secret),
            ("refresh_token", Secret),
            ("client_secret", Secret),
            ("salt", Secret),
            ("email", Email),
            ("contact_email", Email),
            ("emailAddress", Email),
            ("email_address", Email),
            ("phone", Phone),
            ("mobile", Phone),
            ("tel", Phone),
            ("phone_number", Phone),
            ("mobilePhone", Phone),
            ("id_card", IdNumber),
            ("idcard", IdNumber),
            ("id_number", IdNumber),
            ("ssn", IdNumber),
            ("passport_no", IdNumber),
            ("card_no", Card),
            ("credit_card", Card),
            ("iban", Card),
            ("bank_account", Card),
            ("address", Address),
            ("home_address", Address),
            ("street", Address),
            ("real_name", Name),
            ("full_name", Name),
            ("firstName", Name),
            ("身份证号", IdNumber),
            ("手机号", Phone),
            ("登录密码", Secret),
            ("收货地址", Address),
            ("姓名", Name),
        ] {
            assert_eq!(detect_name(name), Some(kind), "{name}");
        }
    }

    #[test]
    fn harmless_names_are_not_flagged() {
        for name in [
            "id", "name", "product_name", "title", "created_at", "token_count", "password_changed_at", "is_email_verified",
            "email_verified", "phone_type", "ip_address", "mac_addr", "remote_address", "passenger", "compass", "telemetry",
            "hotel", "sku", "price_cents", "status", "card_type", "address_count", "description", "hashtag", "ticket", "spin",
            "pinned",
        ] {
            assert_eq!(detect_name(name), None, "{name}");
        }
    }

    #[test]
    fn values_are_recognized() {
        assert_eq!(detect_values(["a@x.com", "bob.smith@example.co.uk"]), Some(MaskKind::Email));
        assert_eq!(detect_values(["13812341234", "+86 13987654321", "15000000000"]), Some(MaskKind::Phone));
        assert_eq!(detect_values(["+1 415 555 2671", "+44 20 7946 0958"]), Some(MaskKind::Phone));
        // 11010519491231002X:校验码正确
        assert_eq!(detect_values(["11010519491231002X"]), Some(MaskKind::IdNumber));
        assert_eq!(detect_values(["4111111111111111", "5500 0000 0000 0004", "340000000000009"]), Some(MaskKind::Card));
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(detect_values([jwt]), Some(MaskKind::Secret));
        assert_eq!(detect_values(["$2b$12$KIXQJ1n7Yk3tH2Pz0q8u6eJw3mJr9cZpI2Wq0Yb1nD4fQ6sT8uV0e"]), Some(MaskKind::Secret));
        assert_eq!(detect_values(["5f4dcc3b5aa765d61d8327deb882cf99"]), Some(MaskKind::Secret));
        assert_eq!(detect_values(["sk-proj-abcdefghijklmnop1234"]), Some(MaskKind::Secret));
        assert_eq!(detect_values(["AKIAIOSFODNN7EXAMPLE"]), Some(MaskKind::Secret));
    }

    #[test]
    fn value_false_positives_are_guarded() {
        // 普通自增 id、金额、日期、短编号
        assert_eq!(detect_values(["1", "2", "3", "42"]), None);
        assert_eq!(detect_values(["12.50", "3.99"]), None);
        assert_eq!(detect_values(["2025-04-01 08:00:00"]), None);
        assert_eq!(detect_values(["SO-001", "SO-002"]), None);
        // 雪花 id 等长数字:偶有一个过 Luhn,整列不会
        let ids = ["1793238383737483264", "1793238383737483265", "1793238383737483266", "1793238383737483267", "1793238383737483268"];
        assert_eq!(detect_values(ids), None);
        // 卡号格式但开头不是卡组织、或同一数字重复
        assert!(!is_card("0000000000000000"));
        assert!(!is_card("4444444444444444"));
        assert!(!is_card("9111111111111111"));
        // 校验码不对的 18 位数字不是身份证
        assert!(!is_cn_id("110105194912310021"));
        // 不带 + 的长数字不当国际号码
        assert_eq!(classify_value("4155552671234"), None);
        // UUID、普通单词、短十六进制
        assert_eq!(classify_value("550e8400-e29b-41d4-a716-446655440000"), None);
        assert_eq!(classify_value("deadbeef"), None);
        assert_eq!(classify_value("Ethiopia"), None);
        // 少数命中达不到 80%
        assert_eq!(detect_values(["a@x.com", "n/a", "none", "-", "unknown"]), None);
    }

    #[test]
    fn mask_styles() {
        use MaskKind::*;
        assert_eq!(mask_value(Secret, "hunter2"), "••••••");
        assert_eq!(mask_value(Secret, "a"), "••••••");
        assert_eq!(mask_value(Email, "alice@example.com"), "a***@example.com");
        assert_eq!(mask_value(Email, "not-an-email"), "n**");
        assert_eq!(mask_value(Phone, "13812341234"), "138****1234");
        assert_eq!(mask_value(IdNumber, "11010519491231002X"), "110***********002X");
        assert_eq!(mask_value(Card, "4111111111111111"), "4111********1111");
        assert_eq!(mask_value(Card, "4111 1111 1111 1111"), "4111 **** **** 1111");
        assert_eq!(mask_value(Address, "北京市朝阳区建国路 1 号"), "北**");
        assert_eq!(mask_value(Name, "Alice Smith"), "A**");
        assert_eq!(mask_value(Name, "张三"), "张**");
        // 太短的号码:只留最后 2 位
        assert_eq!(mask_value(Phone, "12345"), "***45");
        assert_eq!(mask_value(Card, "12"), "**");
        // 空串、NULL
        assert_eq!(mask_value(Email, ""), "");
        assert_eq!(mask_cell(Some(Email), &None), None);
        assert_eq!(mask_cell(None, &Some("x".into())), Some("x".into()));
        // 确定性:同样的输入同样的输出
        assert_eq!(mask_value(Phone, "13812341234"), mask_value(Phone, "13812341234"));
    }

    #[test]
    fn phone_with_country_code_keeps_separators() {
        // 数字 86138123412 34 共 13 位:留前 3 后 4,分隔符原样
        assert_eq!(mask_value(MaskKind::Phone, "+86 138-1234-1234"), "+86 1**-****-1234");
    }

    fn policy_with(enabled: Option<bool>, env: Option<Env>, overrides: &[(&str, Override)], demo: bool) -> Policy {
        let mut s = MaskSettings { demo_mode: demo, ..Default::default() };
        s.connections.insert(
            "c".into(),
            ConnMask { enabled, overrides: overrides.iter().map(|(k, o)| (k.to_string(), *o)).collect(), ..Default::default() },
        );
        Policy::from_settings(&s, "c", env)
    }

    #[test]
    fn defaults_follow_environment_and_demo_forces_on() {
        let s = MaskSettings::default();
        assert!(!Policy::from_settings(&s, "x", Some(Env::Local)).enabled);
        assert!(Policy::from_settings(&s, "x", Some(Env::Staging)).enabled);
        assert!(Policy::from_settings(&s, "x", Some(Env::Prod)).enabled);
        assert!(Policy::from_settings(&s, "x", None).enabled);
        assert!(!policy_with(Some(false), Some(Env::Prod), &[], false).enabled);
        assert!(policy_with(Some(false), Some(Env::Local), &[], true).enabled);
        assert!(policy_with(Some(true), Some(Env::Local), &[], false).enabled);
    }

    #[test]
    fn plan_uses_names_values_and_overrides() {
        let cols = s(&["id", "email", "notes", "contact", "nickname"]);
        let rows = vec![
            vec![Some("1".into()), Some("a@x.com".into()), Some("hi".into()), Some("13812341234".into()), Some("Al".into())],
            vec![Some("2".into()), None, Some("yo".into()), Some("13912341234".into()), Some("Bo".into())],
        ];
        let p = policy_with(Some(true), None, &[("users.notes", Override::Mask), ("users.email", Override::Plain)], false);
        // 表名已知:email 被永不遮罩放行,notes 强制遮罩(认不出类别 → 整体遮罩),contact 按取值认成电话
        assert_eq!(
            p.plan(Some("users"), &cols, &rows),
            vec![None, None, Some(MaskKind::Secret), Some(MaskKind::Phone), None]
        );
        // 查询结果(表名未知):永不遮罩不生效,强制遮罩仍按列名生效
        assert_eq!(
            p.plan(None, &cols, &rows),
            vec![None, Some(MaskKind::Email), Some(MaskKind::Secret), Some(MaskKind::Phone), None]
        );
        // 带 schema 的表名与短表名互认
        assert_eq!(p.plan(Some("public.users"), &cols, &rows)[1], None);
        // 关掉自动识别:只剩手动标记
        let mut p2 = p.clone();
        p2.auto_detect = false;
        assert_eq!(p2.plan(Some("users"), &cols, &rows), vec![None, None, Some(MaskKind::Secret), None, None]);
        // 总开关关掉:什么都不遮
        assert!(Policy::off().plan(None, &cols, &rows).iter().all(Option::is_none));
    }

    #[test]
    fn mask_result_rewrites_rows_and_keeps_null() {
        let cols = s(&["id", "password", "email"]);
        let mut rows = vec![vec![Some("1".into()), Some("hunter2".into()), None]];
        let p = policy_with(Some(true), None, &[], false);
        assert!(p.mask_result(None, &cols, &mut rows));
        assert_eq!(rows[0], vec![Some("1".into()), Some(SECRET_MASK.into()), None]);
        let mut plain = vec![vec![Some("1".into())]];
        assert!(!p.mask_result(None, &s(&["id"]), &mut plain));
    }

    #[test]
    fn source_table_only_for_single_table_queries() {
        assert_eq!(source_table("SELECT email FROM users WHERE id = 1").as_deref(), Some("users"));
        assert_eq!(source_table("select * from \"Users\"").as_deref(), Some("Users"));
        assert_eq!(source_table("SELECT u.email FROM users u JOIN orders o ON o.uid = u.id"), None);
        assert_eq!(source_table("SELECT * FROM users, orders"), None);
    }

    #[test]
    fn settings_roundtrip_with_private_file() {
        let dir = std::env::temp_dir().join(format!("datum-mask-{}", uuid::Uuid::new_v4()));
        let repo = MaskRepo::new(&dir);
        assert_eq!(repo.load(), MaskSettings::default());
        let mut s = MaskSettings { demo_mode: true, ..Default::default() };
        s.connections.insert("c1".into(), ConnMask { enabled: Some(false), auto_detect: false, overrides: [("t.c".to_string(), Override::Mask)].into() });
        repo.save(&s).unwrap();
        assert_eq!(repo.load(), s);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("mask_settings.json")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // 缺字段的旧文件:自动识别默认开
        std::fs::write(dir.join("mask_settings.json"), r#"{"connections":{"c2":{"enabled":true}}}"#).unwrap();
        assert!(repo.load().conn("c2").auto_detect);
        std::fs::remove_dir_all(dir).ok();
    }
}
