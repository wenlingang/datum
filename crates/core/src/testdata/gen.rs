//! 生成器:每列一种,参数可在界面里改。值先生成成 [`Value`],再按方言写成字面量或绑定参数。

use super::meta::BaseType;
use super::rng::Rng;
use super::words::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Locale {
    #[default]
    En,
    Zh,
}

/// 一列的生成方式。`Skip` 表示不写这一列(自增 / 生成列,或交给默认值)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Generator {
    Skip,
    Null,
    Fixed { value: String },
    PickList { values: Vec<String> },
    FullName,
    FirstName,
    LastName,
    Username,
    Email,
    Phone,
    Address,
    City,
    Country,
    PostalCode,
    Company,
    Url,
    Ip,
    Uuid,
    Word,
    Title,
    Sentence,
    Paragraph,
    /// 随机字母数字串(长度上下限,按字符)
    Text { min_len: u32, max_len: u32 },
    Integer {
        #[serde(deserialize_with = "super::meta::lenient_i64")]
        min: i64,
        #[serde(deserialize_with = "super::meta::lenient_i64")]
        max: i64,
    },
    /// 递增序列:没有自增的整数主键 / 唯一列
    Sequence {
        #[serde(deserialize_with = "super::meta::lenient_i64")]
        start: i64,
        #[serde(deserialize_with = "super::meta::lenient_i64")]
        step: i64,
    },
    Decimal { min: f64, max: f64, scale: u32 },
    Float { min: f64, max: f64 },
    Boolean,
    /// 日期 / 时间范围:`YYYY-MM-DD`
    Date { from: String, to: String },
    Timestamp { from: String, to: String },
    Time,
    Json,
    Bytes { len: u32 },
    /// 取父表已有的键(外键列;复合外键各列一起取同一行)
    ForeignKey,
}

impl Generator {
    /// 稳定的类型名(界面下拉、AI 回复用)。
    pub fn kind(&self) -> &'static str {
        match self {
            Generator::Skip => "skip",
            Generator::Null => "null",
            Generator::Fixed { .. } => "fixed",
            Generator::PickList { .. } => "pickList",
            Generator::FullName => "fullName",
            Generator::FirstName => "firstName",
            Generator::LastName => "lastName",
            Generator::Username => "username",
            Generator::Email => "email",
            Generator::Phone => "phone",
            Generator::Address => "address",
            Generator::City => "city",
            Generator::Country => "country",
            Generator::PostalCode => "postalCode",
            Generator::Company => "company",
            Generator::Url => "url",
            Generator::Ip => "ip",
            Generator::Uuid => "uuid",
            Generator::Word => "word",
            Generator::Title => "title",
            Generator::Sentence => "sentence",
            Generator::Paragraph => "paragraph",
            Generator::Text { .. } => "text",
            Generator::Integer { .. } => "integer",
            Generator::Sequence { .. } => "sequence",
            Generator::Decimal { .. } => "decimal",
            Generator::Float { .. } => "float",
            Generator::Boolean => "boolean",
            Generator::Date { .. } => "date",
            Generator::Timestamp { .. } => "timestamp",
            Generator::Time => "time",
            Generator::Json => "json",
            Generator::Bytes { .. } => "bytes",
            Generator::ForeignKey => "foreignKey",
        }
    }

    /// 生成字符串的(语义类)生成器。
    pub fn is_text_like(&self) -> bool {
        matches!(
            self,
            Generator::FullName
                | Generator::FirstName
                | Generator::LastName
                | Generator::Username
                | Generator::Email
                | Generator::Phone
                | Generator::Address
                | Generator::City
                | Generator::Country
                | Generator::PostalCode
                | Generator::Company
                | Generator::Url
                | Generator::Ip
                | Generator::Uuid
                | Generator::Word
                | Generator::Title
                | Generator::Sentence
                | Generator::Paragraph
                | Generator::Text { .. }
        )
    }

    /// 能否用在这种类型的列上(AI 建议与界面改选时校验)。
    pub fn fits(&self, base: &BaseType) -> bool {
        use BaseType as B;
        match self {
            Generator::Skip | Generator::Null | Generator::Fixed { .. } | Generator::PickList { .. } | Generator::ForeignKey => true,
            Generator::Uuid => matches!(base, B::Uuid | B::Text { .. } | B::Other | B::Bytes { .. }),
            Generator::Ip => matches!(base, B::Inet | B::Text { .. } | B::Other),
            Generator::PostalCode | Generator::Phone => matches!(base, B::Text { .. } | B::Other),
            g if g.is_text_like() => matches!(base, B::Text { .. } | B::Other),
            Generator::Integer { .. } | Generator::Sequence { .. } => {
                matches!(base, B::Int { .. } | B::Decimal { .. } | B::Float | B::Text { .. } | B::Other)
            }
            Generator::Decimal { .. } | Generator::Float { .. } => matches!(base, B::Decimal { .. } | B::Float | B::Text { .. } | B::Other),
            Generator::Boolean => matches!(base, B::Bool | B::Int { .. } | B::Text { .. } | B::Other),
            Generator::Date { .. } => matches!(base, B::Date | B::Timestamp | B::Text { .. } | B::Other),
            Generator::Timestamp { .. } => matches!(base, B::Timestamp | B::Text { .. } | B::Other),
            Generator::Time => matches!(base, B::Time | B::Text { .. } | B::Other),
            Generator::Json => matches!(base, B::Json | B::Text { .. } | B::Other),
            Generator::Bytes { .. } => matches!(base, B::Bytes { .. }),
            _ => false,
        }
    }
}

/// 生成出来的一个值。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    /// 定点小数,已按小数位格式化的文本(不经浮点误差)
    Num(String),
    Float(f64),
    Bool(bool),
    Text(String),
    Bytes(Vec<u8>),
}

impl Value {
    /// 展示 / 唯一性比较用的文本;NULL 为 None。
    pub fn display(&self) -> Option<String> {
        match self {
            Value::Null => None,
            Value::Int(i) => Some(i.to_string()),
            Value::Num(s) | Value::Text(s) => Some(s.clone()),
            Value::Float(f) => Some(format_float(*f)),
            Value::Bool(b) => Some(if *b { "true" } else { "false" }.into()),
            Value::Bytes(b) => Some(format!("0x{}", hex(b))),
        }
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn format_float(f: f64) -> String {
    let s = format!("{f:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" { "0".into() } else { s.to_string() }
}

/// 定点小数格式化:四舍五入到 scale 位。
pub fn format_decimal(v: f64, scale: u32) -> String {
    format!("{v:.prec$}", prec = scale as usize)
}

/// 生成时的上下文:列的类型约束 + 序号(唯一化用)。
pub struct Ctx<'a> {
    pub locale: Locale,
    pub base: &'a BaseType,
    /// 第几行(从 0 起),Sequence 用
    pub row: u64,
    /// 唯一列重试的第几次(>0 时给语义串加后缀,扩大取值空间)
    pub attempt: u32,
}

fn digits(rng: &mut Rng, n: usize) -> String {
    (0..n).map(|_| char::from(b'0' + rng.below(10) as u8)).collect()
}

fn alnum(rng: &mut Rng, n: usize) -> String {
    const CS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..n).map(|_| CS[rng.below(CS.len() as u64) as usize] as char).collect()
}

fn ascii_handle(rng: &mut Rng) -> (String, String) {
    let first = rng.pick(EN_FIRST).to_ascii_lowercase();
    let last = rng.pick(EN_LAST).to_ascii_lowercase();
    (first, last)
}

fn words(rng: &mut Rng, list: &[&str], n: usize, sep: &str) -> String {
    (0..n).map(|_| *rng.pick(list)).collect::<Vec<_>>().join(sep)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn sentence(rng: &mut Rng, locale: Locale) -> String {
    match locale {
        Locale::En => {
            let n = 6 + rng.below(8) as usize;
            format!("{}.", capitalize(&words(rng, EN_WORDS, n, " ")))
        }
        Locale::Zh => {
            let n = 1 + rng.below(2) as usize;
            format!("{}。", words(rng, ZH_PHRASES, n, ","))
        }
    }
}

/// 天数 ↔ 日期(公历,1970-01-01 为 0)。
pub fn days_to_date(days: i64) -> (i64, u32, u32) {
    // Howard Hinnant 的 civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn date_to_days(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD`(可带时间,只取日期部分)→ 天数。
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let date = s.get(..10)?;
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(date_to_days(y, m, d))
}

pub fn format_date(days: i64) -> String {
    let (y, m, d) = days_to_date(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 日期范围(含两端);写反了交换,解析不了用默认范围。
pub fn date_range(from: &str, to: &str) -> (i64, i64) {
    let today = chrono::Local::now().date_naive();
    let fallback_to = date_to_days(today.year() as i64, today.month(), today.day());
    let a = parse_date(from).unwrap_or(fallback_to - 730);
    let b = parse_date(to).unwrap_or(fallback_to);
    if a <= b { (a, b) } else { (b, a) }
}

use chrono::Datelike;

/// 生成一个值(不含 NULL 比例、唯一化、外键,这些由引擎处理)。
pub fn generate(g: &Generator, rng: &mut Rng, ctx: &Ctx) -> Value {
    let zh = ctx.locale == Locale::Zh;
    // 唯一列重试时的后缀:取值空间不够(人名、城市)也能凑出不同的值
    let suffix = |rng: &mut Rng| if ctx.attempt > 0 { format!(" {}", 1 + rng.below(9999)) } else { String::new() };
    let text = |s: String| Value::Text(s);
    match g {
        Generator::Skip | Generator::Null | Generator::ForeignKey => Value::Null,
        Generator::Fixed { value } => text(value.clone()),
        Generator::PickList { values } => {
            if values.is_empty() {
                Value::Null
            } else {
                text(rng.pick(values).clone())
            }
        }
        Generator::FullName => {
            let s = if zh {
                format!("{}{}", rng.pick(ZH_SURNAME), rng.pick(ZH_GIVEN))
            } else {
                format!("{} {}", rng.pick(EN_FIRST), rng.pick(EN_LAST))
            };
            text(s + &suffix(rng))
        }
        Generator::FirstName => text((if zh { rng.pick(ZH_GIVEN) } else { rng.pick(EN_FIRST) }).to_string() + &suffix(rng)),
        Generator::LastName => text((if zh { rng.pick(ZH_SURNAME) } else { rng.pick(EN_LAST) }).to_string() + &suffix(rng)),
        Generator::Username => {
            let (f, l) = ascii_handle(rng);
            let n = rng.below(1000);
            let s = match rng.below(3) {
                0 => format!("{f}.{l}{n}"),
                1 => format!("{f}_{}{n}", &l[..1]),
                _ => format!("{}{l}{n}", &f[..1]),
            };
            text(if ctx.attempt > 0 { format!("{s}{}", rng.below(100_000)) } else { s })
        }
        Generator::Email => {
            let (f, l) = ascii_handle(rng);
            // 带上随机数字:唯一邮箱在几千行内很少撞,撞了再重试
            let n = rng.below(10_000);
            let extra = if ctx.attempt > 0 { format!(".{}", alnum(rng, 4)) } else { String::new() };
            text(format!("{f}.{l}{n}{extra}@{}", rng.pick(EMAIL_DOMAINS)))
        }
        Generator::Phone => {
            let s = if zh {
                format!("{}{}", rng.pick(ZH_MOBILE_PREFIX), digits(rng, 8))
            } else {
                format!("+1-{}-555-{}", 200 + rng.below(800), digits(rng, 4))
            };
            text(s)
        }
        Generator::Address => {
            let s = if zh {
                format!("{}{}{}{}号", rng.pick(ZH_PROVINCE), rng.pick(ZH_DISTRICT), rng.pick(ZH_STREET), 1 + rng.below(999))
            } else {
                format!("{} {}, {}", 1 + rng.below(9999), rng.pick(EN_STREET), rng.pick(EN_CITY))
            };
            text(s + &suffix(rng))
        }
        Generator::City => text((if zh { rng.pick(ZH_CITY) } else { rng.pick(EN_CITY) }).to_string() + &suffix(rng)),
        Generator::Country => text((if zh { rng.pick(ZH_COUNTRY) } else { rng.pick(EN_COUNTRY) }).to_string() + &suffix(rng)),
        Generator::PostalCode => text(if zh { format!("{}{}", 1 + rng.below(8), digits(rng, 5)) } else { digits(rng, 5) }),
        Generator::Company => {
            let s = if zh {
                format!("{}{}{}有限公司", rng.pick(ZH_CITY), rng.pick(ZH_COMPANY_WORD), rng.pick(ZH_COMPANY_KIND))
            } else {
                format!("{} {}", rng.pick(EN_COMPANY_PREFIX), rng.pick(EN_COMPANY_SUFFIX))
            };
            text(s + &suffix(rng))
        }
        Generator::Url => {
            let host = rng.pick(EN_COMPANY_PREFIX).to_ascii_lowercase().replace(' ', "");
            text(format!("https://www.{host}.example/{}/{}", rng.pick(URL_PATHS), alnum(rng, 6)))
        }
        Generator::Ip => text(format!("{}.{}.{}.{}", 1 + rng.below(223), rng.below(256), rng.below(256), 1 + rng.below(254))),
        Generator::Uuid => {
            let mut b = rng.bytes(16);
            b[6] = (b[6] & 0x0f) | 0x40;
            b[8] = (b[8] & 0x3f) | 0x80;
            let h = hex(&b);
            let s = format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]);
            if matches!(ctx.base, BaseType::Bytes { .. }) {
                Value::Bytes(b)
            } else {
                text(s)
            }
        }
        Generator::Word => text((if zh { rng.pick(ZH_TITLE_WORDS).to_string() } else { rng.pick(EN_WORDS).to_string() }) + &suffix(rng)),
        Generator::Title => {
            let n = 2 + rng.below(if zh { 2 } else { 3 }) as usize;
            let s = if zh { words(rng, ZH_TITLE_WORDS, n, "") } else { words(rng, EN_TITLE_WORDS, n, " ") };
            text(s + &suffix(rng))
        }
        Generator::Sentence => text(sentence(rng, ctx.locale)),
        Generator::Paragraph => {
            let n = 3 + rng.below(3) as usize;
            let parts: Vec<String> = (0..n).map(|_| sentence(rng, ctx.locale)).collect();
            text(parts.join(if zh { "" } else { " " }))
        }
        Generator::Text { min_len, max_len } => {
            let (lo, hi) = (*min_len.min(max_len), *max_len.max(min_len));
            let n = rng.range_i64(lo as i64, hi.max(1) as i64) as usize;
            text(alnum(rng, n))
        }
        Generator::Integer { min, max } => Value::Int(rng.range_i64(*min, *max)),
        Generator::Sequence { start, step } => Value::Int(start.saturating_add(step.saturating_mul(ctx.row as i64))),
        Generator::Decimal { min, max, scale } => {
            let (lo, hi) = if min <= max { (*min, *max) } else { (*max, *min) };
            let v = lo + rng.f64() * (hi - lo);
            Value::Num(format_decimal(v, *scale))
        }
        Generator::Float { min, max } => {
            let (lo, hi) = if min <= max { (*min, *max) } else { (*max, *min) };
            Value::Float(((lo + rng.f64() * (hi - lo)) * 1e4).round() / 1e4)
        }
        Generator::Boolean => Value::Bool(rng.below(2) == 1),
        Generator::Date { from, to } => {
            let (a, b) = date_range(from, to);
            text(format_date(rng.range_i64(a, b)))
        }
        Generator::Timestamp { from, to } => {
            let (a, b) = date_range(from, to);
            let secs = rng.range_i64(a * 86_400, b * 86_400 + 86_399);
            let day = secs.div_euclid(86_400);
            let s = secs.rem_euclid(86_400);
            text(format!("{} {:02}:{:02}:{:02}", format_date(day), s / 3600, (s / 60) % 60, s % 60))
        }
        Generator::Time => {
            let s = rng.below(86_400);
            text(format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60))
        }
        Generator::Json => {
            let tags = ["new", "sale", "vip", "beta", "gift"];
            let v = serde_json::json!({
                "id": rng.below(100_000),
                "tag": rng.pick(&tags),
                "active": rng.below(2) == 1,
                "score": (rng.f64() * 1000.0).round() / 10.0,
            });
            text(v.to_string())
        }
        Generator::Bytes { len } => Value::Bytes(rng.bytes((*len).max(1) as usize)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(base: &BaseType, locale: Locale) -> Ctx<'_> {
        Ctx { locale, base, row: 0, attempt: 0 }
    }

    #[test]
    fn dates_round_trip() {
        for d in [-1000, 0, 1, 59, 60, 365, 11_000, 20_000, 30_000] {
            let (y, m, dd) = days_to_date(d);
            assert_eq!(date_to_days(y, m, dd), d);
        }
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(parse_date("2024-02-29 10:00:00"), Some(date_to_days(2024, 2, 29)));
        assert_eq!(parse_date("nope"), None);
        let (a, b) = date_range("2025-12-31", "2025-01-01");
        assert!(a < b);
    }

    #[test]
    fn values_have_expected_shape() {
        let text = BaseType::Text { max_len: None };
        let mut r = Rng::new(9);
        let email = generate(&Generator::Email, &mut r, &ctx(&text, Locale::En)).display().unwrap();
        assert!(email.contains('@') && email.is_ascii(), "{email}");
        let uuid = generate(&Generator::Uuid, &mut r, &ctx(&BaseType::Uuid, Locale::En)).display().unwrap();
        assert_eq!(uuid.len(), 36);
        assert_eq!(&uuid[14..15], "4");
        let ts = generate(&Generator::Timestamp { from: "2024-01-01".into(), to: "2024-01-31".into() }, &mut r, &ctx(&BaseType::Timestamp, Locale::En))
            .display()
            .unwrap();
        assert!(ts.starts_with("2024-01-") && ts.len() == 19, "{ts}");
        let d = generate(&Generator::Decimal { min: 1.0, max: 2.0, scale: 2 }, &mut r, &ctx(&text, Locale::En));
        let Value::Num(s) = d else { panic!() };
        assert_eq!(s.split('.').nth(1).unwrap().len(), 2);
        let j = generate(&Generator::Json, &mut r, &ctx(&BaseType::Json, Locale::En)).display().unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&j).unwrap().is_object());
        let ip = generate(&Generator::Ip, &mut r, &ctx(&BaseType::Inet, Locale::En)).display().unwrap();
        assert_eq!(ip.split('.').count(), 4);
    }

    #[test]
    fn chinese_locale_names_cities_phones() {
        let text = BaseType::Text { max_len: None };
        let mut r = Rng::new(3);
        for _ in 0..50 {
            let name = generate(&Generator::FullName, &mut r, &ctx(&text, Locale::Zh)).display().unwrap();
            assert!(name.chars().all(|c| !c.is_ascii()), "{name}");
            let city = generate(&Generator::City, &mut r, &ctx(&text, Locale::Zh)).display().unwrap();
            assert!(ZH_CITY.contains(&city.as_str()));
            let phone = generate(&Generator::Phone, &mut r, &ctx(&text, Locale::Zh)).display().unwrap();
            assert_eq!(phone.len(), 11);
            assert!(phone.starts_with('1') && phone.bytes().all(|b| b.is_ascii_digit()));
            let pc = generate(&Generator::PostalCode, &mut r, &ctx(&text, Locale::Zh)).display().unwrap();
            assert_eq!(pc.len(), 6);
        }
        let s = generate(&Generator::Sentence, &mut r, &ctx(&text, Locale::Zh)).display().unwrap();
        assert!(s.ends_with('。'));
    }

    #[test]
    fn fits_rejects_mismatched_types() {
        assert!(Generator::Email.fits(&BaseType::Text { max_len: Some(10) }));
        assert!(!Generator::Email.fits(&BaseType::Int { min: 0, max: 1 }));
        assert!(!Generator::Bytes { len: 4 }.fits(&BaseType::Text { max_len: None }));
        assert!(Generator::Integer { min: 0, max: 1 }.fits(&BaseType::Decimal { precision: None, scale: 2 }));
        assert!(Generator::Date { from: String::new(), to: String::new() }.fits(&BaseType::Timestamp));
        assert!(!Generator::Timestamp { from: String::new(), to: String::new() }.fits(&BaseType::Date));
    }
}
