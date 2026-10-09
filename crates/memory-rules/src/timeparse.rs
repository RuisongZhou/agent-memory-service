//! Rule-based parsing of time expressions in queries (3.4, 5.3 step 1) and history-intent detection.
//! No model calls. Pure functions over `chrono` + `regex` only, so the file can be unit-tested on its own.
//!
//! All windows are computed on the *local* calendar of the reference clock (`asOf` or now, with its UTC
//! offset), half-open `[start, end)`, and returned in UTC. Weeks start on Monday. When a query contains several
//! expressions the window is their hull ("between May and June" → May 1 .. July 1).
//!
//! Supported (zh): 今天/今日/今晚/今早, 昨天/昨日/昨晚, 前天, 大前天, 刚才/刚刚 (last hour), 上周/上星期/上礼拜,
//! 上上周, 这周/本周/这个星期, 下周, 上个月/上月, 上上个月, 这个月/本月, 下个月, 今年, 去年, 前年, 明年,
//! N天前, N周前/N个星期前, N个月前, N年前, 几天前/前几天, 最近N天/近N天/过去N天 (also 周/个月), X月份, X月,
//! X月X日/号, YYYY年X月[X日], YYYY年, 最近/近期/近来 (14 days). Numbers may be digits or 一二两三…十.
//! Supported (en): today/tonight/this morning…, yesterday, the day before yesterday, last night, this/last/next
//! week, the week before last, this/last weekend, this/last/next month, this/last year, N days/weeks/months/years
//! ago (N = digits, a/an/one…twelve, a few/several/a couple of), last/past N days/weeks/months/years, last
//! <weekday>, in/during/since/of/early/mid/late <Month>, <Month> YYYY, [on] <Month> <day>[, YYYY],
//! <day> [of] <Month>[, YYYY], in/during/of/since YYYY, recently/lately (14 days).
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday,
};
use regex::Regex;
use std::sync::OnceLock;

/// "recently" / "最近" without a number: the last 14 days (including today).
pub const RECENT_DAYS: i64 = 14;

#[derive(Debug, Clone, PartialEq)]
pub struct TimeWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// UTC offset (seconds east) of the reference clock; date-only values are read on this calendar.
    pub offset_secs: i32,
    /// The expressions that produced the window (for the retrieval log).
    pub matched: Vec<String>,
}

impl TimeWindow {
    /// Does `[a, b)` overlap the window?
    pub fn overlaps(&self, a: DateTime<Utc>, b: DateTime<Utc>) -> bool {
        a < self.end && b > self.start
    }
    pub fn contains(&self, t: DateTime<Utc>) -> bool {
        t >= self.start && t < self.end
    }
    pub fn days(&self) -> f64 {
        (self.end - self.start).num_seconds() as f64 / 86_400.0
    }
}

/// Interval covered by a stored time value: a bare date `YYYY-MM-DD` is the whole local day (on the calendar
/// given by `offset_secs`); an RFC3339 instant is a 1-second interval.
pub fn value_interval(s: &str, offset_secs: i32) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        let t = t.with_timezone(&Utc);
        return Some((t, t + Duration::seconds(1)));
    }
    if s.len() != 10 {
        return None;
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let off = FixedOffset::east_opt(offset_secs).unwrap_or(FixedOffset::east_opt(0).unwrap());
    let a = to_utc(&off, d.and_hms_opt(0, 0, 0)?);
    Some((a, a + Duration::days(1)))
}

// ---------------------------------------------------------------- history intent

const ZH_HISTORY: &[&str] = &["以前", "原来", "改之前", "当时", "之前", "曾经"];

/// 历史意图 (5.3 step 1): the question asks about an earlier value.
pub fn history_intent(query: &str) -> bool {
    if ZH_HISTORY.iter().any(|w| query.contains(w)) {
        return true;
    }
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"\b(before|previously|used\s+to|originally|at\s+th(?:e|at)\s+time|formerly|earlier|back\s+then|in\s+the\s+past|prior\s+to)\b",
        )
        .unwrap()
    });
    re.is_match(&query.to_lowercase())
}

// ---------------------------------------------------------------- calendar helpers

fn to_utc(off: &FixedOffset, ndt: NaiveDateTime) -> DateTime<Utc> {
    off.from_local_datetime(&ndt)
        .single()
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| {
            // FixedOffset is never ambiguous; keep a total fallback anyway
            Utc.from_utc_datetime(&ndt)
        })
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).unwrap()
}

type Win = (NaiveDateTime, NaiveDateTime);

fn day_win(d: NaiveDate) -> Win {
    (midnight(d), midnight(d) + Duration::days(1))
}

fn days_win(today: NaiveDate, delta: i64) -> Win {
    day_win(today + Duration::days(delta))
}

fn monday_of(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

fn week_win(today: NaiveDate, delta: i64) -> Win {
    let start = monday_of(today) + Duration::days(7 * delta);
    (midnight(start), midnight(start + Duration::days(7)))
}

fn shift_month(y: i32, m: u32, delta: i64) -> (i32, u32) {
    let idx = y as i64 * 12 + (m as i64 - 1) + delta;
    (idx.div_euclid(12) as i32, (idx.rem_euclid(12) + 1) as u32)
}

fn month_win(y: i32, m: u32) -> Option<Win> {
    let start = NaiveDate::from_ymd_opt(y, m, 1)?;
    let (ny, nm) = shift_month(y, m, 1);
    let end = NaiveDate::from_ymd_opt(ny, nm, 1)?;
    Some((midnight(start), midnight(end)))
}

fn month_rel(today: NaiveDate, delta: i64) -> Option<Win> {
    let (y, m) = shift_month(today.year(), today.month(), delta);
    month_win(y, m)
}

fn year_win(y: i32) -> Option<Win> {
    Some((
        midnight(NaiveDate::from_ymd_opt(y, 1, 1)?),
        midnight(NaiveDate::from_ymd_opt(y + 1, 1, 1)?),
    ))
}

/// Year for a month (and optional day) given without a year: the current year unless that month lies in the
/// future, in which case the previous year (questions to a memory store are mostly about the past).
fn infer_year(today: NaiveDate, month: u32) -> i32 {
    if month > today.month() {
        today.year() - 1
    } else {
        today.year()
    }
}

/// Last `n` units ending today (inclusive): [today - n units + 1 day, tomorrow).
fn last_n_win(today: NaiveDate, n: i64, unit: &str) -> Option<Win> {
    let end = midnight(today + Duration::days(1));
    let start_date = match unit {
        "day" => today - Duration::days(n - 1),
        "week" => today - Duration::days(7 * n - 1),
        "month" => {
            let (y, m) = shift_month(today.year(), today.month(), -n);
            let d = today.day().min(days_in_month(y, m));
            NaiveDate::from_ymd_opt(y, m, d)? + Duration::days(1)
        }
        "year" => {
            NaiveDate::from_ymd_opt(today.year() - n as i32, today.month(), today.day().min(28))?
                + Duration::days(1)
        }
        _ => return None,
    };
    Some((midnight(start_date), end))
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = shift_month(y, m, 1);
    let a = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    let b = NaiveDate::from_ymd_opt(ny, nm, 1).unwrap();
    (b - a).num_days() as u32
}

/// Window for "N units ago": the unit containing today − N units.
fn ago_win(today: NaiveDate, n: i64, unit: &str) -> Option<Win> {
    match unit {
        "day" => Some(days_win(today, -n)),
        "week" => Some(week_win(today, -n)),
        "month" => month_rel(today, -n),
        "year" => year_win(today.year() - n as i32),
        _ => None,
    }
}

fn hull(a: Win, b: Win) -> Win {
    (a.0.min(b.0), a.1.max(b.1))
}

// ---------------------------------------------------------------- numbers

/// Digits or Chinese numerals up to 99 (一 二 两 三 … 十, 十二, 二十三).
pub fn cn_num(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return s.parse().ok();
    }
    fn digit(c: char) -> Option<i64> {
        Some(match c {
            '零' | '〇' => 0,
            '一' => 1,
            '二' | '两' => 2,
            '三' => 3,
            '四' => 4,
            '五' => 5,
            '六' => 6,
            '七' => 7,
            '八' => 8,
            '九' => 9,
            _ => return None,
        })
    }
    let chars: Vec<char> = s.chars().collect();
    if let Some(p) = chars.iter().position(|&c| c == '十') {
        let tens = if p == 0 {
            1
        } else if p == 1 {
            digit(chars[0])?
        } else {
            return None;
        };
        let ones = match chars.len() - p - 1 {
            0 => 0,
            1 => digit(chars[p + 1])?,
            _ => return None,
        };
        return Some(tens * 10 + ones);
    }
    if chars.len() == 1 {
        return digit(chars[0]);
    }
    None
}

fn en_num(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        // Ignore implausibly large relative counts before date arithmetic can overflow/panic.
        return (0..=10_000).contains(&n).then_some(n);
    }
    Some(match s {
        "a" | "an" | "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        "eleven" => 11,
        "twelve" => 12,
        _ => return None,
    })
}

/// Fuzzy quantities ("a few", "several", "a couple of") as an inclusive range of N.
fn en_fuzzy(s: &str) -> Option<(i64, i64)> {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.as_str() {
        "a couple of" | "couple of" | "a couple" => Some((2, 3)),
        "a few" | "few" => Some((2, 5)),
        "several" => Some((3, 7)),
        _ => None,
    }
}

fn month_num(name: &str) -> Option<u32> {
    let n = name.trim_end_matches('.');
    Some(match n {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

fn weekday_of(name: &str) -> Option<Weekday> {
    Some(match name {
        "monday" => Weekday::Mon,
        "tuesday" => Weekday::Tue,
        "wednesday" => Weekday::Wed,
        "thursday" => Weekday::Thu,
        "friday" => Weekday::Fri,
        "saturday" => Weekday::Sat,
        "sunday" => Weekday::Sun,
        _ => return None,
    })
}

fn valid_year(y: i64) -> bool {
    (1900..=2100).contains(&y)
}

// ---------------------------------------------------------------- rules

const CN: &str = "[0-9一二两三四五六七八九十]{1,3}";
const MONTHS: &str = "january|february|march|april|may|june|july|august|september|october|november|december|jan|feb|mar|apr|jun|jul|aug|sept|sep|oct|nov|dec";
const MONTHS_FULL: &str =
    "january|february|march|april|may|june|july|august|september|october|november|december";
const EN_N: &str = r"\d+|an|a|one|two|three|four|five|six|seven|eight|nine|ten|eleven|twelve|a\s+couple\s+of|a\s+few|few|several";

/// What a rule yields for one regex match.
enum Out {
    Local(Win),
    Abs(DateTime<Utc>, DateTime<Utc>),
}

struct Ctx<'a> {
    now: &'a DateTime<FixedOffset>,
    today: NaiveDate,
}

type Handler = fn(&Ctx, &regex::Captures, &str, usize) -> Option<Out>;

struct Rule {
    re: Regex,
    f: Handler,
}

fn rules() -> &'static Vec<Rule> {
    static R: OnceLock<Vec<Rule>> = OnceLock::new();
    R.get_or_init(|| {
        let mut v: Vec<Rule> = Vec::new();
        let mut add = |pat: String, f: Handler| v.push(Rule { re: Regex::new(&pat).unwrap(), f });

        // ---------------- zh: absolute dates (most specific first)
        add(format!(r"(\d{{4}})年({CN})月({CN})[日号]"), |_, c, _, _| {
            let y = c[1].parse::<i32>().ok()?;
            let d = NaiveDate::from_ymd_opt(y, cn_num(&c[2])? as u32, cn_num(&c[3])? as u32)?;
            Some(Out::Local(day_win(d)))
        });
        add(format!(r"(\d{{4}})年({CN})月份?"), |_, c, _, _| {
            Some(Out::Local(month_win(c[1].parse().ok()?, cn_num(&c[2])? as u32)?))
        });
        add(format!(r"({CN})月({CN})[日号]"), |x, c, s, at| {
            if preceded_by_any(s, at, &['个', '几', '多']) {
                return None;
            }
            let m = cn_num(&c[1])? as u32;
            let d = NaiveDate::from_ymd_opt(infer_year(x.today, m), m, cn_num(&c[2])? as u32)?;
            Some(Out::Local(day_win(d)))
        });
        // ---------------- zh: relative counts (before the bare-word rules)
        add(format!(r"(?:最近|近|过去|这)({CN})天"), |x, c, _, _| last_n_win(x.today, cn_num(&c[1])?.max(1), "day").map(Out::Local));
        add(format!(r"(?:最近|近|过去|这)({CN})个?(?:周|星期|礼拜)"), |x, c, _, _| {
            last_n_win(x.today, cn_num(&c[1])?.max(1), "week").map(Out::Local)
        });
        add(format!(r"(?:最近|近|过去|这)({CN})个月"), |x, c, _, _| last_n_win(x.today, cn_num(&c[1])?.max(1), "month").map(Out::Local));
        add(format!(r"(?:最近|近|过去)({CN})年"), |x, c, _, _| last_n_win(x.today, cn_num(&c[1])?.max(1), "year").map(Out::Local));
        add(format!(r"({CN})天(?:以|之)?前"), |x, c, _, _| ago_win(x.today, cn_num(&c[1])?, "day").map(Out::Local));
        add(format!(r"({CN})个?(?:周|星期|礼拜)(?:以|之)?前"), |x, c, _, _| ago_win(x.today, cn_num(&c[1])?, "week").map(Out::Local));
        add(format!(r"({CN})个月(?:以|之)?前"), |x, c, _, _| ago_win(x.today, cn_num(&c[1])?, "month").map(Out::Local));
        add(format!(r"({CN})年(?:以|之)?前"), |x, c, _, _| {
            let n = cn_num(&c[1])?;
            if n > 200 {
                return None; // a year number, not a count
            }
            ago_win(x.today, n, "year").map(Out::Local)
        });
        add(r"几天(?:以|之)?前|前几天".into(), |x, _, _, _| Some(Out::Local(hull(days_win(x.today, -5), days_win(x.today, -2)))));
        // ---------------- zh: months
        add(format!(r"({CN})月份"), |x, c, s, at| {
            if preceded_by_any(s, at, &['个', '几', '多']) {
                return None;
            }
            let m = cn_num(&c[1])? as u32;
            Some(Out::Local(month_win(infer_year(x.today, m), m)?))
        });
        // ---------------- zh: bare relative words
        add(r"大前天".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, -3))));
        add(r"前天".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, -2))));
        add(r"昨天|昨日|昨晚|昨夜".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, -1))));
        add(r"今天|今日|今晚|今早|今夜".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, 0))));
        add(r"刚才|刚刚".into(), |x, _, _, _| {
            let n = x.now.with_timezone(&Utc);
            Some(Out::Abs(n - Duration::hours(1), n + Duration::seconds(1)))
        });
        add(r"上上(?:周|个?星期|个?礼拜)".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, -2))));
        add(r"上(?:周|个?星期|个?礼拜)".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, -1))));
        add(r"这(?:周|个?星期|个?礼拜)|本(?:周|星期)".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, 0))));
        add(r"下(?:周|个?星期|个?礼拜)".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, 1))));
        add(r"上上个?月".into(), |x, _, _, _| month_rel(x.today, -2).map(Out::Local));
        add(r"上个?月".into(), |x, _, _, _| month_rel(x.today, -1).map(Out::Local));
        add(r"这个?月|本月".into(), |x, _, _, _| month_rel(x.today, 0).map(Out::Local));
        add(r"下个?月".into(), |x, _, _, _| month_rel(x.today, 1).map(Out::Local));
        add(r"今年".into(), |x, _, _, _| year_win(x.today.year()).map(Out::Local));
        add(r"去年".into(), |x, _, _, _| year_win(x.today.year() - 1).map(Out::Local));
        add(r"前年".into(), |x, _, _, _| year_win(x.today.year() - 2).map(Out::Local));
        add(r"明年".into(), |x, _, _, _| year_win(x.today.year() + 1).map(Out::Local));
        // bare "X月" (after 上个月/这个月 … have been consumed)
        add(format!(r"({CN})月"), |x, c, s, at| {
            if preceded_by_any(s, at, &['个', '几', '多', '每', '上', '下', '本', '这', '那']) {
                return None;
            }
            let m = cn_num(&c[1])?;
            if !(1..=12).contains(&m) {
                return None;
            }
            let m = m as u32;
            Some(Out::Local(month_win(infer_year(x.today, m), m)?))
        });
        add(r"(\d{4})年".into(), |_, c, _, _| {
            let y = c[1].parse::<i64>().ok()?;
            if !valid_year(y) {
                return None;
            }
            year_win(y as i32).map(Out::Local)
        });
        add(r"最近|近期|近来".into(), |x, _, _, _| Some(Out::Local((midnight(x.today - Duration::days(RECENT_DAYS - 1)), midnight(x.today + Duration::days(1))))));

        // ---------------- en: absolute dates
        // Parse month ranges before individual dates, so a trailing year applies to both ends.
        add(format!(r"\b(?:between|from)\s+({MONTHS})\.?(?:\s+(\d{{4}}))?\s+(?:and|to|through)\s+({MONTHS})\.?\b(?:,?\s+(\d{{4}})\b)?"), |x,c,_,_| {
            let m1=month_num(&c[1])?;let m2=month_num(&c[3])?;
            let first=c.get(2).map(|v|v.as_str().parse::<i32>()).transpose().ok()?;
            let last=c.get(4).map(|v|v.as_str().parse::<i32>()).transpose().ok()?;
            let y2=last.unwrap_or_else(||first.map(|y|y+i32::from(m2<m1)).unwrap_or_else(||infer_year(x.today,m2)));
            let y1=first.unwrap_or(y2-i32::from(m1>m2));
            if !valid_year(y1 as i64)||!valid_year(y2 as i64){return None;}
            let s=month_win(y1,m1)?.0;let e=month_win(y2,m2)?.1;
            if s>=e{return None;}Some(Out::Local((s,e)))
        });
        add(format!(r"\b({MONTHS})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?\b(?:,?\s*(\d{{4}})\b)?"), |x, c, _, _| {
            let m = month_num(&c[1])?;
            let day = c[2].parse::<u32>().ok()?;
            let y = match c.get(3) {
                Some(y) => y.as_str().parse::<i32>().ok()?,
                None => infer_year(x.today, m),
            };
            Some(Out::Local(day_win(NaiveDate::from_ymd_opt(y, m, day)?)))
        });
        add(format!(r"\b(\d{{1,2}})(?:st|nd|rd|th)?\s+(?:of\s+)?({MONTHS})\b\.?(?:,?\s*(\d{{4}})\b)?"), |x, c, _, _| {
            let m = month_num(&c[2])?;
            let day = c[1].parse::<u32>().ok()?;
            let y = match c.get(3) {
                Some(y) => y.as_str().parse::<i32>().ok()?,
                None => infer_year(x.today, m),
            };
            Some(Out::Local(day_win(NaiveDate::from_ymd_opt(y, m, day)?)))
        });
        add(format!(r"\b({MONTHS})\.?,?\s+(?:of\s+)?(\d{{4}})\b"), |_, c, _, _| {
            let y = c[2].parse::<i64>().ok()?;
            if !valid_year(y) {
                return None;
            }
            Some(Out::Local(month_win(y as i32, month_num(&c[1])?)?))
        });
        add(
            format!(r"\b(?:in|during|since|of|by|until|till|from|through|throughout|early|late|mid|end\s+of|beginning\s+of)[\s-]+(?:the\s+)?(?:month\s+of\s+)?({MONTHS_FULL})\b"),
            |x, c, _, _| {
                let m = month_num(&c[1])?;
                Some(Out::Local(month_win(infer_year(x.today, m), m)?))
            },
        );
        add(r"\b(?:in|during|of|since|throughout)\s+(?:the\s+year\s+)?(\d{4})\b".into(), |_, c, _, _| {
            let y = c[1].parse::<i64>().ok()?;
            if !valid_year(y) {
                return None;
            }
            year_win(y as i32).map(Out::Local)
        });
        // ---------------- en: counts
        add(
            format!(r"\b(?:(?:in|over|during|within)\s+)?(?:the\s+)?(?:last|past)\s+({EN_N})\s+(day|week|month|year)s?\b"),
            |x, c, _, _| {
                let unit = &c[2];
                if let Some((_, hi)) = en_fuzzy(&c[1]) {
                    return last_n_win(x.today, hi, unit).map(Out::Local);
                }
                last_n_win(x.today, en_num(&c[1])?.max(1), unit).map(Out::Local)
            },
        );
        add(format!(r"\b({EN_N})\s+(day|week|month|year)s?\s+ago\b"), |x, c, _, _| {
            let unit = &c[2];
            if let Some((lo, hi)) = en_fuzzy(&c[1]) {
                return Some(Out::Local(hull(ago_win(x.today, hi, unit)?, ago_win(x.today, lo, unit)?)));
            }
            ago_win(x.today, en_num(&c[1])?, unit).map(Out::Local)
        });
        // ---------------- en: relative words
        add(r"\bthe\s+day\s+before\s+yesterday\b".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, -2))));
        add(r"\byesterday\b".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, -1))));
        add(r"\blast\s+night\b".into(), |x, _, _, _| {
            let t = midnight(x.today);
            Some(Out::Local((t - Duration::hours(6), t + Duration::hours(6))))
        });
        add(r"\b(?:today|tonight|this\s+morning|this\s+afternoon|this\s+evening)\b".into(), |x, _, _, _| Some(Out::Local(days_win(x.today, 0))));
        add(r"\bthe\s+week\s+before\s+last\b".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, -2))));
        add(r"\blast\s+weekend\b".into(), |x, _, _, _| {
            let mon = midnight(monday_of(x.today));
            Some(Out::Local((mon - Duration::days(2), mon)))
        });
        add(r"\bthis\s+weekend\b".into(), |x, _, _, _| {
            let mon = midnight(monday_of(x.today));
            Some(Out::Local((mon + Duration::days(5), mon + Duration::days(7))))
        });
        add(r"\blast\s+week\b".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, -1))));
        add(r"\bthis\s+week\b".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, 0))));
        add(r"\bnext\s+week\b".into(), |x, _, _, _| Some(Out::Local(week_win(x.today, 1))));
        add(r"\blast\s+month\b".into(), |x, _, _, _| month_rel(x.today, -1).map(Out::Local));
        add(r"\bthis\s+month\b".into(), |x, _, _, _| month_rel(x.today, 0).map(Out::Local));
        add(r"\bnext\s+month\b".into(), |x, _, _, _| month_rel(x.today, 1).map(Out::Local));
        add(r"\blast\s+year\b".into(), |x, _, _, _| year_win(x.today.year() - 1).map(Out::Local));
        add(r"\bthis\s+year\b".into(), |x, _, _, _| year_win(x.today.year()).map(Out::Local));
        add(r"\blast\s+(monday|tuesday|wednesday|thursday|friday|saturday|sunday)\b".into(), |x, c, _, _| {
            let wd = weekday_of(&c[1])?;
            let mut d = x.today - Duration::days(1);
            while d.weekday() != wd {
                d -= Duration::days(1);
            }
            Some(Out::Local(day_win(d)))
        });
        add(r"\b(?:recently|lately)\b".into(), |x, _, _, _| {
            Some(Out::Local((midnight(x.today - Duration::days(RECENT_DAYS - 1)), midnight(x.today + Duration::days(1)))))
        });
        v
    })
}

/// Is the character right before byte offset `at` one of `chars` (or an ASCII digit)?
fn preceded_by_any(s: &str, at: usize, chars: &[char]) -> bool {
    match s[..at].chars().next_back() {
        Some(c) => chars.contains(&c) || c.is_ascii_digit(),
        None => false,
    }
}

/// Parse the time window of a query relative to `now` (the caller's clock, with its UTC offset).
pub fn parse(query: &str, now: DateTime<FixedOffset>) -> Option<TimeWindow> {
    let ctx = Ctx {
        now: &now,
        today: now.date_naive(),
    };
    let off = *now.offset();
    let mut work = query.to_lowercase();
    let mut out: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut matched = Vec::new();
    for rule in rules() {
        let mut hits: Vec<(usize, usize, Out)> = Vec::new();
        for c in rule.re.captures_iter(&work) {
            let m = c.get(0).unwrap();
            if let Some(o) = (rule.f)(&ctx, &c, &work, m.start()) {
                hits.push((m.start(), m.end(), o));
            }
        }
        for (a, b, o) in hits {
            matched.push(work[a..b].trim().to_string());
            let (s, e) = match o {
                Out::Local((s, e)) => (to_utc(&off, s), to_utc(&off, e)),
                Out::Abs(s, e) => (s, e),
            };
            out = Some(match out {
                Some((os, oe)) => (os.min(s), oe.max(e)),
                None => (s, e),
            });
            // consume the span so less specific rules do not match inside it
            work.replace_range(a..b, &" ".repeat(b - a));
        }
    }
    out.map(|(start, end)| TimeWindow {
        start,
        end,
        offset_secs: off.local_minus_utc(),
        matched,
    })
}

/// Start of the reference day (local calendar of `now`) in UTC — "today" for the history-intent rule.
pub fn start_of_today(now: DateTime<FixedOffset>) -> DateTime<Utc> {
    to_utc(now.offset(), midnight(now.date_naive()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<FixedOffset> {
        // Thursday
        DateTime::parse_from_rfc3339("2026-10-08T10:00:00+08:00").unwrap()
    }
    fn w(q: &str) -> (String, String) {
        let t = parse(q, now()).unwrap_or_else(|| panic!("no window for {q}"));
        let off = FixedOffset::east_opt(8 * 3600).unwrap();
        (
            t.start
                .with_timezone(&off)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            t.end
                .with_timezone(&off)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        )
    }
    fn d(a: &str, b: &str) -> (String, String) {
        (format!("{a} 00:00"), format!("{b} 00:00"))
    }

    #[test]
    fn zh_relative() {
        assert_eq!(w("今天做了什么"), d("2026-10-08", "2026-10-09"));
        assert_eq!(w("昨天说的端口"), d("2026-10-07", "2026-10-08"));
        assert_eq!(w("前天"), d("2026-10-06", "2026-10-07"));
        assert_eq!(w("大前天"), d("2026-10-05", "2026-10-06"));
        assert_eq!(w("上周的会议"), d("2026-09-28", "2026-10-05"));
        assert_eq!(w("这周"), d("2026-10-05", "2026-10-12"));
        assert_eq!(w("本周"), d("2026-10-05", "2026-10-12"));
        assert_eq!(w("上个月"), d("2026-09-01", "2026-10-01"));
        assert_eq!(w("这个月"), d("2026-10-01", "2026-11-01"));
        assert_eq!(w("本月"), d("2026-10-01", "2026-11-01"));
        assert_eq!(w("今年"), d("2026-01-01", "2027-01-01"));
        assert_eq!(w("去年的项目"), d("2025-01-01", "2026-01-01"));
        assert_eq!(w("3天前"), d("2026-10-05", "2026-10-06"));
        assert_eq!(w("三天前"), d("2026-10-05", "2026-10-06"));
        assert_eq!(w("两天以前"), d("2026-10-06", "2026-10-07"));
        assert_eq!(w("最近7天"), d("2026-10-02", "2026-10-09"));
        assert_eq!(w("最近三天"), d("2026-10-06", "2026-10-09"));
        assert_eq!(w("最近"), d("2026-09-25", "2026-10-09"));
        let (a, b) = w("刚才说了什么");
        assert_eq!(
            (a.as_str(), b.as_str()),
            ("2026-10-08 09:00", "2026-10-08 10:00")
        );
    }

    #[test]
    fn zh_absolute() {
        assert_eq!(w("三月份的端口"), d("2026-03-01", "2026-04-01"));
        assert_eq!(w("3月份"), d("2026-03-01", "2026-04-01"));
        assert_eq!(w("十二月份"), d("2025-12-01", "2026-01-01"));
        assert_eq!(w("9月28日"), d("2026-09-28", "2026-09-29"));
        assert_eq!(w("9月28号的变更"), d("2026-09-28", "2026-09-29"));
        assert_eq!(w("2025年3月"), d("2025-03-01", "2025-04-01"));
        assert_eq!(w("2025年3月5日"), d("2025-03-05", "2025-03-06"));
        assert_eq!(w("2024年"), d("2024-01-01", "2025-01-01"));
        assert_eq!(w("十月"), d("2026-10-01", "2026-11-01"));
        assert!(parse("三个月的计划", now()).is_none());
        assert!(parse("每个月都要备份", now()).is_none());
        assert!(parse("端口是多少", now()).is_none());
    }

    #[test]
    fn en_relative() {
        assert_eq!(w("what did I do today"), d("2026-10-08", "2026-10-09"));
        assert_eq!(w("Yesterday"), d("2026-10-07", "2026-10-08"));
        assert_eq!(w("the day before yesterday"), d("2026-10-06", "2026-10-07"));
        assert_eq!(
            w("last night"),
            ("2026-10-07 18:00".into(), "2026-10-08 06:00".into())
        );
        assert_eq!(w("this week"), d("2026-10-05", "2026-10-12"));
        assert_eq!(w("last week"), d("2026-09-28", "2026-10-05"));
        assert_eq!(w("last weekend"), d("2026-10-03", "2026-10-05"));
        assert_eq!(w("this month"), d("2026-10-01", "2026-11-01"));
        assert_eq!(w("last month"), d("2026-09-01", "2026-10-01"));
        assert_eq!(w("this year"), d("2026-01-01", "2027-01-01"));
        assert_eq!(w("last year"), d("2025-01-01", "2026-01-01"));
        assert_eq!(w("3 days ago"), d("2026-10-05", "2026-10-06"));
        assert_eq!(w("two weeks ago"), d("2026-09-21", "2026-09-28"));
        assert_eq!(w("a month ago"), d("2026-09-01", "2026-10-01"));
        assert_eq!(w("2 months ago"), d("2026-08-01", "2026-09-01"));
        assert_eq!(w("a few days ago"), d("2026-10-03", "2026-10-07"));
        assert_eq!(w("in the last 7 days"), d("2026-10-02", "2026-10-09"));
        assert_eq!(w("past two weeks"), d("2026-09-25", "2026-10-09"));
        assert_eq!(w("recently"), d("2026-09-25", "2026-10-09"));
        assert_eq!(w("last Friday"), d("2026-10-02", "2026-10-03"));
        assert_eq!(w("last thursday"), d("2026-10-01", "2026-10-02"));
    }

    #[test]
    fn en_absolute() {
        assert_eq!(
            w("What did Caroline do in May 2023?"),
            d("2023-05-01", "2023-06-01")
        );
        assert_eq!(w("in May"), d("2026-05-01", "2026-06-01"));
        assert_eq!(w("in December"), d("2025-12-01", "2026-01-01"));
        assert_eq!(w("on May 8, 2023"), d("2023-05-08", "2023-05-09"));
        assert_eq!(w("on July 4th"), d("2026-07-04", "2026-07-05"));
        assert_eq!(w("8 May, 2023"), d("2023-05-08", "2023-05-09"));
        assert_eq!(w("the 8th of May 2023"), d("2023-05-08", "2023-05-09"));
        assert_eq!(w("in 2022"), d("2022-01-01", "2023-01-01"));
        assert_eq!(
            w("between May and June 2023"),
            d("2023-05-01", "2023-07-01")
        );
        assert!(parse("I may go", now()).is_none());
        assert!(parse("What did June say about the trip?", now()).is_none());
        assert!(parse("When did Caroline go to the support group?", now()).is_none());
        assert_eq!(
            w("from December to January 2023"),
            d("2022-12-01", "2023-02-01")
        );
        assert_eq!(
            w("between May 2022 and June 2023"),
            d("2022-05-01", "2023-07-01")
        );
        assert_eq!(
            w("from December 2022 through January"),
            d("2022-12-01", "2023-02-01")
        );
    }

    #[test]
    fn hull_and_today() {
        assert_eq!(w("昨天和今天"), d("2026-10-07", "2026-10-09"));
        let t = parse("上周", now()).unwrap();
        assert!(t.end <= start_of_today(now()));
        let t = parse("今天", now()).unwrap();
        assert!(t.end > start_of_today(now()));
    }

    #[test]
    fn history() {
        assert!(history_intent("以前的端口是多少"));
        assert!(history_intent("改之前用的什么"));
        assert!(history_intent("What did she use to do before?"));
        assert!(history_intent("what was it originally"));
        assert!(history_intent("Where did he live back then"));
        assert!(!history_intent("端口是多少"));
        assert!(!history_intent("What is Caroline's identity?"));
        assert!(!history_intent("beforehand"));
        assert!(history_intent("What did Caroline do at that time?"));
        assert!(history_intent("Where did Melanie previously work?"));
        assert!(history_intent("What instrument did he used to play?"));
        assert!(history_intent("当时用的哪个端口"));
        assert!(history_intent("原来的对接人是谁"));
        assert!(!history_intent("What does Caroline like?"));
    }

    #[test]
    fn en_eval_phrases() {
        // phrases that occur in the (English) evaluation questions
        assert_eq!(
            w("What did they do last Saturday?"),
            d("2026-10-03", "2026-10-04")
        );
        assert_eq!(
            w("What did Caroline do two weeks ago?"),
            d("2026-09-21", "2026-09-28")
        );
        assert_eq!(
            w("What happened in May 2023?"),
            d("2023-05-01", "2023-06-01")
        );
        assert_eq!(
            w("What did Melanie paint on 8 May 2023?"),
            d("2023-05-08", "2023-05-09")
        );
        assert_eq!(
            w("What did she buy yesterday?"),
            d("2026-10-07", "2026-10-08")
        );
        assert_eq!(
            w("Where did he travel last year?"),
            d("2025-01-01", "2026-01-01")
        );
        assert_eq!(
            w("What has Caroline been reading recently?"),
            d("2026-09-25", "2026-10-09")
        );
        // windows before today mark history intent in search_memories
        assert!(parse("in May 2023", now()).unwrap().end <= start_of_today(now()));
    }

    #[test]
    fn numbers() {
        assert_eq!(cn_num("十"), Some(10));
        assert_eq!(cn_num("十二"), Some(12));
        assert_eq!(cn_num("二十三"), Some(23));
        assert_eq!(cn_num("两"), Some(2));
        assert_eq!(cn_num("7"), Some(7));
        assert_eq!(cn_num("个"), None);
    }

    #[test]
    fn intervals() {
        let (a, b) = value_interval("2026-10-07", 8 * 3600).unwrap();
        assert_eq!(a.to_rfc3339(), "2026-10-06T16:00:00+00:00");
        assert_eq!((b - a).num_hours(), 24);
        let (a, _) = value_interval("2026-10-07T12:00:00+08:00", 0).unwrap();
        assert_eq!(a.to_rfc3339(), "2026-10-07T04:00:00+00:00");
        assert!(value_interval("", 0).is_none());
        assert!(value_interval("2026-10-07invalid", 0).is_none());
    }

    #[test]
    fn huge_relative_counts_are_ignored() {
        assert!(parse("9223372036854775807 days ago", now()).is_none());
        assert!(parse("last 9223372036854775807 weeks", now()).is_none());
    }
}
