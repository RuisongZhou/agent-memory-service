use super::*;
use std::collections::HashSet;
use std::sync::Arc;

fn has(tokens: &[String], want: &[&str]) {
    for w in want {
        assert!(tokens.iter().any(|t| t == w), "missing {w:?} in {tokens:?}");
    }
}

fn lacks(tokens: &[String], bad: &[&str]) {
    for b in bad {
        assert!(
            !tokens.iter().any(|t| t == b),
            "unexpected {b:?} in {tokens:?}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// analyzer

#[test]
fn english_lowercase_and_stop_words() {
    let t = analyze("What did Caroline research about the LGBTQ support groups? When is it?");
    has(&t, &["caroline", "research", "lgbtq", "support", "group"]);
    lacks(
        &t,
        &[
            "what", "did", "the", "about", "when", "is", "it", "What", "Caroline", "groups",
        ],
    );
    // contractions / possessives split at the apostrophe (straight or curly), leftovers dropped
    assert_eq!(
        analyze("Melanie's kids don't like it"),
        vec!["melanie", "kid", "like"]
    );
    assert_eq!(
        analyze("Melanie\u{2019}s kids didn\u{2019}t"),
        vec!["melanie", "kid"]
    );
    // punctuation splits words; speaker prefix and sentence punctuation are not part of tokens
    assert_eq!(
        analyze("Caroline: Hey Mel! Wow, that's great... (really)"),
        vec!["caroline", "hey", "mel", "wow", "great", "really"]
    );
    assert_eq!(
        analyze("When did Caroline go to the LGBTQ support group?"),
        vec!["caroline", "go", "lgbtq", "support", "group"]
    );
}

#[test]
fn english_minimal_plural_stemming() {
    let s = |w: &str| analyze(w).join(" ");
    assert_eq!(s("groups"), "group");
    assert_eq!(s("stories"), "story");
    assert_eq!(s("paintings"), "painting");
    assert_eq!(s("kids"), "kid");
    // unchanged: -ss, -us, words shorter than 4 letters, vowel + "es"
    assert_eq!(s("class"), "class");
    assert_eq!(s("virus"), "virus");
    assert_eq!(s("bus gas yes"), "bus gas yes");
    assert_eq!(s("shoes"), "shoes");
    // singular and plural meet at the same term (index and query side alike)
    assert_eq!(s("book books"), "book");
    assert_eq!(s("hobby hobbies"), "hobby");
    // not applied to tokens with digits or to whole compounds
    assert_eq!(analyze("1990s"), vec!["1990s"]);
    assert_eq!(analyze("docs-site"), vec!["docs-site", "doc", "site"]);
}

#[test]
fn chinese_search_mode() {
    let t = analyze("我们讨论了中华人民共和国的数据库端口配置");
    // search mode: long word plus its dictionary sub-words
    has(
        &t,
        &[
            "中华人民共和国",
            "中华",
            "人民",
            "共和国",
            "讨论",
            "数据库",
            "端口",
            "配置",
        ],
    );
    lacks(&t, &["的", "了", "我们"]);
    let t = analyze("共享记忆库用的是什么数据库？");
    has(&t, &["数据库"]);
    lacks(&t, &["的", "是", "什么", "？"]);
}

#[test]
fn identifiers_whole_plus_parts() {
    assert_eq!(analyze("v1.2.3"), vec!["v1.2.3", "v1", "2", "3"]);
    assert_eq!(
        analyze("surreal.port"),
        vec!["surreal.port", "surreal", "port"]
    );
    assert_eq!(
        analyze("2026-10-08"),
        vec!["2026-10-08", "2026", "10", "08"]
    );
    assert_eq!(
        analyze("bge-reranker-v2-m3"),
        vec!["bge-reranker-v2-m3", "bge", "reranker", "v2", "m3"]
    );
    assert_eq!(
        analyze("see config/test.toml."),
        vec![
            "see",
            "config/test.toml",
            "test.toml",
            "config",
            "test",
            "toml"
        ]
    );
    assert_eq!(
        analyze("snake_case_name"),
        vec!["snake_case_name", "snake", "case", "name"]
    );
    assert_eq!(
        analyze("10.88.21.23"),
        vec!["10.88.21.23", "10", "88", "21", "23"]
    );
    // leading / trailing connectors are not part of the token
    assert_eq!(
        analyze("/usr/local/bin/"),
        vec!["usr/local/bin", "usr", "local", "bin"]
    );
    // compounds are never stop-filtered, their stop-word parts are
    has(&analyze("the.of"), &["the.of"]);
    lacks(&analyze("the.of"), &["the", "of"]);
    has(&analyze("I write C++ and C#"), &["c++", "c#", "c", "write"]);
}

#[test]
fn urls() {
    let t = analyze("docs at https://X.y/z, ok");
    assert_eq!(
        t,
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
    let t = analyze("见https://github.com/foo/bar-baz?tab=readme。谢谢");
    has(
        &t,
        &[
            "https://github.com/foo/bar-baz?tab=readme",
            "github.com/foo/bar-baz?tab=readme",
            "github.com",
            "bar-baz",
            "github",
            "foo",
            "bar",
            "baz",
            "tab",
            "readme",
            "谢谢",
        ],
    );
    // URL stops at CJK text and the trailing full stop is not included
    lacks(&t, &["readme。谢谢"]);
    has(
        &analyze("www.example.com/abc"),
        &[
            "www.example.com/abc",
            "www.example.com",
            "example",
            "com",
            "abc",
        ],
    );
}

#[test]
fn mixed_cjk_latin_and_fullwidth() {
    let t = analyze("把surreal.port改成8000，然后重启agent-memoryd");
    has(
        &t,
        &[
            "surreal.port",
            "surreal",
            "port",
            "8000",
            "改成",
            "重启",
            "agent-memoryd",
            "agent",
            "memoryd",
        ],
    );
    // full-width letters/digits are folded to half-width
    assert_eq!(analyze("ＳｕｒｒｅａｌＤＢ"), vec!["surrealdb"]);
    assert_eq!(analyze("ｖ１．２"), vec!["v1.2", "v1", "2"]);
}

#[test]
fn degenerate_inputs() {
    assert!(analyze("").is_empty());
    assert!(analyze("   ...  --- ，。！").is_empty());
    assert!(analyze("the of and 的 了 是").is_empty());
    // duplicates kept in the stream, removed by analyze()
    assert_eq!(
        analyze_stream("port port PORT"),
        vec!["port", "port", "port"]
    );
    assert_eq!(analyze("port port PORT"), vec!["port"]);
    // very long token is dropped, its parts survive
    let long = format!("{}.{}", "a".repeat(150), "b".repeat(150));
    let t = analyze(&long);
    assert_eq!(t.len(), 2, "{t:?}");
}

#[test]
fn offsets_are_char_boundaries() {
    let s = "配置ｖ１.２ in config/test.toml 和 https://x.y/z 端口";
    let norm: String = s.chars().map(analyzer::halfwidth).collect();
    for (t, a, b) in analyze_with_offsets(s) {
        assert!(
            norm.is_char_boundary(a) && norm.is_char_boundary(b) && a <= b,
            "{t} {a} {b}"
        );
    }
}

// ------------------------------------------------------------------------------------------------
// index

fn doc(id: &str, kind: &str, status: &str, body: &str) -> FtDoc {
    FtDoc {
        id: id.into(),
        kind: kind.into(),
        mtype: if kind == "turn" {
            "turn".into()
        } else {
            "fact".into()
        },
        status: status.into(),
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
fn upsert_is_idempotent_and_delete_works() {
    let (_d, ft) = open_tmp();
    assert!(ft.needs_rebuild()); // new directory
    let all = FtFilter::default();
    ft.upsert(vec![doc(
        "m1",
        "memory",
        "current",
        "SurrealDB 端口是 8000",
    )])
    .unwrap();
    // not visible before commit
    assert!(ft.search("端口", &all, 10).unwrap().is_empty());
    ft.commit().unwrap();
    assert_eq!(ids(&ft.search("端口", &all, 10).unwrap()), vec!["m1"]);

    // same doc again (twice in one batch, and again later): still a single document
    ft.upsert(vec![
        doc("m1", "memory", "current", "SurrealDB 端口是 8000"),
        doc("m1", "memory", "current", "SurrealDB 端口是 8000"),
    ])
    .unwrap();
    ft.commit().unwrap();
    ft.upsert(vec![doc(
        "m1",
        "memory",
        "current",
        "SurrealDB 端口是 8000",
    )])
    .unwrap();
    ft.commit().unwrap();
    assert_eq!(ft.num_docs(), 1);
    assert_eq!(ft.search("surrealdb 端口", &all, 10).unwrap().len(), 1);

    // update replaces the body
    ft.upsert(vec![doc(
        "m1",
        "memory",
        "current",
        "SurrealDB 端口改成 9000",
    )])
    .unwrap();
    ft.commit().unwrap();
    assert!(ft.search("8000", &all, 10).unwrap().is_empty());
    assert_eq!(ids(&ft.search("9000", &all, 10).unwrap()), vec!["m1"]);

    // delete (and deleting a missing id is fine)
    ft.upsert(vec![doc("m2", "memory", "current", "port 9000 on staging")])
        .unwrap();
    ft.commit().unwrap();
    ft.delete(&["m1".to_string(), "nope".to_string()]).unwrap();
    ft.commit().unwrap();
    assert_eq!(ids(&ft.search("9000", &all, 10).unwrap()), vec!["m2"]);
    assert_eq!(ft.num_docs(), 1);

    // commit without changes is a no-op
    ft.commit().unwrap();
    ft.clear().unwrap();
    assert_eq!(ft.num_docs(), 0);
    assert!(ft.search("9000", &all, 10).unwrap().is_empty());
}

#[test]
fn filters() {
    let (_d, ft) = open_tmp();
    let mk = |id: &str,
              kind: &str,
              mtype: &str,
              status: &str,
              scope: &str,
              project: &str,
              platform: &str,
              t: i64| FtDoc {
        id: id.into(),
        kind: kind.into(),
        mtype: mtype.into(),
        status: status.into(),
        scope_kind: scope.into(),
        project: project.into(),
        platform: platform.into(),
        session: if kind == "turn" {
            "s1".into()
        } else {
            String::new()
        },
        effective_at: t,
        body: "deploy the reranker model".into(),
    };
    ft.upsert(vec![
        mk("g1", "memory", "fact", "current", "global", "", "", 100),
        mk(
            "p1", "memory", "fact", "current", "project", "proj1", "", 200,
        ),
        mk(
            "p2",
            "memory",
            "experience",
            "conflict",
            "project",
            "proj2",
            "",
            300,
        ),
        mk(
            "c1",
            "memory",
            "fact",
            "superseded",
            "platform",
            "",
            "codex",
            400,
        ),
        mk(
            "t1", "turn", "turn", "buffer", "turn", "proj1", "codex", 500,
        ),
    ])
    .unwrap();
    ft.commit().unwrap();
    let q = "reranker";
    let set = |f: &FtFilter| -> HashSet<String> {
        ft.search(q, f, 100)
            .unwrap()
            .into_iter()
            .map(|x| x.0)
            .collect()
    };
    let hs = |v: &[&str]| -> HashSet<String> { v.iter().map(|s| s.to_string()).collect() };

    assert_eq!(
        set(&FtFilter::default()),
        hs(&["g1", "p1", "p2", "c1", "t1"])
    );
    assert_eq!(
        set(&FtFilter {
            kinds: vec!["turn".into()],
            ..Default::default()
        }),
        hs(&["t1"])
    );
    assert_eq!(
        set(&FtFilter {
            kinds: vec!["turn".into(), "memory".into()],
            ..Default::default()
        })
        .len(),
        5
    );
    assert_eq!(
        set(&FtFilter {
            types: vec!["experience".into()],
            ..Default::default()
        }),
        hs(&["p2"])
    );
    assert_eq!(
        set(&FtFilter {
            statuses: vec!["current".into(), "conflict".into()],
            ..Default::default()
        }),
        hs(&["g1", "p1", "p2"])
    );
    // visibility: OR of (field, value)
    let vis = FtFilter {
        kinds: vec!["memory".into()],
        visible: vec![
            ("scope_kind".into(), "global".into()),
            ("project".into(), "proj1".into()),
            ("platform".into(), "codex".into()),
        ],
        ..Default::default()
    };
    assert_eq!(set(&vis), hs(&["g1", "p1", "c1"]));
    assert_eq!(
        set(&FtFilter {
            visible: vec![("type".into(), "experience".into())],
            ..Default::default()
        }),
        hs(&["p2"])
    );
    assert_eq!(
        set(&FtFilter {
            visible: vec![("session".into(), "s1".into())],
            ..Default::default()
        }),
        hs(&["t1"])
    );
    assert!(ft
        .search(
            q,
            &FtFilter {
                visible: vec![("bogus".into(), "x".into())],
                ..Default::default()
            },
            10
        )
        .is_err());
    // inclusive time range
    assert_eq!(
        set(&FtFilter {
            time_range: Some((200, 400)),
            ..Default::default()
        }),
        hs(&["p1", "p2", "c1"])
    );
    assert_eq!(
        set(&FtFilter {
            time_range: Some((500, 500)),
            ..Default::default()
        }),
        hs(&["t1"])
    );
    assert!(set(&FtFilter {
        time_range: Some((401, 499)),
        ..Default::default()
    })
    .is_empty());
    assert!(set(&FtFilter {
        time_range: Some((400, 100)),
        ..Default::default()
    })
    .is_empty());
    assert_eq!(
        set(&FtFilter {
            time_range: Some((i64::MIN, i64::MAX)),
            ..Default::default()
        })
        .len(),
        5
    );
    // everything combined
    let all = FtFilter {
        kinds: vec!["memory".into()],
        types: vec!["fact".into()],
        statuses: vec!["current".into(), "conflict".into(), "candidate".into()],
        visible: vec![
            ("scope_kind".into(), "global".into()),
            ("project".into(), "proj1".into()),
        ],
        time_range: Some((150, 1000)),
    };
    assert_eq!(set(&all), hs(&["p1"]));
    // filters do not change scores
    let a = ft.search(q, &FtFilter::default(), 10).unwrap();
    let b = ft.search(q, &all, 10).unwrap();
    let sa = a.iter().find(|x| x.0 == "p1").unwrap().1;
    assert!((sa - b[0].1).abs() < 1e-6, "{sa} vs {}", b[0].1);
}

#[test]
fn bm25_ordering() {
    let (_d, ft) = open_tmp();
    let mut docs = vec![
        doc("both", "memory", "current", "Caroline went to the LGBTQ support group meeting"),
        doc("one", "memory", "current", "Caroline went hiking with friends"),
        doc("rare", "memory", "current", "the support group was a big help"),
        doc("long", "memory", "current", "Caroline talked about painting, her kids, camping, pottery, the beach, music and many other unrelated topics for hours"),
    ];
    for i in 0..20 {
        docs.push(doc(
            &format!("n{i}"),
            "memory",
            "current",
            &format!("Caroline note number {i} about something else"),
        ));
    }
    ft.upsert(docs).unwrap();
    ft.commit().unwrap();
    let r = ft
        .search("Caroline support group", &FtFilter::default(), 10)
        .unwrap();
    // the two docs with the rare terms come first ("caroline" is in almost every doc, so its idf is
    // ~0 and the shorter doc wins on length normalisation); common-term-only docs come after them
    let top2: HashSet<&str> = r[..2].iter().map(|x| x.0.as_str()).collect();
    assert_eq!(top2, ["both", "rare"].into_iter().collect(), "{r:?}");
    assert_eq!(r[2].0, "one", "{r:?}");
    assert!(r[1].1 > 10.0 * r[2].1, "{r:?}");
    // scores are descending and positive
    assert!(r.windows(2).all(|w| w[0].1 >= w[1].1) && r.iter().all(|x| x.1 > 0.0));
    // shorter doc wins for the same single-term match (length normalisation)
    let r = ft.search("Caroline", &FtFilter::default(), 30).unwrap();
    let pos = |id: &str| r.iter().position(|x| x.0 == id).unwrap();
    assert!(pos("one") < pos("long"), "{r:?}");
    // limit respected, limit 0 / stop-word-only / syntax-like queries are safe
    assert_eq!(
        ft.search("Caroline", &FtFilter::default(), 3)
            .unwrap()
            .len(),
        3
    );
    assert!(ft
        .search("Caroline", &FtFilter::default(), 0)
        .unwrap()
        .is_empty());
    assert!(ft
        .search("what is the", &FtFilter::default(), 10)
        .unwrap()
        .is_empty());
    assert!(ft.search("", &FtFilter::default(), 10).unwrap().is_empty());
    let r = ft
        .search(
            "support AND (group OR \"meeting* ~2 title:x [a TO b]",
            &FtFilter::default(),
            10,
        )
        .unwrap();
    assert_eq!(r[0].0, "both");
}

#[test]
fn locomo_style_english_queries() {
    let (_d, ft) = open_tmp();
    let t = |id: &str, body: &str| doc(id, "turn", "buffer", body);
    ft.upsert(vec![
        t(
            "s1-3",
            "Caroline: I attended an LGBTQ support group on 2023-05-07, it was so powerful.",
        ),
        t(
            "s1-4",
            "Melanie: That's awesome! I've been painting a lot lately, mostly sunsets.",
        ),
        t(
            "s2-1",
            "Caroline: Melanie, what did you do with the kids last weekend?",
        ),
        t(
            "s2-2",
            "Melanie: We went camping at the beach and roasted marshmallows.",
        ),
        t(
            "s3-7",
            "Caroline: I'm doing research on adoption agencies, I want to give kids a loving home.",
        ),
        t(
            "s4-1",
            "Melanie: My kids' favourite books are Charlotte's Web and Nothing Is Impossible.",
        ),
    ])
    .unwrap();
    ft.commit().unwrap();
    let all = FtFilter::default();
    let top = |q: &str| ft.search(q, &all, 10).unwrap()[0].0.clone();
    assert_eq!(
        top("When did Caroline go to the LGBTQ support group?"),
        "s1-3"
    );
    assert_eq!(top("What did Melanie paint recently? sunset"), "s1-4");
    assert_eq!(top("Where did Melanie go camping?"), "s2-2");
    assert_eq!(top("What did Caroline research?"), "s3-7");
    assert_eq!(top("What book did Melanie's kids like?"), "s4-1");
    assert_eq!(top("adoption agency"), "s3-7");
    // stop-word-only questions match nothing
    assert!(ft.search("What did she do?", &all, 10).unwrap().is_empty());
}

#[test]
fn identifier_and_chinese_queries() {
    let (_d, ft) = open_tmp();
    ft.upsert(vec![
        doc(
            "a",
            "memory",
            "current",
            "测试环境的配置文件在 config/test.toml，surreal.port 设为 8001",
        ),
        doc("b", "memory", "current", "生产环境配置在 config/prod.toml"),
        doc(
            "c",
            "memory",
            "current",
            "升级到 v1.2.3 之后重排序模型换成 bge-reranker-v2-m3",
        ),
        doc(
            "d",
            "turn",
            "buffer",
            "用户：明天下午三点开会讨论数据库迁移",
        ),
    ])
    .unwrap();
    ft.commit().unwrap();
    let all = FtFilter::default();
    assert_eq!(ft.search("test.toml", &all, 10).unwrap()[0].0, "a");
    assert_eq!(
        ft.search("config/test.toml 在哪", &all, 10).unwrap()[0].0,
        "a"
    );
    assert_eq!(ft.search("surreal.port", &all, 10).unwrap()[0].0, "a");
    assert_eq!(ft.search("v1.2.3", &all, 10).unwrap()[0].0, "c");
    assert_eq!(ft.search("用的哪个 reranker", &all, 10).unwrap()[0].0, "c");
    assert_eq!(
        ft.search("数据库迁移什么时候讨论", &all, 10).unwrap()[0].0,
        "d"
    );
    assert_eq!(ft.search("生产配置", &all, 10).unwrap()[0].0, "b");
    assert_eq!(
        ft.search("ＳＵＲＲＥＡＬ．ＰＯＲＴ", &all, 10).unwrap()[0].0,
        "a"
    );
}

#[test]
fn persistence_and_version_reset() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ft");
    {
        let ft = FullText::open(&p).unwrap();
        assert!(ft.needs_rebuild());
        ft.upsert(vec![doc(
            "m1",
            "memory",
            "current",
            "persisted memory about kayaking",
        )])
        .unwrap();
        ft.commit().unwrap();
    }
    {
        let ft = FullText::open(&p).unwrap();
        assert!(!ft.needs_rebuild());
        assert_eq!(
            ids(&ft.search("kayaking", &FtFilter::default(), 5).unwrap()),
            vec!["m1"]
        );
    }
    // analyzer/schema version change → wiped and flagged
    std::fs::write(p.join("memory_fulltext.version"), "old-analyzer\n").unwrap();
    {
        let ft = FullText::open(&p).unwrap();
        assert!(ft.needs_rebuild());
        assert_eq!(ft.num_docs(), 0);
    }
    // corrupt index → wiped and flagged
    std::fs::write(p.join("meta.json"), "{ not json").unwrap();
    let ft = FullText::open(&p).unwrap();
    assert!(ft.needs_rebuild());
    ft.upsert(vec![doc("m2", "memory", "current", "fresh start")])
        .unwrap();
    ft.commit().unwrap();
    assert_eq!(ft.num_docs(), 1);
}

#[test]
fn concurrent_writers_and_readers() {
    let (_d, ft) = open_tmp();
    let ft = Arc::new(ft);
    let mut hs = Vec::new();
    for w in 0..4 {
        let ft = ft.clone();
        hs.push(std::thread::spawn(move || {
            for i in 0..50 {
                ft.upsert(vec![doc(
                    &format!("w{w}-{i}"),
                    "memory",
                    "current",
                    &format!("writer {w} item {i} 并发写入"),
                )])
                .unwrap();
                if i % 10 == 0 {
                    ft.commit().unwrap();
                }
                let _ = ft.search("并发", &FtFilter::default(), 5).unwrap();
            }
        }));
    }
    for h in hs {
        h.join().unwrap();
    }
    ft.commit().unwrap();
    assert_eq!(ft.num_docs(), 200);
    assert_eq!(
        ft.search("并发写入", &FtFilter::default(), 500)
            .unwrap()
            .len(),
        200
    );
}

fn _assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<FullText>();
    is::<EntityDict>();
}

// ------------------------------------------------------------------------------------------------
// entity dictionary

fn names(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn normalize() {
    assert_eq!(normalize_name("ＳｕｒｒｅａｌＤＢ"), "surrealdb");
    assert_eq!(normalize_name("  Surreal  DB\t"), "surrealdb");
    assert_eq!(normalize_name("共享　记忆库"), "共享记忆库");
    assert_eq!(normalize_name("BGE-M3"), "bge-m3");
}

#[test]
fn entity_resolve_and_longest_match() {
    let mut d = EntityDict::new();
    d.upsert("e:surreal", &names(&["SurrealDB", "Surreal"]));
    d.upsert("e:memory", &names(&["共享", "共享记忆库", "Memory"]));
    d.upsert("e:mel", &names(&["Melanie", "Mel"]));
    d.upsert("e:al", &names(&["Al"]));
    d.upsert("e:mem", &names(&["记忆库"]));

    assert_eq!(
        d.resolve("ＳｕｒｒｅａｌＤＢ").as_deref(),
        Some("e:surreal")
    );
    assert_eq!(d.resolve("surreal db").as_deref(), Some("e:surreal"));
    assert_eq!(d.resolve("共享 记忆库").as_deref(), Some("e:memory"));
    assert_eq!(d.resolve("nobody"), None);

    // full-width text, whitespace inside the name, longest alias wins
    assert_eq!(
        d.longest_match("我们用ＳｕｒｒｅａｌＤＢ做存储"),
        vec!["e:surreal"]
    );
    assert_eq!(
        d.longest_match("we moved to Surreal DB last week"),
        vec!["e:surreal"]
    );
    // 共享记忆库 (longest) consumes 记忆库: e:mem not reported
    assert_eq!(d.longest_match("共享记忆库的设计"), vec!["e:memory"]);
    assert_eq!(d.longest_match("记忆库和共享"), vec!["e:mem", "e:memory"]);

    // ASCII word boundaries
    assert!(d
        .longest_match("also, Melanie's friend")
        .contains(&"e:mel".to_string()));
    assert_eq!(d.longest_match("also"), Vec::<String>::new());
    assert_eq!(d.longest_match("Melanie"), vec!["e:mel"]);
    assert_eq!(d.longest_match("Mel said hi to Al."), vec!["e:mel", "e:al"]);
    assert_eq!(d.longest_match("Melody and Alfred"), Vec::<String>::new());
    assert_eq!(d.longest_match("Al_x and xAl"), Vec::<String>::new());
    // CJK neighbours never block an ASCII alias
    assert_eq!(d.longest_match("昨天Al和Mel来了"), vec!["e:al", "e:mel"]);
    // dedup in order of appearance
    assert_eq!(
        d.longest_match("Mel, Al, Melanie, Al, SurrealDB"),
        vec!["e:mel", "e:al", "e:surreal"]
    );
}

#[test]
fn entity_boundary_falls_back_to_shorter_alias() {
    let mut d = EntityDict::new();
    d.upsert("e:al", &names(&["Al"]));
    d.upsert("e:algreen", &names(&["Al Green"]));
    // "Al Green" is glued to "wood" → rejected; the shorter "Al" at the same position is valid
    assert_eq!(d.longest_match("Al Greenwood"), vec!["e:al"]);
    assert_eq!(d.longest_match("Al Green sang"), vec!["e:algreen"]);
}

#[test]
fn entity_update_remove_and_shared_alias() {
    let mut d = EntityDict::new();
    d.upsert("e1", &names(&["Jon"]));
    d.upsert("e1", &names(&["Jonathan"])); // union
    assert_eq!(d.resolve("jon").as_deref(), Some("e1"));
    assert_eq!(d.resolve("Jonathan").as_deref(), Some("e1"));
    d.replace("e1", &names(&["Jonathan"]));
    assert_eq!(d.resolve("jon"), None);
    // shared alias: resolve → most recent, longest_match → all (most recent first)
    d.upsert("e2", &names(&["Jonathan"]));
    assert_eq!(d.resolve("jonathan").as_deref(), Some("e2"));
    assert_eq!(d.resolve_all("jonathan"), vec!["e2", "e1"]);
    assert_eq!(d.longest_match("hi Jonathan"), vec!["e2", "e1"]);
    d.remove("e2");
    assert_eq!(d.longest_match("hi Jonathan"), vec!["e1"]);
    d.remove("e1");
    assert!(d.longest_match("hi Jonathan").is_empty());
    assert!(d.is_empty());
    // empty names ignored
    d.upsert("e3", &names(&["", "  "]));
    assert!(d.longest_match("anything").is_empty());
}

// ------------------------------------------------------------------------------------------------
// latency (run: cargo test --profile fast -p memory-fulltext -- --ignored --nocapture)

#[test]
#[ignore]
fn bench_latency() {
    use std::time::Instant;
    let (_d, ft) = open_tmp();
    let t0 = Instant::now();
    warm_up();
    println!("jieba warm-up: {:?}", t0.elapsed());
    let en = [
        "Caroline",
        "Melanie",
        "painting",
        "camping",
        "support group",
        "adoption",
        "pottery",
        "beach",
        "kids",
        "career",
        "counseling",
        "LGBTQ",
        "sunrise",
        "concert",
        "friends",
    ];
    let zh = [
        "数据库",
        "端口",
        "配置",
        "重排序",
        "向量",
        "部署",
        "测试环境",
        "记忆库",
        "检索",
        "会议",
        "迁移",
        "项目",
    ];
    let mut docs = Vec::new();
    for i in 0..20_000u32 {
        let a = en[(i as usize * 7) % en.len()];
        let b = en[(i as usize * 13 + 3) % en.len()];
        let c = zh[(i as usize * 5) % zh.len()];
        let e = zh[(i as usize * 11 + 1) % zh.len()];
        let body = format!(
            "Turn {i}: {a} talked about {b} on 2026-10-{:02}; 用户提到{c}和{e}，版本 v1.{}.{} 路径 src/mod{}.rs",
            i % 28 + 1, i % 10, i % 7, i % 50
        );
        docs.push(FtDoc {
            id: format!("d{i}"),
            kind: if i % 3 == 0 {
                "turn".into()
            } else {
                "memory".into()
            },
            mtype: "fact".into(),
            status: if i % 10 == 0 {
                "superseded".into()
            } else {
                "current".into()
            },
            scope_kind: if i % 2 == 0 {
                "global".into()
            } else {
                "project".into()
            },
            project: format!("p{}", i % 5),
            platform: "locomo".into(),
            session: format!("s{}", i % 40),
            effective_at: 1_700_000_000 + i as i64 * 60,
            body,
        });
    }
    let t = Instant::now();
    for chunk in docs.chunks(64) {
        ft.upsert(chunk.to_vec()).unwrap();
    }
    ft.commit().unwrap();
    println!("index 20k docs (64/batch, 1 commit): {:?}", t.elapsed());
    let t = Instant::now();
    for chunk in docs[..2000].chunks(64) {
        ft.upsert(chunk.to_vec()).unwrap();
        ft.commit().unwrap();
    }
    println!(
        "re-upsert 2k docs, commit per 64-doc batch: {:?}",
        t.elapsed()
    );
    assert_eq!(ft.num_docs(), 20_000);

    let filter = FtFilter {
        kinds: vec!["memory".into()],
        statuses: vec!["current".into(), "conflict".into(), "candidate".into()],
        visible: vec![
            ("scope_kind".into(), "global".into()),
            ("project".into(), "p1".into()),
        ],
        time_range: None,
        ..Default::default()
    };
    let queries: Vec<String> = (0..5000)
        .map(|i| {
            if i % 2 == 0 {
                format!(
                    "When did {} talk about {}?",
                    en[i % en.len()],
                    en[(i * 3 + 1) % en.len()]
                )
            } else {
                format!(
                    "{}的{}是什么 v1.{}",
                    zh[i % zh.len()],
                    zh[(i * 7) % zh.len()],
                    i % 10
                )
            }
        })
        .collect();
    let mut lat = Vec::with_capacity(queries.len());
    let t = Instant::now();
    let mut hits = 0usize;
    for (i, q) in queries.iter().enumerate() {
        let s = Instant::now();
        let f = if i % 4 == 0 {
            FtFilter::default()
        } else {
            filter.clone()
        };
        hits += ft.search(q, &f, 30).unwrap().len();
        lat.push(s.elapsed().as_secs_f64() * 1000.0);
    }
    let total = t.elapsed();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| lat[((lat.len() as f64 - 1.0) * p) as usize];
    println!(
        "5000 queries top-30: total {:?}, mean {:.3} ms, p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, max {:.3} ms, avg hits {:.1}",
        total,
        total.as_secs_f64() * 1000.0 / lat.len() as f64,
        pct(0.5),
        pct(0.95),
        pct(0.99),
        lat[lat.len() - 1],
        hits as f64 / lat.len() as f64
    );
    let t = Instant::now();
    let mut n = 0;
    for q in &queries {
        n += analyze(q).len();
    }
    println!("analyze 5000 queries: {:?} ({n} terms)", t.elapsed());

    let mut d = EntityDict::new();
    for i in 0..2000 {
        d.upsert(
            &format!("e{i}"),
            &[format!("Entity{i}"), format!("实体{i}")],
        );
    }
    d.upsert("caro", &["Caroline".to_string()]);
    let _ = d.longest_match("warm");
    let t = Instant::now();
    for q in &queries {
        let _ = d.longest_match(q);
    }
    println!(
        "entity longest_match x5000 (2k entities): {:?}",
        t.elapsed()
    );
}
