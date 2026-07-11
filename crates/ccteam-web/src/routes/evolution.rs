//! `GET /api/v1/projects/{slug}/evolution` — read-only experience aggregate
//! (v0.8.24 C3). Honest empty state when no `experience.jsonl` data.

use std::collections::BTreeMap;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use ccteam_harness::execution::experience::{
    read_all_experience, ExperienceRecord, TurnExperience,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Identity;
use crate::state::AppState;

use super::sessions_api::project_not_visible;

/// Per-role or per-skill fingerprint bucket.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvolutionBucket {
    /// `role` or `skill`.
    pub kind: String,
    /// Role name or skill id.
    pub id: String,
    /// Content digest (12-hex for roles; full skill map digest when present).
    pub sha: String,
    /// Number of turn records attributed to this fingerprint.
    pub turn_count: u64,
    /// Mean cost USD across turns that reported cost (None if none priced).
    pub avg_cost_usd: Option<f64>,
    /// Sum of known costs.
    pub total_cost_usd: Option<f64>,
}

/// Project evolution summary (read-only).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvolutionSummary {
    pub slug: String,
    /// Total turn records in experience.jsonl.
    pub turn_records: u64,
    /// Total verdict records (v0.9 will fill; may be 0 now).
    pub verdict_records: u64,
    pub roles: Vec<EvolutionBucket>,
    pub skills: Vec<EvolutionBucket>,
    /// True when the experience file is missing or empty.
    pub empty: bool,
}

/// `GET /api/v1/projects/{slug}/evolution`
#[utoipa::path(
    get,
    path = "/api/v1/projects/{slug}/evolution",
    tag = "projects",
    params(("slug" = String, Path, description = "Project slug")),
    responses(
        (status = 200, description = "Evolution summary (may be empty)", body = EvolutionSummary),
        (status = 403, description = "Project not visible"),
        (status = 404, description = "Unknown project"),
    ),
)]
pub(crate) async fn handle_evolution(
    State(app): State<AppState>,
    Extension(identity): Extension<Identity>,
    Path(slug): Path<String>,
) -> Response {
    if !crate::routes::api_v1::can_see_project(&app, &identity, &slug) {
        return project_not_visible(&slug);
    }
    let project_dir = app.paths.project_dir(&slug);
    // Honest empty when the project dir does not exist yet.
    if !project_dir.exists() {
        let summary = EvolutionSummary {
            slug,
            turn_records: 0,
            verdict_records: 0,
            roles: vec![],
            skills: vec![],
            empty: true,
        };
        return (StatusCode::OK, Json(summary)).into_response();
    }

    let records = match read_all_experience(&project_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(%slug, error = %e, "evolution: read experience failed");
            Vec::new()
        }
    };

    let mut turn_records = 0u64;
    let mut verdict_records = 0u64;
    // key = (kind, id, sha)
    let mut role_acc: BTreeMap<(String, String), Acc> = BTreeMap::new();
    let mut skill_acc: BTreeMap<(String, String), Acc> = BTreeMap::new();

    for rec in &records {
        match rec {
            ExperienceRecord::Turn(t) => {
                turn_records += 1;
                accumulate_turn(t, &mut role_acc, &mut skill_acc);
            }
            ExperienceRecord::Verdict(_) => {
                verdict_records += 1;
            }
        }
    }

    let roles = finish_buckets("role", role_acc);
    let skills = finish_buckets("skill", skill_acc);
    let empty = turn_records == 0 && verdict_records == 0;

    let summary = EvolutionSummary {
        slug,
        turn_records,
        verdict_records,
        roles,
        skills,
        empty,
    };
    (StatusCode::OK, Json(summary)).into_response()
}

#[derive(Default)]
struct Acc {
    turns: u64,
    cost_sum: f64,
    cost_n: u64,
}

fn accumulate_turn(
    t: &TurnExperience,
    roles: &mut BTreeMap<(String, String), Acc>,
    skills: &mut BTreeMap<(String, String), Acc>,
) {
    if !t.role.is_empty() {
        let sha = t.role_sha.clone().unwrap_or_else(|| "unknown".into());
        let e = roles.entry((t.role.clone(), sha)).or_default();
        e.turns += 1;
        if let Some(c) = t.cost_usd {
            e.cost_sum += c;
            e.cost_n += 1;
        }
    }
    if let Some(map) = &t.skills_sha {
        for (id, sha) in map {
            let e = skills.entry((id.clone(), sha.clone())).or_default();
            e.turns += 1;
            if let Some(c) = t.cost_usd {
                e.cost_sum += c;
                e.cost_n += 1;
            }
        }
    }
}

fn finish_buckets(kind: &str, acc: BTreeMap<(String, String), Acc>) -> Vec<EvolutionBucket> {
    let mut out: Vec<EvolutionBucket> = acc
        .into_iter()
        .map(|((id, sha), a)| EvolutionBucket {
            kind: kind.to_string(),
            id,
            sha,
            turn_count: a.turns,
            avg_cost_usd: if a.cost_n > 0 {
                Some(a.cost_sum / a.cost_n as f64)
            } else {
                None
            },
            total_cost_usd: if a.cost_n > 0 { Some(a.cost_sum) } else { None },
        })
        .collect();
    out.sort_by(|a, b| b.turn_count.cmp(&a.turn_count).then(a.id.cmp(&b.id)));
    out
}
