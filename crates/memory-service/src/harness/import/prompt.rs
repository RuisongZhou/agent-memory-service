//! The prompts of an import from an agent (docs/design/memory-import.md §6.11, memory-import-mcp.md): the
//! export prompt asks an assistant to list what it remembers as the JSONL that `import plan --host jsonl`
//! and the MCP tool `propose_import` read; the importer prompt walks the agent of a host connected to
//! shared-memory through such an import in its conversation. The texts live in
//! `examples/memory-export-prompt.{zh,en}.txt` and `examples/memory-import-host-prompt.{zh,en}.txt`.
use super::{claude, codex, workbuddy};

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

const HOST_ZH: &str = include_str!("../../../../../examples/memory-import-host-prompt.zh.txt");
const HOST_EN: &str = include_str!("../../../../../examples/memory-import-host-prompt.en.txt");
/// Replaced in the importer prompt by the source's label, the export prompt and what to do when the source
/// is the agent itself.
pub const SOURCE: &str = "{source}";
pub const EXPORT: &str = "{export_prompt}";
pub const SELF: &str = "{self}";
/// Hosts whose memory files `import plan` reads: their agents do not export themselves.
const HOSTS_WITH_FILES: [&str; 3] = [claude::HOST, codex::HOST, workbuddy::HOST];

/// What the agent does when the source is `self`: export what it remembers, or, on a host whose memory
/// files `import plan` reads, point the person there (the same notes would otherwise arrive twice, under two
/// sets of unit keys).
fn self_sentence(platform: Option<&str>, lang: Lang) -> String {
    match (platform.filter(|p| HOSTS_WITH_FILES.contains(p)), lang) {
        (None, Lang::Zh) => "来源是 self 时，由你自己按这段提示词的格式和要求，导出你保存或记得的、和项目 {project} 有关的内容，不要推测。".into(),
        (None, Lang::En) => "If the source is self, export what you saved or remember about the project {project} yourself, in the format and under the rules of that prompt; do not guess.".into(),
        (Some(host), Lang::Zh) => format!("来源是 self 时不要自己导出：{host} 的记忆文件由 agent-memoryd 直接读取。请用户运行 agent-memoryd import plan --host {host} --project {PLACEHOLDER}，按它的提示补齐参数，然后停下。"),
        (Some(host), Lang::En) => format!("If the source is self, do not export it yourself: agent-memoryd reads the memory files of {host} directly. Ask the user to run agent-memoryd import plan --host {host} --project {PLACEHOLDER}, adding what it asks for, and stop."),
    }
}

/// The importer prompt for the agent of a host connected to shared-memory: walk the person through importing
/// the export of `source` into `project` (MCP prompt `import_memories`, `import prompt --for host`).
/// `platform` is the connection's, when known. Ends in a newline.
pub fn host_text(project: &str, source: &str, platform: Option<&str>, lang: Lang) -> String {
    let template = match lang {
        Lang::Zh => HOST_ZH,
        Lang::En => HOST_EN,
    };
    template
        .replace(SELF, &self_sentence(platform, lang))
        .replace(EXPORT, text(project, lang).trim_end())
        .replace(SOURCE, source)
        .replace(PLACEHOLDER, project)
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

    #[test]
    fn importer_prompts_embed_the_export_prompt_and_fill_every_placeholder() {
        for lang in [Lang::Zh, Lang::En] {
            let template = match lang {
                Lang::Zh => HOST_ZH,
                Lang::En => HOST_EN,
            };
            for placeholder in [SELF, EXPORT] {
                assert_eq!(template.matches(placeholder).count(), 1, "{lang:?}");
            }
            let prompt = host_text("demo-project", "chatgpt", None, lang);
            assert!(prompt.contains(text("demo-project", lang).trim_end()));
            assert!(prompt.contains("demo-project") && prompt.contains("chatgpt"));
            for placeholder in [PLACEHOLDER, SOURCE, EXPORT, SELF] {
                assert!(!prompt.contains(placeholder), "{lang:?} {placeholder}");
            }
            for tool in [
                "propose_import",
                "apply_import",
                "expectedProposals",
                "finish",
            ] {
                assert!(prompt.contains(tool), "{lang:?} {tool}");
            }
            assert!(prompt.ends_with('\n'));
            // Pasted back as an export by mistake, it gives no memory line.
            assert!(crate::harness::import::jsonl::segments(&prompt)
                .0
                .is_empty());
            // An agent on a host whose files `import plan` reads is sent there instead of exporting.
            let on_codex = host_text("demo-project", "self", Some("codex"), lang);
            assert!(on_codex.contains("import plan --host codex --project demo-project"));
            assert!(!prompt.contains("import plan --host codex"));
        }
    }
}
