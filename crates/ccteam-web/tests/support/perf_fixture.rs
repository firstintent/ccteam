use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ccteam_core::config::{self, CcteamConfig, ProjectEntry};
use ccteam_core::{CcteamPaths, ProjectState};

pub const SLUG: &str = "perf-fixture";
pub const PROGRESS_SOURCE_ROWS: usize = 1_000_000;
pub const LIVE_SESSIONS: usize = 50;
pub const STOPPED_SESSIONS: usize = 380;
pub const HISTORY_TURNS: usize = 10_000;
const TORN_AT_ROW: usize = PROGRESS_SOURCE_ROWS / 2;

#[derive(Debug, Clone)]
pub struct PerfFixture {
    pub paths: CcteamPaths,
    pub project_dir: PathBuf,
    pub progress_path: PathBuf,
    pub history_sid: String,
    pub stats: FixtureStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixtureStats {
    pub fixture_bytes: u64,
    pub progress_bytes: u64,
    pub progress_lines: usize,
    pub flood_rows: usize,
    pub turn_bytes: u64,
    pub torn_offset: u64,
    pub trailing_offset: u64,
    pub generation_time: Duration,
}

pub fn generate(root: &Path, seed: u64) -> PerfFixture {
    let started = Instant::now();
    let home = root.join("home");
    let paths = CcteamPaths {
        root: home.join(".ccteam"),
        projects_root: home.join("projects"),
    };
    let project_dir = paths.projects_root.join(SLUG);
    fs::create_dir_all(&project_dir).unwrap();

    let mut state = ProjectState::initial(SLUG.to_string());
    state.created_at = "2026-08-17T00:00:00Z".parse().unwrap();
    state.last_user_interaction_at = "2026-08-17T00:00:00Z".parse().unwrap();
    state.save(&paths.project_state(SLUG)).unwrap();
    config::save(
        &paths.root,
        &CcteamConfig {
            projects_root: Some(paths.projects_root.clone()),
            default_project: Some(SLUG.to_string()),
            projects: vec![ProjectEntry {
                slug: SLUG.to_string(),
                path: project_dir.clone(),
                host: "local".to_string(),
                remote_slug: None,
                remote_path: None,
                team: "dev".to_string(),
                installed_at: "2026-08-17T00:00:00Z".parse().unwrap(),
            }],
            ..CcteamConfig::default()
        },
    )
    .unwrap();

    let progress_path = paths.progress_jsonl(SLUG);
    fs::create_dir_all(progress_path.parent().unwrap()).unwrap();
    let progress = write_progress(&progress_path, seed);

    for sid in 1..=(LIVE_SESSIONS + STOPPED_SESSIONS) {
        write_meta(&project_dir, sid, sid <= LIVE_SESSIONS);
    }
    let history_sid = "s1".to_string();
    let turn_bytes = write_turns(&project_dir, &history_sid);
    // Exclude config.yaml because it intentionally embeds the absolute temp
    // root. The generated journal/session payload is path-independent.
    let fixture_bytes = progress.bytes + directory_bytes(&project_dir.join(".ccteam/chat"));
    let generation_time = started.elapsed();

    PerfFixture {
        paths,
        project_dir,
        progress_path,
        history_sid,
        stats: FixtureStats {
            fixture_bytes,
            progress_bytes: progress.bytes,
            progress_lines: PROGRESS_SOURCE_ROWS + 1,
            flood_rows: progress.flood_rows,
            turn_bytes,
            torn_offset: progress.torn_offset,
            trailing_offset: progress.trailing_offset,
            generation_time,
        },
    }
}

pub fn assert_known_corruption(fixture: &PerfFixture) {
    let mut file = File::open(&fixture.progress_path).unwrap();
    file.seek(SeekFrom::Start(fixture.stats.torn_offset))
        .unwrap();
    let mut torn = vec![0; 96];
    let read = std::io::Read::read(&mut file, &mut torn).unwrap();
    torn.truncate(read);
    assert!(std::str::from_utf8(&torn).is_err());
    assert!(torn.windows(8).any(|window| window == br#"{"event""#));

    file.seek(SeekFrom::Start(fixture.stats.trailing_offset))
        .unwrap();
    let mut trailing = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut trailing).unwrap();
    assert_eq!(trailing, b"{corrupt-trailing-line");
}

struct ProgressStats {
    bytes: u64,
    flood_rows: usize,
    torn_offset: u64,
    trailing_offset: u64,
}

fn write_progress(path: &Path, seed: u64) -> ProgressStats {
    let flood = templates("chat_tool_call_started", |variant| {
        format!(
                "\"role\":\"worker\",\"sid\":\"s{}\",\"tool\":\"Read\",\"path\":\"/workspace/src/performance/module_{variant:02}.rs\"",
                variant % LIVE_SESSIONS + 1
            )
    });
    let prompts = templates("chat_turn_user_prompt", |variant| {
        format!(
            "\"role\":\"worker\",\"sid\":\"s{}\",\"turn_id\":\"turn-{variant:02}\",\"prompt_excerpt\":\"inspect the generated performance fixture and report findings\"",
            variant % LIVE_SESSIONS + 1
        )
    });
    let completed = templates("chat_turn_completed", |variant| {
        format!(
            "\"role\":\"worker\",\"sid\":\"s{}\",\"turn_id\":\"turn-{variant:02}\",\"model\":\"claude-sonnet-4-5\",\"usage\":{{\"input_tokens\":1200,\"output_tokens\":300,\"cache_read_input_tokens\":8000,\"cache_creation_input_tokens\":0}},\"cost_usd\":0.0125",
            variant % LIVE_SESSIONS + 1
        )
    });
    let done = templates("agent_done", |variant| {
        format!(
            "\"role\":\"worker\",\"session_id\":\"s{}\",\"slug\":\"{SLUG}\",\"status\":\"completed\",\"vendor\":\"claude\",\"turn_id\":\"turn-{variant:02}\",\"cost_usd\":0.0125",
            variant % LIVE_SESSIONS + 1
        )
    });

    let file = File::create(path).unwrap();
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, file);
    let mut rng = XorShift64(seed);
    let mut offset = 0u64;
    let mut flood_rows = 0usize;
    let mut torn_offset = 0u64;
    for row in 0..PROGRESS_SOURCE_ROWS {
        if row == TORN_AT_ROW {
            torn_offset = offset;
            let mut torn = "{\"event\":\"chat_tool_call_started\",\"note\":\"配置："
                .as_bytes()
                .to_vec();
            torn.truncate(torn.len() - 2);
            writer.write_all(&torn).unwrap();
            offset += torn.len() as u64;
        }
        let choice = rng.next() % 100;
        let variant = (rng.next() as usize) % flood.len();
        let line = match choice {
            0..=84 => {
                flood_rows += 1;
                &flood[variant]
            }
            85..=91 => &prompts[variant],
            92..=98 => &completed[variant],
            _ => &done[variant],
        };
        writer.write_all(line).unwrap();
        offset += line.len() as u64;
    }
    let trailing_offset = offset;
    writer.write_all(b"{corrupt-trailing-line").unwrap();
    writer.flush().unwrap();
    let bytes = fs::metadata(path).unwrap().len();
    assert_eq!(
        bytes,
        trailing_offset + b"{corrupt-trailing-line".len() as u64
    );
    ProgressStats {
        bytes,
        flood_rows,
        torn_offset,
        trailing_offset,
    }
}

fn templates(event: &str, body: impl Fn(usize) -> String) -> Vec<Vec<u8>> {
    (0..64)
        .map(|variant| {
            format!(
                "{{\"event\":\"{event}\",{},\"ts\":\"2026-08-17T00:00:{:02}Z\",\"sequence_bucket\":{variant}}}\n",
                body(variant),
                variant % 60
            )
            .into_bytes()
        })
        .collect()
}

fn write_meta(project_dir: &Path, sid: usize, live: bool) {
    let chat_dir = project_dir.join(".ccteam/chat").join(format!("s{sid}"));
    fs::create_dir_all(&chat_dir).unwrap();
    let last_active = if live {
        "2026-08-17T00:00:00Z"
    } else {
        "2026-08-16T00:00:00Z"
    };
    let meta = serde_json::json!({
        "sid": format!("s{sid}"),
        "slug": SLUG,
        "vendor": "claude",
        "protocol": "stream-json",
        "role": "worker",
        "permission_mode": "skip",
        "owner": "user:web-api",
        "vendor_uuid": format!("fixture-session-{sid:04}"),
        "host": "local",
        "created_at": "2026-08-16T00:00:00Z",
        "last_active": last_active,
        "origin": "ccteam",
        "title": format!("Fixture session {sid}"),
        "turn_count": if sid == 1 { HISTORY_TURNS } else { 0 },
        "managed_by": "ccteam"
    });
    fs::write(
        chat_dir.join("meta.json"),
        serde_json::to_vec(&meta).unwrap(),
    )
    .unwrap();
}

fn write_turns(project_dir: &Path, sid: &str) -> u64 {
    let path = project_dir
        .join(".ccteam/chat")
        .join(sid)
        .join("turns.jsonl");
    let file = File::create(&path).unwrap();
    let mut writer = BufWriter::with_capacity(1024 * 1024, file);
    let templates = (0..64)
        .map(|variant| {
            format!(
                "{{\"turn_id\":\"history-{variant:02}\",\"ts\":\"2026-08-17T00:00:{:02}Z\",\"vendor\":\"claude\",\"role\":\"worker\",\"user\":\"deterministic fixture prompt {variant:02}\",\"assistant\":\"deterministic fixture response with enough content to exercise paging {variant:02}\",\"usage\":{{\"input_tokens\":1200,\"output_tokens\":300}},\"tool_calls\":[]}}\n",
                variant % 60
            )
            .into_bytes()
        })
        .collect::<Vec<_>>();
    for turn in 0..HISTORY_TURNS {
        writer
            .write_all(&templates[turn % templates.len()])
            .unwrap();
    }
    writer.flush().unwrap();
    fs::metadata(path).unwrap().len()
}

fn directory_bytes(path: &Path) -> u64 {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .map(|path| {
            if path.is_dir() {
                directory_bytes(&path)
            } else {
                fs::metadata(path).unwrap().len()
            }
        })
        .sum()
}

struct XorShift64(u64);

impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}
