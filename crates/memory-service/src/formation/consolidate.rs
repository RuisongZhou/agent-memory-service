//! Daily maintenance (4.5), idle sessions (3.3), successful-buffer purge (3.2).
use super::store::{self, to_doc, Tx};
use super::{engine::Engine, extract, indexer, prompts, tenant::Tenant, types::*, util};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

fn normalized(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn profile_length_exceeded(text: &str, variant: &str, max_chars: usize) -> bool {
    if variant == "en" {
        text.split_whitespace().count() > 350
    } else {
        text.chars().count() > max_chars
    }
}

fn ids_from(v: &Value, map: &HashMap<String, String>) -> Result<Vec<String>> {
    let a = v
        .as_array()
        .ok_or_else(|| anyhow!("missing source id array"))?;
    let mut out = Vec::new();
    for id in a {
        let id = id
            .as_str()
            .ok_or_else(|| anyhow!("source id must be a string"))?;
        let real = map
            .get(id)
            .ok_or_else(|| anyhow!("unknown source id {id}"))?;
        if !out.contains(real) {
            out.push(real.clone());
        }
    }
    Ok(out)
}

async fn profile(eng: &Arc<Engine>, t: &Arc<Tenant>, scope: MemScope, now: &str) -> Result<Value> {
    let mut facts = t
        .store
        .find_memories("type = 'fact' AND status = 'current'", vec![])
        .await?
        .into_iter()
        .filter(|m| m.scope == scope)
        .collect::<Vec<_>>();
    facts.sort_by(|a, b| b.effective_at.cmp(&a.effective_at).then(a.id.cmp(&b.id)));
    let old = t
        .store
        .find_memories("type = 'profile' AND status = 'current'", vec![])
        .await?
        .into_iter()
        .filter(|m| m.scope == scope)
        .collect::<Vec<_>>();
    if facts.is_empty() {
        let mut tx = Tx::new();
        for m in old {
            tx.merge("memories", &m.id, json!({"status":S_SUPERSEDED}))?;
        }
        t.store.commit(tx).await?;
        return Ok(json!({"skipped":"no current facts"}));
    }
    let p = eng.prompts_for(t);
    let project = scope.kind == "project";
    let map: HashMap<_, _> = facts
        .iter()
        .enumerate()
        .map(|(i, m)| (format!("G{}", i + 1), m.id.clone()))
        .collect();
    let input = facts
        .iter()
        .enumerate()
        .map(|(i, m)| format!("G{} | {} | {}", i + 1, m.effective_at, m.text))
        .collect::<Vec<_>>()
        .join("\n");
    let base_input = prompts::block(
        if project {
            p.l.project_facts
        } else {
            p.l.global_facts
        },
        &input,
    );
    let stage = if project { "P3-project" } else { "P3" };
    let system = if project { p.p3_project } else { p.p3 };
    let mut out = extract::call_logged(
        eng,
        t,
        if project { "P3-project" } else { "P3" },
        if project { p.p3_project } else { p.p3 },
        &prompts::block(
            if project {
                p.l.project_facts
            } else {
                p.l.global_facts
            },
            &input,
        ),
        2048,
    )
    .await?;
    // A transport retry repeats the same invalid result. Feed back the violated
    // output bound while retaining the original prompt, facts and citation checks.
    for repair in 1..=2 {
        let Some(previous) = out["profile"].as_str() else {
            break;
        };
        if !profile_length_exceeded(previous, &p.variant, eng.cfg.profile_max_chars) {
            break;
        }
        let (actual, limit, unit) = if p.variant == "en" {
            (previous.split_whitespace().count(), 350, "words")
        } else {
            (
                previous.chars().count(),
                eng.cfg.profile_max_chars,
                "Unicode characters",
            )
        };
        t.store
            .log(
                "extract_log",
                json!({"stage":"P3_length_validation","createdAt":util::now_str(),
            "promptVariant":p.variant,"repair":repair,"actual":actual,"limit":limit,"unit":unit}),
            )
            .await?;
        let retry_input = format!("{base_input}\n<output validation>\nThe previous JSON below contained a profile of {actual} {unit}, exceeding the original limit of {limit}. Reissue the same JSON schema with a shorter profile within that limit. Follow the original priorities. Use only the supplied facts and list only IDs actually used in the shorter text.\nPrevious JSON: {out}\n</output validation>");
        out = extract::call_logged(eng, t, stage, system, &retry_input, 2048).await?;
    }
    let text = out["profile"]
        .as_str()
        .ok_or_else(|| anyhow!("P3 missing profile text"))?;
    if text.trim().is_empty() {
        bail!("P3 empty profile");
    }
    // English prompt defines 350 words; Chinese defines cfg.profile_max_chars characters.
    if profile_length_exceeded(text, &p.variant, eng.cfg.profile_max_chars) {
        bail!("P3 profile exceeds prompt limit");
    }
    let used = ids_from(&out["memoryIds"], &map)?;
    if used.is_empty() {
        bail!("P3 profile without evidence");
    }
    let mut pairs = Vec::new();
    if let Some(conflicts) = out["conflicts"].as_array() {
        for pair in conflicts {
            let ids = ids_from(pair, &map)?;
            if ids.len() != 2 {
                bail!("P3 conflict requires two facts");
            }
            pairs.push(json!({"ids":ids,"profile":true}));
        }
    }
    let id = util::new_id("m");
    let rec = MemoryRec {
        id: id.clone(),
        mtype: T_PROFILE.into(),
        text: text.into(),
        scope: scope.clone(),
        status: S_CURRENT.into(),
        effective_at: util::date_of(now),
        recorded_at: now.into(),
        stated_at: now.into(),
        last_accessed_at: now.into(),
        profile_kind: Some(if project { "project" } else { "user" }.into()),
        used_memory_ids: used,
        source: Source {
            platform: "maintenance".into(),
            origin: ORIGIN_INFERRED.into(),
            turn_ids: vec![],
        },
        supersedes: old.first().map(|m| m.id.clone()),
        ..Default::default()
    };
    let mut tx = Tx::new();
    tx.put(
        "memories",
        &id,
        super::relations::strip_nulls(to_doc(&rec)?),
    )?;
    for m in old {
        tx.merge(
            "memories",
            &m.id,
            store::patch(
                json!({"status":S_SUPERSEDED,"supersededBy":id,"validUntil":rec.effective_at}),
            ),
        )?;
    }
    let key = store::hashed_id("pc", &serde_json::to_string(&scope)?);
    tx.put(
        "index_meta",
        &key,
        json!({"mid":key,"kind":"profile_conflicts","pairs":pairs}),
    )?;
    t.store.commit(tx).await?;
    // Profile-discovered contradictions are sent to P4 now, even when run_jobs only asked for P3.
    // The proposal is kept for review and never changes either source fact.
    for pair in &pairs {
        let a = pair["ids"][0].as_str().unwrap();
        let b = pair["ids"][1].as_str().unwrap();
        let ma = facts.iter().find(|m| m.id == a).unwrap();
        let mb = facts.iter().find(|m| m.id == b).unwrap();
        let input = format!(
            "P1 {}\nA | {} | {}\nB | {} | {}",
            p.l.profile_mark, ma.effective_at, ma.text, mb.effective_at, mb.text
        );
        let out = extract::call_logged(eng, t, "P4-profile", p.p4, &input, 2048).await?;
        let proposal = out["pairs"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v["pair"] == "P1"))
            .ok_or_else(|| anyhow!("P4 missing requested profile pair"))?;
        persist_proposal(t, a, b, proposal).await?;
    }
    Ok(json!({"memoryId":id,"conflicts":pairs.len()}))
}

async fn persist_proposal(t: &Arc<Tenant>, a: &str, b: &str, proposal: &Value) -> Result<()> {
    let decision = proposal["decision"].as_str().unwrap_or("");
    if !matches!(decision, "merge" | "updates" | "related" | "keep")
        || proposal["reason"].as_str().unwrap_or("").trim().is_empty()
    {
        bail!("invalid P4 decision/reason");
    }
    if decision == "merge"
        && (!matches!(proposal["base"].as_str(), Some("A" | "B"))
            || proposal["mergedText"]
                .as_str()
                .unwrap_or("")
                .trim()
                .is_empty())
    {
        bail!("P4 merge needs base and text");
    }
    let mut value = proposal.clone();
    value["memoryIds"] = json!([a, b]);
    value["status"] = json!(if decision == "keep" {
        "dismissed"
    } else {
        "pending"
    });
    let id = store::hashed_id("p", &format!("{a}::{b}"));
    value["mid"] = json!(id);
    value["createdAt"] = json!(util::now_str());
    let mut tx = Tx::new();
    tx.put("proposals", &id, value.clone())?;
    tx.merge("memories", a, json!({"proposal":value}))?;
    t.store.commit(tx).await
}

/// Only whitespace-identical bodies auto-merge. Preserve both original bodies and all evidence.
async fn merge_exact(t: &Arc<Tenant>, a: &MemoryRec, b: &MemoryRec) -> Result<()> {
    let mut source = a.source.clone();
    for id in &b.source.turn_ids {
        if !source.turn_ids.contains(id) {
            source.turn_ids.push(id.clone());
        }
    }
    let mut entities = a.entity_ids.clone();
    for id in &b.entity_ids {
        if !entities.contains(id) {
            entities.push(id.clone());
        }
    }
    let mut tx = Tx::new();
    tx.merge("memories",&a.id,store::patch(json!({"source":source,"entityIds":entities,"evidenceCount":a.evidence_count+b.evidence_count})))?;
    tx.merge(
        "memories",
        &b.id,
        json!({"status":S_SUPERSEDED,"supersededBy":a.id}),
    )?;
    tx.raw(
        &format!(
            "UPDATE memories:{} SET embedding = NONE RETURN NONE",
            store::safe_id(&b.id)?
        ),
        vec![],
    );
    let evidence = t
        .store
        .select(
            "SELECT * OMIT id FROM evidence WHERE memoryIds CONTAINS $id",
            vec![("id", json!(b.id))],
        )
        .await?;
    for e in evidence {
        if let (Some(id), Some(ids)) = (e["mid"].as_str(), e["memoryIds"].as_array()) {
            let mut ids = ids.clone();
            if !ids.contains(&json!(a.id)) {
                ids.push(json!(a.id));
            }
            tx.merge("evidence", id, json!({"memoryIds":ids}))?;
        }
    }
    tx.index_task("memories", &a.id, "upsert")?;
    tx.index_task("memories", &b.id, "upsert")?;
    t.store.commit(tx).await?;
    t.vecs.write().await.memories.remove(&b.id);
    t.index_notify.notify_one();
    Ok(())
}

async fn merge(eng: &Arc<Engine>, t: &Arc<Tenant>) -> Result<Value> {
    let mut active = t
        .store
        .find_memories(
            "status = 'current' AND type IN ['fact','experience']",
            vec![],
        )
        .await?;
    active.sort_by(|a, b| {
        origin_priority(&b.source.origin)
            .cmp(&origin_priority(&a.source.origin))
            .then(b.evidence_count.cmp(&a.evidence_count))
            .then(b.stated_at.cmp(&a.stated_at))
            .then(a.id.cmp(&b.id))
    });
    let mut removed = HashSet::new();
    let mut exact = 0;
    for i in 0..active.len() {
        if removed.contains(&active[i].id) {
            continue;
        }
        for j in i + 1..active.len() {
            if removed.contains(&active[j].id)
                || active[i].scope != active[j].scope
                || active[i].mtype != active[j].mtype
                || active[i].supersedes.is_some()
                || active[j].supersedes.is_some()
                || active[i].superseded_by.is_some()
                || active[j].superseded_by.is_some()
            {
                continue;
            }
            if normalized(&active[i].text) == normalized(&active[j].text)
                && (active[i].mtype != T_EXPERIENCE || active[i].detail == active[j].detail)
            {
                merge_exact(t, &active[i], &active[j]).await?;
                active[i] = t.store.get_memory(&active[i].id).await?.unwrap();
                removed.insert(active[j].id.clone());
                exact += 1;
            }
        }
    }
    let current: HashMap<String, MemoryRec> = active
        .into_iter()
        .filter(|m| !removed.contains(&m.id))
        .map(|m| (m.id.clone(), m))
        .collect();
    let mut pairs = Vec::<(String, String, bool)>::new();
    let conflicts = t
        .store
        .select(
            "SELECT pairs FROM index_meta WHERE kind = 'profile_conflicts'",
            vec![],
        )
        .await?;
    for group in conflicts {
        if let Some(ps) = group["pairs"].as_array() {
            for p in ps {
                if let (Some(a), Some(b)) = (p["ids"][0].as_str(), p["ids"][1].as_str()) {
                    if current.contains_key(a)
                        && current.contains_key(b)
                        && current[a].proposal.is_none()
                    {
                        pairs.push((a.into(), b.into(), true));
                    }
                }
            }
        }
    }
    let mut mems = current.values().collect::<Vec<_>>();
    mems.sort_by(|a, b| a.id.cmp(&b.id));
    let cache = t.vecs.read().await;
    for (i, a) in mems.iter().enumerate() {
        for b in &mems[i + 1..] {
            if pairs.len() >= 200 {
                break;
            }
            if a.scope != b.scope
                || a.mtype != b.mtype
                || a.proposal.is_some()
                || b.proposal.is_some()
            {
                continue;
            }
            // Conservative engineering candidate threshold, not an automatic merge threshold.
            if let (Some(x), Some(y)) = (cache.memories.get(&a.id), cache.memories.get(&b.id)) {
                if util::cosine(x, y) >= 0.92
                    && !pairs
                        .iter()
                        .any(|(x, y, _)| (x == &a.id && y == &b.id) || (x == &b.id && y == &a.id))
                {
                    pairs.push((a.id.clone(), b.id.clone(), false));
                }
            }
        }
    }
    drop(cache);
    let p = eng.prompts_for(t);
    let mut proposals = 0;
    for batch in pairs.chunks(10) {
        let input = batch
            .iter()
            .enumerate()
            .map(|(i, (a, b, profile))| {
                // Experience text is only situation:lesson. P4 must also see the actual approach
                // and verified result so a failure cannot be proposed as a duplicate of a success.
                let describe = |m: &MemoryRec| {
                    if m.mtype == T_EXPERIENCE {
                        format!(
                            "{} | detail: {}",
                            m.text,
                            m.detail.as_ref().unwrap_or(&Value::Null)
                        )
                    } else {
                        m.text.clone()
                    }
                };
                format!(
                    "P{} {}\nA | {} | {}\nB | {} | {}",
                    i + 1,
                    if *profile { p.l.profile_mark } else { "" },
                    current[a].effective_at,
                    describe(&current[a]),
                    current[b].effective_at,
                    describe(&current[b])
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let out = extract::call_logged(eng, t, "P4", p.p4, &input, 4096).await?;
        for proposal in out["pairs"]
            .as_array()
            .ok_or_else(|| anyhow!("P4 missing pairs"))?
        {
            let Some(idx) = proposal["pair"]
                .as_str()
                .and_then(|s| s.strip_prefix('P'))
                .and_then(|s| s.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1))
            else {
                continue;
            };
            let Some((a, b, _)) = batch.get(idx) else {
                continue;
            };
            let decision = proposal["decision"].as_str().unwrap_or("");
            if !matches!(decision, "merge" | "updates" | "related" | "keep") {
                continue;
            }
            if decision == "merge"
                && (!matches!(proposal["base"].as_str(), Some("A" | "B"))
                    || proposal["mergedText"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .is_empty())
            {
                continue;
            }
            persist_proposal(t, a, b, proposal).await?;
            proposals += 1;
        }
    }
    Ok(json!({"autoMerged":exact,"proposals":proposals}))
}

async fn skills(eng: &Arc<Engine>, t: &Arc<Tenant>, now: &str) -> Result<Value> {
    let mut exp = t
        .store
        .find_memories("type = 'experience' AND status = 'current'", vec![])
        .await?;
    exp.sort_by(|a, b| a.id.cmp(&b.id));
    let mut existing = t
        .store
        .find_memories(
            "type = 'skill' AND status IN ['current','candidate']",
            vec![],
        )
        .await?;
    let cache = t.vecs.read().await;
    let mut groups: Vec<Vec<MemoryRec>> = Vec::new();
    for e in exp {
        let g = groups.iter().position(|g| {
            g[0].scope == e.scope
                && g.iter().any(|m| {
                    let scene = m
                        .detail
                        .as_ref()
                        .and_then(|d| d["situation"].as_str())
                        .zip(e.detail.as_ref().and_then(|d| d["situation"].as_str()))
                        .map(|(a, b)| a == b && !a.is_empty())
                        .unwrap_or(false);
                    let cosine = cache
                        .memories
                        .get(&m.id)
                        .zip(cache.memories.get(&e.id))
                        .map(|(a, b)| util::cosine(a, b) >= 0.80)
                        .unwrap_or(false);
                    scene || cosine
                })
        });
        if let Some(i) = g {
            groups[i].push(e);
        } else {
            groups.push(vec![e]);
        }
    }
    drop(cache);
    let p = eng.prompts_for(t);
    let mut created = Vec::new();
    for group in groups
        .into_iter()
        .filter(|g| g.len() >= eng.cfg.skill_min_experiences)
    {
        let keys = group.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
        let key = store::hashed_id("sg", &keys.join(","));
        if t.store.get_doc("index_meta", &key).await?.is_some() {
            continue;
        }
        let map: HashMap<_, _> = group
            .iter()
            .enumerate()
            .map(|(i, m)| (format!("E{}", i + 1), m.id.clone()))
            .collect();
        let good: HashSet<_> = group
            .iter()
            .filter(|m| {
                matches!(
                    m.detail.as_ref().and_then(|d| d["result"].as_str()),
                    Some(result) if ["success","partial"].iter().any(|label| result == *label ||
                        result.strip_prefix(label).map(|tail| tail.starts_with(':') || tail.starts_with('：') || tail.starts_with(' ')).unwrap_or(false))
                )
            })
            .map(|m| m.id.clone())
            .collect();
        let input = group
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "E{} | {}",
                    i + 1,
                    m.detail
                        .as_ref()
                        .map(Value::to_string)
                        .unwrap_or_else(|| m.text.clone())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let visible = existing
            .iter()
            .filter(|m| m.scope == group[0].scope || m.scope.kind == "global")
            .collect::<Vec<_>>();
        let smap: HashMap<_, _> = visible
            .iter()
            .enumerate()
            .map(|(i, m)| (format!("S{}", i + 1), m.id.clone()))
            .collect();
        let old = visible
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    "S{} | {}",
                    i + 1,
                    m.detail
                        .as_ref()
                        .map(Value::to_string)
                        .unwrap_or_else(|| m.text.clone())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let out = extract::call_logged(
            eng,
            t,
            "P5",
            p.p5,
            &format!(
                "{}\n{}",
                prompts::block(p.l.experiences, &input),
                prompts::block(
                    p.l.existing_skills,
                    if old.is_empty() { p.l.none } else { &old }
                )
            ),
            4096,
        )
        .await?;
        if out["skill"].is_null() {
            t.store
                .put_doc(
                    "index_meta",
                    &key,
                    &json!({"mid":key,"kind":"skill_group","reason":out["reason"]}),
                )
                .await?;
            continue;
        }
        let mut d = out["skill"].clone();
        let name = d["name"]
            .as_str()
            .ok_or_else(|| anyhow!("P5 missing name"))?
            .to_string();
        let pre = d["preconditions"]
            .as_str()
            .ok_or_else(|| anyhow!("P5 missing preconditions"))?
            .to_string();
        let n = d["steps"]
            .as_array()
            .ok_or_else(|| anyhow!("P5 missing steps"))?
            .len();
        if !(2..=7).contains(&n) || name.trim().is_empty() || pre.trim().is_empty() {
            bail!("invalid P5 skill");
        }
        let mut used = Vec::new();
        for section in ["steps", "cautions"] {
            let Some(rows) = d[section].as_array_mut() else {
                if section == "steps" {
                    bail!("P5 missing steps");
                }
                continue;
            };
            for row in rows {
                let text = row["text"]
                    .as_str()
                    .ok_or_else(|| anyhow!("P5 missing step text"))?
                    .to_string();
                let ids = ids_from(&row["sourceMemoryIds"], &map)?;
                if ids.is_empty() {
                    bail!("P5 unsupported step");
                }
                let branch = text.trim_start().starts_with("如果")
                    || text.trim_start().to_ascii_lowercase().starts_with("if ");
                if section == "steps"
                    && !branch
                    && ids.iter().filter(|id| good.contains(*id)).count() < 2
                {
                    bail!("P5 main step needs two successful sources");
                }
                for id in &ids {
                    if !used.contains(id) {
                        used.push(id.clone());
                    }
                }
                row["sourceMemoryIds"] = json!(ids);
            }
        }
        if let Some(label) = d["extends"].as_str() {
            d["extends"] = json!(smap
                .get(label)
                .ok_or_else(|| anyhow!("P5 invalid extends"))?);
        }
        let id = util::new_id("m");
        let rec = MemoryRec {
            id: id.clone(),
            mtype: T_SKILL.into(),
            text: format!("{name}: {pre}"),
            detail: Some(d),
            status: S_CANDIDATE.into(),
            scope: group[0].scope.clone(),
            effective_at: util::date_of(now),
            recorded_at: now.into(),
            stated_at: now.into(),
            last_accessed_at: now.into(),
            used_memory_ids: used,
            source: Source {
                platform: "maintenance".into(),
                origin: ORIGIN_INFERRED.into(),
                turn_ids: vec![],
            },
            ..Default::default()
        };
        let mut tx = Tx::new();
        tx.put(
            "memories",
            &id,
            super::relations::strip_nulls(to_doc(&rec)?),
        )?;
        tx.index_task("memories", &id, "upsert")?;
        tx.put(
            "index_meta",
            &key,
            json!({"mid":key,"kind":"skill_group","memoryId":id}),
        )?;
        t.store.commit(tx).await?;
        // Later groups in the same run must avoid overlap with skills just generated above.
        existing.push(rec);
        created.push(id);
    }
    t.index_notify.notify_one();
    Ok(json!({"created":created}))
}

pub async fn purge(t: &Arc<Tenant>, as_of: &str, days: f64) -> Result<Value> {
    let cutoff = util::ts_secs(as_of) - (days * 86400.0) as i64;
    let turns: Vec<TurnRec> = t
        .store
        .select_as(
            "SELECT * OMIT id, embedding FROM turns WHERE extractState = 'done'",
            vec![],
        )
        .await?;
    let mut tx = Tx::new();
    let mut removed = Vec::new();
    for tr in turns {
        if tr.extracted_at.is_none() {
            continue;
        }
        // Normal buffer age uses source-turn end; zero is the explicit evaluation purge extension.
        if days > 0.0 && util::ts_secs(&tr.ended_at) > cutoff {
            continue;
        }
        let Some(s) = t.store.get_session(&tr.platform, &tr.session).await? else {
            continue;
        };
        if s.pending_tasks
            .iter()
            .any(|task| task.turn_ids.contains(&tr.turn_id))
        {
            continue; // Keep the original approach until a later batch verifies the task outcome.
        }
        if s.last_experience_seq < tr.seq {
            continue;
        }
        let id = store::turn_rid(&tr.platform, &tr.turn_id);
        tx.delete("turns", &id)?;
        tx.index_task("turns", &id, "delete")?;
        removed.push(id);
    }
    t.store.commit(tx).await?;
    {
        let mut c = t.vecs.write().await;
        for id in &removed {
            c.turns.remove(id);
        }
    }
    let sessions: Vec<SessionRec> = t
        .store
        .select_as("SELECT * OMIT id FROM sessions", vec![])
        .await?;
    let mut tx = Tx::new();
    let mut dropped = 0;
    for s in sessions {
        if s.ended_at.is_some()
            && s.last_experience_seq >= s.turn_count
            && t.store
                .session_turns(&s.platform, &s.session)
                .await?
                .is_empty()
        {
            tx.delete("sessions", &store::session_rid(&s.platform, &s.session))?;
            dropped += 1;
        }
    }
    t.store.commit(tx).await?;
    t.index_notify.notify_one();
    Ok(
        json!({"purgedTurns":removed.len(),"purgedSessions":dropped,"retentionDays":days,"remainingTurns":t.store.count("turns",None).await?}),
    )
}

pub async fn run_jobs(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    _c: &Caller,
    body: Value,
) -> ApiResult<Value> {
    let jobs = match body.get("jobs") {
        None => vec![
            "profile".to_string(),
            "project_notes".into(),
            "merge".into(),
            "skills".into(),
            "purge".into(),
        ],
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ApiError::InvalidArgument("jobs must be strings".into()))
            })
            .collect::<ApiResult<Vec<_>>>()?,
        _ => return Err(ApiError::InvalidArgument("jobs must be an array".into())),
    };
    for j in &jobs {
        if !matches!(
            j.as_str(),
            "profile" | "project_notes" | "merge" | "skills" | "purge"
        ) {
            return Err(ApiError::InvalidArgument(format!("unknown job {j}")));
        }
    }
    let now = body["asOf"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(util::now_str);
    if util::parse_ts_fixed(&now).is_none() {
        return Err(ApiError::InvalidArgument("asOf must be RFC3339".into()));
    }
    let days = match body.get("retentionDays") {
        None => eng.cfg.buffer_retention_days,
        Some(v) => v
            .as_f64()
            .ok_or_else(|| ApiError::InvalidArgument("retentionDays must be numeric".into()))?,
    };
    if !days.is_finite() || days < 0.0 {
        return Err(ApiError::InvalidArgument(
            "retentionDays must be nonnegative".into(),
        ));
    }
    let _guard = t.extract_lock.lock().await;
    let mut results = serde_json::Map::new();
    for job in jobs {
        let value = match job.as_str() {
            "profile" => profile(eng, t, MemScope::global(), &now).await?,
            "project_notes" => {
                let facts = t
                    .store
                    .find_memories(
                        "type = 'fact' AND status = 'current' AND scope.kind = 'project'",
                        vec![],
                    )
                    .await?;
                let cutoff = util::ts_secs(&now) - eng.cfg.project_notes_recent_days * 86400;
                let mut projects = facts
                    .into_iter()
                    .filter(|m| util::ts_secs(&m.stated_at) >= cutoff)
                    .filter_map(|m| m.scope.project)
                    .collect::<Vec<_>>();
                projects.sort();
                projects.dedup();
                let mut notes = serde_json::Map::new();
                for project in projects {
                    notes.insert(
                        project.clone(),
                        profile(eng, t, MemScope::project(&project), &now).await?,
                    );
                }
                Value::Object(notes)
            }
            "merge" => merge(eng, t).await?,
            "skills" => skills(eng, t, &now).await?,
            "purge" => purge(t, &now, days).await?,
            _ => unreachable!(),
        };
        results.insert(job, value);
    }
    indexer::drain(eng, t).await?;
    Ok(json!({"status":"completed","jobs":results}))
}

pub async fn run_periodic(eng: Arc<Engine>, t: Arc<Tenant>) {
    let mut timer = tokio::time::interval(Duration::from_secs(60));
    loop {
        timer.tick().await;
        if let Err(e) = periodic_tick(&eng, &t).await {
            tracing::error!(tenant=%t.name,"maintenance: {e:#}");
        }
    }
}

async fn periodic_tick(eng: &Arc<Engine>, t: &Arc<Tenant>) -> Result<()> {
    let now = util::now();
    let timestamp = util::fmt_ts(&now);
    {
        let _guard = t.ingest_lock.lock().await;
        let sessions: Vec<SessionRec> = t
            .store
            .select_as("SELECT * OMIT id FROM sessions", vec![])
            .await?;
        let mut tx = Tx::new();
        let mut ended = false;
        for s in sessions {
            if s.ended_at.is_none()
                && !s.last_turn_at.is_empty()
                && now.timestamp() - util::ts_secs(&s.last_turn_at)
                    >= eng.cfg.session_idle_minutes * 60
            {
                tx.merge(
                    "sessions",
                    &store::session_rid(&s.platform, &s.session),
                    store::patch(json!({"endedAt":timestamp,"updatedAt":timestamp})),
                )?;
                ended = true;
            }
        }
        t.store.commit(tx).await?;
        if ended {
            t.extract_notify.notify_one();
        }
    }
    // Evaluation controls its clock via run_jobs(asOf); fresh tenants defer automatic LLM jobs.
    let hourly = t.store.get_doc("index_meta", "hourly_maintenance").await?;
    let hourly_last = hourly
        .as_ref()
        .and_then(|v| v["lastAt"].as_str())
        .map(util::ts_secs)
        .unwrap_or(now.timestamp());
    if hourly.is_none() || now.timestamp() - hourly_last >= 3600 {
        if hourly.is_some() {
            run_jobs(
                eng,
                t,
                &Caller {
                    tenant: t.name.clone(),
                    platform: "maintenance".into(),
                },
                json!({"jobs":["purge"]}),
            )
            .await?;
        }
        t.store
            .put_doc(
                "index_meta",
                "hourly_maintenance",
                &json!({"mid":"hourly_maintenance","lastAt":timestamp}),
            )
            .await?;
    }
    let previous = t.store.get_doc("index_meta", "daily_maintenance").await?;
    if previous.is_none() {
        t.store
            .put_doc(
                "index_meta",
                "daily_maintenance",
                &json!({"mid":"daily_maintenance","lastAt":timestamp}),
            )
            .await?;
        return Ok(());
    }
    let last = previous
        .as_ref()
        .and_then(|v| v["lastAt"].as_str())
        .map(util::ts_secs)
        .unwrap_or(0);
    if now.timestamp() - last >= 86400 && eng.llm.is_configured() {
        run_jobs(
            eng,
            t,
            &Caller {
                tenant: t.name.clone(),
                platform: "maintenance".into(),
            },
            json!({"jobs":["profile","project_notes","merge","skills"]}),
        )
        .await?;
        t.store
            .put_doc(
                "index_meta",
                "daily_maintenance",
                &json!({"mid":"daily_maintenance","lastAt":timestamp}),
            )
            .await?;
    }
    Ok(())
}
