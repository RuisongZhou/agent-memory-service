//! The export prompt (docs/design/memory-import.md §6.11): asks a host's own agent to list what it
//! remembers as the JSONL that `agent-memoryd import plan --host jsonl --from FILE` reads. The texts live
//! in `examples/memory-export-prompt.{zh,en}.txt` with a `{project}` placeholder.

/// Chinese (the design's text) or English.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

const ZH: &str = include_str!("../../../../../examples/memory-export-prompt.zh.txt");
const EN: &str = include_str!("../../../../../examples/memory-export-prompt.en.txt");
/// Replaced by the project id. The prompts also contain JSON braces, so this is no format string.
pub const PLACEHOLDER: &str = "{project}";

/// The prompt for `project`, ending in a newline.
pub fn text(project: &str, lang: Lang) -> String {
    let template = match lang {
        Lang::Zh => ZH,
        Lang::En => EN,
    };
    template.replace(PLACEHOLDER, project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::import::jsonl::KINDS;

    #[test]
    fn prompts_name_the_project_and_the_export_format() {
        for lang in [Lang::Zh, Lang::En] {
            let template = match lang {
                Lang::Zh => ZH,
                Lang::En => EN,
            };
            assert_eq!(template.matches(PLACEHOLDER).count(), 1, "{lang:?}");
            let prompt = text("demo-project", lang);
            assert!(prompt.contains(" demo-project ") && !prompt.contains(PLACEHOLDER));
            assert!(prompt.ends_with('\n'));
            // The parser's page marker, kinds, fields and the redaction it leaves alone.
            assert!(prompt.contains("{\"continued\": true}"));
            for word in
                KINDS
                    .iter()
                    .chain(&["text", "kind", "date", "source", "scope", "YYYY-MM-DD"])
            {
                assert!(prompt.contains(word), "{lang:?} {word}");
            }
            assert!(prompt.contains("[redacted]"));
        }
        assert!(text("p", Lang::Zh).starts_with("我要把你保存的记忆迁移到另一个记忆服务。"));
    }
}
