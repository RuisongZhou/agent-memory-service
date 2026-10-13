use super::*;
use std::collections::HashSet;

// ------------------------------------------------------------------------------------------------
// analyzer

#[test]
fn english_drops_stop_words_and_plurals() {
    assert_eq!(
        analyze("When did Caroline go to the LGBTQ support groups?"),
        vec!["caroline", "go", "lgbtq", "support", "group"]
    );
    assert_eq!(
        analyze("Melanie's kids don't like it"),
        vec!["melanie", "kid", "like"]
    );
    assert_eq!(analyze("stories class"), vec!["story", "class"]);
}

#[test]
fn chinese_uses_search_mode() {
    let t = analyze("我们讨论了中华人民共和国的数据库");
    for w in ["中华人民共和国", "中华", "人民", "共和国", "讨论", "数据库"] {
        assert!(t.contains(&w.to_string()), "{w} in {t:?}");
    }
    assert!(!t.contains(&"的".to_string()));
}

#[test]
fn identifiers_keep_whole_and_parts() {
    assert_eq!(analyze("v1.2.3"), vec!["v1.2.3", "v1", "2", "3"]);
    assert_eq!(
        analyze("bge-reranker-v2-m3"),
        vec!["bge-reranker-v2-m3", "bge", "reranker", "v2", "m3"]
    );
    assert_eq!(
        analyze("docs at https://X.y/z, ok"),
        vec![
            "doc",
            "https://x.y/z",
            "x.y/z",
            "x.y",
            "http",
            "x",
            "y",
            "z",
            "ok"
        ]
    );
    assert_eq!(analyze("ｖ１．２"), vec!["v1.2", "v1", "2"]);
}

// ------------------------------------------------------------------------------------------------
// index

fn doc(id: &str, kind: &str, body: &str) -> FtDoc {
    FtDoc {
        id: id.into(),
        kind: kind.into(),
        mtype: if kind == "turn" { "turn" } else { "fact" }.into(),
        status: "current".into(),
        scope_kind: "global".into(),
        effective_at: 1_000,
        body: body.into(),
        ..Default::default()
    }
}

fn ids(r: &[(String, f32)]) -> Vec<&str> {
    r.iter().map(|(i, _)| i.as_str()).collect()
}

fn open_tmp() -> (tempfile::TempDir, FullText) {
    let dir = tempfile::tempdir().unwrap();
    let ft = FullText::open(&dir.path().join("fulltext")).unwrap();
    (dir, ft)
}

#[test]
fn upsert_replaces_and_delete_removes() {
    let (_d, ft) = open_tmp();
    let all = FtFilter::default();
    ft.upsert(vec![doc("m1", "memory", "SurrealDB 端口是 8000")])
        .unwrap();
    ft.commit().unwrap();
    ft.upsert(vec![doc("m1", "memory", "SurrealDB 端口改成 9000")])
        .unwrap();
    ft.commit().unwrap();
    assert_eq!(ft.num_docs(), 1);
    assert!(ft.search("8000", &all, 10).unwrap().is_empty());
    assert_eq!(ids(&ft.search("端口", &all, 10).unwrap()), vec!["m1"]);
    ft.delete(&["m1".to_string()]).unwrap();
    ft.commit().unwrap();
    assert_eq!(ft.num_docs(), 0);
}

#[test]
fn filters() {
    let (_d, ft) = open_tmp();
    let mk = |id: &str, kind: &str, status: &str, project: &str, t: i64| FtDoc {
        status: status.into(),
        scope_kind: if project.is_empty() {
            "global"
        } else {
            "project"
        }
        .into(),
        project: project.into(),
        effective_at: t,
        ..doc(id, kind, "deploy the reranker model")
    };
    ft.upsert(vec![
        mk("g1", "memory", "current", "", 100),
        mk("p1", "memory", "current", "proj1", 200),
        mk("p2", "memory", "conflict", "proj2", 300),
        mk("t1", "turn", "buffer", "proj1", 400),
    ])
    .unwrap();
    ft.commit().unwrap();
    let set = |f: FtFilter| -> HashSet<String> {
        ft.search("reranker", &f, 100)
            .unwrap()
            .into_iter()
            .map(|x| x.0)
            .collect()
    };
    let hs = |v: &[&str]| -> HashSet<String> { v.iter().map(|s| s.to_string()).collect() };
    assert_eq!(
        set(FtFilter {
            kinds: vec!["turn".into()],
            ..Default::default()
        }),
        hs(&["t1"])
    );
    assert_eq!(
        set(FtFilter {
            statuses: vec!["conflict".into()],
            ..Default::default()
        }),
        hs(&["p2"])
    );
    assert_eq!(
        set(FtFilter {
            kinds: vec!["memory".into()],
            visible: vec![
                ("scope_kind".into(), "global".into()),
                ("project".into(), "proj1".into()),
            ],
            ..Default::default()
        }),
        hs(&["g1", "p1"])
    );
    assert_eq!(
        set(FtFilter {
            time_range: Some((200, 300)),
            ..Default::default()
        }),
        hs(&["p1", "p2"])
    );
}

#[test]
fn bm25_ranks_rare_terms_and_short_documents_first() {
    let (_d, ft) = open_tmp();
    let mut docs = vec![
        doc("both", "memory", "Caroline went to the LGBTQ support group meeting"),
        doc("one", "memory", "Caroline went hiking with friends"),
        doc("long", "memory", "Caroline talked about painting, her kids, camping, pottery, the beach, music and many other unrelated topics for hours"),
    ];
    for i in 0..20 {
        docs.push(doc(
            &format!("n{i}"),
            "memory",
            &format!("Caroline note number {i}"),
        ));
    }
    ft.upsert(docs).unwrap();
    ft.commit().unwrap();
    let all = FtFilter::default();
    assert_eq!(
        ft.search("Caroline support group", &all, 10).unwrap()[0].0,
        "both"
    );
    let r = ft.search("Caroline", &all, 30).unwrap();
    let pos = |id: &str| r.iter().position(|x| x.0 == id).unwrap();
    assert!(pos("one") < pos("long"));
}

#[test]
fn natural_language_and_identifier_queries() {
    let (_d, ft) = open_tmp();
    ft.upsert(vec![
        doc(
            "adopt",
            "turn",
            "Caroline: I'm doing research on adoption agencies.",
        ),
        doc("camp", "turn", "Melanie: We went camping at the beach."),
        doc(
            "config",
            "memory",
            "测试环境的配置文件在 config/test.toml，surreal.port 设为 8001",
        ),
        doc(
            "model",
            "memory",
            "升级到 v1.2.3 之后重排序模型换成 bge-reranker-v2-m3",
        ),
    ])
    .unwrap();
    ft.commit().unwrap();
    let all = FtFilter::default();
    let top = |q: &str| ft.search(q, &all, 10).unwrap()[0].0.clone();
    assert_eq!(top("What did Caroline research?"), "adopt");
    assert_eq!(top("Where did Melanie go camping?"), "camp");
    assert_eq!(top("config/test.toml 在哪"), "config");
    assert_eq!(top("用的哪个 reranker"), "model");
}

#[test]
fn analyzer_version_change_requests_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ft");
    {
        let ft = FullText::open(&p).unwrap();
        assert!(ft.needs_rebuild());
        ft.upsert(vec![doc("m1", "memory", "persisted kayaking")])
            .unwrap();
        ft.commit().unwrap();
    }
    assert!(!FullText::open(&p).unwrap().needs_rebuild());
    std::fs::write(p.join("memory_fulltext.version"), "old-analyzer\n").unwrap();
    let ft = FullText::open(&p).unwrap();
    assert!(ft.needs_rebuild());
    assert_eq!(ft.num_docs(), 0);
}

// ------------------------------------------------------------------------------------------------
// entity dictionary

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn entity_resolve_and_longest_match() {
    let mut d = EntityDict::new();
    d.upsert("e:surreal", &names(&["SurrealDB"]));
    d.upsert("e:memory", &names(&["共享记忆库"]));
    d.upsert("e:mem", &names(&["记忆库"]));
    d.upsert("e:mel", &names(&["Mel"]));
    assert_eq!(
        d.resolve("ＳｕｒｒｅａｌＤＢ").as_deref(),
        Some("e:surreal")
    );
    assert_eq!(d.resolve("surreal db").as_deref(), Some("e:surreal"));
    // the longest alias consumes shorter ones; ASCII aliases need word boundaries
    assert_eq!(d.longest_match("共享记忆库的设计"), vec!["e:memory"]);
    assert_eq!(d.longest_match("Mel likes Melody"), vec!["e:mel"]);
}

#[test]
fn entity_replace_and_remove() {
    let mut d = EntityDict::new();
    d.upsert("e1", &names(&["Jon", "Jonathan"]));
    d.replace("e1", &names(&["Jonathan"]));
    assert_eq!(d.resolve("jon"), None);
    d.upsert("e2", &names(&["Jonathan"]));
    assert_eq!(d.longest_match("hi Jonathan"), vec!["e2", "e1"]);
    d.remove("e2");
    d.remove("e1");
    assert!(d.is_empty());
}
