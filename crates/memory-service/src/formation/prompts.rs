//! Prompt sets. `zh` = the system prompts of 《共享记忆库 第四章 抽取 Prompt》 with fixed positional fact values (plus the §0
//! language convention); `en` = the LoCoMo-adapted set (same structure, schema and relation rules; extraction
//! scope rewritten for person-to-person conversation; English memories). User templates follow the documented
//! XML-tag layout.

pub struct PromptSet {
    pub variant: &'static str,
    pub p1: &'static str,
    pub p1b: &'static str,
    pub p2: &'static str,
    pub p3: &'static str,
    pub p3_project: &'static str,
    pub p4: &'static str,
    pub p5: &'static str,
    pub l: Labels,
    /// en variant: both speakers' statements are first-hand (origin=user allowed in either message).
    pub both_speakers_first_hand: bool,
}

pub struct Labels {
    pub known_entities: &'static str,
    pub old_memories: &'static str,
    pub previous: &'static str,
    pub previous_note: &'static str,
    pub previous_none: &'static str,
    pub batch: &'static str,
    pub user: &'static str,
    pub assistant: &'static str,
    pub remember: &'static str,
    pub summary: &'static str,
    pub old_experiences: &'static str,
    pub conversation: &'static str,
    pub global_facts: &'static str,
    pub project_facts: &'static str,
    pub profile_mark: &'static str,
    pub experiences: &'static str,
    pub existing_skills: &'static str,
    pub none: &'static str,
    pub situation: &'static str,
    pub approach: &'static str,
    pub lesson: &'static str,
}

pub static ZH: PromptSet = PromptSet {
    variant: "zh",
    p1: include_str!("prompts/p1_zh.txt"),
    p1b: include_str!("prompts/p1b_zh.txt"),
    p2: include_str!("prompts/p2_zh.txt"),
    p3: include_str!("prompts/p3_zh.txt"),
    p3_project: include_str!("prompts/p3p_zh.txt"),
    p4: include_str!("prompts/p4_zh.txt"),
    p5: include_str!("prompts/p5_zh.txt"),
    both_speakers_first_hand: false,
    l: Labels {
        known_entities: "已知实体",
        old_memories: "旧记忆",
        previous: "前文",
        previous_note: "以下是本批之前的两轮摘录，只用于理解指代，不要从中抽取。",
        previous_none: "（会话第一批，无）",
        batch: "本批对话",
        user: "用户：",
        assistant: "助手：",
        remember: "要记住的内容",
        summary: "当前会话摘要",
        old_experiences: "旧经验",
        conversation: "对话",
        global_facts: "全局事实",
        project_facts: "项目事实",
        profile_mark: "[画像]",
        experiences: "经验",
        existing_skills: "已有技能",
        none: "（无）",
        situation: "场景：",
        approach: "做法：",
        lesson: "要点：",
    },
};

pub static EN: PromptSet = PromptSet {
    variant: "en",
    p1: include_str!("prompts/p1_en.txt"),
    p1b: include_str!("prompts/p1b_en.txt"),
    p2: include_str!("prompts/p2_en.txt"),
    p3: include_str!("prompts/p3_en.txt"),
    p3_project: include_str!("prompts/p3p_en.txt"),
    p4: include_str!("prompts/p4_en.txt"),
    p5: include_str!("prompts/p5_en.txt"),
    both_speakers_first_hand: true,
    l: Labels {
        known_entities: "known entities",
        old_memories: "old memories",
        previous: "previous turns",
        previous_note: "Excerpts of the two turns before this batch, for resolving references only; do not extract from them.",
        previous_none: "(first batch of the conversation, none)",
        batch: "current batch",
        // message content already starts with the speaker's name ("Caroline: ...")
        user: "",
        assistant: "",
        remember: "content to remember",
        summary: "current session summary",
        old_experiences: "old experiences",
        conversation: "conversation",
        global_facts: "global facts",
        project_facts: "project facts",
        profile_mark: "[profile]",
        experiences: "experiences",
        existing_skills: "existing skills",
        none: "(none)",
        situation: "situation: ",
        approach: "approach: ",
        lesson: "lesson: ",
    },
};

pub fn get(variant: &str) -> &'static PromptSet {
    if variant == "en" {
        &EN
    } else {
        &ZH
    }
}

/// One generation shares current dialogue between fact and experience extraction.
pub fn formation(p: &PromptSet) -> String {
    let (fact_end, experience_end, structure) = if p.variant == "en" {
        (
            "## Output structure:",
            "Output JSON only",
            include_str!("prompts/p12_structure_en.txt"),
        )
    } else {
        (
            "## 输出结构：",
            "只输出 JSON",
            include_str!("prompts/p12_structure_zh.txt"),
        )
    };
    format!(
        "{}\n\n{}\n\n{}",
        p.p1.split(fact_end).next().unwrap(),
        p.p2.split(experience_end).next().unwrap(),
        structure
    )
}

/// Frequent formation returns facts only, without task analysis, experiences or summaries.
pub fn facts(p: &PromptSet, explicit: bool) -> String {
    let end = if p.variant == "en" {
        "## Output structure:"
    } else {
        "## 输出结构："
    };
    let base = if explicit { p.p1b } else { p.p1 };
    let instructions = base
        .split(end)
        .next()
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("- reason：") && !line.starts_with("- reason:"))
        .collect::<Vec<_>>()
        .join("\n");
    let format = if p.variant == "en" {
        r#"## Output: facts-object-v1
Return only one JSON object: {"facts": [...]}.
Each fact has exactly: text, scope (global/project), entities (string array), effectiveAt (YYYY-MM-DD), origin (explicit/user/assistant), source ({turnId, span}), relations ([{memoryId, relation}]).
Copy source.span verbatim, at most 1024 Unicode characters. Cite only supplied turn IDs. Use M1 etc. for old memories and F1 etc. for facts in output order. No self-references. Relations: same/updates/conflicts/related. Do not output reason, analysis, experiences or summary. No facts: {"facts":[]}.
Write memories in English. Preserve names, dates and identifiers."#
    } else {
        r#"## 输出：facts-object-v1
只输出一个 JSON 对象：{"facts": [...]}。
每条事实只含：text、scope（global/project）、entities（字符串数组）、effectiveAt（YYYY-MM-DD）、origin（explicit/user/assistant）、source（{turnId, span}）、relations（[{memoryId, relation}]）。
source.span 逐字复制，最多 1024 个 Unicode 字符，只引用所提供的轮编号。旧记忆用 M1 等，本批事实按输出顺序用 F1 等，不能引用自身。关系为 same/updates/conflicts/related。不要输出 reason、analysis、experiences 或 summary。无事实时输出 {"facts":[]}。
记忆用简体中文，专有名词和标识符保留原文。"#
    };
    format!("{instructions}\n\n{format}")
}

pub fn session_experience(p: &PromptSet) -> String {
    let end = if p.variant == "en" {
        "Output JSON only"
    } else {
        "只输出 JSON"
    };
    let instruction = if p.variant == "en" {
        "Run only after the session ends. Read the whole supplied conversation. Output analysis, experiences and summary only, using the JSON object below; never output facts. Do not repeat an experience unless it cites a new turn listed in <new turns>. Unknown tasks produce no experience. Clear completed open issues; keep only issues still unresolved at session end."
    } else {
        "仅在 session 结束后执行，阅读所提供的完整会话。只输出下列 JSON 对象的 analysis、experiences、summary，不输出 facts。经验必须至少引用<新增轮>中的一轮，避免重复归纳。未验证结果的任务不生成经验。移除已解决事项，openIssues 只保留 session 结束时仍未解决的事项。"
    };
    format!("{}\n\n{}\n{{\"analysis\":[{{\"task\":\"...\",\"result\":\"success|failure|partial|unknown\",\"turnIds\":[\"...\"]}}],\"experiences\":[{{\"situation\":\"...\",\"approach\":\"...\",\"result\":\"success/failure/partial: ...\",\"lesson\":\"...\",\"scope\":\"global|project\",\"turnIds\":[\"...\"],\"relations\":[{{\"memoryId\":\"E1\",\"relation\":\"same|related\"}}]}}],\"summary\":{{\"goal\":\"...\",\"outcome\":\"...\",\"decisions\":[],\"openIssues\":[]}}}}", p.p2.split(end).next().unwrap(), instruction)
}

/// `<tag>\nbody\n</tag>` block.
pub fn block(tag: &str, body: &str) -> String {
    format!("<{tag}>\n{}\n</{tag}>", body.trim_end())
}
