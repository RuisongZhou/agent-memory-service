//! Credential redaction before import text leaves the machine (design §6.12): matches become
//! `[redacted:<kind>]` and only per-kind counts are reported, never the matched text. Key formats follow
//! `scripts/check_public.py` and the providers' documented prefixes. Where a rule cannot tell, it redacts a
//! harmless value rather than send a secret: `env-secret` matches names by substring, so `primary_key: id` loses
//! its value too. Only values that cannot be credentials are kept: placeholders (`$VAR`, `<token>`, `***`) and,
//! unless the name says password, numbers of fewer than eight digits (`max_tokens: 400`).
use regex::{Captures, Regex};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// Start of every replacement, and of the `[redacted]` the agent-export prompt asks for: a value that holds
/// one is already redacted and is never replaced or counted again.
const MARKER: &str = "[redacted";

/// Text with credentials replaced, and what was replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Redaction {
    /// The redacted text.
    pub text: String,
    /// Replacements per kind.
    pub counts: BTreeMap<&'static str, usize>,
    /// A PEM private-key block was found; the caller skips the whole unit.
    pub private_key: bool,
}

/// A PEM private-key block through its END line, or to the end of the text when unterminated. The key-type
/// words between `BEGIN` and `PRIVATE KEY` are free (RSA, EC, OPENSSH, ENCRYPTED, PGP's `… KEY BLOCK`).
static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----",
        r"(?:.*?-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|.*)",
    ))
    .expect("fixed private-key regex")
});

/// Provider keys: `sk-` (also `sk-ant-`, `sk-proj-`; dots allowed as in check_public.py), GitHub classic and
/// fine-grained tokens, AWS access key ids, Slack tokens, Google API keys, Stripe secret and restricted keys,
/// GitLab personal access tokens, Hugging Face, npm and PyPI tokens. The ASCII word boundary treats CJK as a
/// boundary, so a key written right after Chinese text is found, while `task-…` holds no `sk-` key.
static API_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:\b)(?:sk-[A-Za-z0-9_.-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}",
        r"|AKIA[0-9A-Z]{16}(?-u:\b)|xox[abprs]-[A-Za-z0-9-]{10,}|AIza[0-9A-Za-z_-]{35}",
        r"|[sr]k_(?:live|test)_[A-Za-z0-9]{16,}|glpat-[A-Za-z0-9_-]{20,}|hf_[A-Za-z0-9]{30,}",
        r"|npm_[A-Za-z0-9]{36,}|pypi-[A-Za-z0-9_-]{40,})",
    ))
    .expect("fixed api-key regex")
});

/// The credential after an `Authorization` header (and its scheme, kept) or after `Bearer `: an RFC 6750
/// token68 that does not end in `.`, so a sentence's full stop stays. `$TOKEN` or `<token>` never match.
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?-u:\b)((?i:authorization)["']?[ \t]*[:=][ \t]*["']?"#,
        r"(?:(?i:basic|bearer|digest|token|negotiate|ntlm|apikey|api-key|key)[ \t]+)?",
        r"|(?i:bearer)[ \t]+)",
        r"([A-Za-z0-9._~+/-]*[A-Za-z0-9_~+/-]=*)",
    ))
    .expect("fixed bearer regex")
});

/// Words that stand where a credential could but are none: scheme names ("Authorization: Bearer" with no
/// token) and what prose says after "Bearer" ("Bearer tokens", "bearer authentication").
const NOT_CREDENTIALS: [&str; 12] = [
    "basic",
    "bearer",
    "digest",
    "token",
    "tokens",
    "auth",
    "authentication",
    "authorization",
    "header",
    "scheme",
    "credential",
    "credentials",
];

/// `user:password@` after a URL scheme; the user may be empty (`redis://:password@host`). The password runs to
/// the last `@` before the path, query or fragment, as URL parsers read an unescaped `@` in a password.
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)([A-Za-z][A-Za-z0-9+.-]*://)([^\s/?#@:]*:[^\s/?#]+)@")
        .expect("fixed url-credentials regex")
});

/// English words (case-insensitive) that end a credential flag name or label; `LABEL` adds the Chinese ones.
const SECRET_WORDS: &str = r"password|passwd|passphrase|pwd|secret|token|(?:api|access|secret|private)[ _-]?key|credentials?";

/// A flag's or label's value: a quoted string, or a run to whitespace or CJK punctuation that starts with neither
/// a quote nor a dash (the next flag: `psql --password --host db`). Punctuation and emphasis at the end are split
/// off later (`word_value`).
const VALUE: &str = r#""[^"\r\n]*"|'[^'\r\n]*'|`[^`\r\n]*`|[^\s"'`，。；、）-][^\s，。；、）]*"#;

/// The password attached to `-p` on a MySQL or MariaDB command line (`mysql -u root -p…`, `mysqldump -p… db`).
/// Other commands bundle short flags (`mkdir -pv`, `tar -pxf`), so `-p` counts only here; a bare `-p` prompts.
static MYSQL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:\b)((?:mysql|mariadb)[A-Za-z_-]*(?:[ \t]+[^\s|;&]+)*?[ \t]+-p)",
        r#"("[^"\r\n]*"|'[^'\r\n]*'|[^\s"'`|;&]+)"#,
    ))
    .expect("fixed mysql-password regex")
});

/// The password of `curl -u user:password` (also `--user`, `-U`, `--proxy-user`); the user stays. Other
/// commands use `-u` for a user alone (`docker run -u 1000:1000`).
static CURL_USER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?-u:\b)(curl(?:[ \t]+[^\s|;&]+)*?[ \t]+(?:-u|-U|--user|--proxy-user)(?:[ \t]+|=)?",
        r#"["']?[^\s:"']*:)([^\s"']+)"#,
    ))
    .expect("fixed curl-user regex")
});

/// A flag whose name ends with a secret word, with one or two dashes, and its value after `=` or blanks:
/// `--password=…`, `--token …`, `--openai-api-key …`. A name that goes on (`--password-file`) is no match.
static FLAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(^|[\s"'`(])(--?(?:[A-Za-z0-9]+[-_])*(?i:{SECRET_WORDS}))(=|[ \t]+)({VALUE})"#
    ))
    .expect("fixed flag regex")
});

/// A `NAME=value` or `NAME: value` line, optionally indented, a list item, `export`ed or with a quoted name;
/// the value runs to the end of the line (`.env` and YAML values may contain spaces).
static ENV_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?m)^([ \t]*(?:[-*+][ \t]+)?(?:export[ \t]+)?["']?)"#,
        r#"([A-Za-z_][A-Za-z0-9_.-]*)(["']?[ \t]*[=:][ \t]*)([^\r\n]*)"#,
    ))
    .expect("fixed env-line regex")
});

/// `NAME=value` inside a line (`KEY=… cmd`, `?api_key=…`; `--token=…` is a flag) or `"NAME": value` with a quoted
/// name (JSON); the value is a quoted string or runs to whitespace or a delimiter. `==` is no assignment.
static ENV_INLINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?-u:\b)([A-Za-z_][A-Za-z0-9_.-]*)(["']?[ \t]*=[ \t]*|["'][ \t]*:[ \t]*)"#,
        r#"("[^"\r\n]*"|'[^'\r\n]*'|[^\s"'`,;&=)\]}>][^\s"'`,;&)\]}>]*)"#,
    ))
    .expect("fixed env-inline regex")
});

/// Name parts (case-insensitive substrings) that mark a variable as holding a credential: those of `SECRET_WORDS`.
const SECRET_NAMES: [&str; 8] = [
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "PWD",
    "CREDENTIAL",
];

/// A prose or Markdown label that ends with a secret word, a separator (`:`, `=`, `:=`, `：`) and the value:
/// `DB password: …`, `- **Password**: …`, `**API key:** …`, `密码：…`. Emphasis may close before or after the
/// separator. An English word needs the start or a character other than an ASCII letter or digit before it (so
/// `db_password` counts), a Chinese one nothing (`MySQL密码：…`). A label that goes on (`Token budget:`) is none.
static LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        concat!(
            r"((?:^|[^A-Za-z0-9])(?i:{SECRET_WORDS})|密码|口令|密钥|秘钥|私钥|令牌)",
            r"((?:\*\*|__)?[ \t]*(?::=|[:=：])(?:\*\*|__)?[ \t]*)({VALUE})",
        ),
        SECRET_WORDS = SECRET_WORDS,
        VALUE = VALUE,
    ))
    .expect("fixed label regex")
});

/// Words prose puts after a secret word ("pass the --token flag", "Password: see the vault"): no credential.
const PROSE_WORDS: [&str; 22] = [
    "a",
    "an",
    "and",
    "argument",
    "are",
    "flag",
    "for",
    "from",
    "if",
    "in",
    "is",
    "none",
    "null",
    "of",
    "on",
    "option",
    "or",
    "parameter",
    "see",
    "the",
    "to",
    "with",
];

/// What ends a value without belonging to it: sentence punctuation, closing brackets, quotes and emphasis.
const TRAILING: [char; 12] = ['.', ',', ';', ':', ')', ']', '}', '>', '*', '"', '\'', '`'];

/// Replace credentials with `[redacted:<kind>]` before any text leaves the machine. Kinds:
/// - "private-key": a whole `-----BEGIN … PRIVATE KEY-----` … `-----END … PRIVATE KEY-----` block (also an unterminated BEGIN line to the end of text); sets `private_key`.
/// - "api-key": `sk-` and `sk-ant-`/`sk-proj-` style keys (20+ key chars, dots allowed, as `scripts/check_public.py`), GitHub tokens `gh[pousr]_` + 20+, `github_pat_…`, AWS `AKIA`+16 uppercase/digits, Slack `xox[abprs]-…`, Google `AIza` + 35, Stripe `sk_live_`/`sk_test_`/`rk_live_`/`rk_test_` + 16+ alphanumerics, GitLab `glpat-` + 20+, Hugging Face `hf_` + 30+, npm `npm_` + 36+, PyPI `pypi-` + 40+.
/// - "bearer": the token after `Bearer ` (case-insensitive) and after `Authorization:` headers.
/// - "url-credentials": the `user:password@` part of `scheme://user:password@host`, also with an empty user (keep scheme and host).
/// - "cli-secret": the password attached to `-p` on a `mysql`/`mysqldump`/`mariadb` command line, the password of `curl -u user:password`, and the value of a flag whose name ends with a secret word (`--password=…`, `--password …`, `--token …`, `--api-key …`, also with one dash). Keep the flag.
/// - "env-secret": the value of `NAME=value` / `NAME: value` / `export NAME=value` where NAME (case-insensitive) contains KEY, TOKEN, SECRET, PASSWORD, PASSWD, PASSPHRASE, PWD or CREDENTIAL, and of a prose or Markdown label that ends with a secret word (password, passwd, passphrase, pwd, secret, token, api key, access key, secret key, private key, credential(s), 密码, 口令, 密钥, 秘钥, 私钥, 令牌) followed by `:`, `=` or `：`. Keep the name.
///
/// Apply private-key first, then the others; never double-redact; text without secrets is returned unchanged with empty counts.
///
/// The others run in the order listed, so a provider key inside a header, URL, flag or assignment counts once,
/// as "api-key", and a `--token=…` flag counts as "cli-secret". The `@` after URL credentials stays
/// (`https://[redacted:url-credentials]@host`). A full stop after an `sk-` key or a bearer token stays outside
/// the marker. Bearer and header credentials skip the words in `NOT_CREDENTIALS`. Env values: `NAME: value`
/// counts only at the start of a line (or with a quoted name), `NAME=value` anywhere; a value at the start of a
/// line runs to its end, inside a line to whitespace or a delimiter. Labels run after the env rules, so a line
/// keeps the env rule's longer value. A label's or flag's value runs to whitespace; emphasis and trailing
/// punctuation stay outside the marker. A quoted value keeps its quotes.
///
/// Kept values: empty, holding `[redacted` (also the export prompt's plain `[redacted]`), a comparison (`==`)
/// or path (`token::parse`), a placeholder (`$VAR`, `${VAR}`, `$(cmd)`, `<…>`, `[REDACTED]`, masks such as
/// `***`, `xxx` or `…`), a number of fewer than eight digits unless the name says password (pass, pwd, 密码, 口令;
/// `-p` and `curl -u` always do), a label's or flag's value that starts with `-`, a negated flag (`--no-password`),
/// and a word in `PROSE_WORDS` after a label or flag.
pub fn redact(text: &str) -> Redaction {
    let mut counts = BTreeMap::new();
    let text = replace(
        text,
        &PRIVATE_KEY,
        "private-key",
        &mut counts,
        |_, marker| Some(marker.to_string()),
    );
    let text = replace(&text, &API_KEY, "api-key", &mut counts, |caps, marker| {
        let key = &caps[0];
        let kept = key.trim_end_matches('.');
        let stop = if key.starts_with("sk-") && kept.len() >= "sk-".len() + 20 {
            &key[kept.len()..]
        } else {
            ""
        };
        Some(format!("{marker}{stop}"))
    });
    let text = replace(&text, &BEARER, "bearer", &mut counts, |caps, marker| {
        let credential = &caps[2];
        (!NOT_CREDENTIALS
            .iter()
            .any(|word| credential.eq_ignore_ascii_case(word)))
        .then(|| format!("{}{marker}", &caps[1]))
    });
    let text = replace(
        &text,
        &URL_CREDENTIALS,
        "url-credentials",
        &mut counts,
        |caps, marker| (!caps[2].contains(MARKER)).then(|| format!("{}{marker}@", &caps[1])),
    );
    // `-p` and `curl -u` hold nothing but a password.
    let password_after = |caps: &Captures, marker: &str| {
        let value = secret_value(&caps[2], true, marker)?;
        Some(format!("{}{value}", &caps[1]))
    };
    let text = replace(
        &text,
        &MYSQL_PASSWORD,
        "cli-secret",
        &mut counts,
        password_after,
    );
    let text = replace(&text, &CURL_USER, "cli-secret", &mut counts, password_after);
    let text = replace(&text, &FLAG, "cli-secret", &mut counts, |caps, marker| {
        let (flag, separator, value) = (&caps[2], &caps[3], &caps[4]);
        let negated = flag
            .trim_start_matches('-')
            .get(..3)
            .is_some_and(|start| start.eq_ignore_ascii_case("no-"));
        if negated {
            return None;
        }
        let value = word_value(value, names_password(flag), marker)?;
        Some(format!("{}{flag}{separator}{value}", &caps[1]))
    });
    let text = replace(
        &text,
        &ENV_LINE,
        "env-secret",
        &mut counts,
        |caps, marker| {
            let value = env_value(&caps[2], &caps[4], marker)?;
            Some(format!("{}{}{}{value}", &caps[1], &caps[2], &caps[3]))
        },
    );
    let text = replace(
        &text,
        &ENV_INLINE,
        "env-secret",
        &mut counts,
        |caps, marker| {
            let value = env_value(&caps[1], &caps[3], marker)?;
            Some(format!("{}{}{value}", &caps[1], &caps[2]))
        },
    );
    let text = replace(&text, &LABEL, "env-secret", &mut counts, |caps, marker| {
        let value = word_value(&caps[3], names_password(&caps[1]), marker)?;
        Some(format!("{}{}{value}", &caps[1], &caps[2]))
    });
    let private_key = counts.contains_key("private-key");
    Redaction {
        text,
        counts,
        private_key,
    }
}

/// Replace each match `redact` accepts (`None` keeps the match as it is) and count the replacements.
fn replace(
    text: &str,
    pattern: &Regex,
    kind: &'static str,
    counts: &mut BTreeMap<&'static str, usize>,
    redact: impl Fn(&Captures, &str) -> Option<String>,
) -> String {
    let marker = format!("{MARKER}:{kind}]");
    let mut found = 0;
    let out = pattern.replace_all(text, |caps: &Captures| match redact(caps, &marker) {
        Some(replacement) => {
            found += 1;
            replacement
        }
        None => caps[0].to_string(),
    });
    if found > 0 {
        *counts.entry(kind).or_default() += found;
    }
    out.into_owned()
}

/// `secret_value` of an env-style `value` when `name` holds a credential; `None` for another name.
fn env_value(name: &str, value: &str, marker: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    if !SECRET_NAMES.iter().any(|part| upper.contains(part)) {
        return None;
    }
    secret_value(value, names_password(name), marker)
}

/// `secret_value` of a label's or flag's value, which runs to whitespace: Markdown emphasis before it and
/// `TRAILING` characters after it stay outside the marker, and a word in `PROSE_WORDS` is kept.
fn word_value(value: &str, password: bool, marker: &str) -> Option<String> {
    if value.starts_with(['"', '\'', '`']) {
        return secret_value(value, password, marker);
    }
    let core = value.trim_start_matches('*');
    let word = core.trim_end_matches(TRAILING);
    if PROSE_WORDS.contains(&word.to_ascii_lowercase().as_str()) {
        return None;
    }
    let replaced = secret_value(word, password, marker)?;
    let lead = &value[..value.len() - core.len()];
    Some(format!("{lead}{replaced}{}", &core[word.len()..]))
}

/// The replacement for a credential `value`: the inside of a leading quoted string (quotes and what follows them
/// kept), else the whole value but trailing whitespace. `None` keeps the value: empty or already redacted, a
/// comparison (`==`) or a path (`token::parse`), a placeholder, or a number of fewer than eight digits unless it
/// is a `password` (a PIN, where for other names it is a count or a limit).
fn secret_value(value: &str, password: bool, marker: &str) -> Option<String> {
    let trimmed = value.trim_end();
    if trimmed.is_empty() || trimmed.starts_with(['=', ':']) || trimmed.contains(MARKER) {
        return None;
    }
    let tail = &value[trimmed.len()..];
    let quote = trimmed
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'));
    let (open, inner, rest) = match quote.and_then(|q| trimmed[1..].find(q)) {
        Some(len) => (&trimmed[..1], &trimmed[1..1 + len], &trimmed[1 + len..]),
        None => ("", trimmed, ""),
    };
    let short_number = inner.len() < 8 && inner.bytes().all(|b| b.is_ascii_digit());
    if inner.is_empty() || placeholder(inner) || (short_number && !password) {
        return None;
    }
    Some(format!("{open}{marker}{rest}{tail}"))
}

/// Whether `value` stands for a credential without holding one: `$VAR`, `${…}`, `$(…)`, `<…>`, a `[redacted…]`
/// marker in any case, or a mask such as `***`, `xxx`, `...` or `…`.
fn placeholder(value: &str) -> bool {
    let variable = value.strip_prefix('$').is_some_and(|rest| {
        rest.starts_with(['{', '('])
            || (!rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
    });
    variable
        || value.starts_with('<')
        || value
            .get(..MARKER.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(MARKER))
        || value
            .chars()
            .all(|c| matches!(c, 'x' | 'X' | '*' | '•' | '.' | '…' | '-' | '_'))
}

/// Whether a name, label or flag names a password, whose short numbers are PINs rather than counts or limits.
fn names_password(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["pass", "pwd", "密码", "口令"]
        .iter()
        .any(|word| lower.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic credential: `prefix` and `len` characters cycling through an obviously fake pattern. Built at
    /// run time so that no line of this file looks like a key to the repository's secret scanner.
    fn fake(prefix: &str, len: usize) -> String {
        let body: String = "Fake0Key9".chars().cycle().take(len).collect();
        format!("{prefix}{body}")
    }

    fn pem_line(edge: &str, kind: &str) -> String {
        ["-----", edge, " ", kind, "PRIVATE KEY-----"].concat()
    }

    fn counts(pairs: &[(&'static str, usize)]) -> BTreeMap<&'static str, usize> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn private_key_blocks_are_removed_whole() {
        let block = format!(
            "{}\nMIIfakeBodyLine\nmoreFakeBody\n{}",
            pem_line("BEGIN", "RSA "),
            pem_line("END", "RSA ")
        );
        let r = redact(&format!("before\n{block}\nafter"));
        assert_eq!(r.text, "before\n[redacted:private-key]\nafter");
        assert!(r.private_key);
        assert_eq!(r.counts, counts(&[("private-key", 1)]));
        // An unterminated header takes the rest of the text with it.
        let r = redact(&format!(
            "keep this\n{}\nMIIfake\nno end line",
            pem_line("BEGIN", "")
        ));
        assert_eq!(r.text, "keep this\n[redacted:private-key]");
        assert!(r.private_key);
    }

    #[test]
    fn provider_keys_are_redacted() {
        let keys = [
            fake("sk-", 32),
            fake("sk-ant-api03-", 40),
            fake("sk-proj-", 40),
            fake("ghp_", 36),
            fake("github_pat_", 60),
            ["AKIA", "FAKE0KEY9FAKE0KE"].concat(),
            fake("xoxb-", 30),
            fake("AIza", 35),
            fake("sk_live_", 24),
            fake("sk_test_", 24),
            fake("rk_live_", 24),
            fake("rk_test_", 24),
            fake("glpat-", 20),
            fake("hf_", 34),
            fake("npm_", 36),
            fake("pypi-", 60),
        ];
        for key in &keys {
            let r = redact(&format!("密钥{key} 用于测试"));
            assert_eq!(r.text, "密钥[redacted:api-key] 用于测试", "{key}");
            assert_eq!(r.counts, counts(&[("api-key", 1)]));
            assert!(!r.private_key);
        }
        // A full stop after the key stays; words that merely contain `sk-` and short values are no keys.
        assert_eq!(
            redact(&format!("Use {}.", fake("sk-", 30))).text,
            "Use [redacted:api-key]."
        );
        let plain = format!(
            "task-{} and sk-short and ghp_short and sk_live_short, glpat-short, {} and hf_hub_download",
            "a".repeat(30),
            fake("npm_", 35)
        );
        assert_eq!(redact(&plain).text, plain);
    }

    #[test]
    fn bearer_tokens_and_authorization_headers() {
        let token = fake("eyJ", 40);
        let r = redact(&format!(
            "curl -H \"Authorization: Bearer {token}\" https://api.example.test"
        ));
        assert_eq!(
            r.text,
            "curl -H \"Authorization: Bearer [redacted:bearer]\" https://api.example.test"
        );
        assert_eq!(r.counts, counts(&[("bearer", 1)]));
        assert_eq!(
            redact("authorization: Basic dXNlcjpwYXNz").text,
            "authorization: Basic [redacted:bearer]"
        );
        assert_eq!(
            redact("Authorization: abc123def").text,
            "Authorization: [redacted:bearer]"
        );
        assert_eq!(
            redact(&format!("send BEARER {token}.")).text,
            "send BEARER [redacted:bearer]."
        );
        // Prose and placeholders keep their words.
        for text in [
            "The API uses Bearer tokens.",
            "Authorization: Bearer $TOKEN",
        ] {
            assert_eq!(redact(text).text, text);
        }
    }

    #[test]
    fn url_credentials_keep_scheme_and_host() {
        let r = redact(
            "db at postgres://admin:hunter2@db.internal:5432/app and https://u:p@ss@host.test/x",
        );
        assert_eq!(
            r.text,
            "db at postgres://[redacted:url-credentials]@db.internal:5432/app and https://[redacted:url-credentials]@host.test/x"
        );
        assert_eq!(r.counts, counts(&[("url-credentials", 2)]));
        for text in [
            "ssh://git@example.test/repo.git",
            "mailto:someone@example.test",
            "https://example.test/a:b@c",
        ] {
            assert_eq!(redact(text).text, text);
        }
    }

    #[test]
    fn url_credentials_with_an_empty_user_are_redacted() {
        let r = redact("sessions in redis://:hunter2@cache.internal:6379/0 since May");
        assert_eq!(
            r.text,
            "sessions in redis://[redacted:url-credentials]@cache.internal:6379/0 since May"
        );
        assert_eq!(r.counts, counts(&[("url-credentials", 1)]));
        for text in ["http://[::1]:8080/health", "https://example.test:443/a"] {
            assert_eq!(redact(text).text, text);
        }
    }

    #[test]
    fn quoted_pwd_and_passphrase_keys_are_redacted() {
        let r =
            redact("{\"pwd\": \"hunter2x\", \"passphrase\": \"open sesame\", \"user\": \"me\"}");
        assert_eq!(
            r.text,
            "{\"pwd\": \"[redacted:env-secret]\", \"passphrase\": \"[redacted:env-secret]\", \"user\": \"me\"}"
        );
        assert_eq!(r.counts, counts(&[("env-secret", 2)]));
        // A quoted YAML key at the start of a line; a password's short number is a PIN.
        let r = redact("'PWD': 1234\n");
        assert_eq!(r.text, "'PWD': [redacted:env-secret]\n");
    }

    #[test]
    fn env_style_secrets_keep_their_names() {
        let text = concat!(
            "export GITHUB_TOKEN=abc123\n",
            "DB_PASSWORD: two words # note\n",
            "  - api_key: 'quoted value' trailing\n",
            "PATH=/usr/bin\n",
            "EMPTY_SECRET=\n",
            "QUOTED_SECRET=\"\"\n",
            "{\"client_secret\": \"s3\", \"user\": \"me\"}\n",
        );
        let r = redact(text);
        assert_eq!(
            r.text,
            concat!(
                "export GITHUB_TOKEN=[redacted:env-secret]\n",
                "DB_PASSWORD: [redacted:env-secret]\n",
                "  - api_key: '[redacted:env-secret]' trailing\n",
                "PATH=/usr/bin\n",
                "EMPTY_SECRET=\n",
                "QUOTED_SECRET=\"\"\n",
                "{\"client_secret\": \"[redacted:env-secret]\", \"user\": \"me\"}\n",
            )
        );
        assert_eq!(r.counts, counts(&[("env-secret", 4)]));
        // A short number is a budget or a limit unless the name says password.
        assert_eq!(redact("max_tokens: 400").text, "max_tokens: 400");
        assert_eq!(
            redact("DB_PASSWORD=123456").text,
            "DB_PASSWORD=[redacted:env-secret]"
        );
        assert_eq!(
            redact("TOKEN_TTL=86400000").text,
            "TOKEN_TTL=[redacted:env-secret]"
        );
    }

    #[test]
    fn labeled_secrets_in_prose_and_markdown() {
        let cases = [
            (
                "- **Password**: hunter2x",
                "- **Password**: [redacted:env-secret]",
            ),
            (
                "DB password: hunter2x",
                "DB password: [redacted:env-secret]",
            ),
            (
                "**API key:** abcd1234efgh",
                "**API key:** [redacted:env-secret]",
            ),
            (
                "__Secret key__ = abcd1234efgh",
                "__Secret key__ = [redacted:env-secret]",
            ),
            (
                "Access token = abc123def456",
                "Access token = [redacted:env-secret]",
            ),
            ("私钥：abcd1234efgh", "私钥：[redacted:env-secret]"),
            (
                "密码：hunter2x，然后重启",
                "密码：[redacted:env-secret]，然后重启",
            ),
            ("MySQL密码：hunter2x", "MySQL密码：[redacted:env-secret]"),
            (
                "测试令牌: abc123def456。",
                "测试令牌: [redacted:env-secret]。",
            ),
            (
                "Use the client secret: `s3cr3t-value`.",
                "Use the client secret: `[redacted:env-secret]`.",
            ),
            (
                "(password: **hunter2x**).",
                "(password: **[redacted:env-secret]**).",
            ),
            (
                "set db_password: 123456 for tests",
                "set db_password: [redacted:env-secret] for tests",
            ),
            (
                "Go: token := \"abc123def456\"",
                "Go: token := \"[redacted:env-secret]\"",
            ),
        ];
        for (text, redacted) in cases {
            let r = redact(text);
            assert_eq!(r.text, redacted, "{text}");
            assert_eq!(r.counts, counts(&[("env-secret", 1)]), "{text}");
        }
    }

    #[test]
    fn command_line_secrets_keep_their_flags() {
        let cases = [
            (
                "mysql -u root -phunter2x",
                "mysql -u root -p[redacted:cli-secret]",
            ),
            (
                "mysqldump -phunter2x db > dump.sql",
                "mysqldump -p[redacted:cli-secret] db > dump.sql",
            ),
            (
                "mysql -uroot -p123456 -e 'select 1'",
                "mysql -uroot -p[redacted:cli-secret] -e 'select 1'",
            ),
            (
                "mariadb -h db -p'two words' app",
                "mariadb -h db -p'[redacted:cli-secret]' app",
            ),
            (
                "psql --password hunter2x",
                "psql --password [redacted:cli-secret]",
            ),
            (
                "run --password=hunter2x now",
                "run --password=[redacted:cli-secret] now",
            ),
            (
                "deploy --token abc123def456",
                "deploy --token [redacted:cli-secret]",
            ),
            (
                "`tool --api-key abc123def456`",
                "`tool --api-key [redacted:cli-secret]`",
            ),
            (
                "go run . -openai-api-key=abc123def456",
                "go run . -openai-api-key=[redacted:cli-secret]",
            ),
            (
                "login --password 1234",
                "login --password [redacted:cli-secret]",
            ),
            (
                "curl -u user:hunter2x https://api.example.test",
                "curl -u user:[redacted:cli-secret] https://api.example.test",
            ),
            (
                "curl -s --user 'me:p@ss:word' https://x.test",
                "curl -s --user 'me:[redacted:cli-secret]' https://x.test",
            ),
        ];
        for (text, redacted) in cases {
            let r = redact(text);
            assert_eq!(r.text, redacted, "{text}");
            assert_eq!(r.counts, counts(&[("cli-secret", 1)]), "{text}");
        }
    }

    #[test]
    fn counts_budgets_placeholders_and_prose_are_kept() {
        for text in [
            "Token budget: 400",
            "set max_tokens: 400 in the request",
            "Primary key: id",
            "API_KEY=$OPENAI_API_KEY",
            "token: ${{ secrets.GITHUB_TOKEN }}",
            "Password: <your password>",
            "Secret: [REDACTED]",
            "token = …",
            "pass the --token flag, or --api-key from the vault",
            "mkdir -p build && tar -pxvf a.tar && rsync -pav src dst",
            "mysql -u root -p mydb",
            "docker run -u 1000:1000 img",
            "pip --password-file ./x and docker login --password-stdin",
            "psql --no-password mydb && psql --password --host db",
            "the refresh-token flow",
            "if token == expected:",
        ] {
            let r = redact(text);
            assert_eq!(r.text, text);
            assert!(r.counts.is_empty(), "{text}");
        }
    }

    #[test]
    fn nothing_is_redacted_twice() {
        let key = fake("sk-", 30);
        let token = fake("ghp_", 36);
        let text = format!(
            "OPENAI_API_KEY={key}\nAuthorization: Bearer {key}\nclone https://user:{token}@git.example.test/r.git\nSECRET=\"[redacted]\""
        );
        let r = redact(&text);
        assert_eq!(
            r.text,
            "OPENAI_API_KEY=[redacted:api-key]\nAuthorization: Bearer [redacted:api-key]\nclone https://user:[redacted:api-key]@git.example.test/r.git\nSECRET=\"[redacted]\""
        );
        assert_eq!(r.counts, counts(&[("api-key", 3)]));
        // A provider key behind a flag counts once, as the key.
        let r = redact(&format!("`tool --token {token}`"));
        assert_eq!(r.text, "`tool --token [redacted:api-key]`");
        assert_eq!(r.counts, counts(&[("api-key", 1)]));
    }
}
